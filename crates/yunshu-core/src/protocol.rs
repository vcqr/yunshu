//! WS 上的 JSON 消息协议（手机端 JS 与 PC 共用一套字段名）。

use serde::{Deserialize, Serialize};

/// 手机 → PC
#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum WsIn {
    /// 携带二维码里的 token 发起配对
    Hello { token: String, device: String },
    /// 提交 6 位配对码
    Pair { code: String },
    /// 发送文本到电脑（写入剪贴板）
    SendText { text: String },
}

/// PC → 手机
#[derive(Debug, Serialize, Clone)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum WsOut {
    PairResult {
        ok: bool,
        #[serde(skip_serializing_if = "Option::is_none")]
        session: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        reason: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        pc_name: Option<String>,
    },
    PcText { text: String },
    PcFile { id: String, name: String, size: u64 },
    Progress { id: String, transferred: u64, size: u64, done: bool },
}

/// 飞鸽（PC↔PC）线上消息，两端共用一套。
/// hello/welcome 各带一个"我发给你"的会话凭证：你拿它从我这里下载文件，反之亦然。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum PigeonWire {
    PigeonHello { id: String, name: String, port: u16 },
    PigeonWelcome {
        ok: bool,
        #[serde(skip_serializing_if = "Option::is_none")]
        reason: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        pc_name: Option<String>,
    },
    PcText { text: String },
    /// 广播消息：发给所有在线电脑（各端在"所有人"会话中展示）
    BroadcastText { text: String },
    PcFile { id: String, name: String, size: u64 },
    /// 接收方对 PcFile 的答复：accepted=false 时发送方的该传输标记为已拒绝
    FileDecision { id: String, accepted: bool },
    /// 文件推送开始（接收方据此建文件与传输行）
    FileStart { id: String, name: String, size: u64 },
    /// 接收方对 FileStart 的应答：告知本地已续传字节数（0 = 全新传输）
    FileResume { id: String, have: u64 },
    /// 任一方取消传输：接收方删除半成品，发送方停止推送
    FileCancel { id: String },
    /// 文件推送结束
    FileEnd { id: String, ok: bool },
    Progress { id: String, transferred: u64, size: u64, done: bool },
}
