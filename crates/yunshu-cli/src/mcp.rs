//! MCP stdio server：把云书能力暴露为 MCP 工具，供 AI agent 调用。
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

#[derive(Clone)]
pub struct YunshuMcp {
    shared: Arc<Shared>,
}

#[tool_router]
impl YunshuMcp {
    pub fn new(shared: Arc<Shared>) -> Self {
        Self { shared }
    }

    #[tool(description = "列出云书飞鸽（PC↔PC）发现的主机：id、机器ID、名称、ip:端口、连接状态。发送文本/文件前先调用本工具获取 peer_id。")]
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

    #[tool(description = "向指定 peer 发送本地文件或文件夹（多个合并为一个批次）。对方需在其云书端确认接收后才开始传输，工具会等待传输终态。")]
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
