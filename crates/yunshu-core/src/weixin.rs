//! 微信通道：扫码登录个人微信机器人，长轮询收消息、发文本/文件/图片/视频。
//!
//! 协议对齐腾讯官方 `@tencent-weixin/openclaw-weixin@2.4.10`（MIT），
//! 请求格式与其保持一致。后端所有接口在 HTTP 200 下用 `errcode` 报错
//! （-14 = token 失效），`ret` 与 `errcode` 都要检查。
//!
//! 凭证/配对/断点落配置目录 `weixin.json`；收到的媒体解密后存
//! 「保存目录/微信/」。运行态（登录会话、轮询任务、冷却、收件箱）挂在
//! 本模块全局 RUNTIME 上：应用启动时（有凭证则）与登录确认后拉起长轮询，
//! token 失效按官方插件策略冷却 1 小时，重新扫码换新 token 后自动解除。

use std::collections::{HashMap, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use aes::cipher::{BlockDecrypt, BlockEncrypt, KeyInit};
use base64::Engine as _;
use md5::{Digest, Md5};
use serde::{Deserialize, Serialize};
use serde_json::Value;

const DEFAULT_BASE_URL: &str = "https://ilinkai.weixin.qq.com";
/// 媒体 CDN 基址（官方插件常量；getuploadurl 返回 upload_full_url 时优先用它）
const CDN_BASE_URL: &str = "https://novac2c.cdn.weixin.qq.com/c2c";
/// 对齐插件 package.json 顶层 ilink_appid
const ILINK_APP_ID: &str = "bot";
/// 对齐插件版本号（iLink-App-ClientVersion 由它编码）
const CHANNEL_VERSION: &str = "2.4.10";
/// 观测用 UA（不参与鉴权/路由）
const BOT_AGENT: &str = "yunshu-weixin";
/// get_bot_qrcode / get_qrcode_status 的 bot_type（插件当前构建取值）
const BOT_TYPE: &str = "3";
/// 服务端判定 token 失效的错误码，按官方插件策略冷却 1 小时
const STALE_TOKEN_ERRCODE: i64 = -14;
const STALE_PAUSE_MS: u64 = 60 * 60 * 1000;
/// 登录轮询/长轮询客户端超时
const LONG_POLL_TIMEOUT: Duration = Duration::from_secs(35);
/// 常规请求超时
const API_TIMEOUT: Duration = Duration::from_secs(15);
/// CDN 媒体上传/下载超时
const CDN_TIMEOUT: Duration = Duration::from_secs(600);
/// 单条文本消息长度上限（字符），超出拆分为多条
const MAX_TEXT_CHARS: usize = 1800;
/// 入站媒体大小上限（官方插件同款 100 MiB）
const MEDIA_MAX_BYTES: u64 = 100 * 1024 * 1024;
/// 收件箱容量（运行态环形缓冲，MCP weixin_inbox 读取）
const INBOX_CAP: usize = 100;

// ── 上传媒体类型（proto: UploadMediaType）──
const UPLOAD_IMAGE: u32 = 1;
const UPLOAD_VIDEO: u32 = 2;
const UPLOAD_FILE: u32 = 3;

// ── 消息项类型（proto: MessageItemType）──
const ITEM_TEXT: i64 = 1;
const ITEM_IMAGE: i64 = 2;
const ITEM_VOICE: i64 = 3;
const ITEM_FILE: i64 = 4;
const ITEM_VIDEO: i64 = 5;

// ── 持久化配置 ──

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct WeixinPeer {
    #[serde(default)]
    pub context_token: String,
    #[serde(default)]
    pub updated_at: u64,
    #[serde(default)]
    pub last_text: String,
}

/// 配置目录下 `weixin.json`
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WeixinConfig {
    #[serde(default)]
    pub token: String,
    #[serde(default)]
    pub bot_id: String,
    #[serde(default)]
    pub base_url: String,
    /// 扫码者用户 ID，默认推送目标
    #[serde(default)]
    pub user_id: String,
    #[serde(default)]
    pub saved_at: u64,
    /// userId → 最近一次下发的 context_token（回复时透传提高可达性）
    #[serde(default)]
    pub peers: HashMap<String, WeixinPeer>,
    /// getupdates 断点，重启续传不丢消息
    #[serde(default)]
    pub sync_buf: String,
}

impl Default for WeixinConfig {
    fn default() -> Self {
        Self {
            token: String::new(),
            bot_id: String::new(),
            base_url: String::new(),
            user_id: String::new(),
            saved_at: 0,
            peers: HashMap::new(),
            sync_buf: String::new(),
        }
    }
}

impl WeixinConfig {
    pub fn logged_in(&self) -> bool {
        !self.token.trim().is_empty()
    }

    pub fn effective_base_url(&self) -> String {
        let b = self.base_url.trim();
        if b.is_empty() {
            DEFAULT_BASE_URL.to_string()
        } else {
            b.to_string()
        }
    }
}

fn config_path() -> Option<PathBuf> {
    crate::config_dir().map(|d| d.join("weixin.json"))
}

pub fn load_config() -> Result<WeixinConfig, String> {
    let Some(path) = config_path() else {
        return Err("无法定位配置目录".into());
    };
    if !path.exists() {
        return Ok(WeixinConfig::default());
    }
    let raw = std::fs::read_to_string(&path).map_err(|e| e.to_string())?;
    serde_json::from_str(&raw).map_err(|e| e.to_string())
}

fn persist(cfg: &WeixinConfig) -> Result<(), String> {
    let Some(path) = config_path() else {
        return Err("无法定位配置目录".into());
    };
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    let json = serde_json::to_string_pretty(cfg).map_err(|e| e.to_string())?;
    std::fs::write(&path, json).map_err(|e| e.to_string())?;
    // 含微信登录凭证，权限收紧为属主可读写
    crate::pigeon::set_owner_only(&path);
    Ok(())
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

fn rand_bytes(n: usize) -> Vec<u8> {
    use rand::Rng as _;
    let mut buf = vec![0u8; n];
    rand::rng().fill_bytes(&mut buf);
    buf
}

/// 收到的媒体落盘目录：保存目录/微信/
fn media_dir() -> Option<PathBuf> {
    let dir = crate::settings::Settings::load_or_default()
        .save_dir_path()
        .join("微信");
    std::fs::create_dir_all(&dir).ok()?;
    Some(dir)
}

// ── 运行态 ──

/// 入站媒体引用：getupdates 消息里一个待下载的媒体项
struct InboundMedia {
    kind: &'static str,
    /// CDN 下载参数（media.encrypt_query_param）
    query_param: String,
    /// 服务端可能直接给全 URL（media.full_url）
    full_url: Option<String>,
    /// media.aes_key（base64；图片也可能是 image_item.aeskey 的 hex）
    aes_key_b64: Option<String>,
    /// image_item 顶层的 hex aeskey
    image_aes_hex: Option<String>,
    file_name: Option<String>,
}

/// 一条微信入站消息（文本，或已下载解密落盘的媒体）
#[derive(Debug, Clone, Serialize)]
pub struct WxInboundMsg {
    pub at_ms: u64,
    /// 发送者 ilink 用户 ID
    pub from: String,
    /// text | image | file | video | voice
    pub kind: &'static str,
    /// 文本全文；媒体项为人类可读描述
    pub text: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub file_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub size: Option<u64>,
    /// 媒体解密后的落盘路径
    #[serde(skip_serializing_if = "Option::is_none")]
    pub saved_path: Option<String>,
    /// 媒体下载/解密失败原因
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum LoginStatus {
    /// 未在登录
    #[default]
    Idle,
    /// 已取码待扫码
    Waiting,
    /// 已扫码，等待确认（可能要求配对数字）
    Scanned,
    /// 需要输入手机微信上显示的配对数字
    NeedVerifycode,
    /// 登录成功（凭证已保存、轮询已启动）
    Confirmed,
    /// 该微信已绑定过且未返回新凭证（本地凭证仍有效，可直接使用）
    Binded,
    /// 二维码多次失效/多次输错，登录流程终止
    Failed,
}

#[derive(Debug, Clone, Serialize)]
pub struct LoginView {
    pub status: LoginStatus,
    /// 二维码内容 URL（终端/前端渲染成二维码，也可手机微信直接打开）
    #[serde(skip_serializing_if = "Option::is_none")]
    pub qr_url: Option<String>,
    /// 展示给用户的信息（失败原因等）
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct PeerView {
    pub user_id: String,
    pub updated_at: u64,
    pub last_text: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct StatusView {
    pub logged_in: bool,
    pub bot_id: String,
    pub user_id: String,
    pub saved_at: u64,
    /// 长轮询任务是否在跑
    pub polling: bool,
    /// token 失效冷却剩余毫秒（0 = 未冷却）
    pub paused_remaining_ms: u64,
    pub last_poll_at: u64,
    pub last_inbound_at: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_error: Option<String>,
    pub peers: Vec<PeerView>,
    /// 收件箱条数
    pub inbox_count: usize,
    /// 入站媒体落盘目录
    pub media_dir: Option<String>,
}

struct LoginSession {
    qrcode: String,
    qr_url: String,
    started_at_ms: u64,
    task: tokio::task::JoinHandle<()>,
}

#[derive(Default)]
struct RuntimeInner {
    login: Option<LoginSession>,
    login_status: LoginStatus,
    login_message: Option<String>,
    /// 登录轮询任务持有的待提交配对码
    pending_verify_code: Option<String>,
    /// 长轮询保活任务
    poll_task: Option<tokio::task::JoinHandle<()>>,
    paused_until_ms: u64,
    /// 触发冷却时使用的 token；换新 token（重新扫码）后自动解除
    paused_token: Option<String>,
    last_poll_at: u64,
    last_inbound_at: u64,
    last_error: Option<String>,
    /// 最近收到的消息（环形缓冲）
    inbox: VecDeque<WxInboundMsg>,
    /// 核心事件出口（CLI/Tauri 壳订阅 CoreEvent 打印/渲染入站消息）
    events: Option<std::sync::mpsc::Sender<crate::CoreEvent>>,
}

struct Runtime {
    inner: Mutex<RuntimeInner>,
}

static RUNTIME: OnceLock<Runtime> = OnceLock::new();

fn runtime() -> &'static Runtime {
    RUNTIME.get_or_init(|| Runtime {
        inner: Mutex::new(RuntimeInner::default()),
    })
}

/// 注册核心事件出口：微信入站消息以 CoreEvent::WeixinInbound 推给 UI。
/// 须在 init_polling / 登录之前调用。
pub fn set_event_sink(tx: std::sync::mpsc::Sender<crate::CoreEvent>) {
    if let Ok(mut g) = runtime().inner.lock() {
        g.events = Some(tx);
    }
}

fn push_inbound(msg: WxInboundMsg) {
    let ev = crate::CoreEvent::WeixinInbound { msg: msg.clone() };
    if let Ok(mut g) = runtime().inner.lock() {
        g.last_inbound_at = msg.at_ms;
        g.inbox.push_back(msg);
        while g.inbox.len() > INBOX_CAP {
            g.inbox.pop_front();
        }
        if let Some(tx) = g.events.clone() {
            let _ = tx.send(ev);
        }
    }
}

/// 最近收到的消息（新的在后），最多 limit 条
pub fn inbox_recent(limit: usize) -> Vec<WxInboundMsg> {
    let g = runtime().inner.lock().ok();
    match g {
        Some(g) => g
            .inbox
            .iter()
            .rev()
            .take(limit)
            .cloned()
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .collect(),
        None => Vec::new(),
    }
}

// ── HTTP 协议层 ──

/// iLink-App-ClientVersion: uint32 编码 0x00MMNNPP
fn client_version_u32() -> u32 {
    let parts: Vec<u32> = CHANNEL_VERSION
        .split('.')
        .filter_map(|p| p.parse().ok())
        .collect();
    let (maj, min, pat) = (
        parts.first().copied().unwrap_or(0),
        parts.get(1).copied().unwrap_or(0),
        parts.get(2).copied().unwrap_or(0),
    );
    (maj & 0xff) << 16 | (min & 0xff) << 8 | (pat & 0xff)
}

/// X-WECHAT-UIN：随机 uint32 → 十进制字符串 → base64
fn random_wechat_uin() -> String {
    let n = rand_bytes(4);
    let v = u32::from_be_bytes([n[0], n[1], n[2], n[3]]);
    base64::engine::general_purpose::STANDARD.encode(v.to_string().as_bytes())
}

fn base_info() -> Value {
    serde_json::json!({
        "channel_version": CHANNEL_VERSION,
        "bot_agent": format!("{BOT_AGENT}/{}", env!("CARGO_PKG_VERSION")),
    })
}

fn build_client(timeout: Option<Duration>) -> Result<reqwest::Client, String> {
    crate::install_rustls_provider();
    let mut builder = apply_proxy(reqwest::Client::builder())?;
    if let Some(t) = timeout {
        builder = builder.timeout(t);
    }
    builder
        .build()
        .map_err(|e| format!("HTTP client 构建失败: {e}"))
}

/// 按设置套用代理（仅微信通道的外网请求；局域网传输不受影响）：
/// direct=直连；system（默认）=交给 reqwest（启用 system-proxy 特性后读取系统/环境代理）；
/// custom=显式代理地址 + NO_PROXY 例外。地址无效时直接报错（会体现在微信状态里），不做静默回退。
fn apply_proxy(builder: reqwest::ClientBuilder) -> Result<reqwest::ClientBuilder, String> {
    let s = crate::settings::Settings::load_or_default();
    match s.proxy_mode.as_str() {
        "direct" => Ok(builder.no_proxy()),
        "custom" => {
            let url = s.proxy_url.trim();
            if url.is_empty() {
                return Ok(builder);
            }
            let mut proxy = reqwest::Proxy::all(url).map_err(|_| format!("代理地址无效：{url}"))?;
            let np = s.no_proxy.trim();
            if !np.is_empty() {
                proxy = proxy.no_proxy(reqwest::NoProxy::from_string(np));
            }
            Ok(builder.proxy(proxy))
        }
        _ => Ok(builder),
    }
}

fn api_client() -> Result<reqwest::Client, String> {
    build_client(Some(API_TIMEOUT))
}

fn headers(token: Option<&str>) -> Result<reqwest::header::HeaderMap, String> {
    let mut map = reqwest::header::HeaderMap::new();
    map.insert("Content-Type", "application/json".parse().unwrap());
    map.insert("iLink-App-Id", ILINK_APP_ID.parse().unwrap());
    map.insert(
        "iLink-App-ClientVersion",
        client_version_u32().to_string().parse().unwrap(),
    );
    map.insert("AuthorizationType", "ilink_bot_token".parse().unwrap());
    map.insert("X-WECHAT-UIN", random_wechat_uin().parse().unwrap());
    if let Some(t) = token.map(str::trim).filter(|t| !t.is_empty()) {
        let v = format!("Bearer {t}");
        map.insert("Authorization", v.parse().unwrap());
    }
    Ok(map)
}

async fn post_json(
    base_url: &str,
    endpoint: &str,
    body: &Value,
    token: Option<&str>,
    long_poll: bool,
) -> Result<Value, String> {
    let url = format!("{}/{}", base_url.trim_end_matches('/'), endpoint);
    let client = build_client(if long_poll {
        Some(LONG_POLL_TIMEOUT)
    } else {
        Some(API_TIMEOUT)
    })?;
    let resp = client
        .post(&url)
        .headers(headers(token)?)
        .json(body)
        .send()
        .await
        .map_err(|e| format!("{endpoint} 请求失败: {e}"))?;
    let status = resp.status();
    let text = resp
        .text()
        .await
        .map_err(|e| format!("{endpoint} 读取失败: {e}"))?;
    if !status.is_success() {
        return Err(format!("{endpoint} HTTP {status}: {}", truncate(&text, 200)));
    }
    serde_json::from_str(&text).map_err(|e| format!("{endpoint} 响应解析失败: {e}"))
}

async fn get_json(base_url: &str, endpoint: &str) -> Result<Value, String> {
    let url = format!("{}/{}", base_url.trim_end_matches('/'), endpoint);
    let client = build_client(Some(LONG_POLL_TIMEOUT))?;
    let resp = client
        .get(&url)
        .headers(headers(None)?)
        .send()
        .await
        .map_err(|e| format!("GET {endpoint} 失败: {e}"))?;
    let status = resp.status();
    let text = resp
        .text()
        .await
        .map_err(|e| format!("{endpoint} 读取失败: {e}"))?;
    if !status.is_success() {
        return Err(format!("{endpoint} HTTP {status}: {}", truncate(&text, 200)));
    }
    serde_json::from_str(&text).map_err(|e| format!("{endpoint} 响应解析失败: {e}"))
}

/// 读取业务错误码：ret / errcode 任一非 0 都算失败；返回 Some((code, errmsg))
fn biz_error(v: &Value) -> Option<(i64, String)> {
    let code = v.get("errcode").and_then(|x| x.as_i64()).unwrap_or(0);
    let ret = v.get("ret").and_then(|x| x.as_i64()).unwrap_or(0);
    if code != 0 || ret != 0 {
        let msg = v
            .get("errmsg")
            .and_then(|x| x.as_str())
            .unwrap_or("")
            .to_string();
        return Some((if code != 0 { code } else { ret }, msg));
    }
    None
}

fn truncate(s: &str, max: usize) -> &str {
    if s.len() <= max {
        s
    } else {
        let mut cut = max;
        while cut > 0 && !s.is_char_boundary(cut) {
            cut -= 1;
        }
        &s[..cut]
    }
}

fn urlencode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char)
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

// ── AES-128-ECB + PKCS7（CDN 媒体加密，对齐官方插件 aes-ecb.js）──

/// PKCS7 补位后的密文大小
fn aes_padded_size(plain_len: usize) -> usize {
    (plain_len / 16 + 1) * 16
}

fn aes_ecb_encrypt(plain: &[u8], key: &[u8; 16]) -> Vec<u8> {
    let pad = 16 - (plain.len() % 16);
    let mut data = Vec::with_capacity(plain.len() + pad);
    data.extend_from_slice(plain);
    data.resize(plain.len() + pad, pad as u8);
    let cipher = aes::Aes128::new(key.into());
    for chunk in data.chunks_mut(16) {
        cipher.encrypt_block(aes::Block::from_mut_slice(chunk));
    }
    data
}

fn aes_ecb_decrypt(ciphertext: &[u8], key: &[u8; 16]) -> Result<Vec<u8>, String> {
    if ciphertext.is_empty() || ciphertext.len() % 16 != 0 {
        return Err(format!("密文长度非法：{} 字节", ciphertext.len()));
    }
    let mut data = ciphertext.to_vec();
    let cipher = aes::Aes128::new(key.into());
    for chunk in data.chunks_mut(16) {
        cipher.decrypt_block(aes::Block::from_mut_slice(chunk));
    }
    let pad = *data.last().ok_or("空密文")? as usize;
    if pad == 0 || pad > 16 || pad as usize > data.len() {
        return Err("PKCS7 补位非法（密钥可能不对）".into());
    }
    data.truncate(data.len() - pad);
    Ok(data)
}

fn md5_hex(data: &[u8]) -> String {
    let mut h = Md5::new();
    h.update(data);
    format!("{:x}", h.finalize())
}

/// 解析 CDNMedia.aes_key：两种野外编码都支持——
/// base64(原始 16 字节) 或 base64(32 位 hex 字符串，即 hex 再编码)
fn parse_aes_key(aes_key_b64: &str) -> Result<[u8; 16], String> {
    let decoded = base64::engine::general_purpose::STANDARD
        .decode(aes_key_b64.trim())
        .map_err(|e| format!("aes_key base64 解码失败: {e}"))?;
    match decoded.len() {
        16 => {
            let mut k = [0u8; 16];
            k.copy_from_slice(&decoded);
            Ok(k)
        }
        32 if decoded.iter().all(|b| b.is_ascii_hexdigit()) => {
            let hex = std::str::from_utf8(&decoded).map_err(|_| "aes_key 编码异常".to_string())?;
            hex_to_16(hex)
        }
        n => Err(format!("aes_key 解码后长度异常：{n} 字节")),
    }
}

fn hex_to_16(hex: &str) -> Result<[u8; 16], String> {
    let bytes = hex
        .as_bytes()
        .chunks_exact(2)
        .map(|p| u8::from_str_radix(std::str::from_utf8(p).unwrap_or("zz"), 16))
        .collect::<Result<Vec<u8>, _>>()
        .map_err(|e| format!("hex 解析失败: {e}"))?;
    bytes
        .try_into()
        .map_err(|_| format!("hex 长度异常：{} 位", hex.len()))
}

// ── 文本处理 ──

/// 轻量 markdown 转纯文本（微信机器人消息不渲染 markdown）
pub fn strip_markdown(text: &str) -> String {
    let mut s = text.to_string();
    // 代码围栏标记与行内代码反引号
    while let Some(pos) = s.find("```") {
        let end = s[pos + 3..]
            .find('\n')
            .map(|n| pos + 3 + n + 1)
            .unwrap_or(s.len());
        s.replace_range(pos..end, "");
    }
    // 成对行内代码
    while let Some(a) = s.find('`') {
        match s[a + 1..].find('`') {
            Some(b) => {
                let inner = s[a + 1..a + 1 + b].to_string();
                s.replace_range(a..a + 1 + b + 1, &inner);
            }
            None => {
                s.replace_range(a..a + 1, "");
                break;
            }
        }
    }
    // 粗体
    while let Some(a) = s.find("**") {
        match s[a + 2..].find("**") {
            Some(b) => {
                let inner = s[a + 2..a + 2 + b].to_string();
                s.replace_range(a..a + 2 + b + 2, &inner);
            }
            None => {
                s.replace_range(a..a + 2, "");
                break;
            }
        }
    }
    // 标题井号 / 列表符号
    let mut out = String::with_capacity(s.len());
    for line in s.lines() {
        let trimmed = line.trim_start();
        let replaced = if trimmed.starts_with('#') {
            Some(trimmed.trim_start_matches('#').trim_start().to_string())
        } else if trimmed.starts_with("- ") || trimmed.starts_with("* ") {
            Some(format!("• {}", &trimmed[2..]))
        } else {
            None
        };
        out.push_str(replaced.as_deref().unwrap_or(line));
        out.push('\n');
    }
    out.trim_end().to_string()
}

/// 超长拆分为多条（优先在换行处断开）
fn split_text(text: &str) -> Vec<String> {
    if text.chars().count() <= MAX_TEXT_CHARS {
        return vec![text.to_string()];
    }
    let mut chunks = Vec::new();
    let mut rest = text.to_string();
    while rest.chars().count() > MAX_TEXT_CHARS {
        let cut_char = rest
            .char_indices()
            .nth(MAX_TEXT_CHARS)
            .map(|(i, _)| i)
            .unwrap_or(rest.len());
        let head = &rest[..cut_char];
        let cut = match head.rfind('\n') {
            Some(n) if n > cut_char / 2 => n,
            _ => cut_char,
        };
        chunks.push(rest[..cut].to_string());
        rest = rest[cut..].trim_start_matches('\n').to_string();
    }
    if !rest.is_empty() {
        chunks.push(rest);
    }
    chunks
}

fn new_client_id() -> String {
    let n = rand_bytes(4);
    let v = u32::from_be_bytes([n[0], n[1], n[2], n[3]]);
    format!("yunshu:{}-{:08x}", now_ms(), v)
}

// ── 登录 ──

async fn fetch_qrcode(local_token: Option<&str>) -> Result<(String, String), String> {
    let token_list: Vec<&str> = local_token
        .filter(|t| !t.trim().is_empty())
        .into_iter()
        .collect();
    let body = serde_json::json!({ "local_token_list": token_list });
    let resp = post_json(
        DEFAULT_BASE_URL,
        &format!("ilink/bot/get_bot_qrcode?bot_type={BOT_TYPE}"),
        &body,
        None,
        false,
    )
    .await?;
    let qrcode = resp
        .get("qrcode")
        .and_then(|v| v.as_str())
        .ok_or("get_bot_qrcode 未返回 qrcode")?
        .to_string();
    let qr_url = resp
        .get("qrcode_img_content")
        .and_then(|v| v.as_str())
        .ok_or("get_bot_qrcode 未返回 qrcode_img_content")?
        .to_string();
    Ok((qrcode, qr_url))
}

/// 登录轮询任务：长轮询 get_qrcode_status 驱动状态机，confirmed 落盘并启动保活轮询
async fn login_task(session_qrcode: String) {
    const MAX_REFRESH: u32 = 3;
    const LOGIN_TIMEOUT_MS: u64 = 480_000;

    let initial_qrcode = session_qrcode.clone();
    let mut qrcode = session_qrcode;
    let mut poll_base = DEFAULT_BASE_URL.to_string();
    let mut refresh_count: u32 = 1;
    let deadline = now_ms() + LOGIN_TIMEOUT_MS;
    // 已扫码提示只发一次
    let mut scanned_printed = false;

    loop {
        if now_ms() >= deadline {
            set_login(LoginStatus::Failed, Some("登录超时，请重试".into()));
            return;
        }

        let verify = runtime()
            .inner
            .lock()
            .ok()
            .and_then(|g| g.pending_verify_code.clone());

        let mut endpoint = format!("ilink/bot/get_qrcode_status?qrcode={}", urlencode(&qrcode));
        if let Some(code) = verify.as_deref().filter(|c| !c.is_empty()) {
            endpoint.push_str(&format!("&verify_code={}", urlencode(code)));
        }

        let resp = match get_json(&poll_base, &endpoint).await {
            Ok(v) => v,
            Err(_) => {
                // 网络抖动/网关超时：视为等待，继续轮询
                tokio::time::sleep(Duration::from_secs(1)).await;
                continue;
            }
        };

        let status = resp.get("status").and_then(|v| v.as_str()).unwrap_or("wait");
        tracing::debug!("[WEIXIN] qrcode status={status}");

        match status {
            "wait" => {}
            "scaned" => {
                if verify.is_some() {
                    // 携带配对码后返回 scaned：校验通过
                    if let Ok(mut g) = runtime().inner.lock() {
                        g.pending_verify_code = None;
                    }
                }
                if !scanned_printed {
                    scanned_printed = true;
                    set_login(LoginStatus::Scanned, Some("已扫码，正在验证…".into()));
                }
            }
            "need_verifycode" => {
                if let Ok(mut g) = runtime().inner.lock() {
                    let wrong = g.pending_verify_code.is_some();
                    g.login_status = LoginStatus::NeedVerifycode;
                    g.login_message = Some(if wrong {
                        "配对数字不匹配，请重新输入".into()
                    } else {
                        String::new()
                    });
                }
            }
            "expired" | "verify_code_blocked" => {
                refresh_count += 1;
                if refresh_count > MAX_REFRESH {
                    let msg = if status == "expired" {
                        "二维码多次失效，登录已停止，请重试".to_string()
                    } else {
                        "配对数字多次错误，登录已停止，请稍后重试".to_string()
                    };
                    set_login(LoginStatus::Failed, Some(msg));
                    return;
                }
                if status == "verify_code_blocked" {
                    if let Ok(mut g) = runtime().inner.lock() {
                        g.pending_verify_code = None;
                    }
                }
                let local = load_config().map(|c| c.token).ok();
                match fetch_qrcode(local.as_deref()).await {
                    Ok((q, url)) => {
                        qrcode = q;
                        if let Ok(mut g) = runtime().inner.lock() {
                            if let Some(s) = g.login.as_mut() {
                                s.qrcode = qrcode.clone();
                                s.qr_url = url.clone();
                                s.started_at_ms = now_ms();
                            }
                            g.login_message = Some("二维码已刷新，请重新扫描".into());
                        }
                        scanned_printed = false;
                    }
                    Err(e) => {
                        set_login(LoginStatus::Failed, Some(format!("刷新二维码失败: {e}")));
                        return;
                    }
                }
            }
            "binded_redirect" => {
                set_login(
                    LoginStatus::Binded,
                    Some("该微信已绑定过此机器人，本地凭证仍有效，无需重复登录".into()),
                );
                return;
            }
            "scaned_but_redirect" => {
                if let Some(host) = resp.get("redirect_host").and_then(|v| v.as_str()) {
                    poll_base = format!("https://{host}");
                }
            }
            "confirmed" => {
                let token = resp
                    .get("bot_token")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                let bot_id = resp
                    .get("ilink_bot_id")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                if token.is_empty() || bot_id.is_empty() {
                    set_login(
                        LoginStatus::Failed,
                        Some("登录失败：服务端未返回完整凭证".into()),
                    );
                    return;
                }
                let mut cfg = load_config().unwrap_or_default();
                cfg.token = token;
                cfg.bot_id = bot_id;
                if let Some(b) = resp
                    .get("baseurl")
                    .and_then(|v| v.as_str())
                    .filter(|s| !s.is_empty())
                {
                    cfg.base_url = b.to_string();
                }
                if let Some(u) = resp
                    .get("ilink_user_id")
                    .and_then(|v| v.as_str())
                    .filter(|s| !s.is_empty())
                {
                    cfg.user_id = u.to_string();
                }
                cfg.saved_at = now_ms();
                if let Err(e) = persist(&cfg) {
                    set_login(LoginStatus::Failed, Some(format!("凭证保存失败: {e}")));
                    return;
                }
                start_poll_loop();
                set_login(LoginStatus::Confirmed, Some("已连接微信".into()));
                tracing::info!("[WEIXIN] 登录成功 bot_id={}", cfg.bot_id);
                return;
            }
            _ => {}
        }

        // 已被新登录会话替换/取消则退出
        let replaced = runtime()
            .inner
            .lock()
            .map(|g| match &g.login {
                Some(s) => s.qrcode != initial_qrcode && g.login_status == LoginStatus::Idle,
                None => true,
            })
            .unwrap_or(true);
        if replaced {
            return;
        }

        tokio::time::sleep(Duration::from_secs(1)).await;
    }
}

fn set_login(status: LoginStatus, message: Option<String>) {
    if let Ok(mut g) = runtime().inner.lock() {
        g.login_status = status;
        g.login_message = message;
    }
}

/// 发起扫码登录：取二维码 + 启动登录轮询任务。须在 tokio runtime 上下文中调用。
pub async fn login_start() -> Result<LoginView, String> {
    let cfg = load_config()?;
    let (qrcode, qr_url) = fetch_qrcode(Some(&cfg.token)).await?;

    let task = tokio::spawn(login_task(qrcode.clone()));

    if let Ok(mut g) = runtime().inner.lock() {
        // 取消旧会话任务
        if let Some(old) = g.login.take() {
            old.task.abort();
        }
        g.login_status = LoginStatus::Waiting;
        g.login_message = Some("请用手机微信扫描二维码".into());
        g.pending_verify_code = None;
        g.login = Some(LoginSession {
            qrcode,
            qr_url: qr_url.clone(),
            started_at_ms: now_ms(),
            task,
        });
    }

    Ok(LoginView {
        status: LoginStatus::Waiting,
        qr_url: Some(qr_url),
        message: Some("请用手机微信扫描二维码".into()),
    })
}

/// 读取登录状态（调用方 1~2s 轮询一次）
pub fn login_view() -> LoginView {
    let g = runtime().inner.lock().ok();
    match g {
        Some(g) => LoginView {
            status: g.login_status.clone(),
            qr_url: g.login.as_ref().map(|s| s.qr_url.clone()),
            message: g.login_message.clone().filter(|m| !m.is_empty()),
        },
        None => LoginView {
            status: LoginStatus::Idle,
            qr_url: None,
            message: None,
        },
    }
}

/// 提交配对数字（need_verifycode 时由调用方传入）
pub fn login_submit_code(code: &str) {
    if let Ok(mut g) = runtime().inner.lock() {
        g.pending_verify_code = Some(code.trim().to_string());
    }
}

/// 取消登录流程
pub fn login_cancel() {
    if let Ok(mut g) = runtime().inner.lock() {
        if let Some(s) = g.login.take() {
            s.task.abort();
        }
        g.login_status = LoginStatus::Idle;
        g.login_message = None;
        g.pending_verify_code = None;
    }
}

/// 退出登录：停轮询、清凭证
pub fn logout() -> Result<(), String> {
    login_cancel();
    if let Ok(mut g) = runtime().inner.lock() {
        if let Some(t) = g.poll_task.take() {
            t.abort();
        }
        g.paused_until_ms = 0;
        g.paused_token = None;
        g.last_error = None;
        g.last_poll_at = 0;
        g.last_inbound_at = 0;
    }
    let Some(path) = config_path() else {
        return Err("无法定位配置目录".into());
    };
    if path.exists() {
        std::fs::remove_file(&path).map_err(|e| e.to_string())?;
    }
    Ok(())
}

// ── 保活长轮询 ──

/// 应用启动时调用（有凭证则恢复长轮询）。须在 tokio runtime 上下文中调用。
pub fn init_polling() {
    match load_config() {
        Ok(cfg) if cfg.logged_in() => start_poll_loop(),
        _ => {}
    }
}

/// 启动（或替换）保活长轮询任务
fn start_poll_loop() {
    if let Ok(mut g) = runtime().inner.lock() {
        if let Some(old) = g.poll_task.take() {
            old.abort();
        }
        g.paused_until_ms = 0;
        g.paused_token = None;
        g.last_error = None;
        g.poll_task = Some(tokio::spawn(poll_loop()));
    }
    // 上线告知（best-effort）
    tokio::spawn(async {
        if let Err(e) = notify_lifecycle("notifystart").await {
            tracing::debug!("[WEIXIN] notifystart 失败: {e}");
        }
    });
}

async fn notify_lifecycle(endpoint: &str) -> Result<(), String> {
    let cfg = load_config()?;
    if !cfg.logged_in() {
        return Err("未登录".into());
    }
    let body = serde_json::json!({ "base_info": base_info() });
    let resp = post_json(
        &cfg.effective_base_url(),
        &format!("ilink/bot/msg/{endpoint}"),
        &body,
        Some(&cfg.token),
        false,
    )
    .await?;
    if let Some((code, msg)) = biz_error(&resp) {
        return Err(format!("{endpoint} errcode={code} errmsg={msg}"));
    }
    Ok(())
}

/// 从消息 JSON 提取媒体引用（type 2/3/4/5）；文本项由调用方处理
fn extract_media(item: &Value) -> Option<InboundMedia> {
    let t = item.get("type").and_then(|v| v.as_i64())?;
    let (kind, obj) = match t {
        ITEM_IMAGE => ("image", item.get("image_item")?),
        ITEM_VOICE => ("voice", item.get("voice_item")?),
        ITEM_FILE => ("file", item.get("file_item")?),
        ITEM_VIDEO => ("video", item.get("video_item")?),
        _ => return None,
    };
    let media = obj.get("media")?;
    let query_param = media
        .get("encrypt_query_param")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let full_url = media
        .get("full_url")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .map(str::to_string);
    if query_param.is_empty() && full_url.is_none() {
        return None;
    }
    Some(InboundMedia {
        kind,
        query_param,
        full_url,
        aes_key_b64: media
            .get("aes_key")
            .and_then(|v| v.as_str())
            .map(str::to_string),
        image_aes_hex: obj
            .get("aeskey")
            .and_then(|v| v.as_str())
            .map(str::to_string),
        file_name: obj
            .get("file_name")
            .and_then(|v| v.as_str())
            .map(str::to_string),
    })
}

/// 下载并解密一个媒体项，返回（落盘路径, 原文大小）
async fn download_media(m: &InboundMedia) -> Result<(PathBuf, u64), String> {
    let key: [u8; 16] = if let Some(hex) = m.image_aes_hex.as_deref().filter(|s| !s.is_empty()) {
        hex_to_16(hex)?
    } else {
        parse_aes_key(m.aes_key_b64.as_deref().ok_or("媒体缺少 aes_key")?)?
    };
    let url = match &m.full_url {
        Some(u) => u.clone(),
        None => format!(
            "{CDN_BASE_URL}/download?encrypted_query_param={}",
            urlencode(&m.query_param)
        ),
    };
    let client = build_client(Some(CDN_TIMEOUT))?;
    let resp = client
        .get(&url)
        .send()
        .await
        .map_err(|e| format!("CDN 下载失败: {e}"))?;
    let status = resp.status();
    if !status.is_success() {
        return Err(format!("CDN 下载 HTTP {status}"));
    }
    let bytes = resp.bytes().await.map_err(|e| format!("CDN 读取失败: {e}"))?;
    if bytes.len() as u64 > MEDIA_MAX_BYTES {
        return Err(format!("媒体超过 {} MiB 上限", MEDIA_MAX_BYTES / 1024 / 1024));
    }
    let plain = aes_ecb_decrypt(&bytes, &key)?;
    let size = plain.len() as u64;
    let dir = media_dir().ok_or("无法创建媒体保存目录")?;
    let name = match &m.file_name {
        Some(n) if !n.trim().is_empty() => sanitize_name(n),
        _ => format!("{}-{}", now_ms(), match m.kind {
            "image" => "图片",
            "voice" => "语音.silk",
            "video" => "视频.mp4",
            _ => "文件",
        }),
    };
    let mut path = dir.join(&name);
    if path.exists() {
        path = dir.join(format!("{}-{name}", now_ms()));
    }
    std::fs::write(&path, &plain).map_err(|e| format!("写盘失败: {e}"))?;
    Ok((path, size))
}

/// 去掉文件名里的路径分隔与控制字符
fn sanitize_name(name: &str) -> String {
    name.chars()
        .map(|c| match c {
            '/' | '\\' | ':' | '\0'..='\x1f' => '_',
            _ => c,
        })
        .collect()
}

fn media_desc(kind: &str, file_name: Option<&str>, size: Option<u64>) -> String {
    let zh = match kind {
        "image" => "图片",
        "voice" => "语音",
        "video" => "视频",
        _ => "文件",
    };
    match (file_name, size) {
        (Some(n), Some(s)) => format!("（{zh} {n}，{s} 字节）"),
        (Some(n), None) => format!("（{zh} {n}）"),
        (None, Some(s)) => format!("（{zh}，{s} 字节）"),
        _ => format!("（{zh}）"),
    }
}

/// 单次 getupdates：返回新 buf；成功时采集 context token 落盘、派发入站消息
async fn poll_once(buf: &str) -> Result<String, String> {
    let cfg = load_config()?;
    if !cfg.logged_in() {
        return Err("未登录".into());
    }
    let body = serde_json::json!({
        "get_updates_buf": buf,
        "base_info": base_info(),
    });
    let resp = post_json(
        &cfg.effective_base_url(),
        "ilink/bot/getupdates",
        &body,
        Some(&cfg.token),
        true,
    )
    .await?;

    if let Some((code, msg)) = biz_error(&resp) {
        if code == STALE_TOKEN_ERRCODE {
            let until = now_ms() + STALE_PAUSE_MS;
            if let Ok(mut g) = runtime().inner.lock() {
                g.paused_until_ms = until;
                g.paused_token = Some(cfg.token.clone());
                g.last_error = Some("token 失效(errcode -14)，已冷却至 1 小时，请重新扫码登录".into());
            }
            tracing::error!("[WEIXIN] token 失效(-14)，冷却 1 小时；重新扫码登录后自动恢复");
            return Ok(buf.to_string());
        }
        return Err(format!("getupdates errcode={code} errmsg={msg}"));
    }

    if let Ok(mut g) = runtime().inner.lock() {
        g.last_poll_at = now_ms();
        g.last_error = None;
    }

    let new_buf = resp
        .get("get_updates_buf")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();

    // 一次遍历采集入站消息：配对 token、文本、媒体引用
    struct MsgExt {
        from: String,
        ctx: String,
        text: String,
        media: Vec<InboundMedia>,
    }
    let mut exts: Vec<MsgExt> = Vec::new();
    if let Some(msgs) = resp.get("msgs").and_then(|v| v.as_array()) {
        for m in msgs {
            let from = m
                .get("from_user_id")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            if from.is_empty() {
                continue;
            }
            let ctx = m
                .get("context_token")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let mut text = String::new();
            let mut media = Vec::new();
            if let Some(items) = m.get("item_list").and_then(|v| v.as_array()) {
                for it in items {
                    if it.get("type").and_then(|v| v.as_i64()) == Some(ITEM_TEXT) {
                        if let Some(s) = it.pointer("/text_item/text").and_then(|v| v.as_str()) {
                            if !text.is_empty() {
                                text.push('\n');
                            }
                            text.push_str(s);
                        }
                    } else if let Some(mi) = extract_media(it) {
                        media.push(mi);
                    }
                }
            }
            exts.push(MsgExt {
                from,
                ctx,
                text,
                media,
            });
        }
    }

    // 配对/断点落盘
    if exts.iter().any(|e| !e.ctx.is_empty() || !e.text.is_empty()) || !new_buf.is_empty() {
        let mut cfg = load_config()?;
        for e in &exts {
            if let Some(p) = cfg.peers.get_mut(&e.from) {
                if !e.ctx.is_empty() {
                    p.context_token = e.ctx.clone();
                }
                if !e.text.is_empty() {
                    p.last_text = truncate(&e.text, 50).to_string();
                }
                p.updated_at = now_ms();
            } else {
                cfg.peers.insert(
                    e.from.clone(),
                    WeixinPeer {
                        context_token: e.ctx.clone(),
                        updated_at: now_ms(),
                        last_text: truncate(&e.text, 50).to_string(),
                    },
                );
            }
        }
        if !new_buf.is_empty() {
            cfg.sync_buf = new_buf.clone();
        }
        if let Err(e) = persist(&cfg) {
            tracing::warn!("[WEIXIN] 状态落盘失败: {e}");
        }
    }

    // 媒体下载任务：不阻塞长轮询，完成后各自进收件箱/发事件
    for e in &exts {
        for mi in &e.media {
            let from = e.from.clone();
            let desc = media_desc(mi.kind, mi.file_name.as_deref(), None);
            let mi = InboundMedia {
                kind: mi.kind,
                query_param: mi.query_param.clone(),
                full_url: mi.full_url.clone(),
                aes_key_b64: mi.aes_key_b64.clone(),
                image_aes_hex: mi.image_aes_hex.clone(),
                file_name: mi.file_name.clone(),
            };
            tokio::spawn(async move {
                match download_media(&mi).await {
                    Ok((path, size)) => {
                        tracing::info!("[WEIXIN] 媒体已接收 {}（{} 字节）", path.display(), size);
                        push_inbound(WxInboundMsg {
                            at_ms: now_ms(),
                            from,
                            kind: mi.kind,
                            text: desc,
                            file_name: mi.file_name.clone(),
                            size: Some(size),
                            saved_path: Some(path.to_string_lossy().to_string()),
                            error: None,
                        });
                    }
                    Err(e) => {
                        tracing::warn!("[WEIXIN] 媒体接收失败: {e}");
                        push_inbound(WxInboundMsg {
                            at_ms: now_ms(),
                            from,
                            kind: mi.kind,
                            text: desc,
                            file_name: mi.file_name.clone(),
                            size: None,
                            saved_path: None,
                            error: Some(e),
                        });
                    }
                }
            });
        }
    }

    // 文本入站：纯媒体消息由下载任务推送，这里推文本（有字）或空文本占位
    for e in exts {
        if e.media.is_empty() || !e.text.is_empty() {
            push_inbound(WxInboundMsg {
                at_ms: now_ms(),
                from: e.from,
                kind: "text",
                text: e.text,
                file_name: None,
                size: None,
                saved_path: None,
                error: None,
            });
        }
    }

    Ok(if new_buf.is_empty() {
        buf.to_string()
    } else {
        new_buf
    })
}

async fn poll_loop() {
    let mut buf = load_config().map(|c| c.sync_buf).unwrap_or_default();
    let mut consecutive_failures: u32 = 0;
    tracing::info!(
        "[WEIXIN] 长轮询启动（{}）",
        if buf.is_empty() {
            "无历史 buf"
        } else {
            "沿用历史 buf"
        }
    );

    loop {
        // 冷却中：每 10s 检查一次，换新 token 立即恢复
        {
            let paused = runtime()
                .inner
                .lock()
                .map(|g| g.paused_until_ms > now_ms())
                .unwrap_or(false);
            if paused {
                let token_changed = load_config()
                    .map(|c| {
                        runtime()
                            .inner
                            .lock()
                            .map(|g| g.paused_token.as_deref() != Some(c.token.as_str()))
                            .unwrap_or(false)
                    })
                    .unwrap_or(false);
                if token_changed {
                    if let Ok(mut g) = runtime().inner.lock() {
                        g.paused_until_ms = 0;
                        g.paused_token = None;
                    }
                    tracing::info!("[WEIXIN] 检测到新凭证，解除冷却");
                    continue;
                }
                tokio::time::sleep(Duration::from_secs(10)).await;
                continue;
            }
        }

        match poll_once(&buf).await {
            Ok(new_buf) => {
                buf = new_buf;
                consecutive_failures = 0;
            }
            Err(e) => {
                consecutive_failures += 1;
                tracing::warn!("[WEIXIN] getupdates 失败 ({consecutive_failures}/5): {e}");
                if let Ok(mut g) = runtime().inner.lock() {
                    g.last_error = Some(truncate(&e, 200).to_string());
                }
                let delay = if consecutive_failures >= 5 {
                    consecutive_failures = 0;
                    Duration::from_secs(30)
                } else {
                    Duration::from_secs(2)
                };
                tokio::time::sleep(delay).await;
            }
        }
    }
}

// ── 发送 ──

/// 发送上下文：目标 + 配对 token + 客户端
struct SendCtx {
    cfg: WeixinConfig,
    target: String,
    context_token: Option<String>,
    base: String,
}

async fn send_ctx(to: Option<&str>) -> Result<SendCtx, String> {
    let cfg = load_config()?;
    if !cfg.logged_in() {
        return Err("微信未登录：先在 yunshu-cli 里执行 wx login 扫码".into());
    }
    // 冷却检查（换新 token 自动解除）
    {
        let (paused, paused_token) = runtime()
            .inner
            .lock()
            .map(|g| (g.paused_until_ms > now_ms(), g.paused_token.clone()))
            .unwrap_or((false, None));
        if paused {
            if paused_token.as_deref() != Some(cfg.token.trim()) {
                if let Ok(mut g) = runtime().inner.lock() {
                    g.paused_until_ms = 0;
                    g.paused_token = None;
                }
            } else {
                let min = runtime()
                    .inner
                    .lock()
                    .map(|g| g.paused_until_ms.saturating_sub(now_ms()) / 60_000 + 1)
                    .unwrap_or(60);
                return Err(format!(
                    "微信会话冷却中（token 失效），约 {min} 分钟后重试或重新扫码登录"
                ));
            }
        }
    }
    let target = to
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(String::from)
        .or_else(|| default_target(&cfg))
        .ok_or("没有可用的发送目标：请先在微信里给机器人发一条消息完成配对")?;
    Ok(SendCtx {
        context_token: cfg.peers.get(&target).map(|p| p.context_token.clone()),
        base: cfg.effective_base_url(),
        cfg,
        target,
    })
}

fn default_target(cfg: &WeixinConfig) -> Option<String> {
    if !cfg.user_id.trim().is_empty() {
        return Some(cfg.user_id.trim().to_string());
    }
    // 回退到最近活跃的配对用户
    cfg.peers
        .iter()
        .max_by_key(|(_, p)| p.updated_at)
        .map(|(k, _)| k.clone())
}

/// 发送一条单项消息（sendmessage），处理 -14 冷却
async fn send_item(ctx: &SendCtx, item: Value) -> Result<(), String> {
    let body = serde_json::json!({
        "msg": {
            "from_user_id": "",
            "to_user_id": ctx.target,
            "client_id": new_client_id(),
            "message_type": 2,
            "message_state": 2,
            "item_list": [item],
            "context_token": ctx.context_token,
        },
        "base_info": base_info(),
    });
    let client = api_client()?;
    let url = format!("{}/ilink/bot/sendmessage", ctx.base.trim_end_matches('/'));
    let resp = client
        .post(&url)
        .headers(headers(Some(&ctx.cfg.token))?)
        .json(&body)
        .send()
        .await
        .map_err(|e| format!("sendMessage 请求失败: {e}"))?;
    let status = resp.status();
    let text = resp
        .text()
        .await
        .map_err(|e| format!("sendMessage 读取失败: {e}"))?;
    if !status.is_success() {
        return Err(format!("sendMessage HTTP {status}: {}", truncate(&text, 200)));
    }
    let v: Value = serde_json::from_str(&text).map_err(|e| format!("sendMessage 响应解析失败: {e}"))?;
    if let Some((code, msg)) = biz_error(&v) {
        if code == STALE_TOKEN_ERRCODE {
            let until = now_ms() + STALE_PAUSE_MS;
            if let Ok(mut g) = runtime().inner.lock() {
                g.paused_until_ms = until;
                g.paused_token = Some(ctx.cfg.token.clone());
                g.last_error = Some("token 失效(errcode -14)，已进入冷却".into());
            }
            return Err("微信 token 已失效（errcode -14）：请重新扫码登录".into());
        }
        return Err(format!("sendMessage errcode={code} errmsg={msg}"));
    }
    Ok(())
}

/// 发送文本到指定用户（None 用默认目标）。返回消息条数。
pub async fn send_text(to: Option<&str>, text: &str) -> Result<usize, String> {
    let ctx = send_ctx(to).await?;
    let mut count = 0usize;
    for chunk in split_text(&strip_markdown(text)) {
        send_item(
            &ctx,
            serde_json::json!({ "type": ITEM_TEXT, "text_item": { "text": chunk } }),
        )
        .await?;
        count += 1;
    }
    Ok(count)
}

/// 按扩展名分流媒体：返回（upload media_type, kind 名）
fn media_class(path: &Path) -> (u32, &'static str) {
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    match ext.as_str() {
        "jpg" | "jpeg" | "png" | "gif" | "webp" | "bmp" | "svg" | "heic" | "heif" | "tiff" => {
            (UPLOAD_IMAGE, "image")
        }
        "mp4" | "mov" | "avi" | "mkv" | "webm" | "m4v" | "flv" | "wmv" => (UPLOAD_VIDEO, "video"),
        _ => (UPLOAD_FILE, "file"),
    }
}

/// CDN 上传产物：下载参数 + 密文大小 + aes_key（base64 编码的 16 字节原始密钥）
struct CdnUpload {
    download_param: String,
    cipher_len: usize,
    aes_key_b64: String,
}

/// 上传到微信 CDN：getuploadurl → AES-128-ECB 加密 → POST → 拿下载参数
async fn upload_to_cdn(ctx: &SendCtx, data: &[u8], media_type: u32) -> Result<CdnUpload, String> {
    let filekey = {
        let b = rand_bytes(16);
        b.iter().map(|x| format!("{x:02x}")).collect::<String>()
    };
    let aes_key: [u8; 16] = rand_bytes(16)
        .try_into()
        .map_err(|_| "随机数长度异常".to_string())?;
    let aes_hex: String = aes_key.iter().map(|x| format!("{x:02x}")).collect();
    let rawsize = data.len() as u64;
    let filesize = aes_padded_size(data.len()) as u64;
    let body = serde_json::json!({
        "filekey": filekey,
        "media_type": media_type,
        "to_user_id": ctx.target,
        "rawsize": rawsize,
        "rawfilemd5": md5_hex(data),
        "filesize": filesize,
        "no_need_thumb": true,
        "aeskey": aes_hex,
        "base_info": base_info(),
    });
    let resp = post_json(
        &ctx.base,
        "ilink/bot/getuploadurl",
        &body,
        Some(&ctx.cfg.token),
        false,
    )
    .await?;
    if let Some((code, msg)) = biz_error(&resp) {
        if code == STALE_TOKEN_ERRCODE {
            return Err("微信 token 已失效（errcode -14）：请重新扫码登录".into());
        }
        return Err(format!("getuploadurl errcode={code} errmsg={msg}"));
    }
    let full_url = resp
        .get("upload_full_url")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string);
    let upload_param = resp
        .get("upload_param")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let url = match (&full_url, upload_param.is_empty()) {
        (Some(u), _) => u.clone(),
        (None, false) => format!(
            "{CDN_BASE_URL}/upload?encrypted_query_param={}&filekey={}",
            urlencode(&upload_param),
            urlencode(&filekey)
        ),
        _ => return Err("getuploadurl 未返回上传地址".into()),
    };
    let ciphertext = aes_ecb_encrypt(data, &aes_key);
    let client = build_client(Some(CDN_TIMEOUT))?;
    // 最多重试 3 次（4xx 是客户端错误，直接放弃）
    let mut last_err = String::new();
    let mut download_param = String::new();
    for attempt in 1..=3u32 {
        match client
            .post(&url)
            .header("Content-Type", "application/octet-stream")
            .body(ciphertext.clone())
            .send()
            .await
        {
            Ok(r) if r.status().is_success() => {
                download_param = r
                    .headers()
                    .get("x-encrypted-param")
                    .and_then(|v| v.to_str().ok())
                    .unwrap_or("")
                    .to_string();
                if download_param.is_empty() {
                    return Err("CDN 响应缺少 x-encrypted-param 头".into());
                }
                last_err.clear();
                break;
            }
            Ok(r) if r.status().as_u16() < 500 => {
                let msg = r
                    .headers()
                    .get("x-error-message")
                    .and_then(|v| v.to_str().ok())
                    .unwrap_or("CDN 拒绝上传")
                    .to_string();
                return Err(format!("CDN 上传被拒绝（HTTP {}）：{msg}", r.status()));
            }
            Ok(r) => last_err = format!("HTTP {}", r.status()),
            Err(e) => last_err = e.to_string(),
        }
        if attempt < 3 {
            tokio::time::sleep(Duration::from_secs(2 * attempt as u64)).await;
        }
    }
    if !last_err.is_empty() {
        return Err(format!("CDN 上传失败：{last_err}"));
    }
    Ok(CdnUpload {
        download_param,
        cipher_len: ciphertext.len(),
        aes_key_b64: base64::engine::general_purpose::STANDARD.encode(aes_key),
    })
}

/// 发送一个本地文件/图片/视频到微信（自动按扩展名分流：图片直接显示，
/// 视频以视频消息，其余以文件附件）。可选说明文字（先发为独立文本）。
/// 返回消息描述。
pub async fn send_file(
    to: Option<&str>,
    path: &Path,
    caption: Option<&str>,
) -> Result<String, String> {
    if !path.exists() {
        return Err(format!("路径不存在：{}", path.display()));
    }
    let ctx = send_ctx(to).await?;
    // 说明文字先发（对齐官方插件：caption 与媒体分两条消息）
    if let Some(c) = caption.map(str::trim).filter(|s| !s.is_empty()) {
        send_text(Some(&ctx.target), c).await?;
    }
    let data = tokio::fs::read(path)
        .await
        .map_err(|e| format!("读取文件失败: {e}"))?;
    if data.is_empty() {
        return Err("文件为空".into());
    }
    if data.len() as u64 > MEDIA_MAX_BYTES {
        return Err(format!(
            "文件超过 {} MiB 上限",
            MEDIA_MAX_BYTES / 1024 / 1024
        ));
    }
    let (media_type, kind) = media_class(path);
    let up = upload_to_cdn(&ctx, &data, media_type).await?;
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| "file.bin".into());
    let media = serde_json::json!({
        "encrypt_query_param": up.download_param,
        "aes_key": up.aes_key_b64,
        "encrypt_type": 1,
    });
    let item = match kind {
        "image" => serde_json::json!({
            "type": ITEM_IMAGE,
            "image_item": { "media": media, "mid_size": up.cipher_len },
        }),
        "video" => serde_json::json!({
            "type": ITEM_VIDEO,
            "video_item": { "media": media, "video_size": up.cipher_len },
        }),
        _ => serde_json::json!({
            "type": ITEM_FILE,
            "file_item": { "media": media, "file_name": name, "len": data.len().to_string() },
        }),
    };
    send_item(&ctx, item).await?;
    Ok(format!(
        "{}（{} 字节）",
        path.file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_default(),
        data.len()
    ))
}

/// 批量发送多个文件（每个独立上传+发送）。返回逐文件结果。
pub async fn send_files(
    to: Option<&str>,
    paths: &[PathBuf],
    caption: Option<&str>,
) -> Vec<(String, Result<String, String>)> {
    let mut results = Vec::new();
    let mut first = true;
    for p in paths {
        let cap = if first { caption.map(str::to_string) } else { None };
        first = false;
        let r = send_file(to, p, cap.as_deref()).await;
        results.push((p.to_string_lossy().to_string(), r));
    }
    results
}

// ── 状态视图 ──

pub fn status_view() -> StatusView {
    let cfg = load_config().unwrap_or_default();
    let g = runtime().inner.lock().ok();
    let now = now_ms();
    StatusView {
        logged_in: cfg.logged_in(),
        bot_id: cfg.bot_id.clone(),
        user_id: cfg.user_id.clone(),
        saved_at: cfg.saved_at,
        polling: g
            .as_ref()
            .map(|g| g.poll_task.as_ref().map(|t| !t.is_finished()).unwrap_or(false))
            .unwrap_or(false),
        paused_remaining_ms: g
            .as_ref()
            .map(|g| g.paused_until_ms.saturating_sub(now))
            .unwrap_or(0),
        last_poll_at: g.as_ref().map(|g| g.last_poll_at).unwrap_or(0),
        last_inbound_at: g.as_ref().map(|g| g.last_inbound_at).unwrap_or(0),
        last_error: g.as_ref().and_then(|g| g.last_error.clone()),
        peers: {
            let mut list: Vec<PeerView> = cfg
                .peers
                .iter()
                .map(|(k, p)| PeerView {
                    user_id: k.clone(),
                    updated_at: p.updated_at,
                    last_text: p.last_text.clone(),
                })
                .collect();
            list.sort_by(|a, b| b.updated_at.cmp(&a.updated_at));
            list
        },
        inbox_count: g.as_ref().map(|g| g.inbox.len()).unwrap_or(0),
        media_dir: media_dir().map(|d| d.to_string_lossy().to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn aes_roundtrip() {
        let key = [7u8; 16];
        for len in [0usize, 1, 15, 16, 17, 100, 1024] {
            let plain: Vec<u8> = (0..len).map(|i| (i % 251) as u8).collect();
            let cipher = aes_ecb_encrypt(&plain, &key);
            assert_eq!(cipher.len(), aes_padded_size(len));
            let back = aes_ecb_decrypt(&cipher, &key).unwrap();
            assert_eq!(back, plain);
        }
    }

    #[test]
    fn aes_key_two_encodings() {
        // base64(原始 16 字节)
        let raw: [u8; 16] = core::array::from_fn(|i| i as u8);
        let b64 = base64::engine::general_purpose::STANDARD.encode(raw);
        assert_eq!(parse_aes_key(&b64).unwrap(), raw);
        // base64(32 位 hex 字符串)
        let hex: String = raw.iter().map(|b| format!("{b:02x}")).collect();
        let b64hex = base64::engine::general_purpose::STANDARD.encode(hex.as_bytes());
        assert_eq!(parse_aes_key(&b64hex).unwrap(), raw);
        assert!(parse_aes_key("aaaa").is_err());
    }

    #[test]
    fn split_text_breaks_and_prefers_newline() {
        let one = "短文本";
        assert_eq!(split_text(one), vec!["短文本".to_string()]);
        let long = "ab\n".repeat(900);
        let chunks = split_text(&long);
        assert!(chunks.len() >= 2);
        for c in &chunks {
            assert!(c.chars().count() <= MAX_TEXT_CHARS);
        }
    }

    #[test]
    fn strip_markdown_basics() {
        assert_eq!(strip_markdown("**加粗** 与 `代码`"), "加粗 与 代码");
        assert_eq!(strip_markdown("# 标题\n- 列表"), "标题\n• 列表");
        assert_eq!(strip_markdown("```\nfn x() {}\n```"), "fn x() {}");
    }

    #[test]
    fn urlencode_keeps_safe_chars() {
        assert_eq!(urlencode("a-b_c.d~e"), "a-b_c.d~e");
        assert_eq!(urlencode("a b"), "a%20b");
        assert_eq!(urlencode("中"), "%E4%B8%AD");
    }

    #[test]
    fn media_class_by_extension() {
        assert_eq!(media_class(Path::new("a.JPG")), (UPLOAD_IMAGE, "image"));
        assert_eq!(media_class(Path::new("b.Mp4")), (UPLOAD_VIDEO, "video"));
        assert_eq!(media_class(Path::new("c.pdf")), (UPLOAD_FILE, "file"));
        assert_eq!(media_class(Path::new("d")), (UPLOAD_FILE, "file"));
    }

    #[test]
    fn sanitize_and_extract() {
        assert_eq!(sanitize_name("a/b\\c:d"), "a_b_c_d");
        // 无媒体字段返回 None
        assert!(extract_media(&serde_json::json!({"type": 4})).is_none());
        let item = serde_json::json!({
            "type": 4,
            "file_item": {
                "media": { "encrypt_query_param": "EQP", "aes_key": "AAAA" },
                "file_name": "报告.pdf",
            },
        });
        let m = extract_media(&item).unwrap();
        assert_eq!(m.kind, "file");
        assert_eq!(m.query_param, "EQP");
        assert_eq!(m.file_name.as_deref(), Some("报告.pdf"));
    }
}
