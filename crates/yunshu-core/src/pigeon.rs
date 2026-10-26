//! 飞鸽模式（PC↔PC）：UDP 广播发现 + Noise 加密直连 + 文件加密推送。
//!
//! 加密：Noise_XX_25519_ChaChaPoly_BLAKE2s（WireGuard 同款原语）。
//! 首次连接交换双方静态密钥并 TOFU 固定（存 known_peers.json）；
//! 之后每次连接校验对方静态密钥与固定值一致，不符立即断开（防冒充）。
//! 所有业务字节（聊天/文件邀请/文件块）都在 Noise 传输态内收发，
//! WS 里的每条 Binary 消息就是一个 Noise 密文（≤64KB）。
//!
//! 发现是单向广播：包里带 {id, name, tcp_port}，听到的人主动 TCP 连过去。
//! 单机双实例靠 SO_REUSEADDR 共享 UDP 端口（Windows 下广播送达所有绑定者）。

use crate::protocol::PigeonWire;
use crate::server::AppState;
use crate::{CoreEvent, Direction, PigeonPeerInfo, TransferInfo, TransferStatus};
use futures_util::{SinkExt, StreamExt};
use snow::TransportState;
use std::collections::HashMap;
use std::net::{IpAddr, SocketAddr, UdpSocket as StdUdpSocket};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

pub const PIGEON_UDP_PORT: u16 = 34255;
pub const PEER_TTL: Duration = Duration::from_secs(90);
const NOISE_PATTERN: &str = "Noise_XX_25519_ChaChaPoly_BLAKE2s";
const CHUNK_SIZE: usize = 60_000;

/// UDP 广播里听到的某台电脑
#[derive(Debug, Clone)]
pub struct PeerRecord {
    pub name: String,
    pub ip: IpAddr,
    pub tcp_port: u16,
    pub last_seen: Instant,
}

/// 出站明文消息（pump 统一加密后发送）
pub enum PigeonOut {
    Control(PigeonWire),
    Chunk { offer_id: [u8; 16], data: Vec<u8> },
}

/// 解密后的入站消息
pub enum PigeonIn {
    Control(PigeonWire),
    Chunk { offer_id: [u8; 16], data: Vec<u8> },
}

/// 一条已建立的加密链路（无论本端是握手发起方还是响应方）
pub struct PigeonLink {
    pub name: String,
    pub out: tokio::sync::mpsc::Sender<PigeonOut>,
    pub closed: Arc<AtomicBool>,
}

// ---------- 本机静态密钥与 TOFU 固定 ----------

pub fn load_machine_key() -> Vec<u8> {
    let dir = dirs::config_dir().map(|d| d.join("yunshu"));
    if let Some(d) = &dir {
        let _ = std::fs::create_dir_all(d);
        let path = d.join("pigeon_key.bin");
        if let Ok(bytes) = std::fs::read(&path)
            && bytes.len() == 32
        {
            return bytes;
        }
        let kp = snow::Builder::new(NOISE_PATTERN.parse().expect("pattern"))
            .generate_keypair()
            .expect("生成密钥对");
        let _ = std::fs::write(&path, &kp.private);
        return kp.private;
    }
    snow::Builder::new(NOISE_PATTERN.parse().expect("pattern"))
        .generate_keypair()
        .expect("生成密钥对")
        .private
}

fn pinned_path() -> Option<PathBuf> {
    dirs::config_dir().map(|d| d.join("yunshu").join("known_peers.json"))
}

fn load_pinned() -> HashMap<String, String> {
    pinned_path()
        .and_then(|p| std::fs::read_to_string(p).ok())
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

fn save_pinned(map: &HashMap<String, String>) {
    if let Some(p) = pinned_path()
        && let Ok(json) = serde_json::to_string_pretty(map)
        && let Err(e) = std::fs::write(p, json)
    {
        tracing::warn!("写入固定密钥失败: {e}");
    }
}

/// SHA-256 指纹（冒号分组十六进制，如 ab:cd:…）
pub fn fingerprint(remote_static: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    let h = Sha256::digest(remote_static);
    h.iter().map(|b| format!("{b:02x}")).collect::<Vec<_>>().join(":")
}

/// 密钥指纹是否已被用户确认固定
pub fn is_pinned(st: &AppState, peer_id: &str, remote_static: &[u8]) -> bool {
    let hex: String = remote_static.iter().map(|b| format!("{b:02x}")).collect();
    let _ = st;
    let map = load_pinned();
    match map.get(peer_id) {
        Some(pinned) => pinned == &hex,
        None => false,
    }
}

/// 用户确认后写入固定表
pub fn pin_peer(st: &AppState, peer_id: &str, fingerprint: &str) {
    let mut map = load_pinned();
    map.insert(peer_id.to_string(), fingerprint.to_string());
    save_pinned(&map);
    tracing::info!("设备 {peer_id} 已被用户信任（指纹 {}…）", &fingerprint[..16.min(fingerprint.len())]);
}

/// 已固定的密钥与本次握手不符 → 拒绝（防冒充）
pub fn verify_pinned_matches(peer_id: &str, remote_static: &[u8]) -> Result<(), String> {
    let hex: String = remote_static.iter().map(|b| format!("{b:02x}")).collect();
    let map = load_pinned();
    match map.get(peer_id) {
        Some(pinned) if pinned == &hex => Ok(()),
        Some(_) => Err("对方密钥与首次配对时不符，可能是重装系统或被冒充".into()),
        None => Ok(()),
    }
}

// ---------- Noise 基础 ----------

pub fn new_handshake(initiator: bool) -> Result<snow::HandshakeState, String> {
    let params: snow::params::NoiseParams = NOISE_PATTERN.parse().map_err(|e| format!("{e}"))?;
    let key = load_machine_key();
    let builder = snow::Builder::new(params)
        .local_private_key(&key)
        .map_err(|e| format!("{e}"))?;
    if initiator {
        builder.build_initiator().map_err(|e| format!("{e}"))
    } else {
        builder.build_responder().map_err(|e| format!("{e}"))
    }
}

pub fn seal_msg(noise: &Mutex<TransportState>, msg: &PigeonOut) -> Result<Vec<u8>, String> {
    let mut plain = Vec::with_capacity(64 * 1024);
    match msg {
        PigeonOut::Control(w) => {
            plain.push(b'C');
            serde_json::to_writer(&mut plain, w).map_err(|e| e.to_string())?;
        }
        PigeonOut::Chunk { offer_id, data } => {
            plain.push(b'F');
            plain.extend_from_slice(offer_id);
            plain.extend_from_slice(data);
        }
    }
    let mut buf = vec![0u8; plain.len() + 64];
    let n = noise.lock().unwrap().write_message(&plain, &mut buf).map_err(|e| e.to_string())?;
    buf.truncate(n);
    Ok(buf)
}

pub fn open_msg(noise: &Mutex<TransportState>, cipher: &[u8]) -> Result<PigeonIn, String> {
    let mut buf = vec![0u8; cipher.len()];
    let n = noise.lock().unwrap().read_message(cipher, &mut buf).map_err(|e| e.to_string())?;
    match buf.first() {
        Some(b'C') => Ok(PigeonIn::Control(serde_json::from_slice(&buf[1..n]).map_err(|e| e.to_string())?)),
        Some(b'F') if n >= 17 => {
            let mut id = [0u8; 16];
            id.copy_from_slice(&buf[1..17]);
            Ok(PigeonIn::Chunk { offer_id: id, data: buf[17..n].to_vec() })
        }
        _ => Err("未知消息前缀".into()),
    }
}

// ---------- 发现层 ----------

pub fn make_udp_socket() -> StdUdpSocket {
    let sock = socket2::Socket::new(
        socket2::Domain::IPV4,
        socket2::Type::DGRAM,
        Some(socket2::Protocol::UDP),
    )
    .expect("创建 UDP socket 失败");
    let _ = sock.set_reuse_address(true);
    let _ = sock.set_nonblocking(true);
    let addr: SocketAddr = ([0, 0, 0, 0], PIGEON_UDP_PORT).into();
    sock.bind(&addr.into()).expect("UDP 34255 绑定失败");
    sock.into()
}

/// 同步上下文非阻塞投递控制消息（UI 线程用；队列满或断开则丢弃并返回 false）
pub fn try_push(st: &AppState, peer_id: &str, wire: PigeonWire) -> bool {
    let Ok(conns) = st.pigeon_conns.lock() else { return false };
    match conns.get(peer_id) {
        Some(link) => link.out.try_send(PigeonOut::Control(wire)).is_ok(),
        None => false,
    }
}

/// 异步上下文投递控制消息
pub async fn push_control(st: &AppState, peer_id: &str, wire: PigeonWire) -> bool {
    let link = st.pigeon_conns.lock().unwrap().get(peer_id).map(|l| l.out.clone());
    match link {
        Some(out) => out.send(PigeonOut::Control(wire)).await.is_ok(),
        None => false,
    }
}

pub fn peers_event(st: &AppState) -> CoreEvent {
    let peers: Vec<PigeonPeerInfo> = {
        let known = st.pigeon_peers.lock().unwrap();
        let conns = st.pigeon_conns.lock().unwrap();
        let mut v: Vec<PigeonPeerInfo> = known
            .iter()
            .map(|(id, p)| PigeonPeerInfo {
                id: id.clone(),
                name: p.name.clone(),
                ip: p.ip.to_string(),
                tcp_port: p.tcp_port,
                connected: conns.contains_key(id),
            })
            .collect();
        v.sort_by(|a, b| a.name.cmp(&b.name).then(a.id.cmp(&b.id)));
        v
    };
    CoreEvent::PigeonPeers { peers }
}

pub fn emit_peers(st: &AppState) {
    st.emit(peers_event(st));
}

/// 发现主循环：广播 + 收包 + 过期清扫
pub async fn run_discovery(st: Arc<AppState>) {
    let std_sock = make_udp_socket();
    let sock = Arc::new(tokio::net::UdpSocket::from_std(std_sock).expect("UDP socket 转换失败"));
    let _ = sock.set_broadcast(true);

    let announce = serde_json::to_vec(&Announce {
        v: 1,
        kind: "entry".into(),
        id: st.self_id.clone(),
        name: st.cfg.pc_name.clone(),
        port: st.cfg.port,
    })
    .unwrap_or_default();

    // 多网卡机器上 255.255.255.255 只从默认路由出口发，其他网卡上的实例收不到。
    // 改为：对本机每个 IPv4 网卡的子网广播地址各发一份（外加全局广播兜底）。
    fn broadcast_targets() -> Vec<SocketAddr> {
        let mut v = vec![SocketAddr::from(([255, 255, 255, 255], PIGEON_UDP_PORT))];
        if let Ok(addrs) = if_addrs::get_if_addrs() {
            for i in addrs {
                if let if_addrs::IfAddr::V4(v4) = i.addr {
                    let bc = v4.broadcast.unwrap_or_else(|| {
                        let mask = u32::from_be_bytes(v4.netmask.octets());
                        let ip = u32::from_be_bytes(v4.ip.octets());
                        std::net::Ipv4Addr::from(ip | !mask)
                    });
                    v.push(SocketAddr::from((std::net::Ipv4Addr::from(u32::from(bc)), PIGEON_UDP_PORT)));
                }
            }
        }
        v.sort();
        v.dedup();
        v
    }

    for bc in broadcast_targets() {
        let _ = sock.send_to(&announce, bc).await;
    }

    tracing::info!("飞鸽发现已启动（UDP {}，本机 id {}）", PIGEON_UDP_PORT, &st.self_id[..8]);
    let mut tick = tokio::time::interval(Duration::from_secs(10));
    let mut n = 0u32;
    let mut buf = [0u8; 2048];

    loop {
        tokio::select! {
            r = sock.recv_from(&mut buf) => {
                if let Ok((n, from)) = r {
                    handle_packet(&st, &buf[..n], from).await;
                }
            }
            _ = tick.tick() => {
                n += 1;
                // 启动后 10 秒补一次广播，让对方尽快发现
                // 启动后前 50 秒密集广播（0/10/20/30/40s 各一次），让对方尽快发现
                if n <= 5 || n.is_multiple_of(3) {
                    for bc in broadcast_targets() {
                        let _ = sock.send_to(&announce, bc).await;
                    }
                }
                sweep_expired(&st).await;
            }
        }
    }
}

#[derive(serde::Serialize, serde::Deserialize)]
struct Announce {
    v: u8,
    kind: String,
    id: String,
    name: String,
    port: u16,
}

async fn handle_packet(st: &Arc<AppState>, data: &[u8], from: SocketAddr) {
    let Ok(p) = serde_json::from_slice::<Announce>(data) else { return };
    if p.id != st.self_id && !p.id.is_empty() && p.kind == "entry" {
        let is_new = {
            let mut known = st.pigeon_peers.lock().unwrap();
            let is_new = !known.contains_key(&p.id);
            known.insert(
                p.id.clone(),
                PeerRecord { name: p.name.clone(), ip: from.ip(), tcp_port: p.port, last_seen: Instant::now() },
            );
            is_new
        };
        if is_new {
            tracing::info!("发现电脑「{}」（{}:{}）", p.name, from.ip(), p.port);
            emit_peers(st);
        }
        // 只要没有链路就尝试连接（覆盖：首次发现 / 链路断开后重连）。
        // 双方同时外连会互相拆链路：约定只有 id 更大的一方主动发起，另一侧等待连入。
        let should_connect = p.id > st.self_id
            && {
                let mut connecting = st.pigeon_connecting.lock().unwrap();
                !st.pigeon_conns.lock().unwrap().contains_key(&p.id) && connecting.insert(p.id.clone())
            };
        if should_connect {
            let st2 = st.clone();
            let id = p.id.clone();
            let ip = from.ip();
            tokio::spawn(async move {
                pigeon_connect(st2, id, ip, p.port).await;
            });
        }
    }
}

async fn sweep_expired(st: &Arc<AppState>) {
    let removed = {
        let mut known = st.pigeon_peers.lock().unwrap();
        let before = known.len();
        known.retain(|_, p| p.last_seen.elapsed() < PEER_TTL);
        known.len() != before
    };
    if removed {
        tracing::info!("有电脑心跳超时下线");
        emit_peers(st);
    }
}

// ---------- 客户端（发起方）----------

pub async fn pigeon_connect(st: Arc<AppState>, peer_id: String, ip: IpAddr, port: u16) {
    let result = pigeon_connect_inner(&st, &peer_id, ip, port).await;
    st.pigeon_connecting.lock().unwrap().remove(&peer_id);
    if let Err(e) = result {
        tracing::info!("连接「{peer_id}」失败：{e}（等对方下一次广播再试）");
    }
}

async fn pigeon_connect_inner(st: &Arc<AppState>, peer_id: &str, ip: IpAddr, port: u16) -> Result<(), String> {
    if st.pigeon_conns.lock().unwrap().contains_key(peer_id) {
        return Ok(());
    }
    let name = st
        .pigeon_peers
        .lock()
        .unwrap()
        .get(peer_id)
        .map(|p| p.name.clone())
        .unwrap_or_else(|| "电脑".into());

    let tcp = tokio::time::timeout(Duration::from_secs(5), TcpStream::connect((ip, port)))
        .await
        .map_err(|_| "连接超时".to_string())?
        .map_err(|e| e.to_string())?;
    let (ws, _resp) = tokio_tungstenite::client_async(format!("ws://{ip}:{port}/pigeon-ws"), tcp)
        .await
        .map_err(|e| e.to_string())?;
    let (mut sink, mut stream) = ws.split();

    // ---- Noise XX 握手（本端是 initiator）----
    let mut hs = new_handshake(true)?;
    let mut buf = vec![0u8; 65535];
    let n = hs.write_message(&[], &mut buf).map_err(|e| e.to_string())?;
    sink.send(tokio_tungstenite::tungstenite::Message::binary(buf[..n].to_vec()))
        .await
        .map_err(|e| e.to_string())?;
    let m2 = tokio::time::timeout(Duration::from_secs(10), stream.next())
        .await
        .map_err(|_| "握手超时".to_string())?
        .ok_or("对方断开")?
        .map_err(|e| e.to_string())?;
    let m2 = match m2 {
        tokio_tungstenite::tungstenite::Message::Binary(b) => b.to_vec(),
        _ => return Err("握手消息格式错误".into()),
    };
    hs.read_message(&m2, &mut buf).map_err(|e| e.to_string())?;
    let n = hs.write_message(&[], &mut buf).map_err(|e| e.to_string())?;
    sink.send(tokio_tungstenite::tungstenite::Message::binary(buf[..n].to_vec()))
        .await
        .map_err(|e| e.to_string())?;
    let noise = hs.into_transport_mode().map_err(|e| e.to_string())?;
    let noise = Arc::new(Mutex::new(noise));
    let remote_static = noise.lock().unwrap().get_remote_static().map(|s| s.to_vec()).unwrap_or_default();

    // ---- 加密 hello ----
    let hello = PigeonOut::Control(PigeonWire::PigeonHello {
        id: st.self_id.clone(),
        name: st.cfg.pc_name.clone(),
        port: st.cfg.port,
    });
    let frame = seal_msg(&noise, &hello)?;
    sink.send(tokio_tungstenite::tungstenite::Message::binary(frame))
        .await
        .map_err(|e| e.to_string())?;
    let welcome = tokio::time::timeout(Duration::from_secs(10), stream.next())
        .await
        .map_err(|_| "握手超时".to_string())?
        .ok_or("对方断开")?
        .map_err(|e| e.to_string())?;
    let unwrapped = match welcome {
        tokio_tungstenite::tungstenite::Message::Binary(b) => open_msg(&noise, &b)?,
        _ => return Err("welcome 格式错误".into()),
    };
    if let PigeonIn::Control(PigeonWire::PigeonWelcome { ok, reason, .. }) = unwrapped
        && !ok {
            let r = reason.unwrap_or_else(|| "对方拒绝连接".into());
            // 对方在等待其用户确认本机：本地同步弹确认（指纹来自本次握手）
            if r.contains("待确认") {
                let fp = fingerprint(&remote_static);
                st.emit(crate::CoreEvent::PigeonTrustRequest {
                    peer_id: peer_id.to_string(),
                    name: name.clone(),
                    fingerprint: fp.clone(),
                });
                st.pending_trusts
                    .lock()
                    .unwrap()
                    .insert(peer_id.to_string(), (name, fp));
            }
            return Err(r);
        }
    // 身份校验：已固定的密钥必须与本次握手一致（防冒充）
    verify_pinned_matches(peer_id, &remote_static)?;
    if st.pigeon_conns.lock().unwrap().contains_key(peer_id) {
        return Ok(()); // 对方已抢先建立，放弃本连接
    }

    // ---- 注册链路 + 加密泵 ----
    let (plain_out_tx, plain_out_rx) = tokio::sync::mpsc::channel::<PigeonOut>(64);
    st.pigeon_conns.lock().unwrap().insert(
        peer_id.to_string(),
        PigeonLink { name: name.clone(), out: plain_out_tx.clone(), closed: Arc::new(AtomicBool::new(false)) },
    );
    tracing::info!("已与「{name}」建立加密飞鸽链路（Noise XX）");
    emit_peers(st);

    let (cipher_tx, mut cipher_rx) = tokio::sync::mpsc::channel::<Vec<u8>>(64);
    let (frame_tx, frame_rx) = tokio::sync::mpsc::channel::<Vec<u8>>(64);
    let pump = tokio::spawn(pump(
        st.clone(),
        peer_id.to_string(),
        name.clone(),
        noise.clone(),
        frame_rx,
        plain_out_rx,
        cipher_tx,
    ));
    let writer = tokio::spawn(async move {
        while let Some(frame) = cipher_rx.recv().await {
            if sink.send(tokio_tungstenite::tungstenite::Message::binary(frame)).await.is_err() {
                break;
            }
        }
        let _ = sink.close().await;
    });
    let mut drop_reason = "客户端读流结束（对端关闭）";
    while let Some(Ok(msg)) = stream.next().await {
        let closed = st
            .pigeon_conns
            .lock()
            .unwrap()
            .get(peer_id)
            .map(|l| l.closed.load(Ordering::Relaxed))
            .unwrap_or(true);
        if closed {
            drop_reason = "客户端 closed 标志";
            break;
        }
        match msg {
            tokio_tungstenite::tungstenite::Message::Binary(b) => {
                if frame_tx.send(b.to_vec()).await.is_err() {
                    drop_reason = "客户端加密泵已退出";
                    break;
                }
            }
            tokio_tungstenite::tungstenite::Message::Close(_) => {
                drop_reason = "对端发送了 Close";
                break;
            }
            _ => continue,
        }
    }

    st.pigeon_conns.lock().unwrap().remove(peer_id);
    pump.abort();
    writer.abort();
    tracing::info!("与「{peer_id}」的加密飞鸽链路断开（{drop_reason}）");
    emit_peers(st);
    Ok(())
}

// ---------- 加密泵（两侧共用）----------

/// 从 frame_rx 收密文解密分发；从 plain_out_rx 收明文加密发送。
/// 任一通道关闭或解密失败即结束并清理链路。
pub async fn pump(
    st: Arc<AppState>,
    peer_id: String,
    peer_name: String,
    noise: Arc<Mutex<TransportState>>,
    mut cipher_in: tokio::sync::mpsc::Receiver<Vec<u8>>,
    mut plain_out_rx: tokio::sync::mpsc::Receiver<PigeonOut>,
    cipher_out: tokio::sync::mpsc::Sender<Vec<u8>>,
) {
    loop {
        tokio::select! {
            frame = cipher_in.recv() => match frame {
                Some(f) => match open_msg(&noise, &f) {
                    Ok(plain) => dispatch(&st, &peer_id, plain).await,
                    Err(e) => {
                        tracing::warn!("解密失败，断开「{peer_name}」链路：{e}");
                        break;
                    }
                },
                None => break,
            },
            out = plain_out_rx.recv() => match out {
                Some(o) => match seal_msg(&noise, &o) {
                    Ok(f) => {
                        if cipher_out.send(f).await.is_err() {
                            break;
                        }
                    }
                    Err(e) => {
                        tracing::warn!("加密失败，断开链路：{e}");
                        break;
                    }
                },
                None => break,
            },
        }
        let closed = st
            .pigeon_conns
            .lock()
            .unwrap()
            .get(&peer_id)
            .map(|l| l.closed.load(Ordering::Relaxed))
            .unwrap_or(true);
        if closed {
            break;
        }
    }
    drop_link(&st, &peer_id);
    tracing::info!("「{peer_name}」的加密链路泵结束");
    emit_peers(&st);
}

async fn dispatch(st: &Arc<AppState>, peer_id: &str, msg: PigeonIn) {
    match msg {
        PigeonIn::Control(PigeonWire::PcText { text }) => {
            let peer_name = link_name(st, peer_id);
            tracing::info!("收到「{peer_name}」的聊天消息：{text}");
            st.emit(CoreEvent::ChatFromPc { peer_id: peer_id.to_string(), peer_name, text });
        }
        PigeonIn::Control(PigeonWire::BroadcastText { text }) => {
            let peer_name = link_name(st, peer_id);
            tracing::info!("收到「{peer_name}」的广播消息：{text}");
            st.emit(CoreEvent::ChatBroadcast { peer_id: peer_id.to_string(), peer_name, text });
        }
        PigeonIn::Control(PigeonWire::PcFile { id, name, size }) => {
            let peer_name = link_name(st, peer_id);
            tracing::info!("「{peer_name}」请求发送文件「{name}」（{size} 字节）");
            if std::env::var("YUNSHU_AUTO_ACCEPT").is_ok() {
                tracing::info!("AUTO_ACCEPT：自动接受「{name}」");
                push_control(st, peer_id, PigeonWire::FileDecision { id, accepted: true }).await;
            } else {
                st.emit(CoreEvent::PigeonOfferIncoming {
                    peer_id: peer_id.to_string(),
                    peer_name,
                    offer_id: id,
                    name,
                    size,
                });
            }
        }
        PigeonIn::Control(PigeonWire::FileDecision { id, accepted }) => {
            if accepted {
                let offer = st.offers.lock().unwrap().get(&id).cloned();
                if let Some((path, name, size)) = offer {
                    st.update_transfer(&id, 0, None, TransferStatus::Active, None);
                    let st2 = st.clone();
                    let pid = peer_id.to_string();
                    tokio::spawn(async move {
                        push_file(st2, pid, id, path, name, size).await;
                    });
                } else {
                    st.update_transfer(&id, 0, None, TransferStatus::Failed, None);
                }
            } else {
                st.update_transfer(&id, 0, None, TransferStatus::Refused, None);
                st.emit(CoreEvent::Toast { text: "对方拒绝了文件接收".into() });
            }
        }
        PigeonIn::Control(PigeonWire::FileStart { id, name, size }) => {
            let dest = crate::server::unique_path(&st.cfg.save_dir, &crate::server::sanitize_name(&name));
            let display = dest
                .file_name()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_else(|| name.clone());
            match tokio::fs::File::create(&dest).await {
                Ok(f) => {
                    let row_id = uuid::Uuid::new_v4().simple().to_string();
                    st.new_transfer(TransferInfo::file(row_id.clone(), Direction::FromPhone, display.clone(), size, Some(peer_id.to_string())));
                    st.update_transfer(&row_id, 0, None, TransferStatus::Active, None);
                    st.recv_files.lock().await.insert(
                        id,
                        crate::server::RecvFileState { row_id, file: f, written: 0, dest },
                    );
                }
                Err(e) => {
                    tracing::error!("创建接收文件失败: {e}");
                    let _ = push_control(st, peer_id, PigeonWire::FileDecision { id, accepted: false }).await;
                }
            }
        }
        PigeonIn::Control(PigeonWire::FileEnd { id, ok }) => {
            let state = st.recv_files.lock().await.remove(&id);
            match state {
                Some(s) => {
                    if ok {
                        st.update_transfer(&s.row_id, s.written, Some(s.written), TransferStatus::Done, Some(s.dest.clone()));
                        st.emit(CoreEvent::Toast { text: format!("收到电脑传来的「{}」", s.dest.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default()) });
                    } else {
                        st.update_transfer(&s.row_id, s.written, Some(s.written), TransferStatus::Failed, None);
                        let _ = tokio::fs::remove_file(&s.dest).await;
                    }
                }
                None => tracing::warn!("FileEnd 找不到对应的接收状态（{id}）"),
            }
        }
        PigeonIn::Control(_) => {}
        PigeonIn::Chunk { offer_id, data } => {
            let key = uuid::Uuid::from_bytes(offer_id).simple().to_string();
            let mut states = st.recv_files.lock().await;
            if let Some(s) = states.get_mut(&key)
                && s.file.write_all(&data).await.is_ok()
            {
                s.written += data.len() as u64;
                st.progress_throttled(&s.row_id, s.written, false);
            }
        }
    }
}

/// 发送方：对方接受后，把文件切块经加密通道推送
pub async fn push_file(st: Arc<AppState>, peer_id: String, offer_id: String, path: PathBuf, name: String, size: u64) {
    let out = st.pigeon_conns.lock().unwrap().get(&peer_id).map(|l| l.out.clone());
    let Some(out) = out else {
        st.update_transfer(&offer_id, 0, None, TransferStatus::Failed, None);
        return;
    };
    let Ok(uuid) = uuid::Uuid::parse_str(&offer_id) else { return };
    let id16 = *uuid.as_bytes();
    let Ok(mut file) = tokio::fs::File::open(&path).await else {
        st.update_transfer(&offer_id, 0, None, TransferStatus::Failed, None);
        return;
    };
    let _ = out.send(PigeonOut::Control(PigeonWire::FileStart { id: offer_id.clone(), name: name.clone(), size })).await;
    st.update_transfer(&offer_id, 0, None, TransferStatus::Active, None);
    let mut sent: u64 = 0;
    let mut buf = vec![0u8; CHUNK_SIZE];
    loop {
        match file.read(&mut buf).await {
            Ok(0) => break,
            Ok(n) => {
                if out.send(PigeonOut::Chunk { offer_id: id16, data: buf[..n].to_vec() }).await.is_err() {
                    st.update_transfer(&offer_id, sent, Some(size), TransferStatus::Failed, None);
                    return;
                }
                sent += n as u64;
                st.progress_throttled(&offer_id, sent, false);
            }
            Err(_) => {
                st.update_transfer(&offer_id, sent, Some(size), TransferStatus::Failed, None);
                return;
            }
        }
    }
    let _ = out.send(PigeonOut::Control(PigeonWire::FileEnd { id: offer_id.clone(), ok: true })).await;
    st.update_transfer(&offer_id, sent, Some(size), TransferStatus::Done, Some(path));
    tracing::info!("文件「{name}」已加密推送完成（{sent} 字节）");
}

fn link_name(st: &AppState, peer_id: &str) -> String {
    st.pigeon_conns
        .lock()
        .unwrap()
        .get(peer_id)
        .map(|l| l.name.clone())
        .unwrap_or_else(|| "电脑".into())
}

pub fn drop_link(st: &AppState, peer_id: &str) {
    if st.pigeon_conns.lock().unwrap().remove(peer_id).is_some() {
        emit_peers(st);
    }
}
