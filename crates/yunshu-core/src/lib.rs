//! 云书核心：HTTP/WS 服务、配对、传输、飞鸽（PC↔PC）。不依赖任何 UI。

pub mod clipboard;
pub mod encoder;
pub mod pairing;
pub mod pigeon;
pub mod protocol;
pub mod remote;
pub mod server;
pub mod settings;

use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::sync::mpsc::{Receiver, Sender};

/// 传输方向
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
pub enum Direction {
    ToPhone,
    FromPhone,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
pub enum TransferKind {
    File,
    Text,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
pub enum TransferStatus {
    Waiting,
    Active,
    Done,
    Failed,
    /// 接收方明确拒绝了这次传输
    Refused,
    /// 任一方主动取消（半成品已清理）
    Cancelled,
    /// 任一方主动暂停（链路保持，恢复后继续）
    Paused,
}

/// 批次清单条目（用于 UI 明细展示）
#[derive(Debug, Clone, serde::Serialize)]
pub struct FileEntry {
    pub name: String,
    pub size: u64,
}

/// 一条传输记录（UI 列表数据源，每次变化全量推送）
#[derive(Debug, Clone, serde::Serialize)]
pub struct TransferInfo {
    pub id: String,
    pub direction: Direction,
    pub kind: TransferKind,
    pub name: String,
    pub size: u64,
    pub transferred: u64,
    pub status: TransferStatus,
    /// 完成后的本地路径（用于"打开所在文件夹"）
    pub path: Option<PathBuf>,
    /// 所属设备："phone" 或飞鸽 peer_id
    pub peer: Option<String>,
    /// 批次文件清单（飞鸽批次传输有值；文本/手机单文件为空）
    pub files: Vec<FileEntry>,
}

impl TransferInfo {
    pub fn file(
        id: String,
        direction: Direction,
        name: String,
        size: u64,
        peer: Option<String>,
    ) -> Self {
        TransferInfo {
            id,
            direction,
            kind: TransferKind::File,
            files: vec![FileEntry {
                name: name.clone(),
                size,
            }],
            name,
            size,
            transferred: 0,
            status: TransferStatus::Waiting,
            path: None,
            peer,
        }
    }

    /// 批次传输（多文件/文件夹）
    pub fn batch(
        id: String,
        direction: Direction,
        name: String,
        size: u64,
        files: Vec<FileEntry>,
        peer: Option<String>,
    ) -> Self {
        TransferInfo {
            id,
            direction,
            kind: TransferKind::File,
            name,
            size,
            transferred: 0,
            status: TransferStatus::Waiting,
            path: None,
            peer,
            files,
        }
    }

    pub fn text(id: String, direction: Direction, chars: usize, peer: Option<String>) -> Self {
        TransferInfo {
            id,
            direction,
            kind: TransferKind::Text,
            name: format!("文本（{chars} 字）"),
            size: chars as u64,
            transferred: chars as u64,
            status: TransferStatus::Done,
            path: None,
            peer,
            files: Vec::new(),
        }
    }
}

/// 飞鸽用户列表里的一台电脑
#[derive(Debug, Clone, serde::Serialize)]
pub struct PigeonPeerInfo {
    pub id: String,
    pub name: String,
    pub ip: String,
    pub tcp_port: u16,
    pub connected: bool,
    /// 公开机器 ID（静态公钥指纹前 8 位 hex；远程控制以此定位设备）
    pub machine_id: String,
    /// 手动添加的主机（自助添加，不经广播发现）
    pub manual: bool,
    /// 可达地址 host:port（手动主机的移除/重连用）
    pub addr: String,
}

/// 核心线程 → UI 的事件
#[derive(Debug, Clone, serde::Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum CoreEvent {
    ServerStarted {
        addr: String,
    },
    ServerError {
        reason: String,
    },
    /// 新的二维码地址（每 60 秒轮换 token）
    QrUrl(String),
    /// 二维码备用直达地址（全部网卡 http/https，随 pair token 轮换刷新）
    QrAlternates(Vec<String>),
    PairPending {
        code: String,
        device: String,
    },
    PairOk {
        device: String,
    },
    PairFailed {
        reason: String,
    },
    PeerDisconnected,
    TextFromPhone {
        text: String,
    },
    Toast {
        text: String,
    },
    TransferUpdate {
        #[serde(flatten)]
        t: Box<TransferInfo>,
    },
    /// 手机开始上传，请用户选择保存目录
    AskSaveDir {
        batch: String,
    },
    /// 飞鸽：电脑用户列表有变化（全量推送）
    PigeonPeers {
        peers: Vec<PigeonPeerInfo>,
    },
    /// 飞鸽：收到另一台电脑的聊天消息
    ChatFromPc {
        peer_id: String,
        peer_name: String,
        text: String,
    },
    /// 飞鸽：收到广播消息（所有人会话）
    ChatBroadcast {
        peer_id: String,
        peer_name: String,
        text: String,
    },
    /// 飞鸽：新设备首次连入，等待用户确认指纹
    PigeonTrustRequest {
        peer_id: String,
        name: String,
        fingerprint: String,
    },
    /// 飞鸽：对方发来文件，等待用户确认（批量：多文件/文件夹一次确认）
    PigeonOfferIncoming {
        peer_id: String,
        peer_name: String,
        offer_id: String,
        name: String,
        size: u64,
        files: Vec<FileEntry>,
    },
    /// 飞鸽：新设备静态密钥已固定（TOFU 首次配对）
    PigeonFingerprint {
        peer_id: String,
        fingerprint: String,
    },
    /// 远程控制：本机临时密码（每次启动重新生成，设置里展示）
    RemoteTempPassword {
        password: String,
    },
    /// 远程控制：正在连接对方
    RemoteConnecting {
        peer_id: String,
        name: String,
    },
    /// 远程控制：连接成功，可打开观看窗口（token 为主观看凭证，view_token 为手机只读凭证）
    RemoteViewReady {
        peer_id: String,
        name: String,
        token: String,
        view_token: String,
        port: u16,
        width: u32,
        height: u32,
    },
    /// 远程控制：连接失败
    RemoteConnectFailed {
        peer_id: String,
        reason: String,
    },
    /// 远程控制：会话开始。side: "controller"（本机在看对方）| "controlled"（本机被控）
    RemoteSessionStarted {
        peer_id: String,
        name: String,
        side: String,
    },
    /// 远程控制：会话结束
    RemoteSessionEnded {
        peer_id: String,
        name: String,
        side: String,
        reason: Option<String>,
    },
    /// 远程控制：对方请求协助控制本机（反向协助）
    RemoteAssistRequest {
        peer_id: String,
        name: String,
    },
}

/// UI → 核心线程的请求
#[derive(Debug)]
pub enum UiRequest {
    SendFiles(Vec<PathBuf>),
    SendText(String),
    SetSaveDir {
        batch: String,
        dir: Option<PathBuf>,
    },
    DisconnectDevice,
    /// 后台线程直接在 UI 上弹一条提示
    Toast(String),
    PigeonSendText {
        peer_id: String,
        text: String,
    },
    BroadcastText(String),
    PigeonTrust {
        peer_id: String,
        fingerprint: String,
    },
    PigeonRejectTrust {
        peer_id: String,
    },
    PigeonSendFiles {
        peer_id: String,
        paths: Vec<PathBuf>,
    },
    PigeonDisconnect {
        peer_id: String,
    },
    /// 用户同意接收：去对方拉取文件
    PigeonAcceptFile {
        peer_id: String,
        offer_id: String,
    },
    /// 用户拒绝接收
    PigeonRejectFile {
        peer_id: String,
        offer_id: String,
    },
    /// 用户取消一次飞鸽传输（等待中或推送中，双向皆可发起）
    PigeonCancelFile {
        peer_id: String,
        offer_id: String,
    },
    /// 用户暂停/恢复一次飞鸽推送（仅 PC↔PC）
    PigeonPauseFile {
        peer_id: String,
        offer_id: String,
        paused: bool,
    },
    /// 忘记一条传输记录（仅 UI 列表，不影响磁盘文件）
    ForgetTransfer {
        id: String,
    },
    /// 切换防打扰模式（仅已信任设备可连接、广播静音）
    SetDnd(bool),
    /// 切换隐身模式（不广播本机、不主动外连，其他设备的主机列表将移除本机）
    SetStealth(bool),
    // ---- 自助添加主机 ----
    /// 手动添加并立即连接（addr 为 host:port，name 为备注可空）
    PigeonAddHost { addr: String, name: String },
    /// 移除手动主机（连接中的链路一并断开）
    PigeonRemoveHost { addr: String },
    /// 手动重连某台已添加的主机
    PigeonConnectHost { addr: String },
    // ---- 远程控制 ----
    /// 发起远程控制：machine_id 从发现表定位设备，password 为对方临时/固定/一次性密码
    RemoteConnect {
        machine_id: String,
        password: String,
    },
    /// 断开当前远程控制会话（观看或被控，任一方）
    RemoteDisconnect,
    /// 远程控制会话中向受控端推送文件（文件回传）
    RemoteSendFiles {
        paths: Vec<PathBuf>,
    },
    /// 通过飞鸽链路请求对方协助（对方同意后回一次性密码）
    RemoteAssistRequest {
        peer_id: String,
    },
    /// 答复对方的协助请求
    RemoteAssistRespond {
        peer_id: String,
        accepted: bool,
    },
    /// 设置/清除远程控制固定密码（None/空 = 清除）
    SetRemoteFixedPassword {
        password: Option<String>,
    },
}

/// 核心配置
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CoreConfig {
    pub port: u16,
    pub https_port: u16,
    pub save_dir: PathBuf,
    pub pc_name: String,
    /// 防打扰：仅已信任（指纹固定）设备可连接，广播静音
    pub dnd_mode: bool,
    /// 隐身模式：不广播本机（从其他设备的主机列表消失），也不主动外连；
    /// 手动外发/远程控制等用户主动行为不受影响
    pub stealth_mode: bool,
}

/// 配置目录：~/.yunshu（跨平台一致、直观可寻：身份密钥/信任表/设置/证书都在这里）
pub fn config_dir() -> Option<std::path::PathBuf> {
    dirs::home_dir().map(|h| h.join(".yunshu"))
}

/// 旧版配置在系统配置目录（macOS ~/Library/Application Support/yunshu、
/// Windows %APPDATA%\yunshu、Linux ~/.config/yunshu）。新目录还没有身份密钥
/// 而旧目录存在时整体搬迁——身份密钥换了会连带换机器 ID 并作废所有 TOFU 信任。
pub fn ensure_config_dir() {
    let Some(new_dir) = config_dir() else { return };
    let _ = std::fs::create_dir_all(&new_dir);
    if new_dir.join("pigeon_key.bin").exists() {
        return;
    }
    let Some(legacy) = dirs::config_dir().map(|d| d.join("yunshu")) else {
        return;
    };
    if !legacy.is_dir() {
        return;
    }
    let mut moved = 0;
    if let Ok(entries) = std::fs::read_dir(&legacy) {
        for e in entries.flatten() {
            let p = e.path();
            if p.is_file()
                && std::fs::copy(&p, new_dir.join(e.file_name())).is_ok()
            {
                moved += 1;
            }
        }
    }
    if moved > 0 {
        tracing::info!("已从旧配置目录迁移 {moved} 个文件：{}", legacy.display());
    }
}

/// 在后台线程启动 tokio runtime 与 axum 服务
pub fn run(
    cfg: CoreConfig,
    event_tx: Sender<CoreEvent>,
    ui_rx: Receiver<UiRequest>,
) -> std::thread::JoinHandle<()> {
    std::thread::Builder::new()
        .name("yunshu-core".into())
        .spawn(move || {
            let _ = tracing_subscriber::fmt()
                .with_env_filter(
                    tracing_subscriber::EnvFilter::try_from_default_env()
                        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
                )
                .try_init();
            let rt = tokio::runtime::Builder::new_multi_thread()
                .enable_all()
                .build()
                .expect("tokio runtime 创建失败");
            rt.block_on(server::serve(cfg, event_tx, ui_rx));
        })
        .expect("yunshu-core 线程启动失败")
}

/// 把文件夹打包成 zip（含顶层目录名），放到临时目录（Tauri 壳发送文件夹时用）
pub mod util {
    pub fn zip_folder(src: &std::path::Path) -> Result<std::path::PathBuf, String> {
        let name = src
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| "文件夹".into());
        let out_dir = std::env::temp_dir().join("yunshu_zip");
        std::fs::create_dir_all(&out_dir).map_err(|e| e.to_string())?;
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        let out = out_dir.join(format!("{name}-{stamp}.zip"));
        let file = std::fs::File::create(&out).map_err(|e| e.to_string())?;
        let mut w = zip::ZipWriter::new(file);
        let opts = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Deflated);
        w.add_directory(format!("{name}/"), opts)
            .map_err(|e| e.to_string())?;
        fn walk(
            w: &mut zip::ZipWriter<std::fs::File>,
            dir: &std::path::Path,
            rel: &str,
            opts: zip::write::SimpleFileOptions,
        ) -> Result<(), String> {
            for entry in std::fs::read_dir(dir).map_err(|e| e.to_string())? {
                let path = entry.map_err(|e| e.to_string())?.path();
                let fname = path
                    .file_name()
                    .map(|n| n.to_string_lossy().to_string())
                    .unwrap_or_default();
                let child_rel = format!("{rel}/{fname}");
                if path.is_dir() {
                    w.add_directory(child_rel.clone(), opts)
                        .map_err(|e| e.to_string())?;
                    walk(w, &path, &child_rel, opts)?;
                } else {
                    w.start_file(child_rel.clone(), opts)
                        .map_err(|e| e.to_string())?;
                    let mut f = std::fs::File::open(&path).map_err(|e| e.to_string())?;
                    std::io::copy(&mut f, w).map_err(|e| e.to_string())?;
                }
            }
            Ok(())
        }
        walk(&mut w, src, &name, opts)?;
        w.finish().map_err(|e| e.to_string())?;
        Ok(out)
    }
}
