//! axum 服务：手机页面、WS 配对会话、文件上传下载。

use crate::pairing::Pairing;
use crate::protocol::{WsIn, WsOut};
use crate::{
    CoreConfig, CoreEvent, Direction, TransferInfo, TransferStatus, UiRequest,
};
use axum::body::Body;
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{DefaultBodyLimit, Multipart, Path, Query, State};
use axum::http::{header, StatusCode};
use axum::response::{Html, IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use futures_util::{SinkExt, StreamExt};
use std::collections::HashMap;
use std::net::IpAddr;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tokio::io::AsyncWriteExt;
use tokio::sync::Notify;
use tokio_util::io::ReaderStream;

/// 当前活跃的手机连接（同一时间只允许一台）
#[derive(Clone)]
pub struct Conn {
    pub out: tokio::sync::mpsc::UnboundedSender<String>,
    pub closed: Arc<AtomicBool>,
    pub kick: Arc<Notify>,
}

pub(crate) struct Batch {
    dir: Arc<tokio::sync::Mutex<Option<PathBuf>>>,
    notify: Arc<Notify>,
    created: Instant,
}

pub struct AppState {
    pub(crate) cfg: CoreConfig,
    pub(crate) self_id: String,
    /// 配置目录（HTTPS 证书等持久化文件）
    pub(crate) data_dir: std::path::PathBuf,
    pub(crate) pairing: Mutex<Pairing>,
    pub(crate) conn: Mutex<Option<Conn>>,
    /// PC→手机待下载文件：id -> (路径, 名称, 大小)
    pub(crate) offers: Mutex<HashMap<String, (PathBuf, String, u64)>>,
    pub(crate) transfers: Mutex<Vec<TransferInfo>>,
    /// 进度节流
    pub(crate) progress_last: Mutex<HashMap<String, Instant>>,
    pub(crate) batches: Mutex<HashMap<String, Batch>>,
    pub(crate) event_tx: Sender<CoreEvent>,
    // ---- 飞鸽（PC↔PC，全链路 Noise 加密）----
    /// UDP 广播听到的电脑（发现表）
    pub(crate) pigeon_peers: Mutex<HashMap<String, crate::pigeon::PeerRecord>>,
    /// 已建立的加密链路：peer_id -> 链路
    pub(crate) pigeon_conns: Mutex<HashMap<String, crate::pigeon::PigeonLink>>,
    /// 正在发起连接的 peer（防重入）
    pub(crate) pigeon_connecting: Mutex<std::collections::HashSet<String>>,
    /// 正在接收的加密推送文件：offer_id -> 组装状态
    pub(crate) recv_files: tokio::sync::Mutex<HashMap<String, RecvFileState>>,
    /// 待确认的新设备：peer_id -> (设备名, 指纹)
    pub(crate) pending_trusts: Mutex<HashMap<String, (String, String)>>,
    /// 本会话内被用户拒绝的设备（不再重复弹确认）
    pub(crate) trust_rejected: Mutex<std::collections::HashSet<String>>,
}

/// 接收加密推送文件的组装状态
pub(crate) struct RecvFileState {
    pub row_id: String,
    pub file: tokio::fs::File,
    pub written: u64,
    pub dest: PathBuf,
}

impl AppState {
    pub(crate) fn emit(&self, ev: CoreEvent) {
        let _ = self.event_tx.send(ev);
    }

    pub(crate) fn push_ws(&self, out: &WsOut) {
        if let Ok(json) = serde_json::to_string(out)
            && let Some(conn) = self.conn.lock().unwrap().as_ref() {
                let _ = conn.out.send(json);
            }
    }

    pub(crate) fn new_transfer(&self, t: TransferInfo) {
        self.transfers.lock().unwrap().push(t.clone());
        self.emit(CoreEvent::TransferUpdate { t: Box::new(t) });
    }

    pub(crate) fn update_transfer(&self, id: &str, transferred: u64, size: Option<u64>, status: TransferStatus, path: Option<PathBuf>) {
        let mut list = self.transfers.lock().unwrap();
        if let Some(t) = list.iter_mut().find(|t| t.id == id) {
            t.transferred = transferred;
            if let Some(s) = size {
                t.size = s;
            }
            if t.status != TransferStatus::Done {
                t.status = status;
            }
            if let Some(p) = path {
                t.path = Some(p);
            }
            self.emit(CoreEvent::TransferUpdate { t: Box::new(t.clone()) });
        }
    }

    /// UI 进度节流（120ms），done 永远立即推送
    pub(crate) fn progress_throttled(&self, id: &str, transferred: u64, done: bool) {
        let now = Instant::now();
        let should = {
            let mut m = self.progress_last.lock().unwrap();
            match m.get(id) {
                Some(t) if now - *t < Duration::from_millis(120) && !done => false,
                _ => {
                    m.insert(id.to_string(), now);
                    true
                }
            }
        };
        if should {
            self.update_transfer(id, transferred, None, TransferStatus::Active, None);
        }
    }
}

fn lan_ip() -> Option<IpAddr> {
    let addrs = if_addrs::get_if_addrs().ok()?;
    let v4: Vec<IpAddr> = addrs
        .into_iter()
        .map(|i| i.ip())
        .filter(|ip| matches!(ip, IpAddr::V4(_)))
        .collect();
    v4.iter()
        .find(|ip| matches!(ip, IpAddr::V4(v) if v.is_private()))
        .or_else(|| v4.iter().find(|ip| !ip.is_loopback()))
        .copied()
}

pub async fn serve(cfg: CoreConfig, event_tx: Sender<CoreEvent>, ui_rx: Receiver<UiRequest>) {
    let save_dir = cfg.save_dir.clone();
    let _ = std::fs::create_dir_all(&save_dir);

    let ip = lan_ip().unwrap_or_else(|| "127.0.0.1".parse().unwrap());
    let addr = format!("{}:{}", ip, cfg.port);

    let data_dir = dirs::config_dir().map(|d| d.join("yunshu")).unwrap_or_else(std::env::temp_dir);
    let state = Arc::new(AppState {
        cfg: cfg.clone(),
        self_id: uuid::Uuid::new_v4().simple().to_string(),
        data_dir,
        pairing: Mutex::new(Pairing::default()),
        conn: Mutex::new(None),
        offers: Mutex::new(HashMap::new()),
        transfers: Mutex::new(Vec::new()),
        progress_last: Mutex::new(HashMap::new()),
        batches: Mutex::new(HashMap::new()),
        event_tx,
        pigeon_peers: Mutex::new(HashMap::new()),
        pigeon_conns: Mutex::new(HashMap::new()),
        pigeon_connecting: Mutex::new(std::collections::HashSet::new()),
        recv_files: tokio::sync::Mutex::new(HashMap::new()),
        pending_trusts: Mutex::new(HashMap::new()),
        trust_rejected: Mutex::new(std::collections::HashSet::new()),
    });

    state.emit(CoreEvent::ServerStarted { addr: addr.clone() });

    // 飞鸽发现（PC↔PC）
    {
        let st = state.clone();
        tokio::spawn(crate::pigeon::run_discovery(st));
    }

    // 每 60 秒轮换二维码 token
    {
        let st = state.clone();
        let ip_str = ip.to_string();
        let https_port = cfg.https_port;
        tokio::spawn(async move {
            loop {
                let token = uuid::Uuid::new_v4().simple().to_string();
                st.pairing.lock().unwrap().add_token(token.clone());
                let url = format!("https://{ip_str}:{https_port}/pair?t={token}");
                tracing::info!("二维码刷新: {url}");
                st.emit(CoreEvent::QrUrl(url));
                tokio::time::sleep(Duration::from_secs(60)).await;
            }
        });
    }

    // UI 请求处理（阻塞读 std channel）
    {
        let st = state.clone();
        std::thread::spawn(move || loop {
            match ui_rx.recv() {
                Ok(UiRequest::SendFiles(files)) => {
                    if !st.pairing.lock().unwrap().has_session() {
                        st.emit(CoreEvent::PairFailed { reason: "没有已连接的手机，请先扫码配对".into() });
                        continue;
                    }
                    for f in files {
                        offer_file(&st, f);
                    }
                }
                Ok(UiRequest::SendText(text)) => {
                    if !st.pairing.lock().unwrap().has_session() {
                        st.emit(CoreEvent::PairFailed { reason: "没有已连接的手机，请先扫码配对".into() });
                        continue;
                    }
                    let chars = text.chars().count();
                    st.push_ws(&WsOut::PcText { text: text.clone() });
                    st.new_transfer(TransferInfo::text(uuid::Uuid::new_v4().simple().to_string(), Direction::ToPhone, chars, Some("phone".into())));
                }
                Ok(UiRequest::SetSaveDir { batch, dir }) => {
                    if let Some(b) = st.batches.lock().unwrap().get(&batch) {
                        *b.dir.blocking_lock() = dir;
                        b.notify.notify_waiters();
                    }
                }
                Ok(UiRequest::DisconnectDevice) => {
                    st.pairing.lock().unwrap().reset();
                    if let Some(conn) = st.conn.lock().unwrap().as_ref() {
                        conn.closed.store(true, Ordering::Relaxed);
                        conn.kick.notify_waiters();
                    }
                }
                Ok(UiRequest::Toast(text)) => {
                    st.emit(CoreEvent::Toast { text });
                }
                Ok(UiRequest::PigeonSendText { peer_id, text }) => {
                    let name = st
                        .pigeon_conns
                        .lock()
                        .unwrap()
                        .get(&peer_id)
                        .map(|l| l.name.clone())
                        .unwrap_or_default();
                    let chars = text.chars().count();
                    let sent = crate::pigeon::try_push(&st, &peer_id, crate::protocol::PigeonWire::PcText { text });
                    if sent {
                        st.new_transfer(TransferInfo::text(uuid::Uuid::new_v4().simple().to_string(), Direction::ToPhone, chars, Some(peer_id.to_string())));
                    } else {
                        st.emit(CoreEvent::Toast { text: format!("「{name}」已断开，无法发送") });
                    }
                }
                Ok(UiRequest::PigeonSendFiles { peer_id, paths }) => {
                    let connected = st.pigeon_conns.lock().unwrap().contains_key(&peer_id);
                    if !connected {
                        st.emit(CoreEvent::Toast { text: "对方已断开，无法发送".into() });
                        continue;
                    }
                    for f in paths {
                        offer_to_pigeon(&st, &peer_id, f);
                    }
                }
                Ok(UiRequest::PigeonDisconnect { peer_id }) => {
                    if let Some(link) = st.pigeon_conns.lock().unwrap().get(&peer_id) {
                        link.closed.store(true, Ordering::Relaxed);
                    }
                    crate::pigeon::drop_link(&st, &peer_id);
                }
                Ok(UiRequest::BroadcastText(text)) => {
                    let n = {
                        let conns = st.pigeon_conns.lock().unwrap();
                        let mut sent = 0;
                        for link in conns.values() {
                            if link
                                .out
                                .try_send(crate::pigeon::PigeonOut::Control(crate::protocol::PigeonWire::BroadcastText { text: text.clone() }))
                                .is_ok()
                            {
                                sent += 1;
                            }
                        }
                        sent
                    };
                    if n > 0 {
                        st.new_transfer(TransferInfo::text(
                            uuid::Uuid::new_v4().simple().to_string(),
                            Direction::ToPhone,
                            text.chars().count(),
                            Some("broadcast".into()),
                        ));
                        st.emit(CoreEvent::ChatBroadcast {
                            peer_id: "me".into(),
                            peer_name: st.cfg.pc_name.clone(),
                            text,
                        });
                    } else {
                        st.emit(CoreEvent::Toast { text: "没有在线电脑可广播".into() });
                    }
                }
                Ok(UiRequest::PigeonTrust { peer_id, fingerprint }) => {
                    let mut pending = st.pending_trusts.lock().unwrap();
                    match pending.get(&peer_id) {
                        Some((_, fp)) if fp == &fingerprint => {
                            pending.remove(&peer_id);
                            st.trust_rejected.lock().unwrap().remove(&peer_id);
                            crate::pigeon::pin_peer(&st, &peer_id, &fingerprint);
                            st.emit(CoreEvent::Toast {
                                text: format!("已信任设备（指纹 {}…）", &fingerprint[..16.min(fingerprint.len())]),
                            });
                        }
                        _ => st.emit(CoreEvent::Toast { text: "确认已过期，请等待对方重新连接".into() }),
                    }
                }
                Ok(UiRequest::PigeonRejectTrust { peer_id }) => {
                    st.pending_trusts.lock().unwrap().remove(&peer_id);
                    st.trust_rejected.lock().unwrap().insert(peer_id);
                }
                Ok(UiRequest::PigeonAcceptFile { peer_id, offer_id }) => {
                    // 接受后由对方在加密通道内推送文件，这里只需答复
                    crate::pigeon::try_push(&st, &peer_id, crate::protocol::PigeonWire::FileDecision { id: offer_id, accepted: true });
                }
                Ok(UiRequest::PigeonRejectFile { peer_id, offer_id }) => {
                    crate::pigeon::try_push(&st, &peer_id, crate::protocol::PigeonWire::FileDecision { id: offer_id, accepted: false });
                }
                Err(_) => break,
            }
        });
    }

    let router = Router::new()
        .route("/", get(serve_page))
        .route("/pair", get(serve_page))
        .route("/ws", get(ws_handler))
        .route("/pigeon-ws", get(pigeon_ws_handler))
        .route("/file/{id}", get(download))
        .route("/upload", post(upload))
        .layer(DefaultBodyLimit::max(4 * 1024 * 1024 * 1024))
        .with_state(state.clone());

    // HTTPS：自签证书（手机端一次信任后获得安全上下文，剪贴板 API 可用）
    let https_addr = std::net::SocketAddr::from(([0, 0, 0, 0], cfg.https_port));
    match https_config(&state.data_dir).await {
        Ok(tls) => {
            let r = router.clone();
            tokio::spawn(async move {
                let server = axum_server::bind_rustls(https_addr, tls);
                tracing::info!("HTTPS 已启动: https://0.0.0.0:{}（手机首次访问需信任自签证书）", https_addr.port());
                if let Err(e) = server.serve(r.into_make_service()).await {
                    tracing::error!("HTTPS 服务异常: {e}");
                }
            });
        }
        Err(e) => tracing::error!("HTTPS 证书准备失败（仅提供 HTTP）: {e}"),
    }

    match tokio::net::TcpListener::bind(("0.0.0.0", cfg.port)).await {
        Ok(listener) => {
            tracing::info!("云书服务已启动: http://{addr}");
            if let Err(e) = axum::serve(listener, router).await {
                tracing::error!("服务异常退出: {e}");
                state.emit(CoreEvent::ServerError { reason: format!("服务异常退出：{e}") });
            }
        }
        Err(e) => {
            tracing::error!("端口 {} 绑定失败: {e}", cfg.port);
            state.emit(CoreEvent::ServerError { reason: format!("端口 {} 被占用：{e}", cfg.port) });
        }
    }
}

/// 自签证书：首次生成并持久化（含所有本机 IPv4 SAN + localhost）
async fn https_config(data_dir: &std::path::Path) -> Result<axum_server::tls_rustls::RustlsConfig, String> {
    use rcgen::{CertificateParams, KeyPair, SanType};
    let _ = std::fs::create_dir_all(data_dir);
    let cert_p = data_dir.join("https_cert.pem");
    let key_p = data_dir.join("https_key.pem");

    if !(cert_p.exists() && key_p.exists()) {
        let mut subjects = vec!["yunshu.local".to_string(), "localhost".to_string()];
        if let Ok(addrs) = if_addrs::get_if_addrs() {
            for i in addrs {
                if let IpAddr::V4(v4) = i.ip() {
                    subjects.push(format!("{v4}"));
                }
            }
        }
        let mut params = CertificateParams::new(subjects.clone()).map_err(|e| e.to_string())?;
        params.subject_alt_names = subjects
            .iter()
            .filter_map(|s| if let Ok(ip) = s.parse::<IpAddr>() { Some(SanType::IpAddress(ip)) } else { None })
            .collect();
        params
            .subject_alt_names
            .push(SanType::DnsName("yunshu.local".try_into().map_err(|e| format!("{e}"))?));
        let key = KeyPair::generate().map_err(|e| e.to_string())?;
        let cert = params.self_signed(&key).map_err(|e| e.to_string())?;
        std::fs::write(&cert_p, cert.pem()).map_err(|e| e.to_string())?;
        std::fs::write(&key_p, key.serialize_pem()).map_err(|e| e.to_string())?;
        tracing::info!("已生成 HTTPS 自签证书（{}）", cert_p.display());
    }

    axum_server::tls_rustls::RustlsConfig::from_pem_file(&cert_p, &key_p)
        .await
        .map_err(|e| format!("加载证书失败: {e}"))
}

async fn serve_page() -> Response {
    (
        [(header::CACHE_CONTROL, "no-store")],
        Html(include_str!("../assets/phone.html")),
    )
        .into_response()
}

async fn ws_handler(State(st): State<Arc<AppState>>, ws: WebSocketUpgrade) -> Response {
    ws.on_upgrade(move |socket| handle_socket(st, socket))
}

async fn pigeon_ws_handler(State(st): State<Arc<AppState>>, ws: WebSocketUpgrade) -> Response {
    ws.on_upgrade(move |socket| handle_pigeon_socket(st, socket))
}

/// 飞鸽服务器侧：对方听到我的广播后连进来
async fn handle_pigeon_socket(st: Arc<AppState>, socket: WebSocket) {
    use crate::protocol::PigeonWire;

    let (mut sink, mut stream) = socket.split();

    // ---- Noise XX 握手（本端是 responder）----
    let mut hs = match crate::pigeon::new_handshake(false) {
        Ok(h) => h,
        Err(e) => {
            tracing::error!("Noise 初始化失败: {e}");
            return;
        }
    };
    // <- msg1
    let m1 = match tokio::time::timeout(Duration::from_secs(10), stream.next()).await {
        Ok(Some(Ok(Message::Binary(b)))) => b,
        _ => return,
    };
    let mut buf = vec![0u8; 65535];
    let n = match hs.read_message(&m1, &mut buf) {
        Ok(n) => n,
        Err(_) => return,
    };
    let _ = &buf[..n];
    // -> msg2
    let mut m2 = vec![0u8; 65535];
    let n2 = match hs.write_message(&[], &mut m2) {
        Ok(n) => n,
        Err(_) => return,
    };
    if sink.send(Message::Binary(m2[..n2].to_vec().into())).await.is_err() {
        return;
    }
    // <- msg3
    let m3 = match tokio::time::timeout(Duration::from_secs(10), stream.next()).await {
        Ok(Some(Ok(Message::Binary(b)))) => b,
        _ => return,
    };
    if hs.read_message(&m3, &mut buf).is_err() {
        return;
    }
    let noise = match hs.into_transport_mode() {
        Ok(t) => t,
        Err(_) => return,
    };
    let remote_static = noise.get_remote_static().map(|s| s.to_vec()).unwrap_or_default();
    let noise = Arc::new(Mutex::new(noise));

    // ---- 加密通道内：hello（id 必须在发现表里）----
    let first = match tokio::time::timeout(Duration::from_secs(10), stream.next()).await {
        Ok(Some(Ok(Message::Binary(b)))) => b,
        _ => return,
    };
    let hello = match crate::pigeon::open_msg(&noise, &first) {
        Ok(crate::pigeon::PigeonIn::Control(PigeonWire::PigeonHello { id, name, .. })) => (id, name),
        _ => return,
    };
    let (peer_id, peer_name) = hello;
    let known_ok = st
        .pigeon_peers
        .lock()
        .unwrap()
        .get(&peer_id)
        .map(|p| p.last_seen.elapsed() < crate::pigeon::PEER_TTL + Duration::from_secs(60))
        .unwrap_or(false);
    let welcome = if !known_ok {
        PigeonWire::PigeonWelcome { ok: false, reason: Some("不在线上发现列表中".into()), pc_name: None }
    } else if st.trust_rejected.lock().unwrap().contains(&peer_id) {
        PigeonWire::PigeonWelcome { ok: false, reason: Some("你已被拒绝接入".into()), pc_name: None }
    } else if !crate::pigeon::is_pinned(&st, &peer_id, &remote_static) {
        let fp = crate::pigeon::fingerprint(&remote_static);
        st.pending_trusts.lock().unwrap().insert(peer_id.clone(), (peer_name.clone(), fp.clone()));
        tracing::info!("新设备「{peer_name}」待确认（指纹 {fp}）");
        st.emit(CoreEvent::PigeonTrustRequest {
            peer_id: peer_id.clone(),
            name: peer_name.clone(),
            fingerprint: fp,
        });
        PigeonWire::PigeonWelcome { ok: false, reason: Some("新设备待确认".into()), pc_name: None }
    } else {
        PigeonWire::PigeonWelcome { ok: true, reason: None, pc_name: Some(st.cfg.pc_name.clone()) }
    };
    let frame = match crate::pigeon::seal_msg(&noise, &crate::pigeon::PigeonOut::Control(welcome)) {
        Ok(f) => f,
        Err(_) => return,
    };
    if sink.send(Message::Binary(frame.into())).await.is_err() {
        return;
    }
    if !known_ok {
        return;
    }
    if st.pigeon_conns.lock().unwrap().contains_key(&peer_id) {
        // 双方同时连对方时的竞争：拒绝后来者
        tracing::info!("「{peer_name}」的重复链路被拒绝（已有链路）");
        return;
    }

    // ---- 注册链路 + 启动加密泵 ----
    let (plain_out_tx, plain_out_rx) = tokio::sync::mpsc::channel::<crate::pigeon::PigeonOut>(64);
    st.pigeon_conns.lock().unwrap().insert(
        peer_id.clone(),
        crate::pigeon::PigeonLink { name: peer_name.clone(), out: plain_out_tx.clone(), closed: Arc::new(AtomicBool::new(false)) },
    );
    tracing::info!("「{peer_name}」连入加密飞鸽链路（Noise XX，静态密钥已校验/固定）");
    crate::pigeon::emit_peers(&st);

    let (cipher_tx, mut cipher_rx) = tokio::sync::mpsc::channel::<Vec<u8>>(64);
    let (frame_tx, frame_rx) = tokio::sync::mpsc::channel::<Vec<u8>>(64);
    let pump = tokio::spawn(crate::pigeon::pump(
        st.clone(),
        peer_id.clone(),
        peer_name.clone(),
        noise.clone(),
        frame_rx,
        plain_out_rx,
        cipher_tx,
    ));
    // ws 写桥：pump 加密帧 -> sink
    let writer = tokio::spawn(async move {
        while let Some(frame) = cipher_rx.recv().await {
            if sink.send(Message::Binary(frame.into())).await.is_err() {
                break;
            }
        }
        let _ = sink.close().await;
    });
    // ws 读循环：Binary -> pump
    let mut drop_reason = "读流结束（对端关闭）";
    while let Some(Ok(msg)) = stream.next().await {
        let closed = st
            .pigeon_conns
            .lock()
            .unwrap()
            .get(&peer_id)
            .map(|l| l.closed.load(Ordering::Relaxed))
            .unwrap_or(true);
        if closed {
            drop_reason = "本端关闭标志";
            break;
        }
        match msg {
            Message::Binary(b) => {
                if frame_tx.send(b.to_vec()).await.is_err() {
                    drop_reason = "加密泵已退出";
                    break;
                }
            }
            Message::Close(_) => {
                drop_reason = "对端发送了 Close";
                break;
            }
            _ => continue,
        }
    }

    crate::pigeon::drop_link(&st, &peer_id);
    pump.abort();
    writer.abort();
    tracing::info!("「{peer_name}」的加密飞鸽链路断开（{drop_reason}）");
    crate::pigeon::emit_peers(&st);
}

fn offer_to_pigeon(st: &Arc<AppState>, peer_id: &str, path: PathBuf) {
    let Ok(meta) = std::fs::metadata(&path) else {
        st.emit(CoreEvent::ServerError { reason: format!("无法读取文件：{}", path.display()) });
        return;
    };
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| "未命名文件".into());
    let id = uuid::Uuid::new_v4().simple().to_string();
    let size = meta.len();
    st.offers.lock().unwrap().insert(id.clone(), (path, name.clone(), size));
    st.new_transfer(TransferInfo::file(id.clone(), Direction::ToPhone, name.clone(), size, Some(peer_id.to_string())));
    crate::pigeon::try_push(st, peer_id, crate::protocol::PigeonWire::PcFile { id, name, size });
}

async fn handle_socket(st: Arc<AppState>, mut socket: WebSocket) {
    // 同一时间只允许一台手机
    if st.conn.lock().unwrap().is_some() {
        if let Ok(json) = serde_json::to_string(&WsOut::PairResult {
            ok: false, session: None,
            reason: Some("电脑上已有其他设备连接".into()),
            pc_name: None,
        }) {
            let _ = socket.send(Message::text(json)).await;
        }
        return;
    }
    let (out_tx, mut out_rx) = tokio::sync::mpsc::unbounded_channel::<String>();
    {
        let mut conn_slot = st.conn.lock().unwrap();
        *conn_slot = Some(Conn {
            out: out_tx.clone(),
            closed: Arc::new(AtomicBool::new(false)),
            kick: Arc::new(Notify::new()),
        });
    }
    let (closed, kick) = {
        let conn = st.conn.lock().unwrap().clone().expect("conn 刚刚设置");
        (conn.closed, conn.kick)
    };
    let out_tx2 = out_tx.clone();

    let (mut sink, mut stream) = socket.split();
    let (closed_w, kick_w) = (closed.clone(), kick.clone());
    let writer = tokio::spawn(async move {
        loop {
            tokio::select! {
                msg = out_rx.recv() => match msg {
                    Some(m) => {
                        if sink.send(Message::text(m)).await.is_err() { break; }
                    }
                    None => break,
                },
                _ = kick_w.notified() => {
                    if closed_w.load(Ordering::Relaxed) {
                        let _ = sink.send(Message::Close(None)).await;
                        break;
                    }
                }
            }
        }
    });

    let mut paired = false;
    while let Some(Ok(msg)) = stream.next().await {
        if closed.load(Ordering::Relaxed) {
            break;
        }
        let text = match msg {
            Message::Text(t) => t,
            Message::Close(_) => break,
            _ => continue,
        };
        let Ok(input) = serde_json::from_str::<WsIn>(text.as_str()) else {
            continue;
        };
        match input {
            WsIn::Hello { token, device } => {
                let mut p = st.pairing.lock().unwrap();
                if !p.token_valid(&token) {
                    let _ = out_tx2.send(serde_json::to_string(&WsOut::PairResult {
                        ok: false, session: None,
                        reason: Some("二维码已过期，请在电脑上刷新后重新扫码".into()),
                        pc_name: None,
                    }).unwrap());
                    break;
                }
                if p.has_session() {
                    let _ = out_tx2.send(serde_json::to_string(&WsOut::PairResult {
                        ok: false, session: None,
                        reason: Some("已有其他设备连接，请先在电脑端断开".into()),
                        pc_name: None,
                    }).unwrap());
                    break;
                }
                let device = if device.trim().is_empty() { "手机".into() } else { device.trim().to_string() };
                let code = p.begin_pending(device.clone());
                drop(p);
                tracing::info!("手机「{device}」发起配对，配对码 {code}");
                st.emit(CoreEvent::PairPending { code, device });
            }
            WsIn::Pair { code } => match st.pairing.lock().unwrap().verify(code.trim()) {
                Ok((session, device)) => {
                    paired = true;
                    let pc_name = st.cfg.pc_name.clone();
                    tracing::info!("「{device}」配对成功");
                    let _ = out_tx2.send(serde_json::to_string(&WsOut::PairResult {
                        ok: true, session: Some(session), reason: None, pc_name: Some(pc_name),
                    }).unwrap());
                    st.emit(CoreEvent::PairOk { device });
                }
                Err(reason) => {
                    tracing::info!("配对失败：{reason}");
                    let _ = out_tx2.send(serde_json::to_string(&WsOut::PairResult {
                        ok: false, session: None, reason: Some(reason.into()), pc_name: None,
                    }).unwrap());
                    st.emit(CoreEvent::PairFailed { reason: reason.into() });
                }
            },
            WsIn::SendText { text } if paired => {
                let text = text.trim().to_string();
                if text.is_empty() {
                    continue;
                }
                crate::clipboard::copy_text(&text);
                let chars = text.chars().count();
                st.new_transfer(TransferInfo::text(uuid::Uuid::new_v4().simple().to_string(), Direction::FromPhone, chars, Some("phone".into())));
                st.emit(CoreEvent::TextFromPhone { text });
            }
            WsIn::SendText { .. } => {
                let _ = out_tx2.send(serde_json::to_string(&WsOut::PairResult {
                    ok: false, session: None, reason: Some("尚未完成配对".into()), pc_name: None,
                }).unwrap());
            }
        }
    }

    // 清理
    *st.conn.lock().unwrap() = None;
    st.pairing.lock().unwrap().reset();
    writer.abort();
    st.emit(CoreEvent::PeerDisconnected);
}

fn offer_file(st: &Arc<AppState>, path: PathBuf) {
    let Ok(meta) = std::fs::metadata(&path) else {
        st.emit(CoreEvent::ServerError { reason: format!("无法读取文件：{}", path.display()) });
        return;
    };
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| "未命名文件".into());
    let id = uuid::Uuid::new_v4().simple().to_string();
    let size = meta.len();
    st.offers.lock().unwrap().insert(id.clone(), (path, name.clone(), size));
    st.new_transfer(TransferInfo::file(id.clone(), Direction::ToPhone, name.clone(), size, Some("phone".into())));
    st.push_ws(&WsOut::PcFile { id, name, size });
}

async fn download(
    State(st): State<Arc<AppState>>,
    Path(id): Path<String>,
    Query(q): Query<HashMap<String, String>>,
) -> Response {
    let Some(s) = q.get("s") else {
        return err_page(StatusCode::UNAUTHORIZED, "未配对");
    };
    // 该端点现在只服务手机浏览器（电脑间文件走加密推送通道）
    if !st.pairing.lock().unwrap().session_valid(s) {
        return err_page(StatusCode::UNAUTHORIZED, "会话已失效，请重新配对");
    }
    let Some((path, name, size)) = st.offers.lock().unwrap().get(&id).cloned() else {
        return err_page(StatusCode::NOT_FOUND, "文件不存在或已被清理");
    };
    let Ok(file) = tokio::fs::File::open(&path).await else {
        return err_page(StatusCode::NOT_FOUND, "文件读取失败");
    };

    st.update_transfer(&id, 0, None, TransferStatus::Active, None);

    let total = Arc::new(AtomicU64::new(0));
    let st2 = st.clone();
    let id2 = id.clone();
    let total2 = total.clone();
    let stream = ReaderStream::new(file).map(move |chunk| match chunk {
        Ok(b) => {
            let n = total2.fetch_add(b.len() as u64, Ordering::Relaxed) + b.len() as u64;
            let done = n >= size;
            st2.progress_throttled(&id2, n, done);
            if done {
                st2.update_transfer(&id2, n, None, TransferStatus::Done, None);
            }
            Ok(b)
        }
        Err(e) => {
            st2.update_transfer(&id2, 0, None, TransferStatus::Failed, None);
            Err(e)
        }
    });

    let encoded = urlencoding::encode(&name);
    let headers = [
        (header::CONTENT_TYPE, "application/octet-stream".to_string()),
        (header::CONTENT_LENGTH, size.to_string()),
        (
            header::CONTENT_DISPOSITION,
            format!("attachment; filename=\"download\"; filename*=UTF-8''{encoded}"),
        ),
    ];
    (StatusCode::OK, headers, Body::from_stream(stream)).into_response()
}

pub(crate) fn sanitize_name(name: &str) -> String {
    let cleaned: String = name
        .chars()
        .map(|c| if c.is_control() || c == '/' || c == '\\' || c == ':' { '_' } else { c })
        .collect();
    let cleaned = cleaned.trim().to_string();
    if cleaned.is_empty() { "未命名文件".into() } else { cleaned }
}

pub(crate) fn unique_path(dir: &std::path::Path, name: &str) -> PathBuf {
    let p = dir.join(name);
    if !p.exists() {
        return p;
    }
    let stem = dir.join(name);
    let ext = stem.extension().map(|e| format!(".{}", e.to_string_lossy())).unwrap_or_default();
    let base = stem.file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or_default();
    for i in 1..1000u32 {
        let candidate = dir.join(format!("{base} ({i}){ext}"));
        if !candidate.exists() {
            return candidate;
        }
    }
    dir.join(format!("{base}-{uuid}{ext}", uuid = uuid::Uuid::new_v4().simple()))
}

async fn upload(
    State(st): State<Arc<AppState>>,
    Query(q): Query<HashMap<String, String>>,
    mut mp: Multipart,
) -> Response {
    let Some(s) = q.get("s") else {
        return json_err("未配对");
    };
    if !st.pairing.lock().unwrap().session_valid(s) {
        return json_err("会话已失效，请重新配对");
    }

    // 批次：并发/连续的上传请求共用一次目录选择
    let (dir_slot, notify) = {
        let mut batches = st.batches.lock().unwrap();
        batches.retain(|_, b| b.created.elapsed() < Duration::from_secs(300));
        if let Some(b) = batches.values().next() {
            (b.dir.clone(), b.notify.clone())
        } else {
            let b = Batch { dir: Arc::new(tokio::sync::Mutex::new(None)), notify: Arc::new(Notify::new()), created: Instant::now() };
            let batch_id = uuid::Uuid::new_v4().simple().to_string();
            let (dir_slot, notify) = (b.dir.clone(), b.notify.clone());
            batches.insert(batch_id.clone(), b);
            drop(batches);
            st.emit(CoreEvent::AskSaveDir { batch: batch_id });
            (dir_slot, notify)
        }
    };

    // 等用户选择目录，超时 120s 用默认目录
    let started = Instant::now();
    let dir = loop {
        let cur = dir_slot.lock().await.clone();
        if let Some(d) = cur {
            break d;
        }
        if started.elapsed() > Duration::from_secs(120) {
            break st.cfg.save_dir.clone();
        }
        tokio::select! {
            _ = notify.notified() => {}
            _ = tokio::time::sleep(Duration::from_millis(300)) => {}
        }
    };
    let _ = std::fs::create_dir_all(&dir);

    let mut saved = Vec::new();
    while let Ok(Some(mut field)) = mp.next_field().await {
        if field.name() != Some("file") {
            continue;
        }
        let name = sanitize_name(field.file_name().unwrap_or("未命名文件"));
        let id = uuid::Uuid::new_v4().simple().to_string();
        let dest = unique_path(&dir, &name);
        let display_name = dest.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_else(|| name.clone());

        st.new_transfer(TransferInfo::file(id.clone(), Direction::FromPhone, display_name.clone(), 0, Some("phone".into())));
        st.update_transfer(&id, 0, None, TransferStatus::Active, None);

        let mut file = match tokio::fs::File::create(&dest).await {
            Ok(f) => f,
            Err(e) => {
                st.update_transfer(&id, 0, None, TransferStatus::Failed, None);
                tracing::error!("创建文件失败 {dest:?}: {e}");
                continue;
            }
        };
        let mut written: u64 = 0;
        let mut failed = false;
        while let Some(chunk) = match field.chunk().await {
            Ok(c) => c,
            Err(_) => { failed = true; None }
        } {
            if let Err(e) = file.write_all(&chunk).await {
                tracing::error!("写入文件失败: {e}");
                failed = true;
                break;
            }
            written += chunk.len() as u64;
            st.progress_throttled(&id, written, false);
        }
        let _ = file.flush().await;
        drop(file);
        if failed {
            st.update_transfer(&id, written, Some(written), TransferStatus::Failed, None);
            let _ = tokio::fs::remove_file(&dest).await;
            continue;
        }
        st.update_transfer(&id, written, Some(written), TransferStatus::Done, Some(dest.clone()));
        saved.push(serde_json::json!({"name": display_name, "size": written}));
    }

    (StatusCode::OK, Json(serde_json::json!({"ok": true, "files": saved}))).into_response()
}

fn err_page(code: StatusCode, msg: &str) -> Response {
    (code, format!("云书：{msg}")).into_response()
}

fn json_err(msg: &str) -> Response {
    (StatusCode::UNAUTHORIZED, Json(serde_json::json!({"ok": false, "error": msg}))).into_response()
}
