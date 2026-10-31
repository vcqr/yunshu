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
use crate::{
    CoreEvent, Direction, FileEntry, PigeonPeerInfo, TransferInfo, TransferKind, TransferStatus,
};
use futures_util::{SinkExt, StreamExt};
use sha2::Digest;
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
pub const PEER_TTL: Duration = Duration::from_secs(45);
const NOISE_PATTERN: &str = "Noise_XX_25519_ChaChaPoly_BLAKE2s";
const CHUNK_SIZE: usize = 60_000;

/// UDP 广播里听到的某台电脑
#[derive(Debug, Clone)]
pub struct PeerRecord {
    pub name: String,
    pub ip: IpAddr,
    pub tcp_port: u16,
    /// 公开机器 ID（静态密钥指纹前 8 位 hex，跨重启稳定；远程控制以此定位设备）
    pub machine_id: String,
    pub last_seen: Instant,
}

/// 出站明文消息（pump 统一加密后发送）
pub enum PigeonOut {
    Control(PigeonWire),
    Chunk {
        offer_id: [u8; 16],
        offset: u64,
        data: Vec<u8>,
    },
}

/// 解密后的入站消息
pub enum PigeonIn {
    Control(PigeonWire),
    Chunk {
        offer_id: [u8; 16],
        offset: u64,
        data: Vec<u8>,
    },
}

/// 一条已建立的加密链路（无论本端是握手发起方还是响应方）
pub struct PigeonLink {
    pub name: String,
    pub out: tokio::sync::mpsc::Sender<PigeonOut>,
    pub closed: Arc<AtomicBool>,
}

// ---------- 本机静态密钥与 TOFU 固定 ----------

/// Unix 下把文件权限收紧为属主可读写（私钥/信任表等敏感落盘）。
pub(crate) fn set_owner_only(path: &std::path::Path) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600));
    }
    #[cfg(not(unix))]
    let _ = path;
}

/// X25519 私钥 → 公钥（snow 不暴露本地静态公钥；
/// 机器 ID 须从公钥派生，控制端才能在握手后复算校验对方身份）
pub fn public_from_private(private: &[u8]) -> [u8; 32] {
    let mut key = [0u8; 32];
    if private.len() == 32 {
        key.copy_from_slice(private);
    }
    *x25519_dalek::PublicKey::from(&x25519_dalek::StaticSecret::from(key)).as_bytes()
}

pub fn load_machine_key() -> Vec<u8> {
    crate::ensure_config_dir();
    let dir = crate::config_dir();
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
        set_owner_only(&path);
        return kp.private;
    }
    snow::Builder::new(NOISE_PATTERN.parse().expect("pattern"))
        .generate_keypair()
        .expect("生成密钥对")
        .private
}

fn pinned_path() -> Option<PathBuf> {
    crate::ensure_config_dir();
    crate::config_dir().map(|d| d.join("known_peers.json"))
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
    } else if let Some(p) = pinned_path() {
        set_owner_only(&p);
    }
}

/// SHA-256 指纹（冒号分组十六进制，如 ab:cd:…）
pub fn fingerprint(remote_static: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    let h = Sha256::digest(remote_static);
    h.iter()
        .map(|b| format!("{b:02x}"))
        .collect::<Vec<_>>()
        .join(":")
}

/// 稳定批次 id：sha256(数量 | 每项(名称|大小|mtime毫秒)) 前 32 位 hex。
/// 同一批磁盘内容重发时 id 不变，接收方按 id 找到半成品续传；任何文件被修改（mtime 变化）→ 新 id 全新传输。
pub fn batch_id(items: &[(String, u64, u128)]) -> String {
    use sha2::{Digest, Sha256};
    let mut h = Sha256::new();
    h.update((items.len() as u64).to_le_bytes());
    for (name, size, mtime) in items {
        h.update(name.as_bytes());
        h.update([0]);
        h.update(size.to_le_bytes());
        h.update([0]);
        h.update(mtime.to_le_bytes());
        h.update([0]);
    }
    sha_hex(&h.finalize())[..32].to_string()
}

/// 批次半成品文件路径（按清单下标编号，文件名可能含子目录不安全）
pub fn part_path(dir: &std::path::Path, index: usize) -> std::path::PathBuf {
    dir.join(format!("{index}.part"))
}

/// 流偏移 → (文件下标, 文件内偏移)。恰好落在文件边界时归一为下一文件开头；
/// 越过全部数据（offset == 总大小）返回 (len, 0)，由调用方直接收尾。
pub fn stream_locate(sizes: &[u64], mut offset: u64) -> (usize, u64) {
    for (i, s) in sizes.iter().enumerate() {
        if offset < *s {
            return (i, offset);
        }
        offset -= s;
    }
    (sizes.len(), 0)
}

/// 断点续传的安全偏移：从清单头开始累计，遇到「未写满」或「未校验」的文件停止，
/// 该文件的部分写入计入偏移（其后的数据会被重推覆盖）。
/// 续传粒度为单文件：只有「写满且已校验」的文件计入偏移，
/// 第一个未完成的文件及其后全部从头重收（保证哈希覆盖完整文件）。
pub fn resume_have(sizes: &[u64], written: &[u64], verified: &[bool]) -> u64 {
    let mut have = 0u64;
    for ((&size, &w), &v) in sizes.iter().zip(written).zip(verified) {
        if w == size && v {
            have += size;
        } else {
            break;
        }
    }
    have
}

pub fn sha_hex(d: &[u8]) -> String {
    d.iter().map(|b| format!("{b:02x}")).collect()
}

/// 从批次清单推导显示名（FileStart 只带 id，名字由清单重建）
fn name_display(files: &[crate::server::BatchFileState]) -> String {
    if files.len() == 1 {
        files[0]
            .name
            .rsplit('/')
            .next()
            .unwrap_or(&files[0].name)
            .to_string()
    } else {
        let first_root = files[0].name.split('/').next().unwrap_or_default();
        if !first_root.is_empty()
            && files
                .iter()
                .all(|f| f.name.split('/').next() == Some(first_root))
        {
            format!("{first_root}/")
        } else {
            format!("{} 项", files.len())
        }
    }
}

/// 保存目录所在卷的剩余空间（字节）；非 Windows 或查询失败返回 None（跳过预检）
#[cfg(windows)]
pub fn free_space(path: &std::path::Path) -> Option<u64> {
    use windows_sys::Win32::Storage::FileSystem::GetDiskFreeSpaceExW;
    let wide: Vec<u16> = path
        .to_string_lossy()
        .encode_utf16()
        .chain(std::iter::once(0))
        .collect();
    let mut free: u64 = 0;
    let ok = unsafe {
        GetDiskFreeSpaceExW(
            wide.as_ptr(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            &mut free,
        )
    };
    (ok != 0).then_some(free)
}

#[cfg(not(windows))]
pub fn free_space(_path: &std::path::Path) -> Option<u64> {
    None
}

/// 密钥指纹是否已被用户确认固定。
/// 按指纹键控（而非 peer_id）：peer_id 是发现层会话标识，密钥指纹才是稳定的设备身份；
/// 同机多实例共享机器密钥时，一次信任对所有实例生效。
pub fn is_pinned(remote_static: &[u8]) -> bool {
    let map = load_pinned();
    map.contains_key(&fingerprint(remote_static))
}

/// 用户确认后写入固定表
pub fn pin_fingerprint(fp: &str) {
    let mut map = load_pinned();
    map.insert(fp.to_string(), "1".to_string());
    save_pinned(&map);
    tracing::info!("设备已被用户信任（指纹 {}…）", &fp[..16.min(fp.len())]);
}

// ---------- 自助添加主机（手动登记，不经广播发现） ----------

/// 手动添加的主机（持久化于配置目录 manual_hosts.json）。
/// addr 为 "host:port"；peer_id / machine_id / 名称在首次成功连接后回填。
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ManualHost {
    pub addr: String,
    pub name: String,
    #[serde(default)]
    pub machine_id: String,
    #[serde(default)]
    pub peer_id: Option<String>,
}

fn manual_hosts_path() -> Option<PathBuf> {
    crate::ensure_config_dir();
    crate::config_dir().map(|d| d.join("manual_hosts.json"))
}

pub fn load_manual_hosts() -> Vec<ManualHost> {
    manual_hosts_path()
        .and_then(|p| std::fs::read_to_string(p).ok())
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

pub fn save_manual_hosts(hosts: &[ManualHost]) {
    if let Some(p) = manual_hosts_path()
        && let Ok(json) = serde_json::to_string_pretty(hosts)
    {
        if let Err(e) = std::fs::write(&p, json) {
            tracing::warn!("写入手动主机表失败: {e}");
        }
        set_owner_only(&p);
    }
}

/// 解析用户输入的地址：`192.168.1.5` / `192.168.1.5:8001` / `puter.local`。
/// 缺省端口 34256；暂不支持 IPv6 字面量。
pub fn parse_host_addr(input: &str) -> Result<String, String> {
    let input = input.trim();
    let (host, port) = match input.rsplit_once(':') {
        Some((h, p)) if !h.is_empty() && !p.is_empty() && p.chars().all(|c| c.is_ascii_digit()) => (
            h,
            p.parse::<u16>()
                .map_err(|_| "端口无效（1-65535）".to_string())?,
        ),
        _ if input.contains("::") => {
            return Err("暂不支持 IPv6 字面量，请使用 IPv4 地址或主机名".into());
        }
        _ => (input, 34256),
    };
    let host = host.trim();
    let ok = !host.is_empty()
        && host.len() <= 253
        && host
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '-');
    if !ok {
        return Err("地址格式：IPv4 或主机名，可带端口（如 192.168.1.5 或 192.168.1.5:8001）".into());
    }
    Ok(format!("{host}:{port}"))
}

pub fn addr_host(addr: &str) -> &str {
    addr.rsplit_once(':').map(|(h, _)| h).unwrap_or(addr)
}

pub fn addr_port(addr: &str) -> u16 {
    addr.rsplit_once(':')
        .and_then(|(_, p)| p.parse().ok())
        .unwrap_or(34256)
}

fn same_addr(manual_addr: &str, ip: &str, port: u16) -> bool {
    manual_addr == &format!("{ip}:{port}")
}

/// 手动连接入口：guard 防重入，quiet 控制失败是否弹提示（后台重试静默）
pub async fn pigeon_connect_manual(st: Arc<AppState>, addr: String, quiet: bool) {
    if !st
        .pigeon_connecting
        .lock()
        .unwrap()
        .insert(format!("manual:{addr}"))
    {
        return; // 已有同地址连接在途
    }
    let result = pigeon_connect_manual_inner(&st, &addr).await;
    st.pigeon_connecting
        .lock()
        .unwrap()
        .remove(&format!("manual:{addr}"));
    match result {
        Ok(()) => {}
        Err(e) => {
            tracing::info!("手动连接 {addr} 失败：{e}");
            if !quiet {
                st.emit(CoreEvent::Toast {
                    text: format!("连接 {addr} 失败：{e}"),
                });
            }
        }
    }
}

async fn pigeon_connect_manual_inner(st: &Arc<AppState>, addr: &str) -> Result<(), String> {
    // 该地址已有存活链路则不重复连
    let known_peer = st
        .manual_hosts
        .lock()
        .unwrap()
        .get(addr)
        .and_then(|m| m.peer_id.clone());
    if let Some(pid) = known_peer
        && st.pigeon_conns.lock().unwrap().contains_key(&pid)
    {
        return Ok(());
    }
    let port: u16 = addr_port(addr);
    let host = addr_host(addr).to_string();
    let display_name = st
        .manual_hosts
        .lock()
        .unwrap()
        .get(addr)
        .map(|m| m.name.clone())
        .unwrap_or_else(|| addr.to_string());

    let tcp = tokio::time::timeout(Duration::from_secs(5), TcpStream::connect((host.as_str(), port)))
        .await
        .map_err(|_| "连接超时".to_string())?
        .map_err(|e| e.to_string())?;
    let (ws, _resp) = tokio_tungstenite::client_async(format!("ws://{addr}/pigeon-ws"), tcp)
        .await
        .map_err(|e| e.to_string())?;
    let (mut sink, mut stream) = ws.split();

    // ---- Noise XX 握手（initiator，同发现流程）----
    let mut hs = new_handshake(true)?;
    let mut buf = vec![0u8; 65535];
    let n = hs.write_message(&[], &mut buf).map_err(|e| e.to_string())?;
    sink.send(tokio_tungstenite::tungstenite::Message::binary(
        buf[..n].to_vec(),
    ))
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
    sink.send(tokio_tungstenite::tungstenite::Message::binary(
        buf[..n].to_vec(),
    ))
    .await
    .map_err(|e| e.to_string())?;
    let noise = hs.into_transport_mode().map_err(|e| e.to_string())?;
    let noise = Arc::new(Mutex::new(noise));
    let remote_static = noise
        .lock()
        .unwrap()
        .get_remote_static()
        .map(|s| s.to_vec())
        .unwrap_or_default();

    // ---- 加密 hello / welcome ----
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
    let tokio_tungstenite::tungstenite::Message::Binary(wb) = welcome else {
        return Err("welcome 格式错误".into());
    };
    let (ok, welcome_id, reason, pc_name) = match open_msg(&noise, &wb)? {
        PigeonIn::Control(PigeonWire::PigeonWelcome {
            ok,
            id,
            reason,
            pc_name,
        }) => (ok, id, reason, pc_name),
        _ => return Err("welcome 格式错误".into()),
    };
    if !ok {
        let r = reason.unwrap_or_else(|| "对方拒绝连接".into());
        if r.contains("待确认") {
            // 对方在等它的用户确认本机；本地同步弹确认（id 已随 welcome 带回）
            let fp = fingerprint(&remote_static);
            let pid = welcome_id.clone().unwrap_or_else(|| format!("manual:{addr}"));
            st.emit(crate::CoreEvent::PigeonTrustRequest {
                peer_id: pid.clone(),
                name: display_name.clone(),
                fingerprint: fp.clone(),
            });
            st.pending_trusts
                .lock()
                .unwrap()
                .insert(pid, (display_name.clone(), fp));
        }
        return Err(r);
    }
    let Some(peer_id) = welcome_id else {
        return Err("对方版本过旧，不支持自助添加（请双方升级后重试）".into());
    };
    // 未固定指纹的设备不建业务链路（同发现流程），确认后由重试自动接上
    if !is_pinned(&remote_static) {
        let fp = fingerprint(&remote_static);
        st.pending_trusts
            .lock()
            .unwrap()
            .insert(peer_id.clone(), (display_name.clone(), fp.clone()));
        st.emit(crate::CoreEvent::PigeonTrustRequest {
            peer_id: peer_id.clone(),
            name: display_name.clone(),
            fingerprint: fp,
        });
        let _ = sink.close().await;
        return Err("新设备待确认".into());
    }
    if st.pigeon_conns.lock().unwrap().contains_key(&peer_id) {
        return Ok(());
    }

    // ---- 回填手动主机记录（机器 ID 来自本次握手对端静态公钥）----
    let mut record_changed = false;
    {
        let mut hosts = st.manual_hosts.lock().unwrap();
        if let Some(m) = hosts.get_mut(addr) {
            let machine = crate::remote::machine_id_from_static(&remote_static);
            record_changed = m.peer_id.as_deref() != Some(peer_id.as_str()) || m.machine_id != machine;
            m.peer_id = Some(peer_id.clone());
            m.machine_id = machine;
            // 用户没起备注名时采用对方的电脑名
            if let Some(n) = pc_name
                && (m.name.is_empty() || m.name == *addr)
            {
                m.name = n;
            }
        }
    }
    if record_changed {
        let list: Vec<ManualHost> = st.manual_hosts.lock().unwrap().values().cloned().collect();
        save_manual_hosts(&list);
    }
    let link_name = st
        .manual_hosts
        .lock()
        .unwrap()
        .get(addr)
        .map(|m| m.name.clone())
        .unwrap_or_else(|| display_name.clone());

    // ---- 注册链路 + 加密泵（同发现流程）----
    let (plain_out_tx, plain_out_rx) = tokio::sync::mpsc::channel::<PigeonOut>(64);
    st.pigeon_conns.lock().unwrap().insert(
        peer_id.clone(),
        PigeonLink {
            name: link_name.clone(),
            out: plain_out_tx.clone(),
            closed: Arc::new(AtomicBool::new(false)),
        },
    );
    tracing::info!("已与「{link_name}」（{addr}）建立加密飞鸽链路（手动添加）");
    emit_peers(st);

    let (cipher_tx, mut cipher_rx) = tokio::sync::mpsc::channel::<Vec<u8>>(64);
    let (frame_tx, frame_rx) = tokio::sync::mpsc::channel::<Vec<u8>>(64);
    let pump = tokio::spawn(pump(
        st.clone(),
        peer_id.clone(),
        link_name.clone(),
        noise.clone(),
        frame_rx,
        plain_out_rx,
        cipher_tx,
    ));
    let writer = tokio::spawn(async move {
        while let Some(frame) = cipher_rx.recv().await {
            if sink
                .send(tokio_tungstenite::tungstenite::Message::binary(frame))
                .await
                .is_err()
            {
                break;
            }
        }
        let _ = sink.close().await;
    });
    let mut drop_reason = "手动链路读流结束（对端关闭）";
    while let Some(Ok(msg)) = stream.next().await {
        let closed = st
            .pigeon_conns
            .lock()
            .unwrap()
            .get(&peer_id)
            .map(|l| l.closed.load(Ordering::Relaxed))
            .unwrap_or(true);
        if closed {
            drop_reason = "本端 closed 标志";
            break;
        }
        match msg {
            tokio_tungstenite::tungstenite::Message::Binary(b) => {
                if frame_tx.send(b.to_vec()).await.is_err() {
                    drop_reason = "加密泵已退出";
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

    st.pigeon_conns.lock().unwrap().remove(&peer_id);
    pump.abort();
    writer.abort();
    tracing::info!("与「{link_name}」（{addr}）的手动链路断开（{drop_reason}）");
    emit_peers(st);
    Ok(())
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
        PigeonOut::Chunk {
            offer_id,
            offset,
            data,
        } => {
            plain.push(b'F');
            plain.extend_from_slice(offer_id);
            plain.extend_from_slice(&offset.to_le_bytes());
            plain.extend_from_slice(data);
        }
    }
    let mut buf = vec![0u8; plain.len() + 64];
    let n = noise
        .lock()
        .unwrap()
        .write_message(&plain, &mut buf)
        .map_err(|e| e.to_string())?;
    buf.truncate(n);
    Ok(buf)
}

pub fn open_msg(noise: &Mutex<TransportState>, cipher: &[u8]) -> Result<PigeonIn, String> {
    let mut buf = vec![0u8; cipher.len()];
    let n = noise
        .lock()
        .unwrap()
        .read_message(cipher, &mut buf)
        .map_err(|e| e.to_string())?;
    match buf.first() {
        Some(b'C') => Ok(PigeonIn::Control(
            serde_json::from_slice(&buf[1..n]).map_err(|e| e.to_string())?,
        )),
        Some(b'F') if n >= 25 => {
            let mut id = [0u8; 16];
            id.copy_from_slice(&buf[1..17]);
            let offset = u64::from_le_bytes(buf[17..25].try_into().unwrap());
            Ok(PigeonIn::Chunk {
                offer_id: id,
                offset,
                data: buf[25..n].to_vec(),
            })
        }
        _ => Err("未知消息前缀".into()),
    }
}

// ---------- 发现层 ----------

pub fn make_udp_socket() -> Option<StdUdpSocket> {
    let sock = socket2::Socket::new(
        socket2::Domain::IPV4,
        socket2::Type::DGRAM,
        Some(socket2::Protocol::UDP),
    )
    .ok()?;
    let _ = sock.set_reuse_address(true);
    // macOS/BSD：UDP 同端口重复绑定（同机双开）必须 SO_REUSEPORT，SO_REUSEADDR 不够；
    // Linux 靠 SO_REUSEADDR 即可，Windows 的 SO_REUSEADDR 语义本身允许重复绑定
    #[cfg(target_os = "macos")]
    let _ = sock.set_reuse_port(true);
    let _ = sock.set_nonblocking(true);
    let addr: SocketAddr = ([0, 0, 0, 0], PIGEON_UDP_PORT).into();
    // 绑定失败（如其他实例占用且无法共存）：降级为无发现，不 panic（panic=abort 会杀掉整个 app）
    sock.bind(&addr.into()).ok()?;
    Some(sock.into())
}

/// 同步上下文非阻塞投递控制消息（UI 线程用；队列满或断开则丢弃并返回 false）
pub fn try_push(st: &AppState, peer_id: &str, wire: PigeonWire) -> bool {
    let Ok(conns) = st.pigeon_conns.lock() else {
        return false;
    };
    match conns.get(peer_id) {
        Some(link) => link.out.try_send(PigeonOut::Control(wire)).is_ok(),
        None => false,
    }
}

/// 异步上下文投递控制消息
pub async fn push_control(st: &AppState, peer_id: &str, wire: PigeonWire) -> bool {
    let link = st
        .pigeon_conns
        .lock()
        .unwrap()
        .get(peer_id)
        .map(|l| l.out.clone());
    match link {
        Some(out) => out.send(PigeonOut::Control(wire)).await.is_ok(),
        None => false,
    }
}

pub fn peers_event(st: &AppState) -> CoreEvent {
    let v: Vec<PigeonPeerInfo> = {
        let known = st.pigeon_peers.lock().unwrap();
        let conns = st.pigeon_conns.lock().unwrap();
        let manual = st.manual_hosts.lock().unwrap();
        let mut v: Vec<PigeonPeerInfo> = known
            .iter()
            .map(|(id, p)| PigeonPeerInfo {
                id: id.clone(),
                name: p.name.clone(),
                ip: p.ip.to_string(),
                tcp_port: p.tcp_port,
                connected: conns.contains_key(id),
                machine_id: p.machine_id.clone(),
                manual: manual.values().any(|m| {
                    m.peer_id.as_deref() == Some(id.as_str())
                        || same_addr(&m.addr, &p.ip.to_string(), p.tcp_port)
                }),
                addr: format!("{}:{}", p.ip, p.tcp_port),
            })
            .collect();
        // 手动主机在发现表覆盖不到时补条目：对方隐身不广播、或尚未连上（离线也可见）
        for m in manual.values() {
            let covered = m
                .peer_id
                .as_deref()
                .is_some_and(|pid| known.contains_key(pid))
                || known
                    .values()
                    .any(|p| same_addr(&m.addr, &p.ip.to_string(), p.tcp_port));
            if covered {
                continue;
            }
            let connected = m
                .peer_id
                .as_deref()
                .is_some_and(|pid| conns.contains_key(pid));
            v.push(PigeonPeerInfo {
                id: m.peer_id.clone().unwrap_or_else(|| format!("manual:{}", m.addr)),
                name: m.name.clone(),
                ip: addr_host(&m.addr).to_string(),
                tcp_port: addr_port(&m.addr),
                connected,
                machine_id: m.machine_id.clone(),
                manual: true,
                addr: m.addr.clone(),
            });
        }
        v
    };
    // 同一机器身份去重：升级过渡期残留的旧随机 id 条目、手动主机记录里的过期
    // peer_id，都可能和新条目并存成重复卡片。按 machine_id 合并——优先保留已
    // 连接的（其 id 在连接表里，聊天/传文件可直接投递），其次保留发现条目
    // （id 来自最新广播，手动记录可能是过期 id）
    let mut deduped: Vec<PigeonPeerInfo> = Vec::with_capacity(v.len());
    for p in v {
        if p.machine_id.is_empty() {
            deduped.push(p); // 极旧版本无机器 ID，无从合并
            continue;
        }
        match deduped
            .iter_mut()
            .find(|x| x.machine_id == p.machine_id)
        {
            Some(existing) => {
                if (p.connected && !existing.connected)
                    || (p.connected == existing.connected && existing.manual && !p.manual)
                {
                    *existing = p;
                }
            }
            None => deduped.push(p),
        }
    }
    let mut v = deduped;
    v.sort_by(|a, b| a.name.cmp(&b.name).then(a.id.cmp(&b.id)));
    CoreEvent::PigeonPeers { peers: v }
}

pub fn emit_peers(st: &AppState) {
    st.emit(peers_event(st));
}

/// 本机发现身份（run_discovery 启动时登记，退出告别广播使用）
static SELF_IDENTITY: std::sync::OnceLock<SelfIdentity> = std::sync::OnceLock::new();
struct SelfIdentity {
    id: String,
    name: String,
}

/// 本机每个 IPv4 网卡的子网广播地址（外加全局广播兜底）。
/// 多网卡机器上 255.255.255.255 只从默认路由出口发，其他网卡上的实例收不到。
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
                v.push(SocketAddr::from((
                    std::net::Ipv4Addr::from(u32::from(bc)),
                    PIGEON_UDP_PORT,
                )));
            }
        }
    }
    v.sort();
    v.dedup();
    v
}

/// 退出前广播下线通知（尽力而为，总计约 150ms，不阻塞退出流程）：
/// 对端收到 leave 即从设备列表移除，无需等心跳超时
pub fn broadcast_leave() {
    let Some(identity) = SELF_IDENTITY.get() else {
        return; // 发现服务未运行（UDP 被占用等），无 peers 可告别
    };
    let Ok(packet) = serde_json::to_vec(&Announce {
        v: 1,
        kind: "leave".into(),
        id: identity.id.clone(),
        name: identity.name.clone(),
        port: 0,
        mid: String::new(),
    }) else {
        return;
    };
    let Ok(sock) = std::net::UdpSocket::bind(("0.0.0.0", 0)) else {
        return;
    };
    let _ = sock.set_broadcast(true);
    for _ in 0..3 {
        for bc in broadcast_targets() {
            let _ = sock.send_to(&packet, bc);
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// 发现主循环：广播 + 收包 + 过期清扫
pub async fn run_discovery(st: Arc<AppState>) {
    let Some(std_sock) = make_udp_socket() else {
        tracing::warn!(
            "UDP {} 绑定失败，本实例禁用飞鸽发现（PC↔PC），手机传输不受影响",
            PIGEON_UDP_PORT
        );
        st.emit(CoreEvent::Toast {
            text: format!("UDP {PIGEON_UDP_PORT} 被占用，飞鸽发现已禁用（可能已有 Yunshu 实例在运行）"),
        });
        return;
    };
    let sock = Arc::new(tokio::net::UdpSocket::from_std(std_sock).expect("UDP socket 转换失败"));
    let _ = sock.set_broadcast(true);

    let announce = serde_json::to_vec(&Announce {
        v: 1,
        kind: "entry".into(),
        id: st.self_id.clone(),
        name: st.cfg.pc_name.clone(),
        port: st.cfg.port,
        mid: crate::remote::machine_id(),
    })
    .unwrap_or_default();

    // 登记本机身份：进程退出前 broadcast_leave() 用它构造告别广播
    let _ = SELF_IDENTITY.set(SelfIdentity {
        id: st.self_id.clone(),
        name: st.cfg.pc_name.clone(),
    });

    // 启动即广播一次（隐身模式不开机广播）
    if !st.stealth.load(Ordering::Relaxed) {
        for bc in broadcast_targets() {
            let _ = sock.send_to(&announce, bc).await;
        }
    }

    tracing::info!(
        "飞鸽发现已启动（UDP {}，本机 id {}）",
        PIGEON_UDP_PORT,
        &st.self_id[..8]
    );
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
                // 隐身模式不广播（本机不出现在其他设备列表）；
                // 退出隐身时 announce_now 置位，下个 tick 立即补发恢复可见
                if !st.stealth.load(Ordering::Relaxed)
                    && (n <= 5 || n.is_multiple_of(3) || st.announce_now.swap(false, Ordering::Relaxed))
                {
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
    /// 公开机器 ID（静态密钥指纹前 8 位 hex；旧版本无此字段，远程控制对旧版不可用）
    #[serde(default)]
    mid: String,
}

async fn handle_packet(st: &Arc<AppState>, data: &[u8], from: SocketAddr) {
    let Ok(p) = serde_json::from_slice::<Announce>(data) else {
        return;
    };
    if p.kind == "leave" && p.id != st.self_id && !p.id.is_empty() {
        // 对端优雅退出：立即从发现表移除（TCP 链路若有，随断开自清理）
        let removed = st.pigeon_peers.lock().unwrap().remove(&p.id).is_some();
        if removed {
            tracing::info!("电脑「{}」已退出，从列表移除", p.name);
            emit_peers(st);
        }
        return;
    }
    if p.id != st.self_id && !p.id.is_empty() && p.kind == "entry" {
        let is_new = {
            let mut known = st.pigeon_peers.lock().unwrap();
            let is_new = !known.contains_key(&p.id);
            known.insert(
                p.id.clone(),
                PeerRecord {
                    name: p.name.clone(),
                    ip: from.ip(),
                    tcp_port: p.port,
                    machine_id: p.mid,
                    last_seen: Instant::now(),
                },
            );
            is_new
        };
        if is_new {
            tracing::info!("发现电脑「{}」（{}:{}）", p.name, from.ip(), p.port);
            emit_peers(st);
        }
        // 只要没有链路就尝试连接（覆盖：首次发现 / 链路断开后重连）。
        // 双方同时外连会互相拆链路：约定只有 id 更大的一方主动发起，另一侧等待连入。
        // 隐身模式不主动外连——一旦外连，对方立刻看得到本机（隐身即失效）；
        // 用户手动发送/远程控制等主动行为不受此限制
        let should_connect = !st.stealth.load(Ordering::Relaxed)
            && p.id > st.self_id
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

async fn pigeon_connect_inner(
    st: &Arc<AppState>,
    peer_id: &str,
    ip: IpAddr,
    port: u16,
) -> Result<(), String> {
    if st.pigeon_conns.lock().unwrap().contains_key(peer_id) {
        return Ok(());
    }
    let addr_key = format!("{ip}:{port}");
    // 防打扰：近期握手后判为未信任的地址，10 分钟内不再反复握手
    if st.dnd.load(Ordering::Relaxed)
        && st
            .dnd_skipped
            .lock()
            .unwrap()
            .get(&addr_key)
            .is_some_and(|t| t.elapsed() < Duration::from_secs(600))
    {
        return Err("防打扰：跳过未信任设备".into());
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
    sink.send(tokio_tungstenite::tungstenite::Message::binary(
        buf[..n].to_vec(),
    ))
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
    sink.send(tokio_tungstenite::tungstenite::Message::binary(
        buf[..n].to_vec(),
    ))
    .await
    .map_err(|e| e.to_string())?;
    let noise = hs.into_transport_mode().map_err(|e| e.to_string())?;
    let noise = Arc::new(Mutex::new(noise));
    let remote_static = noise
        .lock()
        .unwrap()
        .get_remote_static()
        .map(|s| s.to_vec())
        .unwrap_or_default();

    // 防打扰：未信任设备在 hello 前静默断开（对方不会收到连接确认，也不弹任何窗）
    if st.dnd.load(Ordering::Relaxed) && !is_pinned(&remote_static) {
        st.dnd_skipped
            .lock()
            .unwrap()
            .insert(format!("{ip}:{port}"), Instant::now());
        let _ = sink.close().await;
        tracing::info!("防打扰：跳过未信任设备（{ip}:{port}），10 分钟内不再尝试");
        return Err("防打扰：跳过未信任设备".into());
    }

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
        && !ok
    {
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
    // 身份即密钥：指纹已固定 = 密钥与首次信任时完全一致（SHA-256 派生），无需再比对
    // 未固定指纹的设备不建业务链路：对端若是改制客户端可伪造 welcome ok:true，
    // 链路一旦注册，其聊天/文件邀请/协助请求即可绕过信任确认直达本机 UI
    if !is_pinned(&remote_static) {
        let fp = fingerprint(&remote_static);
        st.pending_trusts
            .lock()
            .unwrap()
            .insert(peer_id.to_string(), (name.clone(), fp.clone()));
        st.emit(crate::CoreEvent::PigeonTrustRequest {
            peer_id: peer_id.to_string(),
            name: name.clone(),
            fingerprint: fp,
        });
        let _ = sink.close().await;
        tracing::info!("「{name}」未在信任列表，已挂起连接等待本机确认");
        return Err("新设备待确认".into());
    }
    if st.pigeon_conns.lock().unwrap().contains_key(peer_id) {
        return Ok(()); // 对方已抢先建立，放弃本连接
    }

    // ---- 注册链路 + 加密泵 ----
    let (plain_out_tx, plain_out_rx) = tokio::sync::mpsc::channel::<PigeonOut>(64);
    st.pigeon_conns.lock().unwrap().insert(
        peer_id.to_string(),
        PigeonLink {
            name: name.clone(),
            out: plain_out_tx.clone(),
            closed: Arc::new(AtomicBool::new(false)),
        },
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
            if sink
                .send(tokio_tungstenite::tungstenite::Message::binary(frame))
                .await
                .is_err()
            {
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
    drop_link(st.clone(), peer_id.clone()).await;
    tracing::info!("「{peer_name}」的加密链路泵结束");
    emit_peers(&st);
}

async fn dispatch(st: &Arc<AppState>, peer_id: &str, msg: PigeonIn) {
    match msg {
        PigeonIn::Control(PigeonWire::PcText { text }) => {
            let peer_name = link_name(st, peer_id);
            tracing::info!("收到「{peer_name}」的聊天消息：{text}");
            st.emit(CoreEvent::ChatFromPc {
                peer_id: peer_id.to_string(),
                peer_name,
                text,
            });
        }
        PigeonIn::Control(PigeonWire::BroadcastText { text }) => {
            // 防打扰：广播消息整体静音（能连上的都是已信任设备，按用户要求一并静音）
            if st.dnd.load(Ordering::Relaxed) {
                tracing::info!("防打扰：已静音来自「{}」的广播消息", link_name(st, peer_id));
                return;
            }
            let peer_name = link_name(st, peer_id);
            tracing::info!("收到「{peer_name}」的广播消息：{text}");
            st.emit(CoreEvent::ChatBroadcast {
                peer_id: peer_id.to_string(),
                peer_name,
                text,
            });
        }
        PigeonIn::Control(PigeonWire::PcFileOffer {
            id,
            name,
            total_size,
            files,
        }) => {
            let peer_name = link_name(st, peer_id);
            tracing::info!(
                "「{peer_name}」请求发送「{name}」（{} 项，{total_size} 字节）",
                files.len()
            );
            let states: Vec<crate::server::BatchFileState> = files
                .iter()
                .map(|f| crate::server::BatchFileState {
                    name: f.name.clone(),
                    size: f.size,
                    written: 0,
                    sha: None,
                })
                .collect();
            st.pigeon_incoming
                .lock()
                .unwrap()
                .insert(id.clone(), (peer_id.to_string(), states));
            if std::env::var("YUNSHU_AUTO_ACCEPT").is_ok() {
                // 自动接受：磁盘空间不足时直接拒绝
                if let Some(free) = free_space(&st.cfg.save_dir)
                    && free < total_size
                {
                    tracing::warn!(
                        "磁盘空间不足（需 {total_size}，剩 {free}），自动拒绝「{name}」"
                    );
                    st.emit(CoreEvent::Toast {
                        text: format!("磁盘空间不足，已拒绝「{name}」"),
                    });
                    push_control(
                        st,
                        peer_id,
                        PigeonWire::FileDecision {
                            id,
                            accepted: false,
                        },
                    )
                    .await;
                    return;
                }
                tracing::info!("AUTO_ACCEPT：自动接受「{name}」");
                // 自动接受同样要登记进 ready：FileStart 只认已接受的批次
                if let Some(entry) = st.pigeon_incoming.lock().unwrap().remove(&id) {
                    st.pigeon_ready.lock().unwrap().insert(id.clone(), entry);
                }
                push_control(st, peer_id, PigeonWire::FileDecision { id, accepted: true }).await;
            } else {
                let entries: Vec<FileEntry> = files
                    .iter()
                    .map(|f| FileEntry {
                        name: f.name.clone(),
                        size: f.size,
                    })
                    .collect();
                st.emit(CoreEvent::PigeonOfferIncoming {
                    peer_id: peer_id.to_string(),
                    peer_name,
                    offer_id: id,
                    name,
                    size: total_size,
                    files: entries,
                });
            }
        }
        PigeonIn::Control(PigeonWire::FileDecision { id, accepted }) => {
            if accepted {
                let src = st.pigeon_offers.lock().unwrap().get(&id).cloned();
                if let Some(src) = src {
                    st.update_transfer(&id, 0, Some(src.total_size), TransferStatus::Active, None);
                    let st2 = st.clone();
                    let pid = peer_id.to_string();
                    tokio::spawn(async move {
                        push_batch(st2, pid, id, src).await;
                    });
                } else {
                    st.update_transfer(&id, 0, None, TransferStatus::Failed, None);
                }
            } else {
                st.update_transfer(&id, 0, None, TransferStatus::Refused, None);
                st.emit(CoreEvent::Toast {
                    text: "对方拒绝了文件接收".into(),
                });
            }
        }
        PigeonIn::Control(PigeonWire::FileStart { id }) => {
            // 只认用户已接受的批次：未接受（或压根没有）的 FileStart 一律拒绝，
            // 否则改制客户端可不经确认直接把文件落进保存目录
            let ready = st.pigeon_ready.lock().unwrap().remove(&id);
            let Some((peer, mut files)) = ready else {
                tracing::warn!("FileStart 找不到已接受的批次（{id}）");
                let _ = push_control(st, peer_id, PigeonWire::FileCancel { id }).await;
                return;
            };
            if peer != peer_id {
                tracing::warn!("FileStart 与批次接受来源不符（{id}），已拒绝");
                st.pigeon_ready.lock().unwrap().insert(id.clone(), (peer, files));
                let _ = push_control(st, peer_id, PigeonWire::FileCancel { id }).await;
                return;
            }
            let dir = st.cfg.save_dir.join(".yunshu-parts").join(&id);
            if let Err(e) = tokio::fs::create_dir_all(&dir).await {
                tracing::error!("创建半成品目录失败 {dir:?}: {e}");
                let _ = push_control(st, peer_id, PigeonWire::FileCancel { id }).await;
                return;
            }
            // 断点续传：读已有 manifest（条目名/大小完全一致才采信）
            if let Ok(txt) = tokio::fs::read_to_string(dir.join("manifest.json")).await
                && let Ok(saved) = serde_json::from_str::<Vec<crate::server::BatchFileState>>(&txt)
                && saved.len() == files.len()
                && saved
                    .iter()
                    .zip(files.iter())
                    .all(|(s, f)| s.name == f.name && s.size == f.size)
            {
                files = saved;
            }
            let sizes: Vec<u64> = files.iter().map(|f| f.size).collect();
            let verified: Vec<bool> = files.iter().map(|f| f.sha.is_some()).collect();
            let written: Vec<u64> = files.iter().map(|f| f.written).collect();
            let have = resume_have(&sizes, &written, &verified);
            let total: u64 = sizes.iter().sum();
            // 恢复点（含未校验的文件从头重收）：其后全部清零重来
            let (idx, _in_off) = stream_locate(&sizes, have);
            for (i, f) in files.iter_mut().enumerate().skip(idx) {
                if f.written != 0 || f.sha.is_some() {
                    f.written = 0;
                    f.sha = None;
                    let _ = tokio::fs::File::create(part_path(&dir, i)).await;
                }
            }
            crate::server::save_manifest(&dir, &files);
            // 磁盘空间预检：剩余空间须容得下尚未接收的字节
            if let Some(free) = free_space(&st.cfg.save_dir)
                && free < total - have
            {
                tracing::warn!("磁盘空间不足（尚需 {}，仅剩 {free}）", total - have);
                let row_id = uuid::Uuid::new_v4().simple().to_string();
                let entries: Vec<FileEntry> = files
                    .iter()
                    .map(|f| FileEntry {
                        name: f.name.clone(),
                        size: f.size,
                    })
                    .collect();
                st.new_transfer(TransferInfo::batch(
                    row_id.clone(),
                    Direction::FromPhone,
                    name_display(&files),
                    total,
                    entries,
                    Some(peer_id.to_string()),
                ));
                st.update_transfer(&row_id, have, Some(total), TransferStatus::Failed, None);
                st.emit(CoreEvent::Toast {
                    text: format!("磁盘空间不足（尚需 {} 字节），传输已中止", total - have),
                });
                let _ = push_control(st, peer_id, PigeonWire::FileCancel { id }).await;
                return;
            }
            let row_id = uuid::Uuid::new_v4().simple().to_string();
            let entries: Vec<FileEntry> = files
                .iter()
                .map(|f| FileEntry {
                    name: f.name.clone(),
                    size: f.size,
                })
                .collect();
            st.new_transfer(TransferInfo::batch(
                row_id.clone(),
                Direction::FromPhone,
                name_display(&files),
                total,
                entries,
                Some(peer_id.to_string()),
            ));
            st.update_transfer(&row_id, have, Some(total), TransferStatus::Active, None);
            st.recv_files.lock().await.insert(
                id.clone(),
                crate::server::BatchRecvState {
                    row_id,
                    peer_id: peer.to_string(),
                    name: name_display(&files),
                    dir,
                    files,
                    index: idx,
                    file: None,
                    written: 0,
                    have,
                    hash_pending: false,
                    running_hash: None,
                    saved_mark: have,
                },
            );
            if have > 0 {
                tracing::info!("批次从流偏移 {have} 字节处续传接收");
            }
            let _ = push_control(st, peer_id, PigeonWire::FileResume { id, have }).await;
        }
        PigeonIn::Control(PigeonWire::FileResume { id, have }) => {
            if let Some(tx) = st.pigeon_push_resume.lock().unwrap().remove(&id) {
                let _ = tx.send(have);
            }
        }
        PigeonIn::Control(PigeonWire::FileHash { id, index, sha }) => {
            let failure = {
                let mut states = st.recv_files.lock().await;
                let Some(s) = states.get_mut(&id) else { return };
                // 越界守卫须在最前：index == files.len()（全部校验完、等 FileEnd）时
                // s.files[s.index] 会越界 panic，而 release 是 panic=abort 全进程退出
                if s.index >= s.files.len()
                    || index as usize != s.index
                    || (!s.hash_pending && s.files[s.index].size != 0)
                {
                    tracing::warn!("FileHash 与当前状态不符（{id} #{index}）");
                    return;
                }
                let actual = match s.running_hash.take() {
                    Some(h) => sha_hex(&h.finalize()),
                    None => sha_hex(&sha2::Sha256::new().finalize()), // 空文件
                };
                s.hash_pending = false;
                if actual == sha {
                    s.files[s.index].sha = Some(sha);
                    s.written = 0;
                    s.index += 1;
                    crate::server::save_manifest(&s.dir, &s.files);
                    None
                } else {
                    tracing::error!(
                        "「{}」第 {} 项哈希校验失败",
                        s.files[s.index].name,
                        s.index + 1
                    );
                    Some((s.row_id.clone(), s.peer_id.clone()))
                }
            };
            if let Some((row_id, peer)) = failure {
                let state = st.recv_files.lock().await.remove(&id);
                if let Some(mut s) = state {
                    s.file = None;
                    let _ = std::fs::remove_dir_all(&s.dir);
                }
                st.update_transfer(&row_id, 0, None, TransferStatus::Failed, None);
                st.emit(CoreEvent::Toast {
                    text: "文件校验失败，传输已中止（数据可能损坏）".into(),
                });
                let _ = push_control(st, &peer, PigeonWire::FileCancel { id }).await;
            }
        }
        PigeonIn::Control(PigeonWire::FilePause { id, paused }) => {
            let status = if paused {
                TransferStatus::Paused
            } else {
                TransferStatus::Active
            };
            // 发送方收到：控制推送节奏
            if let Some(flag) = st.pigeon_push_paused.lock().unwrap().get(&id) {
                flag.store(paused, Ordering::Relaxed);
                let cur = st
                    .transfers
                    .lock()
                    .unwrap()
                    .iter()
                    .find(|t| t.id == id)
                    .map(|t| (t.transferred, t.size));
                if let Some((transferred, size)) = cur {
                    st.update_transfer(&id, transferred, Some(size), status, None);
                }
                tracing::info!(
                    "批次推送{}（应接收方请求）",
                    if paused { "已暂停" } else { "已恢复" }
                );
                return;
            }
            // 接收方收到：同步行状态
            let info = {
                let states = st.recv_files.lock().await;
                states.get(&id).map(|s| {
                    (
                        s.row_id.clone(),
                        s.have,
                        s.files.iter().map(|f| f.size).sum::<u64>(),
                    )
                })
            };
            if let Some((row_id, have, total)) = info {
                st.update_transfer(&row_id, have, Some(total), status, None);
                tracing::info!(
                    "批次接收{}（应发送方请求）",
                    if paused { "已暂停" } else { "已恢复" }
                );
            }
        }
        PigeonIn::Control(PigeonWire::FileCancel { id }) => {
            // 发送方向：推送中置取消标记；未被接受的批次直接撤回
            if let Some(flag) = st.pigeon_push_cancel.lock().unwrap().get(&id) {
                flag.store(true, Ordering::Relaxed);
            }
            if let Some(flag) = st.pigeon_push_paused.lock().unwrap().get(&id) {
                flag.store(true, Ordering::Relaxed);
            }
            st.pigeon_offers.lock().unwrap().remove(&id);
            // 等待决策/已接受未开始的批次一并清掉
            st.pigeon_incoming.lock().unwrap().remove(&id);
            st.pigeon_ready.lock().unwrap().remove(&id);
            let state = st.recv_files.lock().await.remove(&id);
            if let Some(mut s) = state {
                s.file = None;
                let _ = std::fs::remove_dir_all(&s.dir);
                st.update_transfer(&s.row_id, s.have, None, TransferStatus::Cancelled, None);
                st.emit(CoreEvent::Toast {
                    text: format!("「{}」的传输已取消", s.name),
                });
            } else if let Some(t) = st
                .transfers
                .lock()
                .unwrap()
                .iter()
                .find(|t| t.id == id)
                .map(|t| (t.transferred, t.size, t.status))
            {
                // 本端是发送方：批次被对方取消/撤回
                if !matches!(
                    t.2,
                    TransferStatus::Done | TransferStatus::Cancelled | TransferStatus::Refused
                ) {
                    st.update_transfer(&id, t.0, Some(t.1), TransferStatus::Cancelled, None);
                }
                tracing::info!("批次 {} 已被对方取消", &id[..8.min(id.len())]);
            } else if !st.transfers.lock().unwrap().iter().any(|t| t.id == id) {
                // 等待接受阶段被对方撤回：本端可能挂着确认弹窗，推一条取消状态让 UI 收尾
                let info = TransferInfo {
                    id,
                    direction: Direction::FromPhone,
                    kind: TransferKind::File,
                    name: String::new(),
                    size: 0,
                    transferred: 0,
                    status: TransferStatus::Cancelled,
                    path: None,
                    peer: Some(peer_id.to_string()),
                    files: Vec::new(),
                };
                st.emit(CoreEvent::TransferUpdate { t: Box::new(info) });
            }
        }
        PigeonIn::Control(PigeonWire::FileEnd { id, ok }) => {
            let state = st.recv_files.lock().await.remove(&id);
            let Some(mut s) = state else {
                tracing::warn!("FileEnd 找不到对应的接收状态（{id}）");
                return;
            };
            s.file = None; // 句柄已在边界处落盘关闭
            if !ok {
                // 发送方异常中断：保留半成品与 manifest，对方重发同一批次时自动续传
                crate::server::save_manifest(&s.dir, &s.files);
                st.update_transfer(&s.row_id, s.have, None, TransferStatus::Failed, None);
                tracing::info!(
                    "批次「{}」推送中断，已保留 {} 字节半成品待续传",
                    s.name,
                    s.have
                );
                return;
            }
            // 完整性：全部文件已写满且通过哈希校验
            if s.index != s.files.len()
                || s.hash_pending
                || s.files
                    .iter()
                    .any(|f| f.sha.is_none() || f.written != f.size)
            {
                crate::server::save_manifest(&s.dir, &s.files);
                st.update_transfer(&s.row_id, s.have, None, TransferStatus::Failed, None);
                tracing::warn!("批次「{}」的 FileEnd 时清单未完成，保留半成品", s.name);
                return;
            }
            // 落盘：按相对路径改名进保存目录（逐段 sanitize 防穿越）
            let mut fail_reason: Option<String> = None;
            for (i, f) in s.files.iter().enumerate() {
                let Some(dest) = crate::server::safe_join(&st.cfg.save_dir, &f.name) else {
                    fail_reason = Some(format!("非法路径 {}", f.name));
                    break;
                };
                if let Some(parent) = dest.parent()
                    && let Err(e) = tokio::fs::create_dir_all(parent).await
                {
                    fail_reason = Some(format!("创建目录失败: {e}"));
                    break;
                }
                let final_path = crate::server::unique_path(
                    dest.parent().unwrap_or(&st.cfg.save_dir),
                    &dest
                        .file_name()
                        .map(|n| n.to_string_lossy().to_string())
                        .unwrap_or_default(),
                );
                let part = part_path(&s.dir, i);
                if !part.exists() {
                    // 空文件没有任何数据块，补一个空 part
                    if let Err(e) = std::fs::write(&part, b"") {
                        fail_reason = Some(format!("补写空文件失败: {e}"));
                        break;
                    }
                }
                if let Err(e) = tokio::fs::rename(&part, &final_path).await {
                    fail_reason = Some(format!("落盘失败: {e}"));
                    break;
                }
            }
            if let Some(reason) = fail_reason {
                tracing::error!("批次「{}」落盘失败：{reason}", s.name);
                crate::server::save_manifest(&s.dir, &s.files);
                st.update_transfer(&s.row_id, s.have, None, TransferStatus::Failed, None);
                return;
            }
            let _ = tokio::fs::remove_dir_all(&s.dir).await;
            tracing::info!(
                "批次「{}」落盘完成（{} 项）→ 保存目录 {:?}",
                s.name,
                s.files.len(),
                st.cfg.save_dir
            );
            // 打开所在文件夹的定位：全部同根 → 根目录；否则保存目录
            let first_root = s.files[0].name.split('/').next().unwrap_or_default();
            let path = if !first_root.is_empty()
                && s.files
                    .iter()
                    .all(|f| f.name.split('/').next() == Some(first_root))
            {
                st.cfg.save_dir.join(first_root)
            } else {
                st.cfg.save_dir.clone()
            };
            st.update_transfer(
                &s.row_id,
                s.have,
                Some(s.have),
                TransferStatus::Done,
                Some(path),
            );
            st.emit(CoreEvent::Toast {
                text: format!("收到「{}」（{} 项）", s.name, s.files.len()),
            });
        }
        PigeonIn::Control(PigeonWire::RemoteAssistRequest {}) => {
            let peer_name = link_name(st, peer_id);
            tracing::info!("「{peer_name}」请求远程协助控制本机");
            crate::remote::on_assist_request(st, peer_id, &peer_name);
        }
        PigeonIn::Control(PigeonWire::RemoteAssistResponse { accepted, password }) => {
            tracing::info!(
                "「{}」对协助请求的答复：{}",
                link_name(st, peer_id),
                if accepted { "同意" } else { "拒绝" }
            );
            crate::remote::on_assist_response(st, peer_id, accepted, password);
        }
        PigeonIn::Control(_) => {}
        PigeonIn::Chunk {
            offer_id,
            offset,
            data,
        } => {
            let key = uuid::Uuid::from_bytes(offer_id).simple().to_string();
            let failure: Option<(String, u64, String)> = 'blk: {
                let mut states = st.recv_files.lock().await;
                let Some(s) = states.get_mut(&key) else {
                    break 'blk None;
                };
                // 数据块自带流偏移，自校验：越位忽略，落后则整批回退重写
                if offset > s.have {
                    tracing::warn!(
                        "收到越位数据块（offset {offset} > 流偏移 {}），忽略",
                        s.have
                    );
                    break 'blk None;
                }
                if offset < s.have {
                    let sizes: Vec<u64> = s.files.iter().map(|f| f.size).collect();
                    let (idx, _) = stream_locate(&sizes, offset);
                    s.file = None;
                    for i in idx..s.files.len() {
                        if s.files[i].written != 0 || s.files[i].sha.is_some() {
                            s.files[i].written = 0;
                            s.files[i].sha = None;
                            let _ = tokio::fs::File::create(part_path(&s.dir, i)).await;
                        }
                    }
                    s.index = idx;
                    s.written = 0;
                    s.have = offset;
                    s.hash_pending = false;
                    s.running_hash = None;
                    s.saved_mark = offset;
                }
                if s.hash_pending || s.index >= s.files.len() {
                    break 'blk None;
                }
                // 惰性打开当前文件（从文件头重写，粒度=单文件）
                if s.file.is_none() {
                    let part = part_path(&s.dir, s.index);
                    let Ok(f) = std::fs::OpenOptions::new()
                        .create(true)
                        .read(true)
                        .write(true)
                        .truncate(true)
                        .open(&part)
                    else {
                        tracing::error!("打开半成品文件失败 {part:?}");
                        break 'blk Some((s.row_id.clone(), s.have, s.peer_id.clone()));
                    };
                    s.file = Some(tokio::fs::File::from_std(f));
                    s.running_hash = Some(sha2::Sha256::new());
                }
                let size_i = s.files[s.index].size;
                match s.file.as_mut().expect("file 已打开").write_all(&data).await {
                    Ok(()) => {
                        if let Some(h) = s.running_hash.as_mut() {
                            use sha2::Digest;
                            h.update(&data);
                        }
                        s.written += data.len() as u64;
                        s.have += data.len() as u64;
                        st.progress_throttled(&s.row_id, s.have, false);
                        // 文件写满 → 落盘等校验；批次内每 1MB 存一次 manifest
                        if s.written == size_i {
                            if let Some(f) = s.file.as_mut() {
                                let _ = f.flush().await;
                            }
                            s.file = None;
                            s.files[s.index].written = size_i;
                            s.hash_pending = true;
                            crate::server::save_manifest(&s.dir, &s.files);
                        } else if s.have - s.saved_mark >= 1_048_576 {
                            s.files[s.index].written = s.written;
                            crate::server::save_manifest(&s.dir, &s.files);
                            s.saved_mark = s.have;
                        }
                        break 'blk None;
                    }
                    Err(e) => {
                        tracing::error!("写入半成品失败: {e}");
                        break 'blk Some((s.row_id.clone(), s.have, s.peer_id.clone()));
                    }
                }
            };
            if let Some((row_id, have, peer)) = failure {
                st.recv_files.lock().await.remove(&key);
                st.update_transfer(&row_id, have, None, TransferStatus::Failed, None);
                // 通知发送方停止推送（半成品保留，修复磁盘后可续传）
                let _ = push_control(st, &peer, PigeonWire::FileCancel { id: key }).await;
            }
        }
    }
}

/// 发送方：对方接受后，把批次文件按清单顺序切块推送（虚拟字节流，支持断点续传/取消/逐文件校验）
pub(crate) async fn push_batch(
    st: Arc<AppState>,
    peer_id: String,
    offer_id: String,
    src: crate::server::BatchSource,
) {
    let cancel = Arc::new(AtomicBool::new(false));
    let paused = Arc::new(AtomicBool::new(false));
    st.pigeon_push_cancel
        .lock()
        .unwrap()
        .insert(offer_id.clone(), cancel.clone());
    st.pigeon_push_paused
        .lock()
        .unwrap()
        .insert(offer_id.clone(), paused.clone());
    push_batch_impl(&st, &peer_id, &offer_id, &src, &cancel, &paused).await;
    st.pigeon_push_cancel.lock().unwrap().remove(&offer_id);
    st.pigeon_push_paused.lock().unwrap().remove(&offer_id);
    st.pigeon_push_resume.lock().unwrap().remove(&offer_id);
}

async fn push_batch_impl(
    st: &Arc<AppState>,
    peer_id: &str,
    offer_id: &str,
    src: &crate::server::BatchSource,
    cancel: &AtomicBool,
    paused: &AtomicBool,
) {
    let out = st
        .pigeon_conns
        .lock()
        .unwrap()
        .get(peer_id)
        .map(|l| l.out.clone());
    let Some(out) = out else {
        st.update_transfer(offer_id, 0, None, TransferStatus::Failed, None);
        return;
    };
    let Ok(uuid) = uuid::Uuid::parse_str(offer_id) else {
        return;
    };
    let id16 = *uuid.as_bytes();
    let sizes: Vec<u64> = src.files.iter().map(|f| f.2).collect();

    // FileStart → 等接收方应答流偏移（旧版本对端不回复，超时按 0 全量推）
    let _ = out
        .send(PigeonOut::Control(PigeonWire::FileStart {
            id: offer_id.to_string(),
        }))
        .await;
    let (tx, rx) = tokio::sync::oneshot::channel();
    st.pigeon_push_resume
        .lock()
        .unwrap()
        .insert(offer_id.to_string(), tx);
    let mut sent = tokio::select! {
        r = tokio::time::timeout(Duration::from_secs(10), rx) => match r {
            Ok(Ok(have)) => have.min(src.total_size),
            _ => 0,
        },
        // 等待应答期间被取消
        _ = wait_cancelled(cancel) => {
            let _ = out.send(PigeonOut::Control(PigeonWire::FileCancel { id: offer_id.to_string() })).await;
            st.update_transfer(offer_id, 0, Some(src.total_size), TransferStatus::Cancelled, None);
            return;
        }
    };
    let (start_idx, start_off) = stream_locate(&sizes, sent);
    if sent > 0 && sent < src.total_size {
        tracing::info!("批次「{}」从流偏移 {} 字节处续传", src.name, sent);
    }
    st.update_transfer(
        offer_id,
        sent,
        Some(src.total_size),
        TransferStatus::Active,
        None,
    );

    for (file_idx, (path, _rel, _size)) in src.files.iter().enumerate().skip(start_idx) {
        let in_off = if file_idx == start_idx { start_off } else { 0 };
        let Ok(mut f) = tokio::fs::File::open(path).await else {
            st.update_transfer(
                offer_id,
                sent,
                Some(src.total_size),
                TransferStatus::Failed,
                None,
            );
            return;
        };
        let mut hasher = sha2::Sha256::new();
        // 续传：当前文件的前缀字节虽不重发，但哈希必须覆盖全文件——先回读前缀
        if in_off > 0 {
            let mut prefix = vec![0u8; CHUNK_SIZE];
            let mut take = in_off;
            while take > 0 {
                let want = (take.min(CHUNK_SIZE as u64)) as usize;
                match f.read(&mut prefix[..want]).await {
                    Ok(n) if n > 0 => {
                        hasher.update(&prefix[..n]);
                        take -= n as u64;
                    }
                    _ => {
                        st.update_transfer(
                            offer_id,
                            sent,
                            Some(src.total_size),
                            TransferStatus::Failed,
                            None,
                        );
                        return;
                    }
                }
            }
        }
        let mut buf = vec![0u8; CHUNK_SIZE];
        loop {
            if cancel.load(Ordering::Relaxed) {
                let _ = out
                    .send(PigeonOut::Control(PigeonWire::FileCancel {
                        id: offer_id.to_string(),
                    }))
                    .await;
                st.update_transfer(
                    offer_id,
                    sent,
                    Some(src.total_size),
                    TransferStatus::Cancelled,
                    None,
                );
                tracing::info!("批次「{}」的推送已被用户取消", src.name);
                return;
            }
            // 暂停：块间挂起等待恢复或取消（100ms 轮询，CPU 可忽略）
            if paused.load(Ordering::Relaxed) {
                tracing::info!("批次「{}」推送已暂停", src.name);
                while paused.load(Ordering::Relaxed) && !cancel.load(Ordering::Relaxed) {
                    tokio::time::sleep(Duration::from_millis(100)).await;
                }
                if !cancel.load(Ordering::Relaxed) {
                    tracing::info!("批次「{}」推送已恢复", src.name);
                }
                continue;
            }
            match f.read(&mut buf).await {
                Ok(0) => break,
                Ok(n) => {
                    if out
                        .send(PigeonOut::Chunk {
                            offer_id: id16,
                            offset: sent,
                            data: buf[..n].to_vec(),
                        })
                        .await
                        .is_err()
                    {
                        st.update_transfer(
                            offer_id,
                            sent,
                            Some(src.total_size),
                            TransferStatus::Failed,
                            None,
                        );
                        return;
                    }
                    hasher.update(&buf[..n]);
                    sent += n as u64;
                    st.progress_throttled(offer_id, sent, false);
                }
                Err(_) => {
                    st.update_transfer(
                        offer_id,
                        sent,
                        Some(src.total_size),
                        TransferStatus::Failed,
                        None,
                    );
                    return;
                }
            }
        }
        // 单文件推送完：报哈希（接收方校验通过后继续下一文件）
        let sha = sha_hex(&hasher.finalize());
        if out
            .send(PigeonOut::Control(PigeonWire::FileHash {
                id: offer_id.to_string(),
                index: file_idx as u32,
                sha,
            }))
            .await
            .is_err()
        {
            st.update_transfer(
                offer_id,
                sent,
                Some(src.total_size),
                TransferStatus::Failed,
                None,
            );
            return;
        }
    }
    let _ = out
        .send(PigeonOut::Control(PigeonWire::FileEnd {
            id: offer_id.to_string(),
            ok: true,
        }))
        .await;
    let path = src
        .files
        .first()
        .map(|f| f.0.parent().unwrap_or(std::path::Path::new(".")))
        .unwrap_or(std::path::Path::new("."))
        .to_path_buf();
    st.update_transfer(
        offer_id,
        sent,
        Some(src.total_size),
        TransferStatus::Done,
        Some(path),
    );
    tracing::info!(
        "批次「{}」已加密推送完成（{} 项，{} 字节）",
        src.name,
        src.files.len(),
        sent
    );
}

/// 轮询取消标记（推送协程与取消指令分属不同任务）
async fn wait_cancelled(cancel: &AtomicBool) {
    loop {
        if cancel.load(Ordering::Relaxed) {
            return;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

fn link_name(st: &AppState, peer_id: &str) -> String {
    st.pigeon_conns
        .lock()
        .unwrap()
        .get(peer_id)
        .map(|l| l.name.clone())
        .unwrap_or_else(|| "电脑".into())
}

pub async fn drop_link(st: Arc<AppState>, peer_id: String) {
    if st.pigeon_conns.lock().unwrap().remove(&peer_id).is_some() {
        emit_peers(&st);
    }
    // 断链清理：该 peer 接收中的批次落盘、保留半成品（重连重发后可续传），传输行标失败
    let stale: Vec<crate::server::BatchRecvState> = {
        let mut states = st.recv_files.lock().await;
        let keys: Vec<String> = states
            .iter()
            .filter(|(_, s)| s.peer_id == peer_id)
            .map(|(k, _)| k.clone())
            .collect();
        keys.iter().filter_map(|k| states.remove(k)).collect()
    };
    for mut s in stale {
        if let Some(f) = s.file.as_mut() {
            let _ = f.flush().await;
        }
        s.file = None;
        crate::server::save_manifest(&s.dir, &s.files);
        st.update_transfer(&s.row_id, s.have, None, TransferStatus::Failed, None);
        tracing::info!("链路断开，「{}」已保留 {} 字节半成品待续传", s.name, s.have);
    }
    // 未开始推送的邀请清单一并清理（含已接受未开始的）
    st.pigeon_incoming
        .lock()
        .unwrap()
        .retain(|_, (peer, _)| peer != &peer_id);
    st.pigeon_ready
        .lock()
        .unwrap()
        .retain(|_, (peer, _)| peer != &peer_id);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn batch_id_stable_and_sensitive() {
        let a = vec![
            ("docs/a.txt".into(), 10u64, 100u128),
            ("docs/b.txt".into(), 20, 200),
        ];
        // 顺序敏感：同序同内容 id 一致（build_batch_source 负责先排序）
        assert_eq!(batch_id(&a), batch_id(&a.clone()));
        let b2 = vec![
            ("docs/a.txt".to_string(), 10u64, 100u128),
            ("docs/b.txt".to_string(), 20u64, 200u128),
        ];
        assert_eq!(batch_id(&a), batch_id(&b2));
        // 大小变化 → id 变化
        let c = vec![
            ("docs/a.txt".into(), 11u64, 100u128),
            ("docs/b.txt".into(), 20, 200),
        ];
        assert_ne!(batch_id(&a), batch_id(&c));
        // mtime 变化 → id 变化
        let d = vec![
            ("docs/a.txt".into(), 10u64, 101u128),
            ("docs/b.txt".into(), 20, 200),
        ];
        assert_ne!(batch_id(&a), batch_id(&d));
        // 32 位 hex
        assert_eq!(batch_id(&a).len(), 32);
    }

    #[test]
    fn stream_locate_boundaries() {
        let sizes = [100u64, 50, 0, 30];
        assert_eq!(stream_locate(&sizes, 0), (0, 0));
        assert_eq!(stream_locate(&sizes, 99), (0, 99));
        // 文件边界归一到下一文件开头
        assert_eq!(stream_locate(&sizes, 100), (1, 0));
        assert_eq!(stream_locate(&sizes, 150), (3, 0)); // 空文件跳过
        assert_eq!(stream_locate(&sizes, 150 + 30), (4, 0)); // 全部完成
        assert_eq!(stream_locate(&sizes, 1_000_000), (4, 0));
        assert_eq!(stream_locate(&[], 0), (0, 0));
    }

    #[test]
    fn resume_have_boundary_granularity() {
        let sizes = [100u64, 50, 30];
        // 全部完成且校验 → 全量偏移
        assert_eq!(
            resume_have(&sizes, &[100, 50, 30], &[true, true, true]),
            180
        );
        // 当前文件写了一半（未校验）→ 只累计其前缀之前的完成文件，
        // 该文件从头重收（保证哈希覆盖完整文件）
        assert_eq!(
            resume_have(&sizes, &[100, 20, 0], &[true, false, false]),
            100
        );
        // 写满但哈希未到 → 不计入，从头重收该文件
        assert_eq!(
            resume_have(&sizes, &[100, 50, 30], &[true, false, false]),
            100
        );
        // 首个文件未完成 → 从 0 开始
        assert_eq!(resume_have(&sizes, &[40, 0, 0], &[false, false, false]), 0);
        // 空清单
        assert_eq!(resume_have(&[], &[], &[]), 0);
    }

    #[test]
    fn sha_hex_format() {
        assert_eq!(sha_hex(&[]), "");
        assert_eq!(sha_hex(&[0xde, 0xad]), "dead");
    }

    #[test]
    fn fingerprint_format() {
        let fp = fingerprint(&[0u8; 32]);
        // 32 字节 → SHA-256 → 64 hex，冒号分 32 组
        assert_eq!(fp.len(), 32 * 3 - 1);
        assert_eq!(fp.matches(':').count(), 31);
    }

    #[test]
    fn parse_host_addr_forms() {
        // 缺省端口 / 显式端口 / 主机名
        assert_eq!(parse_host_addr("192.168.1.5").unwrap(), "192.168.1.5:34256");
        assert_eq!(parse_host_addr("192.168.1.5:8001").unwrap(), "192.168.1.5:8001");
        assert_eq!(parse_host_addr(" puter.local ").unwrap(), "puter.local:34256");
        assert_eq!(parse_host_addr("puter.local:80").unwrap(), "puter.local:80");
        // 非法输入
        assert!(parse_host_addr("").is_err());
        assert!(parse_host_addr("1.2.3.4:").is_err());
        assert!(parse_host_addr("1.2.3.4:99999").is_err());
        assert!(parse_host_addr("a b c").is_err());
        assert!(parse_host_addr("::1").is_err());
        assert!(parse_host_addr("http://1.2.3.4").is_err());
    }
}
