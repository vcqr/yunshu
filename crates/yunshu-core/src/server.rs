//! axum 服务：手机页面、WS 配对会话、文件上传下载。

use crate::pairing::Pairing;
use crate::protocol::{WsIn, WsOut};
use crate::{
    CoreConfig, CoreEvent, Direction, FileEntry, TransferInfo, TransferKind, TransferStatus,
    UiRequest,
};
use axum::body::Body;
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{DefaultBodyLimit, Multipart, Path, Query, State};
use axum::http::{HeaderMap, StatusCode, header};
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
use tokio::io::{AsyncReadExt, AsyncSeekExt, AsyncWriteExt};
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
    /// tokio runtime 句柄：UI 请求处理线程（非 runtime 线程）把异步清理工作投递回 runtime
    pub(crate) rt_handle: tokio::runtime::Handle,
    /// 防打扰：仅已信任（指纹固定）设备可连接，广播静音
    pub(crate) dnd: AtomicBool,
    /// 防打扰下握手后判为未信任的地址（ip:port）→ 时间，10 分钟内不再反复握手
    pub(crate) dnd_skipped: Mutex<HashMap<String, Instant>>,
    /// 隐身模式：不广播本机、不主动外连（对方主机列表将移除本机）
    pub(crate) stealth: AtomicBool,
    /// 退出隐身时置位：发现循环下个 tick 立即补发一次广播（不等常规节奏）
    pub(crate) announce_now: AtomicBool,
    // ---- 飞鸽（PC↔PC，全链路 Noise 加密）----
    /// UDP 广播听到的电脑（发现表）
    pub(crate) pigeon_peers: Mutex<HashMap<String, crate::pigeon::PeerRecord>>,
    /// 已建立的加密链路：peer_id -> 链路
    pub(crate) pigeon_conns: Mutex<HashMap<String, crate::pigeon::PigeonLink>>,
    /// 正在发起连接的 peer（防重入）
    pub(crate) pigeon_connecting: Mutex<std::collections::HashSet<String>>,
    /// 手动添加的主机：addr -> 记录（持久化 manual_hosts.json，离线也显示在列表）
    pub(crate) manual_hosts: Mutex<HashMap<String, crate::pigeon::ManualHost>>,
    /// 正在接收的加密推送批次：offer_id -> 组装状态
    pub(crate) recv_files: tokio::sync::Mutex<HashMap<String, BatchRecvState>>,
    /// 收到邀请、尚未接受（等用户决策）的批次清单：offer_id -> (peer_id, 清单)
    pub(crate) pigeon_incoming: Mutex<HashMap<String, IncomingBatch>>,
    /// 用户已接受、等待对方 FileStart 的批次清单：offer_id -> (peer_id, 清单)。
    /// FileStart 只认这份清单——否则跳过用户决策直接发 FileStart 即可落盘任意文件
    pub(crate) pigeon_ready: Mutex<HashMap<String, IncomingBatch>>,
    /// 已发出邀请、待对方决策的批次：offer_id -> 来源
    pub(crate) pigeon_offers: Mutex<HashMap<String, BatchSource>>,
    /// 正在推送的文件：offer_id -> 取消标记（推送协程轮询）
    pub(crate) pigeon_push_cancel: Mutex<HashMap<String, Arc<AtomicBool>>>,
    /// 正在推送的文件：offer_id -> 暂停标记（推送协程在块间挂起）
    pub(crate) pigeon_push_paused: Mutex<HashMap<String, Arc<AtomicBool>>>,
    /// 等待接收方 FileResume 应答的推送：offer_id -> have 应答通道
    pub(crate) pigeon_push_resume: Mutex<HashMap<String, tokio::sync::oneshot::Sender<u64>>>,
    /// 待确认的新设备：peer_id -> (设备名, 指纹)
    pub(crate) pending_trusts: Mutex<HashMap<String, (String, String)>>,
    /// 本会话内被用户拒绝的设备（不再重复弹确认）
    pub(crate) trust_rejected: Mutex<std::collections::HashSet<String>>,
    // ---- 远程控制（PC↔PC 屏幕共享 + 键鼠注入）----
    pub(crate) remote: Mutex<crate::remote::RemoteState>,
}

/// 正在接收的加密推送批次：offer_id -> 组装状态（半成品在 .yunshu-parts/{id}/，manifest 支持续传）
pub(crate) struct BatchRecvState {
    pub row_id: String,
    pub peer_id: String,
    pub name: String,
    pub dir: PathBuf,
    pub files: Vec<BatchFileState>,
    /// 当前推送到的文件下标
    pub index: usize,
    /// 当前文件句柄（惰性打开，写满即落盘关闭）
    pub file: Option<tokio::fs::File>,
    /// 当前文件已写字节
    pub written: u64,
    /// 批次流偏移（= 已完成前缀 + 当前文件已写）
    pub have: u64,
    /// 当前文件已写满、等待 FileHash 校验
    pub hash_pending: bool,
    /// 当前文件的滚动 SHA-256
    pub running_hash: Option<sha2::Sha256>,
    /// 上次 manifest 落盘时的流偏移（节流）
    pub saved_mark: u64,
}

/// 批次清单条目（接收侧持久化到 manifest.json）
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub(crate) struct BatchFileState {
    pub name: String,
    pub size: u64,
    pub written: u64,
    pub sha: Option<String>,
}

/// 待接收批次的邀请清单（FileStart 前暂存）：peer_id -> 清单
pub(crate) type IncomingBatch = (String, Vec<BatchFileState>);

/// 发送侧批次来源
#[derive(Clone)]
pub struct BatchSource {
    /// (绝对路径, 相对名, 大小)
    pub files: Vec<(PathBuf, String, u64)>,
    pub name: String,
    pub total_size: u64,
}

impl AppState {
    pub(crate) fn emit(&self, ev: CoreEvent) {
        let _ = self.event_tx.send(ev);
    }

    pub(crate) fn push_ws(&self, out: &WsOut) {
        if let Ok(json) = serde_json::to_string(out)
            && let Some(conn) = self.conn.lock().unwrap().as_ref()
        {
            let _ = conn.out.send(json);
        }
    }

    pub(crate) fn new_transfer(&self, t: TransferInfo) {
        self.transfers.lock().unwrap().push(t.clone());
        self.emit(CoreEvent::TransferUpdate { t: Box::new(t) });
    }

    pub(crate) fn update_transfer(
        &self,
        id: &str,
        transferred: u64,
        size: Option<u64>,
        status: TransferStatus,
        path: Option<PathBuf>,
    ) {
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
            self.emit(CoreEvent::TransferUpdate {
                t: Box::new(t.clone()),
            });
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

/// 默认路由所在网卡的 IPv4：UDP connect 只做路由选择、不发包，
/// 避免多网卡机器（WSL/Hyper-V/虚拟机网卡）误选虚拟地址。仅接受私有网段。
fn default_route_ip() -> Option<IpAddr> {
    let s = std::net::UdpSocket::bind(("0.0.0.0", 0)).ok()?;
    s.connect(("8.8.8.8", 80)).ok()?;
    let ip = s.local_addr().ok()?.ip();
    matches!(ip, IpAddr::V4(v) if v.is_private()).then_some(ip)
}

/// 全部候选局域网 IPv4（默认路由网卡优先，其余私有地址随后）
fn all_lan_ips() -> Vec<IpAddr> {
    let mut v: Vec<IpAddr> = default_route_ip().into_iter().collect();
    if let Ok(addrs) = if_addrs::get_if_addrs() {
        for i in addrs {
            if let IpAddr::V4(v4) = i.ip()
                && v4.is_private()
                && !v.contains(&IpAddr::V4(v4))
            {
                v.push(IpAddr::V4(v4));
            }
        }
    }
    v
}

fn lan_ip() -> Option<IpAddr> {
    all_lan_ips().into_iter().next()
}

pub async fn serve(cfg: CoreConfig, event_tx: Sender<CoreEvent>, ui_rx: Receiver<UiRequest>) {
    let save_dir = cfg.save_dir.clone();
    let _ = std::fs::create_dir_all(&save_dir);

    let ip = lan_ip().unwrap_or_else(|| "127.0.0.1".parse().unwrap());
    let addr = format!("{}:{}", ip, cfg.port);

    let data_dir = crate::config_dir().unwrap_or_else(std::env::temp_dir);
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
        rt_handle: tokio::runtime::Handle::current(),
        dnd: AtomicBool::new(cfg.dnd_mode),
        dnd_skipped: Mutex::new(HashMap::new()),
        stealth: AtomicBool::new(cfg.stealth_mode),
        announce_now: AtomicBool::new(false),
        pigeon_peers: Mutex::new(HashMap::new()),
        pigeon_conns: Mutex::new(HashMap::new()),
        pigeon_connecting: Mutex::new(std::collections::HashSet::new()),
        manual_hosts: Mutex::new(
            crate::pigeon::load_manual_hosts()
                .into_iter()
                .map(|m| (m.addr.clone(), m))
                .collect(),
        ),
        recv_files: tokio::sync::Mutex::new(HashMap::new()),
        pigeon_incoming: Mutex::new(HashMap::new()),
        pigeon_ready: Mutex::new(HashMap::new()),
        pigeon_offers: Mutex::new(HashMap::new()),
        pigeon_push_cancel: Mutex::new(HashMap::new()),
        pigeon_push_paused: Mutex::new(HashMap::new()),
        pigeon_push_resume: Mutex::new(HashMap::new()),
        pending_trusts: Mutex::new(HashMap::new()),
        trust_rejected: Mutex::new(std::collections::HashSet::new()),
        remote: Mutex::new(crate::remote::RemoteState::new(
            crate::settings::Settings::load_or_default().remote_fixed_password_hash,
        )),
    });

    state.emit(CoreEvent::ServerStarted { addr: addr.clone() });
    // 远程控制临时密码（每次启动重新生成）
    state.emit(CoreEvent::RemoteTempPassword {
        password: state
            .remote
            .lock()
            .unwrap()
            .auth
            .temp_password()
            .to_string(),
    });

    // 飞鸽发现（PC↔PC）
    {
        let st = state.clone();
        tokio::spawn(crate::pigeon::run_discovery(st));
    }

    // 手动主机后台重连：每 60 秒尝试未连接的手动主机（隐身时不主动外连）
    {
        let st = state.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_secs(3)).await;
            loop {
                if !st.stealth.load(Ordering::Relaxed) {
                    let addrs: Vec<String> = st.manual_hosts.lock().unwrap().keys().cloned().collect();
                    for addr in addrs {
                        crate::pigeon::pigeon_connect_manual(st.clone(), addr, true).await;
                    }
                }
                tokio::time::sleep(Duration::from_secs(60)).await;
            }
        });
    }

    // 每 60 秒轮换二维码 token
    {
        let st = state.clone();
        let ip_str = ip.to_string();
        let https_port = cfg.https_port;
        let http_port = cfg.port;
        let candidates = all_lan_ips();
        tokio::spawn(async move {
            loop {
                let token = uuid::Uuid::new_v4().simple().to_string();
                st.pairing.lock().unwrap().add_token(token.clone());
                let url = format!("https://{ip_str}:{https_port}/pair?t={token}");
                // 备用直达地址（二维码地址不对时手动访问）：全部候选网卡的
                // http（免证书警告）/ https 两种形式，随 token 轮换同步刷新
                let mut alts: Vec<String> = Vec::new();
                for c in &candidates {
                    let c = c.to_string();
                    // http 备用地址同样带上配对路径与 token（否则打开页面无 token，
                    // 无法发起配对；二维码协议切换也取自这些条目）
                    alts.push(format!("http://{c}:{http_port}/pair?t={token}"));
                    let alt_https = format!("https://{c}:{https_port}/pair?t={token}");
                    if alt_https != url {
                        alts.push(alt_https);
                    }
                }
                tracing::info!("二维码刷新: {url}");
                st.emit(CoreEvent::QrUrl(url));
                st.emit(CoreEvent::QrAlternates(alts));
                tokio::time::sleep(Duration::from_secs(60)).await;
            }
        });
    }

    // UI 请求处理（阻塞读 std channel）
    {
        let st = state.clone();
        std::thread::spawn(move || {
            loop {
                match ui_rx.recv() {
                    Ok(UiRequest::SendFiles(files)) => {
                        if !st.pairing.lock().unwrap().has_session() {
                            st.emit(CoreEvent::PairFailed {
                                reason: "没有已连接的手机，请先扫码配对".into(),
                            });
                            continue;
                        }
                        for f in files {
                            offer_file(&st, f);
                        }
                    }
                    Ok(UiRequest::SendText(text)) => {
                        if !st.pairing.lock().unwrap().has_session() {
                            st.emit(CoreEvent::PairFailed {
                                reason: "没有已连接的手机，请先扫码配对".into(),
                            });
                            continue;
                        }
                        let chars = text.chars().count();
                        st.push_ws(&WsOut::PcText { text: text.clone() });
                        st.new_transfer(TransferInfo::text(
                            uuid::Uuid::new_v4().simple().to_string(),
                            Direction::ToPhone,
                            chars,
                            Some("phone".into()),
                        ));
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
                        let sent = crate::pigeon::try_push(
                            &st,
                            &peer_id,
                            crate::protocol::PigeonWire::PcText { text },
                        );
                        if sent {
                            st.new_transfer(TransferInfo::text(
                                uuid::Uuid::new_v4().simple().to_string(),
                                Direction::ToPhone,
                                chars,
                                Some(peer_id.to_string()),
                            ));
                        } else {
                            st.emit(CoreEvent::Toast {
                                text: format!("「{name}」已断开，无法发送"),
                            });
                        }
                    }
                    Ok(UiRequest::PigeonSendFiles { peer_id, paths }) => {
                        let connected = st.pigeon_conns.lock().unwrap().contains_key(&peer_id);
                        if !connected {
                            st.emit(CoreEvent::Toast {
                                text: "对方已断开，无法发送".into(),
                            });
                            continue;
                        }
                        // 多文件/文件夹合并为一个批次，对方一次确认
                        match build_batch_source(&paths) {
                            Ok((id, src)) => {
                                let entries: Vec<FileEntry> = src
                                    .files
                                    .iter()
                                    .map(|(_, rel, size)| FileEntry {
                                        name: rel.clone(),
                                        size: *size,
                                    })
                                    .collect();
                                st.pigeon_offers
                                    .lock()
                                    .unwrap()
                                    .insert(id.clone(), src.clone());
                                st.new_transfer(TransferInfo::batch(
                                    id.clone(),
                                    Direction::ToPhone,
                                    src.name.clone(),
                                    src.total_size,
                                    entries,
                                    Some(peer_id.to_string()),
                                ));
                                crate::pigeon::try_push(
                                    &st,
                                    &peer_id,
                                    crate::protocol::PigeonWire::PcFileOffer {
                                        id,
                                        name: src.name.clone(),
                                        total_size: src.total_size,
                                        files: src
                                            .files
                                            .iter()
                                            .map(|(_, rel, size)| crate::protocol::OfferFile {
                                                name: rel.clone(),
                                                size: *size,
                                            })
                                            .collect(),
                                    },
                                );
                            }
                            Err(e) => st.emit(CoreEvent::Toast {
                                text: format!("发送失败：{e}"),
                            }),
                        }
                    }
                    Ok(UiRequest::PigeonDisconnect { peer_id }) => {
                        if let Some(link) = st.pigeon_conns.lock().unwrap().get(&peer_id) {
                            link.closed.store(true, Ordering::Relaxed);
                        }
                        let st2 = st.clone();
                        st.rt_handle.spawn(crate::pigeon::drop_link(st2, peer_id));
                    }
                    Ok(UiRequest::BroadcastText(text)) => {
                        let n = {
                            let conns = st.pigeon_conns.lock().unwrap();
                            let mut sent = 0;
                            for link in conns.values() {
                                if link
                                    .out
                                    .try_send(crate::pigeon::PigeonOut::Control(
                                        crate::protocol::PigeonWire::BroadcastText {
                                            text: text.clone(),
                                        },
                                    ))
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
                            st.emit(CoreEvent::Toast {
                                text: "没有在线电脑可广播".into(),
                            });
                        }
                    }
                    Ok(UiRequest::PigeonTrust {
                        peer_id,
                        fingerprint,
                    }) => {
                        let mut pending = st.pending_trusts.lock().unwrap();
                        match pending.get(&peer_id) {
                            Some((_, fp)) if fp == &fingerprint => {
                                pending.remove(&peer_id);
                                st.trust_rejected.lock().unwrap().remove(&peer_id);
                                crate::pigeon::pin_fingerprint(&fingerprint);
                                st.emit(CoreEvent::Toast {
                                    text: format!(
                                        "已信任设备（指纹 {}…）",
                                        &fingerprint[..16.min(fingerprint.len())]
                                    ),
                                });
                                // 手动添加的主机可能在等这次确认：固定后立即重连
                                let st2 = st.clone();
                                st.rt_handle.spawn(async move {
                                    let addrs: Vec<String> =
                                        st2.manual_hosts.lock().unwrap().keys().cloned().collect();
                                    for addr in addrs {
                                        crate::pigeon::pigeon_connect_manual(st2.clone(), addr, true)
                                            .await;
                                    }
                                });
                            }
                            _ => st.emit(CoreEvent::Toast {
                                text: "确认已过期，请等待对方重新连接".into(),
                            }),
                        }
                    }
                    Ok(UiRequest::PigeonRejectTrust { peer_id }) => {
                        st.pending_trusts.lock().unwrap().remove(&peer_id);
                        st.trust_rejected.lock().unwrap().insert(peer_id);
                    }
                    Ok(UiRequest::PigeonAcceptFile { peer_id, offer_id }) => {
                        // 磁盘空间预检：剩余空间容不下批次就直接拒绝
                        let total = st
                            .pigeon_incoming
                            .lock()
                            .unwrap()
                            .get(&offer_id)
                            .map(|(_, files)| files.iter().map(|f| f.size).sum::<u64>());
                        if let Some(total) = total
                            && let Some(free) = crate::pigeon::free_space(&st.cfg.save_dir)
                            && free < total
                        {
                            crate::pigeon::try_push(
                                &st,
                                &peer_id,
                                crate::protocol::PigeonWire::FileDecision {
                                    id: offer_id.clone(),
                                    accepted: false,
                                },
                            );
                            st.pigeon_incoming.lock().unwrap().remove(&offer_id);
                            st.emit(CoreEvent::Toast {
                                text: format!("磁盘空间不足（需要 {} 字节），已拒绝接收", total),
                            });
                            continue;
                        }
                        // 接受：批次转入 ready 表——对方的 FileStart 只有命中这里才开始收
                        if let Some(entry) = st.pigeon_incoming.lock().unwrap().remove(&offer_id) {
                            st.pigeon_ready.lock().unwrap().insert(offer_id.clone(), entry);
                        }
                        // 接受后由对方在加密通道内推送文件，这里只需答复
                        crate::pigeon::try_push(
                            &st,
                            &peer_id,
                            crate::protocol::PigeonWire::FileDecision {
                                id: offer_id,
                                accepted: true,
                            },
                        );
                    }
                    Ok(UiRequest::PigeonRejectFile { peer_id, offer_id }) => {
                        st.pigeon_incoming.lock().unwrap().remove(&offer_id);
                        st.pigeon_ready.lock().unwrap().remove(&offer_id);
                        crate::pigeon::try_push(
                            &st,
                            &peer_id,
                            crate::protocol::PigeonWire::FileDecision {
                                id: offer_id,
                                accepted: false,
                            },
                        );
                    }
                    Ok(UiRequest::PigeonCancelFile { peer_id, offer_id }) => {
                        let mut cancelled_locally = false;
                        // 等待决策/已接受未开始的批次清单先行清理
                        st.pigeon_incoming.lock().unwrap().remove(&offer_id);
                        st.pigeon_ready.lock().unwrap().remove(&offer_id);
                        // 发送方向：推送中 → 登记取消标记，推送协程停止并回发 FileCancel
                        if let Some(flag) = st.pigeon_push_cancel.lock().unwrap().get(&offer_id) {
                            flag.store(true, Ordering::Relaxed);
                            cancelled_locally = true;
                        }
                        // 接收方向：停止写入并删除半成品目录（文件句柄在异步侧，投递回 runtime 清理）
                        if st.recv_files.blocking_lock().contains_key(&offer_id) {
                            let st2 = st.clone();
                            let id = offer_id.clone();
                            st.rt_handle.spawn(async move {
                                if let Some(mut s) = st2.recv_files.lock().await.remove(&id) {
                                    if let Some(f) = s.file.as_mut() {
                                        let _ = f.flush().await;
                                    }
                                    s.file = None;
                                    let _ = tokio::fs::remove_dir_all(&s.dir).await;
                                    st2.update_transfer(
                                        &s.row_id,
                                        s.have,
                                        None,
                                        TransferStatus::Cancelled,
                                        None,
                                    );
                                }
                            });
                            cancelled_locally = true;
                        }
                        if !cancelled_locally {
                            // 等待对方接受阶段：直接通知对方撤回/取消
                            crate::pigeon::try_push(
                                &st,
                                &peer_id,
                                crate::protocol::PigeonWire::FileCancel {
                                    id: offer_id.clone(),
                                },
                            );
                        }
                        let has_row = st
                            .transfers
                            .lock()
                            .unwrap()
                            .iter()
                            .any(|t| t.id == offer_id);
                        if !cancelled_locally && has_row {
                            // 本机发送等待中：行标取消（保留已有进度显示）
                            let cur = st
                                .transfers
                                .lock()
                                .unwrap()
                                .iter()
                                .find(|t| t.id == offer_id)
                                .map(|t| (t.transferred, t.size));
                            if let Some((transferred, size)) = cur {
                                st.update_transfer(
                                    &offer_id,
                                    transferred,
                                    Some(size),
                                    TransferStatus::Cancelled,
                                    None,
                                );
                            }
                        } else if !cancelled_locally && !has_row {
                            // 本机挂着待确认弹窗：推一条取消状态让 UI 关闭并收尾
                            let info = TransferInfo {
                                id: offer_id.clone(),
                                direction: Direction::FromPhone,
                                kind: TransferKind::File,
                                name: String::new(),
                                size: 0,
                                transferred: 0,
                                status: TransferStatus::Cancelled,
                                path: None,
                                peer: Some(peer_id.clone()),
                                files: Vec::new(),
                            };
                            st.emit(CoreEvent::TransferUpdate { t: Box::new(info) });
                        }
                    }
                    Ok(UiRequest::PigeonPauseFile {
                        peer_id,
                        offer_id,
                        paused,
                    }) => {
                        let mut acted = false;
                        // 发送方向：设置暂停标记，推送协程在块间挂起，并同步本端行状态
                        if let Some(flag) = st.pigeon_push_paused.lock().unwrap().get(&offer_id) {
                            flag.store(paused, Ordering::Relaxed);
                            acted = true;
                            let cur = st
                                .transfers
                                .lock()
                                .unwrap()
                                .iter()
                                .find(|t| t.id == offer_id)
                                .map(|t| (t.transferred, t.size));
                            if let Some((transferred, size)) = cur {
                                let status = if paused {
                                    TransferStatus::Paused
                                } else {
                                    TransferStatus::Active
                                };
                                st.update_transfer(
                                    &offer_id,
                                    transferred,
                                    Some(size),
                                    status,
                                    None,
                                );
                            }
                        }
                        // 接收方向：本地行状态同步（数据是否真停取决于发送方是否配合）
                        {
                            let info = {
                                let states = st.recv_files.blocking_lock();
                                states.get(&offer_id).map(|s| {
                                    (
                                        s.row_id.clone(),
                                        s.have,
                                        s.files.iter().map(|f| f.size).sum::<u64>(),
                                    )
                                })
                            };
                            if let Some((row_id, have, total)) = info {
                                let status = if paused {
                                    TransferStatus::Paused
                                } else {
                                    TransferStatus::Active
                                };
                                st.update_transfer(&row_id, have, Some(total), status, None);
                                acted = true;
                            }
                        }
                        // 通知对方同步（发送方收到后控制推送节奏，接收方收到后同步行状态）
                        crate::pigeon::try_push(
                            &st,
                            &peer_id,
                            crate::protocol::PigeonWire::FilePause {
                                id: offer_id,
                                paused,
                            },
                        );
                        let _ = acted;
                    }
                    Ok(UiRequest::ForgetTransfer { id }) => {
                        st.transfers.lock().unwrap().retain(|t| t.id != *id);
                    }
                    Ok(UiRequest::SetDnd(enabled)) => {
                        st.dnd.store(enabled, Ordering::Relaxed);
                        if !enabled {
                            st.dnd_skipped.lock().unwrap().clear();
                        }
                        st.emit(CoreEvent::Toast {
                            text: if enabled {
                                "防打扰已开启：仅已信任设备可连接，广播消息已静音".into()
                            } else {
                                "防打扰已关闭".into()
                            },
                        });
                    }
                    Ok(UiRequest::SetStealth(enabled)) => {
                        st.stealth.store(enabled, Ordering::Relaxed);
                        if enabled {
                            // 立即从其他设备的主机列表消失（对方收到 leave 即移除）
                            crate::pigeon::broadcast_leave();
                        } else {
                            st.announce_now.store(true, Ordering::Relaxed);
                        }
                        st.emit(CoreEvent::Toast {
                            text: if enabled {
                                "隐身模式已开启：不再向局域网广播本机，也不主动外连".into()
                            } else {
                                "隐身模式已关闭：恢复广播，本机将重新出现在其他设备列表".into()
                            },
                        });
                    }
                    Ok(UiRequest::PigeonAddHost { addr, name }) => {
                        match crate::pigeon::parse_host_addr(&addr) {
                            Ok(addr) => {
                                let added = {
                                    let mut hosts = st.manual_hosts.lock().unwrap();
                                    if hosts.contains_key(&addr) {
                                        None
                                    } else {
                                        let m = crate::pigeon::ManualHost {
                                            addr: addr.clone(),
                                            name: if name.trim().is_empty() {
                                                addr.clone()
                                            } else {
                                                name.trim().to_string()
                                            },
                                            machine_id: String::new(),
                                            peer_id: None,
                                        };
                                        hosts.insert(addr.clone(), m);
                                        Some(addr.clone())
                                    }
                                };
                                if added.is_some() {
                                    let list: Vec<crate::pigeon::ManualHost> =
                                        st.manual_hosts.lock().unwrap().values().cloned().collect();
                                    crate::pigeon::save_manual_hosts(&list);
                                    crate::pigeon::emit_peers(&st);
                                }
                                // 新增或重连都立即发起一次连接（失败弹提示）
                                let st2 = st.clone();
                                st.rt_handle
                                    .spawn(async move { crate::pigeon::pigeon_connect_manual(st2, addr, false).await });
                            }
                            Err(e) => st.emit(CoreEvent::Toast { text: e }),
                        }
                    }
                    Ok(UiRequest::PigeonRemoveHost { addr }) => {
                        let removed = st.manual_hosts.lock().unwrap().remove(&addr);
                        if let Some(m) = removed {
                            let list: Vec<crate::pigeon::ManualHost> =
                                st.manual_hosts.lock().unwrap().values().cloned().collect();
                            crate::pigeon::save_manual_hosts(&list);
                            crate::pigeon::emit_peers(&st);
                            // 连接中的链路一并断开
                            if let Some(pid) = m.peer_id
                                && st.pigeon_conns.lock().unwrap().contains_key(&pid)
                            {
                                if let Some(link) = st.pigeon_conns.lock().unwrap().get(&pid) {
                                    link.closed.store(true, Ordering::Relaxed);
                                }
                                let st2 = st.clone();
                                st.rt_handle.spawn(crate::pigeon::drop_link(st2, pid));
                            }
                            st.emit(CoreEvent::Toast {
                                text: format!("已移除主机「{}」", m.name),
                            });
                        }
                    }
                    Ok(UiRequest::PigeonConnectHost { addr }) => {
                        let st2 = st.clone();
                        st.rt_handle
                            .spawn(async move { crate::pigeon::pigeon_connect_manual(st2, addr, false).await });
                    }
                    Ok(
                        req @ (UiRequest::RemoteConnect { .. }
                        | UiRequest::RemoteDisconnect
                        | UiRequest::RemoteSendFiles { .. }
                        | UiRequest::RemoteAssistRequest { .. }
                        | UiRequest::RemoteAssistRespond { .. }
                        | UiRequest::SetRemoteFixedPassword { .. }),
                    ) => {
                        crate::remote::handle_ui_request(&st, req);
                    }
                    Err(_) => break,
                }
            }
        });
    }

    let router = Router::new()
        .route("/", get(serve_page))
        .route("/pair", get(serve_page))
        .route("/ws", get(ws_handler))
        .route("/pigeon-ws", get(pigeon_ws_handler))
        .route(crate::remote::REMOTE_WS_PATH, get(remote_ws_handler))
        .route(crate::remote::VIEW_WS_PATH, get(remote_view_ws_handler))
        .route("/view", get(serve_view_page))
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
                tracing::info!(
                    "HTTPS 已启动: https://0.0.0.0:{}（手机首次访问需信任自签证书）",
                    https_addr.port()
                );
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
            // ConnectInfo：远程控制按来源 IP 做防爆破锁定
            if let Err(e) = axum::serve(
                listener,
                router.into_make_service_with_connect_info::<std::net::SocketAddr>(),
            )
            .await
            {
                tracing::error!("服务异常退出: {e}");
                state.emit(CoreEvent::ServerError {
                    reason: format!("服务异常退出：{e}"),
                });
            }
        }
        Err(e) => {
            tracing::error!("端口 {} 绑定失败: {e}", cfg.port);
            state.emit(CoreEvent::ServerError {
                reason: format!("端口 {} 被占用：{e}", cfg.port),
            });
        }
    }
}

/// 自签证书：首次生成并持久化（含所有本机 IPv4 SAN + localhost）
async fn https_config(
    data_dir: &std::path::Path,
) -> Result<axum_server::tls_rustls::RustlsConfig, String> {
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
            .filter_map(|s| {
                if let Ok(ip) = s.parse::<IpAddr>() {
                    Some(SanType::IpAddress(ip))
                } else {
                    None
                }
            })
            .collect();
        params.subject_alt_names.push(SanType::DnsName(
            "yunshu.local".try_into().map_err(|e| format!("{e}"))?,
        ));
        let key = KeyPair::generate().map_err(|e| e.to_string())?;
        let cert = params.self_signed(&key).map_err(|e| e.to_string())?;
        std::fs::write(&cert_p, cert.pem()).map_err(|e| e.to_string())?;
        std::fs::write(&key_p, key.serialize_pem()).map_err(|e| e.to_string())?;
        crate::pigeon::set_owner_only(&cert_p);
        crate::pigeon::set_owner_only(&key_p);
        tracing::info!("已生成 HTTPS 自签证书（{}）", cert_p.display());
    }

    axum_server::tls_rustls::RustlsConfig::from_pem_file(&cert_p, &key_p)
        .await
        .map_err(|e| format!("加载证书失败: {e}"))
}

/// 手机页面安全头。页面是内联脚本/样式的单文件 HTML，script-src 只能放开内联；
/// 收益在其余指令：connect-src 阻断被注入脚本向外域回传、frame-ancestors 防
/// 点击劫持、referrer-policy 不外泄 URL（内含配对 token）。
const PAGE_HEADERS: [(&str, &str); 4] = [
    ("cache-control", "no-store"),
    (
        "content-security-policy",
        "default-src 'none'; img-src 'self' data:; style-src 'unsafe-inline'; script-src 'unsafe-inline'; connect-src 'self' ws: wss:; base-uri 'none'; frame-ancestors 'none'; form-action 'self'",
    ),
    ("x-content-type-options", "nosniff"),
    ("referrer-policy", "no-referrer"),
];

async fn serve_page() -> Response {
    (PAGE_HEADERS, Html(include_str!("../assets/phone.html"))).into_response()
}

/// 手机「只看不控」页面（观看中设备的屏幕直播，只读凭证鉴权）
async fn serve_view_page() -> Response {
    (PAGE_HEADERS, Html(include_str!("../assets/view.html"))).into_response()
}

/// 手机 WS 入口的浏览器来源校验：跨源网页（任意网站都能向局域网地址发起 WS）
/// 不得连接——否则可长期占用唯一连接槽位或探测配对状态。非浏览器客户端无
/// Origin 头，放行（PC↔PC 的飞鸽链路走独立路径，不经此入口）。
fn origin_allowed(origin: Option<&str>, host: Option<&str>) -> bool {
    let Some(origin) = origin else {
        return true;
    };
    let Some(host) = host else {
        return false;
    };
    let auth = origin
        .strip_prefix("https://")
        .or_else(|| origin.strip_prefix("http://"))
        .unwrap_or("");
    // authority 到第一个路径/查询字符为止；本服务端口非 80/443，浏览器必带端口
    let auth = auth.split(['/', '?', '#']).next().unwrap_or("");
    auth == host
}

async fn ws_handler(
    State(st): State<Arc<AppState>>,
    headers: HeaderMap,
    ws: WebSocketUpgrade,
) -> Response {
    let origin = headers.get(header::ORIGIN).and_then(|v| v.to_str().ok());
    let host = headers.get(header::HOST).and_then(|v| v.to_str().ok());
    if !origin_allowed(origin, host) {
        return (StatusCode::FORBIDDEN, "禁止跨源连接").into_response();
    }
    ws.on_upgrade(move |socket| handle_socket(st, socket))
}

async fn pigeon_ws_handler(State(st): State<Arc<AppState>>, ws: WebSocketUpgrade) -> Response {
    ws.on_upgrade(move |socket| handle_pigeon_socket(st, socket))
}

/// 远程控制受控端入口（独立 Noise 会话，认证通过即进入会话循环）
async fn remote_ws_handler(
    State(st): State<Arc<AppState>>,
    axum::extract::ConnectInfo(addr): axum::extract::ConnectInfo<std::net::SocketAddr>,
    ws: WebSocketUpgrade,
) -> Response {
    ws.on_upgrade(move |socket| crate::remote::handle_remote_socket(st, addr.ip(), socket))
}

/// 本地观看桥：控制端会话建立后，观看窗口凭 token 接入收帧发输入
async fn remote_view_ws_handler(
    State(st): State<Arc<AppState>>,
    Query(q): Query<HashMap<String, String>>,
    ws: WebSocketUpgrade,
) -> Response {
    let token = q.get("token").cloned();
    ws.on_upgrade(move |socket| crate::remote::handle_view_socket(st, token, socket))
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
    if sink
        .send(Message::Binary(m2[..n2].to_vec().into()))
        .await
        .is_err()
    {
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
    let remote_static = noise
        .get_remote_static()
        .map(|s| s.to_vec())
        .unwrap_or_default();
    let noise = Arc::new(Mutex::new(noise));

    // ---- 加密通道内：hello（id 必须在发现表里）----
    let first = match tokio::time::timeout(Duration::from_secs(10), stream.next()).await {
        Ok(Some(Ok(Message::Binary(b)))) => b,
        _ => return,
    };
    let hello = match crate::pigeon::open_msg(&noise, &first) {
        Ok(crate::pigeon::PigeonIn::Control(PigeonWire::PigeonHello { id, name, .. })) => {
            (id, name)
        }
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
    // id 全分支都带：发起方在「待确认」阶段就能拿到对端 id（自助添加依赖此字段）
    let (welcome, welcome_ok) = if !known_ok {
        (
            PigeonWire::PigeonWelcome {
                ok: false,
                id: Some(st.self_id.clone()),
                reason: Some("不在线上发现列表中".into()),
                pc_name: None,
            },
            false,
        )
    } else if st.trust_rejected.lock().unwrap().contains(&peer_id) {
        (
            PigeonWire::PigeonWelcome {
                ok: false,
                id: Some(st.self_id.clone()),
                reason: Some("你已被拒绝接入".into()),
                pc_name: None,
            },
            false,
        )
    } else if st.dnd.load(Ordering::Relaxed) && !crate::pigeon::is_pinned(&remote_static) {
        // 防打扰：未信任设备直接拒绝，不弹信任确认
        tracing::info!("防打扰：已拒绝未信任设备「{peer_name}」的连入");
        (
            PigeonWire::PigeonWelcome {
                ok: false,
                id: Some(st.self_id.clone()),
                reason: Some("对方开启了防打扰".into()),
                pc_name: None,
            },
            false,
        )
    } else if !crate::pigeon::is_pinned(&remote_static) {
        let fp = crate::pigeon::fingerprint(&remote_static);
        st.pending_trusts
            .lock()
            .unwrap()
            .insert(peer_id.clone(), (peer_name.clone(), fp.clone()));
        tracing::info!("新设备「{peer_name}」待确认（指纹 {fp}）");
        st.emit(CoreEvent::PigeonTrustRequest {
            peer_id: peer_id.clone(),
            name: peer_name.clone(),
            fingerprint: fp,
        });
        (
            PigeonWire::PigeonWelcome {
                ok: false,
                id: Some(st.self_id.clone()),
                reason: Some("新设备待确认".into()),
                pc_name: None,
            },
            false,
        )
    } else {
        (
            PigeonWire::PigeonWelcome {
                ok: true,
                id: Some(st.self_id.clone()),
                reason: None,
                pc_name: Some(st.cfg.pc_name.clone()),
            },
            true,
        )
    };
    let frame = match crate::pigeon::seal_msg(&noise, &crate::pigeon::PigeonOut::Control(welcome)) {
        Ok(f) => f,
        Err(_) => return,
    };
    if sink.send(Message::Binary(frame.into())).await.is_err() {
        return;
    }
    // welcome 未放行（未信任/已拒绝/防打扰/不在发现表）：不注册链路即断开。
    // 链路一旦注册，未信任设备的聊天/文件邀请/协助请求便可绕过确认直达本机 UI
    if !welcome_ok {
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
        crate::pigeon::PigeonLink {
            name: peer_name.clone(),
            out: plain_out_tx.clone(),
            closed: Arc::new(AtomicBool::new(false)),
        },
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

    crate::pigeon::drop_link(st.clone(), peer_id.clone()).await;
    pump.abort();
    writer.abort();
    tracing::info!("「{peer_name}」的加密飞鸽链路断开（{drop_reason}）");
    crate::pigeon::emit_peers(&st);
}

/// 把若干路径（文件/文件夹混合）整理成一个传输批次：文件夹递归展开为相对路径清单。
/// 批次 id 由全部条目的（名称|大小|mtime）派生，内容不变重发时可续传。
pub(crate) fn build_batch_source(paths: &[PathBuf]) -> Result<(String, BatchSource), String> {
    fn walk(
        dir: &PathBuf,
        rel_prefix: &str,
        files: &mut Vec<(PathBuf, String, u64)>,
        items: &mut Vec<(String, u64, u128)>,
    ) -> Result<(), String> {
        for entry in
            std::fs::read_dir(dir).map_err(|e| format!("无法读取目录 {}: {e}", dir.display()))?
        {
            let entry = entry.map_err(|e| e.to_string())?;
            let p = entry.path();
            let fname = entry.file_name().to_string_lossy().to_string();
            let child_rel = format!("{rel_prefix}/{fname}");
            if p.is_dir() {
                walk(&p, &child_rel, files, items)?;
            } else if p.is_file() {
                let meta = entry.metadata().map_err(|e| e.to_string())?;
                let mtime = meta
                    .modified()
                    .ok()
                    .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                    .map(|d| d.as_millis())
                    .unwrap_or(0);
                files.push((p.clone(), child_rel.clone(), meta.len()));
                items.push((child_rel, meta.len(), mtime));
            }
        }
        Ok(())
    }

    let mut files: Vec<(PathBuf, String, u64)> = Vec::new();
    let mut items: Vec<(String, u64, u128)> = Vec::new();
    for p in paths {
        if p.is_dir() {
            let root = p
                .file_name()
                .map(|n| sanitize_name(&n.to_string_lossy()))
                .unwrap_or_else(|| "文件夹".into());
            walk(p, &root, &mut files, &mut items)?;
        } else if p.is_file() {
            let meta =
                std::fs::metadata(p).map_err(|e| format!("无法读取文件 {}: {e}", p.display()))?;
            let rel = sanitize_name(
                &p.file_name()
                    .map(|n| n.to_string_lossy().to_string())
                    .unwrap_or_default(),
            );
            let mtime = meta
                .modified()
                .ok()
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|d| d.as_millis())
                .unwrap_or(0);
            files.push((p.clone(), rel.clone(), meta.len()));
            items.push((rel, meta.len(), mtime));
        }
    }
    if files.is_empty() {
        return Err("没有可发送的文件".into());
    }
    // 排序保证 id 与清单顺序稳定（read_dir 顺序依赖系统）
    files.sort_by(|a, b| a.1.cmp(&b.1));
    items.sort_by(|a, b| a.0.cmp(&b.0));

    let id = crate::pigeon::batch_id(&items);
    let total_size = items.iter().map(|i| i.1).sum();
    let name = if files.len() == 1 {
        files[0]
            .1
            .rsplit('/')
            .next()
            .unwrap_or(&files[0].1)
            .to_string()
    } else {
        let first_root = files[0].1.split('/').next().unwrap_or_default();
        let all_same_root = files
            .iter()
            .all(|f| f.1.split('/').next() == Some(first_root));
        if all_same_root && !first_root.is_empty() {
            format!("{first_root}/")
        } else {
            format!("{} 项", files.len())
        }
    };
    Ok((
        id,
        BatchSource {
            files,
            name,
            total_size,
        },
    ))
}

/// 相对路径安全拼接：逐段 sanitize，拒绝空段与穿越（..）
pub(crate) fn safe_join(root: &std::path::Path, rel: &str) -> Option<PathBuf> {
    let mut out = root.to_path_buf();
    for seg in rel.split('/') {
        if seg.is_empty() || seg == "." || seg == ".." {
            return None;
        }
        out.push(sanitize_name(seg));
    }
    Some(out)
}

/// 批次清单落盘（断点续传依据；小文件同步写即可）
pub(crate) fn save_manifest(dir: &std::path::Path, files: &[BatchFileState]) {
    match serde_json::to_string(files) {
        Ok(json) => {
            if let Err(e) = std::fs::write(dir.join("manifest.json"), json) {
                tracing::warn!("写入 manifest 失败: {e}");
            }
        }
        Err(e) => tracing::warn!("序列化 manifest 失败: {e}"),
    }
}

/// hello 校验通过后原子占位（同一时间只允许一台手机）。
/// 已是本连接自己的槽位（改名重发 hello）视为成功；被其他连接占用则返回 false。
fn install_conn(
    st: &Arc<AppState>,
    out: &tokio::sync::mpsc::UnboundedSender<String>,
    closed: &Arc<AtomicBool>,
    kick: &Arc<Notify>,
) -> bool {
    let mut slot = st.conn.lock().unwrap();
    match slot.as_ref() {
        Some(c) if Arc::ptr_eq(&c.closed, closed) => true,
        Some(_) => false,
        None => {
            *slot = Some(Conn {
                out: out.clone(),
                closed: closed.clone(),
                kick: kick.clone(),
            });
            true
        }
    }
}

async fn handle_socket(st: Arc<AppState>, mut socket: WebSocket) {
    // 快速拒绝（权威检查在 hello 后的 install_conn 原子占位）
    if st.conn.lock().unwrap().is_some() {
        if let Ok(json) = serde_json::to_string(&WsOut::PairResult {
            ok: false,
            session: None,
            reason: Some("电脑上已有其他设备连接".into()),
            pc_name: None,
            http_port: None,
        }) {
            let _ = socket.send(Message::text(json)).await;
        }
        return;
    }
    let (out_tx, mut out_rx) = tokio::sync::mpsc::unbounded_channel::<String>();
    let closed = Arc::new(AtomicBool::new(false));
    let kick = Arc::new(Notify::new());
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
    // hello 校验通过（占位成功）前不计入配对态；未占位的连接限时关闭，
    // 防止无凭证连接长期占着 WS 与写任务
    let mut installed = false;
    let started = Instant::now();
    while let Some(Ok(msg)) = stream.next().await {
        if closed.load(Ordering::Relaxed) {
            break;
        }
        if !installed && started.elapsed() > Duration::from_secs(30) {
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
            WsIn::Hello {
                token,
                device,
                session,
            } => {
                // 断线恢复：手机锁屏/切后台会断 WS，重连时带着原会话回来免重新扫码
                let restore = {
                    let p = st.pairing.lock().unwrap();
                    session
                        .as_deref()
                        .map(str::trim)
                        .filter(|s| !s.is_empty())
                        .filter(|s| p.session_valid(s))
                        .map(str::to_string)
                };
                if let Some(s) = restore {
                    if !install_conn(&st, &out_tx, &closed, &kick) {
                        let _ = out_tx2.send(
                            serde_json::to_string(&WsOut::PairResult {
                                ok: false,
                                session: None,
                                reason: Some("电脑上已有其他设备连接".into()),
                                pc_name: None,
                                http_port: None,
                            })
                            .unwrap(),
                        );
                        break;
                    }
                    installed = true;
                    paired = true;
                    let _ = out_tx2.send(
                        serde_json::to_string(&WsOut::PairResult {
                            ok: true,
                            session: Some(s),
                            reason: None,
                            pc_name: Some(st.cfg.pc_name.clone()),
                            http_port: Some(st.cfg.port),
                        })
                        .unwrap(),
                    );
                    tracing::info!("手机「{}」恢复会话", device.trim());
                    st.emit(CoreEvent::PairOk {
                        device: device.trim().to_string(),
                    });
                    continue;
                }
                if !st.pairing.lock().unwrap().token_valid(&token) {
                    let _ = out_tx2.send(
                        serde_json::to_string(&WsOut::PairResult {
                            ok: false,
                            session: None,
                            reason: Some("二维码已过期，请在电脑上刷新后重新扫码".into()),
                            pc_name: None,
                            http_port: None,
                        })
                        .unwrap(),
                    );
                    break;
                }
                // token 有效才占位：无凭证的连接拿不到唯一槽位（防占位 DoS）
                if !install_conn(&st, &out_tx, &closed, &kick) {
                    let _ = out_tx2.send(
                        serde_json::to_string(&WsOut::PairResult {
                            ok: false,
                            session: None,
                            reason: Some("电脑上已有其他设备连接".into()),
                            pc_name: None,
                            http_port: None,
                        })
                        .unwrap(),
                    );
                    break;
                }
                installed = true;
                // 注：不再以 has_session 拒绝——WS 入口已保证同一时间仅一条连接，
                // 会话跨连接保留后，新配对直接覆盖旧会话（旧设备恢复会失败并回到扫码流程）
                let device = if device.trim().is_empty() {
                    "手机".into()
                } else {
                    device.trim().to_string()
                };
                let code = st.pairing.lock().unwrap().begin_pending(device.clone());
                tracing::info!("手机「{device}」发起配对，配对码 {code}");
                st.emit(CoreEvent::PairPending { code, device });
            }
            WsIn::Pair { code } => match st.pairing.lock().unwrap().verify(code.trim()) {
                Ok((session, device)) => {
                    paired = true;
                    let pc_name = st.cfg.pc_name.clone();
                    tracing::info!("「{device}」配对成功");
                    let _ = out_tx2.send(
                        serde_json::to_string(&WsOut::PairResult {
                            ok: true,
                            session: Some(session),
                            reason: None,
                            pc_name: Some(pc_name),
                            http_port: Some(st.cfg.port),
                        })
                        .unwrap(),
                    );
                    st.emit(CoreEvent::PairOk { device });
                }
                Err(reason) => {
                    tracing::info!("配对失败：{reason}");
                    let _ = out_tx2.send(
                        serde_json::to_string(&WsOut::PairResult {
                            ok: false,
                            session: None,
                            reason: Some(reason.into()),
                            pc_name: None,
                            http_port: None,
                        })
                        .unwrap(),
                    );
                    st.emit(CoreEvent::PairFailed {
                        reason: reason.into(),
                    });
                }
            },
            WsIn::SendText { text } if paired => {
                let text = text.trim().to_string();
                if text.is_empty() {
                    continue;
                }
                crate::clipboard::copy_text(&text);
                let chars = text.chars().count();
                st.new_transfer(TransferInfo::text(
                    uuid::Uuid::new_v4().simple().to_string(),
                    Direction::FromPhone,
                    chars,
                    Some("phone".into()),
                ));
                st.emit(CoreEvent::TextFromPhone { text });
            }
            WsIn::SendText { .. } => {
                let _ = out_tx2.send(
                    serde_json::to_string(&WsOut::PairResult {
                        ok: false,
                        session: None,
                        reason: Some("尚未完成配对".into()),
                        pc_name: None,
                        http_port: None,
                    })
                    .unwrap(),
                );
            }
        }
    }

    // 清理。注意不清 pairing 会话：手机锁屏/切后台断 WS 后重连要靠它静默恢复，
    // 旧下载链接也因此保持有效；会话仅被新配对覆盖或电脑端主动断开时重置。
    // 只清自己占的槽位：未通过 hello 的连接（含被拒/超时）不动 st.conn
    {
        let mut slot = st.conn.lock().unwrap();
        let mine = slot
            .as_ref()
            .map(|c| Arc::ptr_eq(&c.closed, &closed))
            .unwrap_or(false);
        if mine {
            *slot = None;
            st.emit(CoreEvent::PeerDisconnected);
        }
    }
    writer.abort();
}

fn offer_file(st: &Arc<AppState>, path: PathBuf) {
    let Ok(meta) = std::fs::metadata(&path) else {
        st.emit(CoreEvent::ServerError {
            reason: format!("无法读取文件：{}", path.display()),
        });
        return;
    };
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| "未命名文件".into());
    let id = uuid::Uuid::new_v4().simple().to_string();
    let size = meta.len();
    st.offers
        .lock()
        .unwrap()
        .insert(id.clone(), (path, name.clone(), size));
    st.new_transfer(TransferInfo::file(
        id.clone(),
        Direction::ToPhone,
        name.clone(),
        size,
        Some("phone".into()),
    ));
    st.push_ws(&WsOut::PcFile { id, name, size });
}

async fn download(
    State(st): State<Arc<AppState>>,
    Path(id): Path<String>,
    headers: HeaderMap,
    Query(_q): Query<HashMap<String, String>>,
) -> Response {
    // 文件 id 本身（uuid simple，128 位不可猜测）即下载凭证；
    // 不再校验配对会话——否则手机断线（锁屏/切后台）后所有旧下载链接全部失效
    let Some((path, name, size)) = st.offers.lock().unwrap().get(&id).cloned() else {
        return err_page(StatusCode::NOT_FOUND, "文件不存在或已被清理");
    };
    let range_header = headers
        .get(header::RANGE)
        .and_then(|v| v.to_str().ok())
        .map(String::from);
    tracing::info!("手机下载请求: id={id} name={name} size={size} range={range_header:?}");
    let Ok(mut file) = tokio::fs::File::open(&path).await else {
        return err_page(StatusCode::NOT_FOUND, "文件读取失败");
    };

    // Range 分段（iOS Safari 下载器会先发 bytes=0- 探测；也是断点续传的基础）。
    // 仅支持单段 bytes=start-end / bytes=start-；size==0 直接 200 空体。
    let (status, start, expect_len) = if size == 0 {
        (StatusCode::OK, 0u64, 0u64)
    } else if let Some(r) = range_header
        .as_deref()
        .and_then(|r| r.strip_prefix("bytes="))
    {
        let mut it = r.splitn(2, '-');
        let s: u64 = it.next().unwrap_or("").parse().unwrap_or(0);
        let e: Option<u64> = it.next().and_then(|x| x.parse().ok());
        let max = size - 1;
        let start = s.min(max);
        let end = e.unwrap_or(max).min(max);
        if end >= start {
            (StatusCode::PARTIAL_CONTENT, start, end - start + 1)
        } else {
            // 非法区间（end < start，此前会下溢回绕出超大长度）：回退整文件 200
            (StatusCode::OK, 0, size)
        }
    } else {
        (StatusCode::OK, 0, size)
    };
    if start > 0 && file.seek(std::io::SeekFrom::Start(start)).await.is_err() {
        return err_page(StatusCode::INTERNAL_SERVER_ERROR, "文件定位失败");
    }

    st.update_transfer(&id, 0, None, TransferStatus::Active, None);

    let total = Arc::new(AtomicU64::new(0));
    let st2 = st.clone();
    let id2 = id.clone();
    let total2 = total.clone();
    let stream = ReaderStream::new(file.take(expect_len)).map(move |chunk| match chunk {
        Ok(b) => {
            let n = total2.fetch_add(b.len() as u64, Ordering::Relaxed) + b.len() as u64;
            let done = n >= expect_len;
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
    // ASCII 回退名：部分手机浏览器不解析 filename*，至少给出可读的真实名
    let ascii_name: String = name
        .chars()
        .filter(|c| c.is_ascii() && !c.is_control() && *c != '"' && *c != '\\')
        .collect();
    let ascii_name = if ascii_name.trim().is_empty() {
        "download".to_string()
    } else {
        ascii_name
    };
    let mut resp_headers = vec![
        (header::CONTENT_TYPE, "application/octet-stream".to_string()),
        (header::CONTENT_LENGTH, expect_len.to_string()),
        (header::ACCEPT_RANGES, "bytes".to_string()),
        (
            header::CONTENT_DISPOSITION,
            format!("attachment; filename=\"{ascii_name}\"; filename*=UTF-8''{encoded}"),
        ),
    ];
    if status == StatusCode::PARTIAL_CONTENT {
        resp_headers.push((
            header::CONTENT_RANGE,
            // RFC 7233 格式："bytes 1000-1999/3000000"，连字符两侧与斜杠前不留空格
            format!("bytes {start}-{}/{}", start + expect_len - 1, size),
        ));
    }
    let mut resp = (status, Body::from_stream(stream)).into_response();
    for (k, v) in resp_headers {
        resp.headers_mut().insert(
            k,
            axum::http::HeaderValue::from_str(&v).expect("响应头值非法"),
        );
    }
    resp
}

pub(crate) fn sanitize_name(name: &str) -> String {
    let cleaned: String = name
        .chars()
        .map(|c| {
            if c.is_control() || c == '/' || c == '\\' || c == ':' {
                '_'
            } else {
                c
            }
        })
        .collect();
    let cleaned = cleaned.trim().to_string();
    if cleaned.is_empty() {
        "未命名文件".into()
    } else {
        cleaned
    }
}

pub(crate) fn unique_path(dir: &std::path::Path, name: &str) -> PathBuf {
    let p = dir.join(name);
    if !p.exists() {
        return p;
    }
    let stem = dir.join(name);
    let ext = stem
        .extension()
        .map(|e| format!(".{}", e.to_string_lossy()))
        .unwrap_or_default();
    let base = stem
        .file_stem()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_default();
    for i in 1..1000u32 {
        let candidate = dir.join(format!("{base} ({i}){ext}"));
        if !candidate.exists() {
            return candidate;
        }
    }
    dir.join(format!(
        "{base}-{uuid}{ext}",
        uuid = uuid::Uuid::new_v4().simple()
    ))
}

async fn upload(
    State(st): State<Arc<AppState>>,
    headers: HeaderMap,
    Query(q): Query<HashMap<String, String>>,
    mut mp: Multipart,
) -> Response {
    // 会话凭证优先走请求头（不进 URL，避免被浏览器历史/云同步记录）；兼容旧查询参数
    let session = headers
        .get("x-yunshu-session")
        .and_then(|v| v.to_str().ok())
        .map(str::to_string)
        .or_else(|| q.get("s").cloned());
    let Some(s) = session else {
        return json_err("未配对");
    };
    if !st.pairing.lock().unwrap().session_valid(&s) {
        return json_err("会话已失效，请重新配对");
    }

    // 批次：并发/连续的上传请求共用一次目录选择
    let (dir_slot, notify) = {
        let mut batches = st.batches.lock().unwrap();
        batches.retain(|_, b| b.created.elapsed() < Duration::from_secs(300));
        if let Some(b) = batches.values().next() {
            (b.dir.clone(), b.notify.clone())
        } else {
            let b = Batch {
                dir: Arc::new(tokio::sync::Mutex::new(None)),
                notify: Arc::new(Notify::new()),
                created: Instant::now(),
            };
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
        let display_name = dest
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| name.clone());

        st.new_transfer(TransferInfo::file(
            id.clone(),
            Direction::FromPhone,
            display_name.clone(),
            0,
            Some("phone".into()),
        ));
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
            Err(_) => {
                failed = true;
                None
            }
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
        st.update_transfer(
            &id,
            written,
            Some(written),
            TransferStatus::Done,
            Some(dest.clone()),
        );
        saved.push(serde_json::json!({"name": display_name, "size": written}));
    }

    (
        StatusCode::OK,
        Json(serde_json::json!({"ok": true, "files": saved})),
    )
        .into_response()
}

fn err_page(code: StatusCode, msg: &str) -> Response {
    (code, format!("云书：{msg}")).into_response()
}

fn json_err(msg: &str) -> Response {
    (
        StatusCode::UNAUTHORIZED,
        Json(serde_json::json!({"ok": false, "error": msg})),
    )
        .into_response()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitize_name_strips_separators() {
        assert_eq!(sanitize_name("a/b/c.txt"), "a_b_c.txt");
        assert_eq!(sanitize_name("..\\evil"), ".._evil");
        assert_eq!(sanitize_name("  正常名.txt "), "正常名.txt");
        // 控制字符与盘符
        assert_eq!(sanitize_name("C:\\x"), "C__x");
        // 空名兜底
        assert_eq!(sanitize_name("  "), "未命名文件");
    }

    #[test]
    fn origin_check() {
        // 非浏览器客户端（无 Origin 头）放行
        assert!(origin_allowed(None, Some("192.168.1.5:34256")));
        assert!(origin_allowed(Some("http://192.168.1.5:34256"), Some("192.168.1.5:34256")));
        assert!(origin_allowed(Some("https://192.168.1.5:34257"), Some("192.168.1.5:34257")));
        // 跨源（任意网页向局域网地址发起的 WS）一律拒绝
        assert!(!origin_allowed(Some("http://evil.example"), Some("192.168.1.5:34256")));
        // 同主机不同端口也不算同源
        assert!(!origin_allowed(Some("http://192.168.1.5:9999"), Some("192.168.1.5:34256")));
        // 非 http(s) scheme 的怪 Origin 与缺 Host 一律拒绝
        assert!(!origin_allowed(Some("ftp://192.168.1.5:34256"), Some("192.168.1.5:34256")));
        assert!(!origin_allowed(Some("http://192.168.1.5:34256"), None));
    }

    #[test]
    fn safe_join_rejects_traversal() {
        let root = std::path::Path::new("C:\\save");
        assert!(safe_join(root, "docs/a.txt").is_some());
        assert!(safe_join(root, "..\\evil").is_none() || safe_join(root, "../evil").is_none());
        assert!(safe_join(root, "a/../b").is_none());
        assert!(safe_join(root, "").is_none());
        // 正常相对路径逐段 sanitize
        let joined = safe_join(root, "docs/子目录/文件.txt").unwrap();
        assert!(joined.starts_with(root));
    }

    #[test]
    fn unique_path_avoids_collision() {
        let dir =
            std::env::temp_dir().join(format!("yunshu-test-{}", uuid::Uuid::new_v4().simple()));
        std::fs::create_dir_all(&dir).unwrap();
        let first = unique_path(&dir, "a.txt");
        std::fs::write(&first, b"x").unwrap();
        let second = unique_path(&dir, "a.txt");
        assert_ne!(first, second);
        assert!(second.to_string_lossy().contains("a (1)"));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn build_batch_source_mixed_and_stable() {
        let base =
            std::env::temp_dir().join(format!("yunshu-test-{}", uuid::Uuid::new_v4().simple()));
        let folder = base.join("资料包");
        std::fs::create_dir_all(folder.join("子目录")).unwrap();
        std::fs::write(folder.join("b.txt"), b"bb").unwrap();
        std::fs::write(folder.join("子目录/c.bin"), b"ccc").unwrap();
        std::fs::write(base.join("a top.bin"), b"a").unwrap();

        let (id, src) = build_batch_source(&[folder.clone(), base.join("a top.bin")]).unwrap();
        // 清单按相对名排序：资料包/… 在前（'资' > 'a'？按字节序 a top.bin 的 'a' 更小）
        assert_eq!(src.files.len(), 3);
        assert_eq!(src.total_size, 1 + 2 + 3);
        // 名称排序后 a top.bin 在前
        assert_eq!(src.files[0].1, "a top.bin");
        assert!(src.files.iter().any(|(_, rel, _)| rel == "资料包/b.txt"));
        assert!(
            src.files
                .iter()
                .any(|(_, rel, _)| rel == "资料包/子目录/c.bin")
        );
        assert_eq!(id.len(), 32);
        // 同内容重建 id 稳定
        let (id2, _) = build_batch_source(&[base.join("a top.bin"), folder]).unwrap();
        assert_eq!(id, id2);
        std::fs::remove_dir_all(&base).unwrap();
    }

    #[test]
    fn free_space_smoke() {
        // 仅验证不 panic 且数量级合理（存在卷）
        if let Some(free) = crate::pigeon::free_space(&std::env::temp_dir()) {
            assert!(free > 0);
        }
    }
}
