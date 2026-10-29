//! MCP stdio server：把 Yunshu 能力暴露为 MCP 工具，供 AI agent 调用。
//! 安全默认：不提供自动信任/自动接收类工具——信任与来件是人工门，
//! 需要在交互终端里由人决策（后续可用 MCP elicitation 转给客户端人工批准）。

use crate::shared::{self, Shared};
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::{CallToolResult, ContentBlock};
use rmcp::{ErrorData as McpError, ServerHandler, ServiceExt, tool, tool_handler, tool_router};
use std::path::PathBuf;
use std::sync::Arc;

fn json_result(v: serde_json::Value) -> Result<CallToolResult, McpError> {
    Ok(CallToolResult::success(vec![ContentBlock::text(
        serde_json::to_string_pretty(&v).unwrap_or_default(),
    )]))
}

/// 工具级失败：调用已到达本工具但没办成（对端断开/超时/拒绝等），消息给 agent 看
fn fail(msg: String) -> Result<CallToolResult, McpError> {
    Ok(CallToolResult::error(vec![ContentBlock::text(msg)]))
}

#[derive(Debug, serde::Deserialize, rmcp::schemars::JsonSchema)]
pub struct SendTextParams {
    /// 对方 peer_id（先用 pigeon_list_peers 查询；支持 id/机器ID 前缀）
    pub peer_id: String,
    /// 要发送的文本
    pub text: String,
}

#[derive(Debug, serde::Deserialize, rmcp::schemars::JsonSchema)]
pub struct SendFilesParams {
    /// 对方 peer_id（先用 pigeon_list_peers 查询；支持 id/机器ID 前缀）
    pub peer_id: String,
    /// 本地文件/文件夹路径列表（不支持含空格的路径）
    pub paths: Vec<String>,
    /// 等待传输终态的最长秒数（默认 300；0 = 只发起不等待结果）
    #[serde(default)]
    pub wait_secs: Option<u64>,
}

#[derive(Debug, serde::Deserialize, rmcp::schemars::JsonSchema)]
pub struct WeixinSendTextParams {
    /// 接收者 ilink 用户 ID（缺省发给扫码绑定的微信账号；通常留空）
    #[serde(default)]
    pub to: Option<String>,
    /// 要发送的文本（markdown 会被转为纯文本，超 1800 字自动拆多条）
    pub text: String,
}

#[derive(Debug, serde::Deserialize, rmcp::schemars::JsonSchema)]
pub struct WeixinSendFilesParams {
    /// 本地文件路径列表（图片/视频按类型直发，其余作文件附件；单个 ≤100 MiB）
    pub paths: Vec<String>,
    /// 说明文字（随第一个文件发送）
    #[serde(default)]
    pub caption: Option<String>,
    /// 接收者 ilink 用户 ID（缺省发给扫码绑定的微信账号）
    #[serde(default)]
    pub to: Option<String>,
}

#[derive(Debug, serde::Deserialize, rmcp::schemars::JsonSchema)]
pub struct WeixinInboxParams {
    /// 返回条数（默认 20，最多 100）
    #[serde(default)]
    pub limit: Option<u64>,
}

#[derive(Clone)]
pub struct YunshuMcp {
    shared: Arc<Shared>,
}

#[tool_router]
impl YunshuMcp {
    pub fn new(shared: Arc<Shared>) -> Self {
        Self { shared }
    }

    #[tool(description = "列出 Yunshu 飞鸽（PC↔PC）发现的主机：id、机器ID、名称、ip:端口、连接状态。发送文本/文件前先调用本工具获取 peer_id。")]
    fn pigeon_list_peers(&self) -> Result<CallToolResult, McpError> {
        let peers = self.shared.mirror.lock().unwrap().peers.clone();
        json_result(serde_json::to_value(&peers).unwrap_or_default())
    }

    #[tool(description = "向指定 peer 发送加密文本（Noise XX 加密直连，在线即达）。对方未连接时返回错误。")]
    async fn pigeon_send_text(
        &self,
        Parameters(p): Parameters<SendTextParams>,
    ) -> Result<CallToolResult, McpError> {
        let peer = match self.shared.find_peer(&p.peer_id) {
            Ok(p) => p,
            Err(e) => return fail(e),
        };
        match shared::send_text(&self.shared, &peer.id, &p.text).await {
            Ok(v) => json_result(v),
            Err(e) => fail(e),
        }
    }

    #[tool(description = "向指定 peer 发送本地文件或文件夹（多个合并为一个批次）。对方需在其 Yunshu 端确认接收后才开始传输，工具会等待传输终态。")]
    async fn pigeon_send_files(
        &self,
        Parameters(p): Parameters<SendFilesParams>,
    ) -> Result<CallToolResult, McpError> {
        let peer = match self.shared.find_peer(&p.peer_id) {
            Ok(p) => p,
            Err(e) => return fail(e),
        };
        let paths: Vec<PathBuf> = p.paths.into_iter().map(PathBuf::from).collect();
        if paths.is_empty() {
            return fail("paths 不能为空".into());
        }
        for path in &paths {
            if !path.exists() {
                return fail(format!("路径不存在：{}", path.display()));
            }
        }
        match shared::send_files(&self.shared, &peer.id, paths, p.wait_secs.unwrap_or(300)).await {
            Ok(v) if v.get("ok").and_then(|b| b.as_bool()).unwrap_or(false) => json_result(v),
            Ok(v) => fail(v.to_string()),
            Err(e) => fail(e),
        }
    }

    #[tool(description = "微信通道状态：是否已登录（扫码绑定的个人微信机器人）、长轮询是否运行、默认推送目标、已配对联系人、收件箱条数。未登录时提示用户在 yunshu-cli 交互终端执行 wx login 扫码。")]
    async fn weixin_status(&self) -> Result<CallToolResult, McpError> {
        let s = yunshu_core::weixin::status_view();
        json_result(serde_json::to_value(&s).unwrap_or_default())
    }

    #[tool(description = "通过个人微信机器人发送文本（收件方是扫码绑定的微信账号）。适合把任务结果、通知发给机主本人。发送失败会返回原因（未登录/token 失效等）。")]
    async fn weixin_send_text(
        &self,
        Parameters(p): Parameters<WeixinSendTextParams>,
    ) -> Result<CallToolResult, McpError> {
        match yunshu_core::weixin::send_text(p.to.as_deref(), &p.text).await {
            Ok(n) => json_result(serde_json::json!({ "ok": true, "messages": n })),
            Err(e) => fail(e),
        }
    }

    #[tool(description = "通过微信发送本地文件/图片/视频给机主（自动按类型分流；逐个上传，返回逐文件结果）。可附说明文字。")]
    async fn weixin_send_files(
        &self,
        Parameters(p): Parameters<WeixinSendFilesParams>,
    ) -> Result<CallToolResult, McpError> {
        if p.paths.is_empty() {
            return fail("paths 不能为空".into());
        }
        let paths: Vec<PathBuf> = p.paths.into_iter().map(PathBuf::from).collect();
        for path in &paths {
            if !path.exists() {
                return fail(format!("路径不存在：{}", path.display()));
            }
        }
        let results = yunshu_core::weixin::send_files(p.to.as_deref(), &paths, p.caption.as_deref())
            .await;
        let items: Vec<serde_json::Value> = results
            .into_iter()
            .map(|(path, r)| match r {
                Ok(desc) => serde_json::json!({ "path": path, "ok": true, "desc": desc }),
                Err(e) => serde_json::json!({ "path": path, "ok": false, "error": e }),
            })
            .collect();
        let all_ok = items.iter().all(|i| i.get("ok").and_then(|b| b.as_bool()).unwrap_or(false));
        let v = serde_json::json!({ "ok": all_ok, "files": items });
        if all_ok {
            json_result(v)
        } else {
            fail(v.to_string())
        }
    }

    #[tool(description = "读取微信收件箱：最近收到的文本消息与媒体（媒体已自动下载解密，saved_path 是落盘路径；error 是接收失败原因）。机主给机器人发消息后用本工具读取。")]
    async fn weixin_inbox(
        &self,
        Parameters(p): Parameters<WeixinInboxParams>,
    ) -> Result<CallToolResult, McpError> {
        let limit = p.limit.unwrap_or(20).clamp(1, 100) as usize;
        let items = yunshu_core::weixin::inbox_recent(limit);
        json_result(serde_json::json!({ "count": items.len(), "messages": items }))
    }
}

// 宏参数只收字面量（env!() 不行），缺省时 serverInfo 会落成 rmcp 自身——必须显式写；
// version 随 workspace 版本一起改
#[tool_handler(name = "yunshu-cli", version = "0.1.1")]
impl ServerHandler for YunshuMcp {}

/// 以 stdio 传输运行 MCP server（stdout 是协议通道，日志一律走 stderr）
pub async fn run(shared: Arc<Shared>) -> Result<(), Box<dyn std::error::Error>> {
    let service = YunshuMcp::new(shared)
        .serve(rmcp::transport::stdio())
        .await?;
    service.waiting().await?;
    Ok(())
}
