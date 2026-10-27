//! 云书核心：HTTP/WS 服务、配对、传输、飞鸽（PC↔PC）。不依赖任何 UI。

pub mod clipboard;
pub mod pairing;
pub mod pigeon;
pub mod protocol;
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
}

impl TransferInfo {
    pub fn file(id: String, direction: Direction, name: String, size: u64, peer: Option<String>) -> Self {
        TransferInfo { id, direction, kind: TransferKind::File, name, size, transferred: 0, status: TransferStatus::Waiting, path: None, peer }
    }

    pub fn text(id: String, direction: Direction, chars: usize, peer: Option<String>) -> Self {
        TransferInfo { id, direction, kind: TransferKind::Text, name: format!("文本（{chars} 字）"), size: chars as u64, transferred: chars as u64, status: TransferStatus::Done, path: None, peer }
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
}

/// 核心线程 → UI 的事件
#[derive(Debug, Clone, serde::Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum CoreEvent {
    ServerStarted { addr: String },
    ServerError { reason: String },
    /// 新的二维码地址（每 60 秒轮换 token）
    QrUrl(String),
    PairPending { code: String, device: String },
    PairOk { device: String },
    PairFailed { reason: String },
    PeerDisconnected,
    TextFromPhone { text: String },
    Toast { text: String },
    TransferUpdate { #[serde(flatten)] t: Box<TransferInfo> },
    /// 手机开始上传，请用户选择保存目录
    AskSaveDir { batch: String },
    /// 飞鸽：电脑用户列表有变化（全量推送）
    PigeonPeers { peers: Vec<PigeonPeerInfo> },
    /// 飞鸽：收到另一台电脑的聊天消息
    ChatFromPc { peer_id: String, peer_name: String, text: String },
    /// 飞鸽：收到广播消息（所有人会话）
    ChatBroadcast { peer_id: String, peer_name: String, text: String },
    /// 飞鸽：新设备首次连入，等待用户确认指纹
    PigeonTrustRequest { peer_id: String, name: String, fingerprint: String },
    /// 飞鸽：对方发来文件，等待用户确认
    PigeonOfferIncoming { peer_id: String, peer_name: String, offer_id: String, name: String, size: u64 },
    /// 飞鸽：新设备静态密钥已固定（TOFU 首次配对）
    PigeonFingerprint { peer_id: String, fingerprint: String },
}

/// UI → 核心线程的请求
#[derive(Debug)]
pub enum UiRequest {
    SendFiles(Vec<PathBuf>),
    SendText(String),
    SetSaveDir { batch: String, dir: Option<PathBuf> },
    DisconnectDevice,
    /// 后台线程直接在 UI 上弹一条提示
    Toast(String),
    PigeonSendText { peer_id: String, text: String },
    BroadcastText(String),
    PigeonTrust { peer_id: String, fingerprint: String },
    PigeonRejectTrust { peer_id: String },
    PigeonSendFiles { peer_id: String, paths: Vec<PathBuf> },
    PigeonDisconnect { peer_id: String },
    /// 用户同意接收：去对方拉取文件
    PigeonAcceptFile { peer_id: String, offer_id: String },
    /// 用户拒绝接收
    PigeonRejectFile { peer_id: String, offer_id: String },
}

/// 核心配置
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CoreConfig {
    pub port: u16,
    pub https_port: u16,
    pub save_dir: PathBuf,
    pub pc_name: String,
}

/// 在后台线程启动 tokio runtime 与 axum 服务
pub fn run(cfg: CoreConfig, event_tx: Sender<CoreEvent>, ui_rx: Receiver<UiRequest>) -> std::thread::JoinHandle<()> {
    std::thread::Builder::new()
        .name("yunshu-core".into())
        .spawn(move || {
            let _ = tracing_subscriber::fmt()
                .with_env_filter(tracing_subscriber::EnvFilter::try_from_default_env()
                    .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")))
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
        let opts = zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated);
        w.add_directory(format!("{name}/"), opts).map_err(|e| e.to_string())?;
        fn walk(w: &mut zip::ZipWriter<std::fs::File>, dir: &std::path::Path, rel: &str, opts: zip::write::SimpleFileOptions) -> Result<(), String> {
            for entry in std::fs::read_dir(dir).map_err(|e| e.to_string())? {
                let path = entry.map_err(|e| e.to_string())?.path();
                let fname = path.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
                let child_rel = format!("{rel}/{fname}");
                if path.is_dir() {
                    w.add_directory(child_rel.clone(), opts).map_err(|e| e.to_string())?;
                    walk(w, &path, &child_rel, opts)?;
                } else {
                    w.start_file(child_rel.clone(), opts).map_err(|e| e.to_string())?;
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
