//! 进程内共享状态：核心事件镜像 + UiRequest 通道 + 事件广播 + 动作辅助。
//! 镜像职责与 Tauri 壳的 apply_to_cache 一致；core 的动作请求没有同步回执，
//! 统一在这里实现「发请求 → 订阅事件流等终态」，交互终端与 MCP 工具共用。

use std::sync::{Arc, Mutex};
use std::time::Duration;
use yunshu_core::{
    CoreEvent, PigeonPeerInfo, TransferInfo, TransferKind, TransferStatus, UiRequest,
};

/// 一条收到的聊天
#[derive(Clone)]
pub struct ChatLine {
    pub peer_name: String,
    pub text: String,
    pub broadcast: bool,
}

/// 一条等待用户决策的来件批次
#[derive(Clone)]
pub struct PendingOffer {
    pub peer_id: String,
    pub peer_name: String,
    pub offer_id: String,
    pub name: String,
    pub size: u64,
}

#[derive(Default)]
pub struct Mirror {
    pub addr: Option<String>,
    pub server_error: Option<String>,
    pub peers: Vec<PigeonPeerInfo>,
    pub transfers: Vec<TransferInfo>,
    /// 待确认的新设备（peer_id, 名称, 指纹）
    pub pending_trusts: Vec<(String, String, String)>,
    pub offers: Vec<PendingOffer>,
    pub chats: Vec<ChatLine>,
}

pub struct Shared {
    pub ui_tx: std::sync::mpsc::Sender<UiRequest>,
    /// 核心事件广播：CLI 打印与动作等待方各持一个订阅
    pub events: tokio::sync::broadcast::Sender<CoreEvent>,
    pub mirror: Mutex<Mirror>,
}

impl Shared {
    pub fn new(ui_tx: std::sync::mpsc::Sender<UiRequest>) -> Arc<Self> {
        Arc::new(Self {
            ui_tx,
            events: tokio::sync::broadcast::channel(1024).0,
            mirror: Mutex::new(Mirror::default()),
        })
    }

    pub fn send(&self, req: UiRequest) -> bool {
        self.ui_tx.send(req).is_ok()
    }

    /// 按 完整 id/机器ID → id 前缀 → 机器ID 前缀 → 精确名字 定位 peer；
    /// 命中多台视为未找到（要求更长的前缀）
    pub fn find_peer(&self, key: &str) -> Result<PigeonPeerInfo, String> {
        let peers: Vec<PigeonPeerInfo> = self.mirror.lock().unwrap().peers.clone();
        let mut hits: Vec<PigeonPeerInfo> = peers
            .iter()
            .filter(|p| p.id == key || p.machine_id == key)
            .cloned()
            .collect();
        if hits.is_empty() {
            hits = peers
                .iter()
                .filter(|p| p.id.starts_with(key))
                .cloned()
                .collect();
        }
        if hits.is_empty() {
            hits = peers
                .iter()
                .filter(|p| p.machine_id.starts_with(key))
                .cloned()
                .collect();
        }
        if hits.is_empty() {
            hits = peers.iter().filter(|p| p.name == key).cloned().collect();
        }
        match hits.len() {
            1 => Ok(hits.remove(0)),
            0 => Err(format!("主机列表里没有「{key}」（等发现广播，或先 addhost）")),
            _ => Err(format!(
                "「{key}」匹配到 {} 台主机，请用更长的 id 前缀",
                hits.len()
            )),
        }
    }
}

/// 把一条核心事件并入镜像
pub fn apply(shared: &Shared, ev: &CoreEvent) {
    let mut m = shared.mirror.lock().unwrap();
    match ev {
        CoreEvent::ServerStarted { addr } => m.addr = Some(addr.clone()),
        CoreEvent::ServerError { reason } => m.server_error = Some(reason.clone()),
        CoreEvent::PigeonPeers { peers } => {
            // 已连上的 peer 不再有待确认请求
            let connected: Vec<&str> = peers
                .iter()
                .filter(|p| p.connected)
                .map(|p| p.id.as_str())
                .collect();
            m.pending_trusts
                .retain(|(pid, _, _)| !connected.contains(&pid.as_str()));
            m.peers = peers.clone();
        }
        CoreEvent::TransferUpdate { t } => {
            match m.transfers.iter_mut().find(|x| x.id == t.id) {
                Some(x) => *x = (**t).clone(),
                None => m.transfers.push((**t).clone()),
            }
            m.offers.retain(|o| o.offer_id != t.id);
        }
        CoreEvent::ChatFromPc {
            peer_name, text, ..
        } => push_chat(&mut m, peer_name, text, false),
        CoreEvent::ChatBroadcast {
            peer_name, text, ..
        } => push_chat(&mut m, peer_name, text, true),
        CoreEvent::PigeonTrustRequest {
            peer_id,
            name,
            fingerprint,
        } => {
            if !m
                .pending_trusts
                .iter()
                .any(|(pid, _, _)| pid == peer_id)
            {
                m.pending_trusts.push((
                    peer_id.clone(),
                    name.clone(),
                    fingerprint.clone(),
                ));
            }
        }
        CoreEvent::PigeonOfferIncoming {
            peer_id,
            peer_name,
            offer_id,
            name,
            size,
            ..
        } => {
            if !m.offers.iter().any(|o| o.offer_id == *offer_id) {
                m.offers.push(PendingOffer {
                    peer_id: peer_id.clone(),
                    peer_name: peer_name.clone(),
                    offer_id: offer_id.clone(),
                    name: name.clone(),
                    size: *size,
                });
            }
        }
        _ => {}
    }
}

fn push_chat(m: &mut Mirror, peer_name: &str, text: &str, broadcast: bool) {
    m.chats.push(ChatLine {
        peer_name: peer_name.to_string(),
        text: text.to_string(),
        broadcast,
    });
    if m.chats.len() > 200 {
        m.chats.remove(0);
    }
}

/// 传输状态中文表述
pub fn status_zh(s: TransferStatus) -> &'static str {
    match s {
        TransferStatus::Waiting => "等待对方确认",
        TransferStatus::Active => "传输中",
        TransferStatus::Done => "完成",
        TransferStatus::Failed => "失败",
        TransferStatus::Refused => "对方拒绝",
        TransferStatus::Cancelled => "已取消",
        TransferStatus::Paused => "已暂停",
    }
}

fn terminal(s: TransferStatus) -> bool {
    matches!(
        s,
        TransferStatus::Done
            | TransferStatus::Failed
            | TransferStatus::Refused
            | TransferStatus::Cancelled
    )
}

/// 发送加密文本并等结果：成功以新的 Text 传输记录为准，失败以 Toast「无法发送」
/// 为准（core 的 server.rs 只有这两条路径）；5 秒内两者皆无视为结果未知
pub async fn send_text(
    shared: &Shared,
    peer_id: &str,
    text: &str,
) -> Result<serde_json::Value, String> {
    if !shared.send(UiRequest::PigeonSendText {
        peer_id: peer_id.to_string(),
        text: text.to_string(),
    }) {
        return Err("核心线程已退出".into());
    }
    let mut rx = shared.events.subscribe();
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    loop {
        match tokio::time::timeout_at(deadline, rx.recv()).await {
            Ok(Ok(CoreEvent::TransferUpdate { t }))
                if t.peer.as_deref() == Some(peer_id) && t.kind == TransferKind::Text =>
            {
                return Ok(serde_json::json!({
                    "ok": true,
                    "peer_id": peer_id,
                    "chars": t.size,
                }));
            }
            Ok(Ok(CoreEvent::Toast { text })) if text.contains("无法发送") => return Err(text),
            Ok(Ok(_)) => {}
            Ok(Err(tokio::sync::broadcast::error::RecvError::Lagged(_))) => {}
            _ => return Err("发送结果未知（5 秒内无回执）".into()),
        }
    }
}

/// 发送文件批次（多路径合并一批，对方确认后开始传输）并等终态。
/// wait_secs = 0 表示只发起不等待。
pub async fn send_files(
    shared: &Shared,
    peer_id: &str,
    paths: Vec<std::path::PathBuf>,
    wait_secs: u64,
) -> Result<serde_json::Value, String> {
    // 已在途（非终态）的批次 id 视为「旧」：同内容重发的批次 id 稳定派生会重合，
    // 只按「新出现的非旧记录」定位本次批次
    let known: std::collections::HashSet<String> = shared
        .mirror
        .lock()
        .unwrap()
        .transfers
        .iter()
        .filter(|t| !terminal(t.status))
        .map(|t| t.id.clone())
        .collect();
    if !shared.send(UiRequest::PigeonSendFiles {
        peer_id: peer_id.to_string(),
        paths,
    }) {
        return Err("核心线程已退出".into());
    }
    if wait_secs == 0 {
        return Ok(serde_json::json!({
            "ok": true,
            "note": "已发起批次邀请，对方确认后开始传输（未等待结果）",
        }));
    }
    let mut rx = shared.events.subscribe();
    let deadline = tokio::time::Instant::now() + Duration::from_secs(wait_secs);
    let mut last: Option<TransferInfo> = None;
    loop {
        let ev = match tokio::time::timeout_at(deadline, rx.recv()).await {
            Ok(Ok(ev)) => ev,
            Ok(Err(tokio::sync::broadcast::error::RecvError::Lagged(_))) => continue,
            _ => {
                return Err(match &last {
                    Some(t) => format!(
                        "等待超时（{wait_secs} 秒），批次「{}」未到终态，当前：{}（{}/{} 字节）",
                        t.name,
                        status_zh(t.status),
                        t.transferred,
                        t.size
                    ),
                    None => format!("等待超时（{wait_secs} 秒），未见到新批次（对方可能未连接）"),
                });
            }
        };
        match ev {
            CoreEvent::Toast { text } if text.contains("无法发送") => return Err(text),
            CoreEvent::TransferUpdate { t }
                if t.peer.as_deref() == Some(peer_id) && !known.contains(&t.id) =>
            {
                let reached = terminal(t.status);
                last = Some((*t).clone());
                if reached {
                    let t = last.unwrap();
                    return Ok(serde_json::json!({
                        "ok": t.status == TransferStatus::Done,
                        "batch": t.name,
                        "status": status_zh(t.status),
                        "files": t.files.len(),
                        "size": t.size,
                        "transferred": t.transferred,
                        "path": t.path.as_ref().map(|p| p.to_string_lossy().to_string()),
                    }));
                }
            }
            _ => {}
        }
    }
}

/// 交互模式下把事件打印成一行（传输只在状态变化时打印，避免刷屏）
pub fn print_event(
    ev: &CoreEvent,
    seen: &mut std::collections::HashMap<String, &'static str>,
) {
    match ev {
        CoreEvent::ServerStarted { addr } => println!("[core] 服务已启动 {addr}"),
        CoreEvent::ServerError { reason } => println!("[core] ✗ 服务启动失败：{reason}"),
        CoreEvent::QrUrl(url) => println!("[core] 手机配对地址：{url}"),
        CoreEvent::ChatFromPc {
            peer_name, text, ..
        } => println!("← 「{peer_name}」{text}"),
        CoreEvent::ChatBroadcast {
            peer_name, text, ..
        } => println!("← （广播）「{peer_name}」{text}"),
        CoreEvent::Toast { text } => println!("· {text}"),
        CoreEvent::TransferUpdate { t } => {
            let zh = status_zh(t.status);
            if seen.get(&t.id) != Some(&zh) {
                seen.insert(t.id.clone(), zh);
                println!(
                    "[传输] {}：{}（{}/{} 字节）",
                    t.name, zh, t.transferred, t.size
                );
            }
        }
        CoreEvent::PigeonTrustRequest {
            peer_id,
            name,
            fingerprint,
        } => println!(
            "⚠ 新设备「{name}」等待信任确认（指纹 {}…）：trust {peer_id} / rejecttrust {peer_id}",
            &fingerprint[..16.min(fingerprint.len())]
        ),
        CoreEvent::PigeonOfferIncoming {
            peer_name,
            offer_id,
            name,
            size,
            files,
            ..
        } => println!(
            "⚠ 「{peer_name}」发来批次「{name}」（{} 项，{size} 字节）：accept {} / reject {}",
            files.len(),
            &offer_id[..8.min(offer_id.len())],
            &offer_id[..8.min(offer_id.len())]
        ),
        CoreEvent::PigeonPeers { peers } => {
            let on = peers.iter().filter(|p| p.connected).count();
            println!("[主机] {} 台，已连接 {on}", peers.len());
        }
        CoreEvent::WeixinInbound { msg } => {
            match (&msg.saved_path, &msg.error) {
                (Some(p), _) => println!("← 微信「{}」{} → {p}", msg.from, msg.text),
                (None, Some(e)) => println!("← 微信「{}」{}（接收失败：{e}）", msg.from, msg.text),
                _ => println!("← 微信「{}」{}", msg.from, msg.text),
            }
        }
        _ => {}
    }
}
