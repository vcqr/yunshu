//! 远程控制（PC↔PC 屏幕共享 + 键鼠注入）。
//!
//! 传输复用飞鸽的加密直连方案：同端口新路径 /remote-ws，独立 Noise XX 会话
//! （与文件链路隔离，视频流不会挤占文件推送队列）。明文帧前缀：
//! 'C' 控制 JSON（RemoteWire）/ 'V' 视频帧分块（seq + idx + total + JPEG 数据）。
//! Noise 单条消息上限 64KB，视频帧按 60KB 分块，接收端按 seq 重组。
//!
//! 认证对齐 RustDesk 习惯：密码正确 = 授权完成，不走指纹确认弹窗（无人值守前提）。
//! 密码三种模式：临时密码（每次启动随机生成）/ 固定密码（pbkdf2 落盘）/ 一次性密码
//! （反向协助现场生成，用后即焚）。错误 5 次锁 10 分钟（IP 维度）。
//!
//! 采集：xcap 主屏截屏 → 盒滤波缩放 → jpeg-encoder（约 20fps，画面不变跳帧）。
//! 注入：enigo 独立线程（Enigo 不保证 Send，生命周期与会话绑定）。
//! 坐标协议：输入事件一律传 [0,1] 归一化坐标，受控端映射到 enigo 绝对坐标空间
//! （macOS 为逻辑点，Windows 为物理像素），天然规避 DPI/分辨率差异。

use crate::server::AppState;
use crate::{CoreEvent, UiRequest};
use futures_util::{SinkExt, StreamExt};
use snow::TransportState;
use std::collections::HashMap;
use std::net::IpAddr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tokio::net::TcpStream;

pub const REMOTE_WS_PATH: &str = "/remote-ws";
pub const VIEW_WS_PATH: &str = "/remote-view-ws";
/// Noise 明文上限 65535，视频分块留足余量（对齐飞鸽文件块）
const CHUNK_SIZE: usize = 60_000;
/// 密码错误锁定阈值与时长
const MAX_FAILS: u32 = 5;
const LOCK_MINS: u64 = 10;
/// 一次性密码有效期
const ONETIME_TTL: Duration = Duration::from_secs(300);
const ONETIME_PW_LEN: usize = 8;
/// 剪贴板同步文本上限（单条 Noise 消息内放得下）
const CLIPBOARD_MAX: usize = 40_000;

/// 测试模式（合成帧源，不碰真实屏幕/剪贴板/输入）
pub fn fake_screen_enabled() -> bool {
    std::env::var("YUNSHU_FAKE_SCREEN").is_ok_and(|v| v == "1")
}

// ---------- 协议 ----------

/// 受控端屏幕信息（采集分辨率 + enigo 绝对坐标空间）
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ScreenInfo {
    pub width: u32,
    pub height: u32,
    pub logical_w: u32,
    pub logical_h: u32,
}

/// 远程控制线上消息（加密通道内的 'C' JSON，两端共用）
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum RemoteWire {
    /// 控制端 → 受控端认证。mode: "temp" | "fixed" | "onetime"
    RemoteAuth {
        mode: String,
        password: String,
        name: String,
        machine_id: String,
    },
    /// 受控端 → 控制端认证结果（第二次应答附屏幕信息）
    RemoteAuthResult {
        ok: bool,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        reason: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pc_name: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        screen: Option<ScreenInfo>,
    },
    /// 键鼠输入（归一化坐标）
    RemoteInput { ev: InputEvent },
    /// 剪贴板同步
    RemoteClipboard { text: String },
    /// 控制端 → 受控端：设置对方剪贴板并注入平台粘贴组合键
    RemotePaste { text: String },
    /// 控制端调画质（max_width 缩放上限 / quality JPEG 质量）
    RemoteQuality { max_width: u32, quality: u8 },
    /// 任一方结束会话
    RemoteEnd {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        reason: Option<String>,
    },
}

/// 键鼠输入事件。坐标一律 [0,1] 归一化（相对采集画面）；key 为字符或键名
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(tag = "t", rename_all = "snake_case")]
pub enum InputEvent {
    MouseMove {
        x: f64,
        y: f64,
    },
    MouseDown {
        btn: u8,
    },
    MouseUp {
        btn: u8,
    },
    /// 垂直滚动：正值向下（浏览器 deltaY 语义），受控端换算成滚轮格数
    Wheel {
        dy: f64,
    },
    KeyDown {
        key: String,
    },
    KeyUp {
        key: String,
    },
}

/// 出站明文消息（统一加密后发送）
#[derive(Debug, Clone)]
pub enum RemoteOut {
    Control(RemoteWire),
    FrameChunk {
        seq: u32,
        idx: u16,
        total: u16,
        data: Vec<u8>,
    },
}

/// 入站明文消息
#[derive(Debug, Clone)]
pub enum RemoteIn {
    Control(RemoteWire),
    FrameChunk {
        seq: u32,
        idx: u16,
        total: u16,
        data: Vec<u8>,
    },
}

fn seal_plain(noise: &Mutex<TransportState>, plain: &[u8]) -> Result<Vec<u8>, String> {
    let mut buf = vec![0u8; plain.len() + 64];
    let n = noise
        .lock()
        .unwrap()
        .write_message(plain, &mut buf)
        .map_err(|e| e.to_string())?;
    buf.truncate(n);
    Ok(buf)
}

pub fn seal_msg(noise: &Mutex<TransportState>, msg: &RemoteOut) -> Result<Vec<u8>, String> {
    let mut plain = Vec::with_capacity(64 * 1024);
    match msg {
        RemoteOut::Control(w) => {
            plain.push(b'C');
            serde_json::to_writer(&mut plain, w).map_err(|e| e.to_string())?;
        }
        RemoteOut::FrameChunk {
            seq,
            idx,
            total,
            data,
        } => {
            plain.push(b'V');
            plain.extend_from_slice(&seq.to_le_bytes());
            plain.extend_from_slice(&idx.to_le_bytes());
            plain.extend_from_slice(&total.to_le_bytes());
            plain.extend_from_slice(data);
        }
    }
    seal_plain(noise, &plain)
}

pub fn open_msg(noise: &Mutex<TransportState>, cipher: &[u8]) -> Result<RemoteIn, String> {
    let mut buf = vec![0u8; cipher.len()];
    let n = noise
        .lock()
        .unwrap()
        .read_message(cipher, &mut buf)
        .map_err(|e| e.to_string())?;
    match buf.first() {
        Some(b'C') => Ok(RemoteIn::Control(
            serde_json::from_slice(&buf[1..n]).map_err(|e| e.to_string())?,
        )),
        Some(b'V') if n >= 9 => {
            let seq = u32::from_le_bytes(buf[1..5].try_into().unwrap());
            let idx = u16::from_le_bytes(buf[5..7].try_into().unwrap());
            let total = u16::from_le_bytes(buf[7..9].try_into().unwrap());
            Ok(RemoteIn::FrameChunk {
                seq,
                idx,
                total,
                data: buf[9..n].to_vec(),
            })
        }
        _ => Err("未知消息前缀".into()),
    }
}

/// 视频帧分块重组：发送端可能因链路拥塞整帧丢弃，收到新 seq 的 idx=0 即重建缓冲，
/// 未完成的旧帧随之作废。
#[derive(Default)]
struct FrameAssembler {
    seq: u32,
    total: u16,
    buf: Vec<u8>,
    got: Vec<bool>,
}
impl FrameAssembler {
    fn feed(&mut self, seq: u32, idx: u16, total: u16, data: &[u8]) -> Option<Vec<u8>> {
        if idx == 0 {
            self.seq = seq;
            self.total = total;
            self.buf = Vec::with_capacity(total as usize * CHUNK_SIZE);
            self.got = vec![false; total as usize];
        } else if seq != self.seq || total != self.total {
            return None; // 上一帧的残尾，丢弃
        }
        let i = idx as usize;
        if i >= self.got.len() || self.got[i] {
            return None; // 越界或重复块
        }
        self.got[i] = true;
        self.buf.extend_from_slice(data);
        (self.got.iter().all(|&g| g)).then(|| std::mem::take(&mut self.buf))
    }
}

// ---------- 机器 ID 与密码 ----------

/// 公开机器 ID：静态密钥 SHA-256 指纹前 8 位 hex（16⁸≈43 亿，局域网无碰撞之忧）。
/// 随 UDP 广播包分发，输入对方 ID 即可从发现表定位 ip:port。
pub fn machine_id() -> String {
    use sha2::Digest;
    let mut h = sha2::Sha256::new();
    h.update(crate::pigeon::load_machine_key());
    crate::pigeon::sha_hex(&h.finalize())[..8].to_string()
}

/// 临时密码：6 位数字，每次启动重新生成
fn gen_temp_password() -> String {
    format!("{:06}", rand::random::<u32>() % 1_000_000)
}

/// 一次性密码：8 位无易混淆字符（无 0/O/1/I/L）
pub fn gen_onetime_password() -> String {
    const SET: &[u8] = b"23456789ABCDEFGHJKMNPQRSTUVWXYZ";
    (0..ONETIME_PW_LEN)
        .map(|_| SET[rand::random::<u32>() as usize % SET.len()] as char)
        .collect()
}

fn hex_enc(b: &[u8]) -> String {
    crate::pigeon::sha_hex(b)
}

fn hex_dec(s: &str) -> Option<Vec<u8>> {
    (0..s.len() / 2)
        .map(|i| u8::from_str_radix(&s[i * 2..i * 2 + 2], 16).ok())
        .collect()
}

/// pbkdf2-sha256 加盐哈希。格式：pbkdf2-sha256$<rounds>$<salt hex>$<hash hex>
pub fn hash_password(password: &str) -> String {
    const ROUNDS: u32 = 210_000;
    let salt: Vec<u8> = (0..16).map(|_| rand::random::<u8>()).collect();
    let hash = pbkdf2::pbkdf2_hmac_array::<sha2::Sha256, 32>(password.as_bytes(), &salt, ROUNDS);
    format!(
        "pbkdf2-sha256${ROUNDS}${}${}",
        hex_enc(&salt),
        hex_enc(&hash)
    )
}

fn verify_password_hash(password: &str, stored: &str) -> bool {
    let parts: Vec<&str> = stored.split('$').collect();
    if parts.len() != 4 || parts[0] != "pbkdf2-sha256" {
        return false;
    }
    let (Ok(rounds), Some(salt), Some(expect)) = (
        parts[1].parse::<u32>(),
        hex_dec(parts[2]),
        hex_dec(parts[3]),
    ) else {
        return false;
    };
    let actual = pbkdf2::pbkdf2_hmac_array::<sha2::Sha256, 32>(password.as_bytes(), &salt, rounds);
    constant_eq(&actual, &expect)
}

/// 恒时比较（LAN 场景侧信道价值有限，成本为零就顺手做了）
fn constant_eq(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len() && a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

/// 密码三种来源统一校验器 + 防爆破锁定
pub struct RemoteAuthState {
    temp_password: String,
    fixed_hash: Option<String>,
    one_time: Option<(String, Instant)>,
    failures: HashMap<IpAddr, FailState>,
}

#[derive(Default)]
struct FailState {
    count: u32,
    locked_until: Option<Instant>,
}

#[derive(Debug, PartialEq, Eq)]
pub enum AuthError {
    /// 锁定中，剩余秒数
    Locked(u64),
    Wrong,
}

impl RemoteAuthState {
    pub fn new(fixed_hash: Option<String>) -> Self {
        RemoteAuthState {
            temp_password: gen_temp_password(),
            fixed_hash,
            one_time: None,
            failures: HashMap::new(),
        }
    }

    pub fn temp_password(&self) -> &str {
        &self.temp_password
    }

    /// 重新生成临时密码
    pub fn regenerate_temp(&mut self) -> String {
        self.temp_password = gen_temp_password();
        self.temp_password.clone()
    }

    pub fn set_fixed_hash(&mut self, hash: Option<String>) {
        self.fixed_hash = hash;
    }

    pub fn has_fixed(&self) -> bool {
        self.fixed_hash.is_some()
    }

    pub fn set_one_time(&mut self, password: String) {
        self.one_time = Some((password, Instant::now()));
    }

    /// 校验密码。返回 Ok(是否一次性密码)。
    pub fn verify(&mut self, ip: IpAddr, password: &str) -> Result<bool, AuthError> {
        if let Some(until) = self.failures.get(&ip).and_then(|f| f.locked_until)
            && let Some(left) = until.checked_duration_since(Instant::now())
        {
            return Err(AuthError::Locked(left.as_secs().max(1)));
        }

        // 用后即焚：一次性密码只消耗一次，过期作废
        let one_time_hit = self
            .one_time
            .take_if(|(pw, t)| pw == password && t.elapsed() < ONETIME_TTL)
            .is_some();

        let temp_hit = constant_eq(password.as_bytes(), self.temp_password.as_bytes());
        let fixed_hit = self
            .fixed_hash
            .as_deref()
            .is_some_and(|h| verify_password_hash(password, h));

        if one_time_hit || temp_hit || fixed_hit {
            self.failures.remove(&ip);
            Ok(one_time_hit)
        } else {
            let f = self.failures.entry(ip).or_default();
            f.count += 1;
            if f.count >= MAX_FAILS {
                f.locked_until = Some(Instant::now() + Duration::from_secs(LOCK_MINS * 60));
                f.count = 0;
                tracing::warn!("远程控制：IP {ip} 连续输错 {MAX_FAILS} 次，锁定 {LOCK_MINS} 分钟");
            }
            Err(AuthError::Wrong)
        }
    }
}

// ---------- 画质参数 ----------

#[derive(Debug, Clone, Copy)]
pub struct QualityParams {
    /// 采集画面最长边缩放上限（0 = 原始分辨率）
    pub max_width: u32,
    pub quality: u8,
    /// 目标帧间隔
    pub interval_ms: u64,
}

impl Default for QualityParams {
    fn default() -> Self {
        QualityParams {
            max_width: 1920,
            quality: 70,
            interval_ms: 50,
        }
    }
}

// ---------- 屏幕采集 ----------

pub struct Frame {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
}

/// 屏幕源抽象：真实采集用 xcap，测试注入合成帧（为二期硬编/多显示器留路）
pub trait ScreenSource: Send {
    /// enigo 绝对坐标空间的屏幕边界（macOS 逻辑点 / Windows 物理像素）
    fn input_bounds(&self) -> (u32, u32);
    fn capture(&mut self) -> Option<Frame>;
}

pub struct XcapSource {
    monitor: xcap::Monitor,
}

impl XcapSource {
    /// 主屏；取不到主屏时退回第一个显示器
    pub fn new() -> Option<Self> {
        let monitors = xcap::Monitor::all().ok()?;
        let monitor = monitors
            .iter()
            .find(|m| m.is_primary().unwrap_or(false))
            .or_else(|| monitors.first())?
            .clone();
        Some(XcapSource { monitor })
    }
}

impl ScreenSource for XcapSource {
    fn input_bounds(&self) -> (u32, u32) {
        match (self.monitor.width(), self.monitor.height()) {
            (Ok(w), Ok(h)) if w > 0 && h > 0 => (w, h),
            _ => (0, 0),
        }
    }

    fn capture(&mut self) -> Option<Frame> {
        let img = self.monitor.capture_image().ok()?;
        Some(Frame {
            width: img.width(),
            height: img.height(),
            rgba: img.into_raw(),
        })
    }
}

/// 测试/无屏环境：动画渐变帧（内容随时间变化，保证不跳帧）
pub struct FakeSource {
    pub width: u32,
    pub height: u32,
    t: u32,
}

impl FakeSource {
    pub fn new() -> Self {
        FakeSource {
            width: 320,
            height: 200,
            t: 0,
        }
    }
}

impl Default for FakeSource {
    fn default() -> Self {
        Self::new()
    }
}

impl ScreenSource for FakeSource {
    fn input_bounds(&self) -> (u32, u32) {
        (self.width, self.height)
    }
    fn capture(&mut self) -> Option<Frame> {
        self.t = self.t.wrapping_add(1);
        let mut rgba = vec![0u8; (self.width * self.height * 4) as usize];
        for (i, px) in rgba.as_chunks_mut::<4>().0.iter_mut().enumerate() {
            let x = (i % self.width as usize) as u32;
            px[0] = (x + self.t * 7) as u8;
            px[1] = (self.t * 3) as u8;
            px[2] = 128;
            px[3] = 255;
        }
        Some(Frame {
            width: self.width,
            height: self.height,
            rgba,
        })
    }
}

/// 环境变量 YUNSHU_FAKE_SCREEN=1 时用合成帧源（集成测试 / 无显示器调试）
pub fn make_screen_source() -> Result<Box<dyn ScreenSource>, String> {
    if fake_screen_enabled() {
        return Ok(Box::new(FakeSource::new()));
    }
    XcapSource::new()
        .map(|s| Box::new(s) as Box<dyn ScreenSource>)
        .ok_or_else(|| "无法枚举显示器".into())
}

/// 盒滤波缩放（目标为最长边上限；不超上限原样返回）
pub fn scale_frame(src: &Frame, max_side: u32) -> Frame {
    let longest = src.width.max(src.height);
    if max_side == 0 || longest <= max_side {
        return Frame {
            width: src.width,
            height: src.height,
            rgba: src.rgba.clone(),
        };
    }
    let ratio = max_side as f64 / longest as f64;
    let tw = ((src.width as f64 * ratio).round() as u32).max(1);
    let th = ((src.height as f64 * ratio).round() as u32).max(1);
    let mut out = vec![0u8; (tw * th * 4) as usize];
    let (sw, sh) = (src.width as usize, src.height as usize);
    for y in 0..th as usize {
        let sy0 = y * sh / th as usize;
        let sy1 = ((y + 1) * sh / th as usize).max(sy0 + 1).min(sh);
        for x in 0..tw as usize {
            let sx0 = x * sw / tw as usize;
            let sx1 = ((x + 1) * sw / tw as usize).max(sx0 + 1).min(sw);
            let (mut r, mut g, mut b) = (0u32, 0u32, 0u32);
            for sy in sy0..sy1 {
                for sx in sx0..sx1 {
                    let o = (sy * sw + sx) * 4;
                    r += src.rgba[o] as u32;
                    g += src.rgba[o + 1] as u32;
                    b += src.rgba[o + 2] as u32;
                }
            }
            let n = ((sy1 - sy0) * (sx1 - sx0)) as u32;
            let o = (y * tw as usize + x) * 4;
            out[o] = (r / n) as u8;
            out[o + 1] = (g / n) as u8;
            out[o + 2] = (b / n) as u8;
            out[o + 3] = 255;
        }
    }
    Frame {
        width: tw,
        height: th,
        rgba: out,
    }
}

/// 内容指纹（跳帧：画面没变就不编码不发送）
fn fnv_hash(data: &[u8]) -> u64 {
    let mut h: u64 = 0xcbf29ce484222325;
    for &b in data {
        h ^= b as u64;
        h = h.wrapping_mul(0x100000001b3);
    }
    h
}

pub fn encode_jpeg(frame: &Frame, quality: u8) -> Option<Vec<u8>> {
    let mut out = Vec::with_capacity(frame.rgba.len() / 6);
    jpeg_encoder::Encoder::new(&mut out, quality.clamp(30, 95))
        .encode(
            &frame.rgba,
            frame.width.min(u16::MAX as u32) as u16,
            frame.height.min(u16::MAX as u32) as u16,
            jpeg_encoder::ColorType::Rgba,
        )
        .ok()?;
    Some(out)
}

// ---------- 输入注入 ----------

enum InputCmd {
    Move { x: f64, y: f64 },
    Button { btn: u8, down: bool },
    Wheel { dy: f64 },
    Key { name: String, down: bool },
    Paste { text: String },
}

/// 键名 → enigo::Key（单字符走 Unicode，按受控端本机布局解析）
fn map_key(name: &str) -> Option<enigo::Key> {
    use enigo::Key as K;
    Some(match name {
        "Enter" => K::Return,
        "Backspace" => K::Backspace,
        "Delete" => K::Delete,
        "Tab" => K::Tab,
        "Escape" => K::Escape,
        "ArrowUp" => K::UpArrow,
        "ArrowDown" => K::DownArrow,
        "ArrowLeft" => K::LeftArrow,
        "ArrowRight" => K::RightArrow,
        "Home" => K::Home,
        "End" => K::End,
        "PageUp" => K::PageUp,
        "PageDown" => K::PageDown,
        "CapsLock" => K::CapsLock,
        "ControlLeft" | "ControlRight" => K::Control,
        "ShiftLeft" | "ShiftRight" => K::Shift,
        "AltLeft" | "AltRight" => K::Alt,
        "MetaLeft" | "MetaRight" => K::Meta,
        "F1" => K::F1,
        "F2" => K::F2,
        "F3" => K::F3,
        "F4" => K::F4,
        "F5" => K::F5,
        "F6" => K::F6,
        "F7" => K::F7,
        "F8" => K::F8,
        "F9" => K::F9,
        "F10" => K::F10,
        "F11" => K::F11,
        "F12" => K::F12,
        s if s.chars().count() == 1 => K::Unicode(s.chars().next()?),
        _ => return None,
    })
}

fn map_button(btn: u8) -> enigo::Button {
    match btn {
        1 => enigo::Button::Middle,
        2 => enigo::Button::Right,
        3 => enigo::Button::Back,
        4 => enigo::Button::Forward,
        _ => enigo::Button::Left,
    }
}

/// 注入线程：Enigo 生命周期绑定在线程内（跨平台 Send 保证不齐），会话结束随 Sender drop 退出
fn spawn_input_thread(bounds: (u32, u32)) -> std::sync::mpsc::Sender<InputCmd> {
    let (tx, rx) = std::sync::mpsc::channel::<InputCmd>();
    let fake = fake_screen_enabled();
    std::thread::Builder::new()
        .name("remote-input".into())
        .spawn(move || {
            use enigo::{Keyboard, Mouse};
            if fake {
                for _ in rx {} // 测试模式：只消费不注入
                return;
            }
            let mut enigo = match enigo::Enigo::new(&enigo::Settings::default()) {
                Ok(e) => e,
                Err(e) => {
                    tracing::error!("enigo 初始化失败（macOS 需辅助功能权限）: {e}");
                    return;
                }
            };
            let (bw, bh) = (bounds.0.max(1) as f64, bounds.1.max(1) as f64);
            for cmd in rx {
                let r: Result<(), _> = match cmd {
                    InputCmd::Move { x, y } => {
                        let px = (x.clamp(0.0, 1.0) * bw) as i32;
                        let py = (y.clamp(0.0, 1.0) * bh) as i32;
                        enigo.move_mouse(px, py, enigo::Coordinate::Abs)
                    }
                    InputCmd::Button { btn, down } => {
                        let dir = if down {
                            enigo::Direction::Press
                        } else {
                            enigo::Direction::Release
                        };
                        enigo.button(map_button(btn), dir)
                    }
                    InputCmd::Wheel { dy } => {
                        // 浏览器一格约 100px，换算成滚轮格数并限幅
                        let clicks = (-dy / 100.0).round().clamp(-10.0, 10.0) as i32;
                        if clicks != 0 {
                            enigo.scroll(clicks, enigo::Axis::Vertical)
                        } else {
                            Ok(())
                        }
                    }
                    InputCmd::Key { name, down } => match map_key(&name) {
                        Some(k) => {
                            let dir = if down {
                                enigo::Direction::Press
                            } else {
                                enigo::Direction::Release
                            };
                            enigo.key(k, dir)
                        }
                        None => Ok(()),
                    },
                    InputCmd::Paste { text } => {
                        crate::clipboard::copy_text(&text);
                        // 注入平台粘贴组合键（macOS Cmd+V，其余 Ctrl+V）
                        let (modifier, v) = if cfg!(target_os = "macos") {
                            (enigo::Key::Meta, enigo::Key::Unicode('v'))
                        } else {
                            (enigo::Key::Control, enigo::Key::Unicode('v'))
                        };
                        let _ = enigo.key(modifier, enigo::Direction::Press);
                        let r = enigo.key(v, enigo::Direction::Click);
                        let _ = enigo.key(modifier, enigo::Direction::Release);
                        r
                    }
                };
                if let Err(e) = r {
                    tracing::debug!("输入注入失败: {e}");
                }
            }
        })
        .expect("注入线程启动失败");
    tx
}

// ---------- 会话状态（AppState 持有） ----------

/// 受控端活跃会话
pub struct ControlledSession {
    /// 控制方机器 ID（认证时自报）
    pub peer_id: String,
    pub peer_name: String,
    pub killed: Arc<AtomicBool>,
}

/// 控制端观看会话（本地观看桥的接入凭证与转发通道）
pub struct ViewingSession {
    pub peer_id: String,
    pub name: String,
    pub token: String,
    pub closed: Arc<AtomicBool>,
    pub out: tokio::sync::mpsc::Sender<RemoteOut>,
    pub bridge: tokio::sync::broadcast::Sender<BridgeMsg>,
}

/// 本地观看桥消息：帧（二进制）或 JSON（文本）
#[derive(Debug, Clone)]
pub enum BridgeMsg {
    Frame(Vec<u8>),
    Json(String),
}

/// 远程控制共享状态（AppState.remote）
pub struct RemoteState {
    pub auth: RemoteAuthState,
    pub controlled: Option<ControlledSession>,
    pub viewing: Option<ViewingSession>,
    /// 反向协助：待处理的请求方（飞鸽 peer_id）
    pub assist_pending: Option<String>,
}

impl RemoteState {
    pub fn new(fixed_hash: Option<String>) -> Self {
        RemoteState {
            auth: RemoteAuthState::new(fixed_hash),
            controlled: None,
            viewing: None,
            assist_pending: None,
        }
    }
}

// ---------- 受控端（/remote-ws responder） ----------

type AxumSink =
    futures_util::stream::SplitSink<axum::extract::ws::WebSocket, axum::extract::ws::Message>;
type AxumStream = futures_util::stream::SplitStream<axum::extract::ws::WebSocket>;

/// Noise XX 握手（本端是 responder）。成功返回传输态与对方静态公钥。
async fn noise_handshake_responder(
    sink: &mut AxumSink,
    stream: &mut AxumStream,
) -> Result<(Arc<Mutex<TransportState>>, Vec<u8>), String> {
    use axum::extract::ws::Message;
    let mut hs = crate::pigeon::new_handshake(false)?;
    let mut buf = vec![0u8; 65535];
    let m1 = tokio::time::timeout(Duration::from_secs(10), stream.next())
        .await
        .map_err(|_| "握手超时".to_string())?
        .ok_or("对方断开")?
        .map_err(|e| e.to_string())?;
    let Message::Binary(m1) = m1 else {
        return Err("握手消息格式错误".into());
    };
    hs.read_message(&m1, &mut buf).map_err(|e| e.to_string())?;
    let mut m2 = vec![0u8; 65535];
    let n2 = hs.write_message(&[], &mut m2).map_err(|e| e.to_string())?;
    sink.send(Message::Binary(m2[..n2].to_vec().into()))
        .await
        .map_err(|e| e.to_string())?;
    let m3 = tokio::time::timeout(Duration::from_secs(10), stream.next())
        .await
        .map_err(|_| "握手超时".to_string())?
        .ok_or("对方断开")?
        .map_err(|e| e.to_string())?;
    let Message::Binary(m3) = m3 else {
        return Err("握手消息格式错误".into());
    };
    hs.read_message(&m3, &mut buf).map_err(|e| e.to_string())?;
    let remote_static = {
        // into_transport_mode 前取远程静态公钥
        hs.get_remote_static()
            .map(|s| s.to_vec())
            .unwrap_or_default()
    };
    let transport = hs.into_transport_mode().map_err(|e| e.to_string())?;
    Ok((Arc::new(Mutex::new(transport)), remote_static))
}

/// 会话收尾：置 killed、解锁状态位、通知 UI
fn end_session(
    st: &Arc<AppState>,
    killed: Arc<AtomicBool>,
    peer_id: String,
    peer_name: String,
    reason: Option<String>,
) {
    killed.store(true, Ordering::Relaxed);
    st.remote.lock().unwrap().controlled = None;
    st.emit(CoreEvent::RemoteSessionEnded {
        peer_id,
        name: peer_name,
        side: "controlled".into(),
        reason,
    });
}

pub async fn handle_remote_socket(
    st: Arc<AppState>,
    peer_ip: IpAddr,
    socket: axum::extract::ws::WebSocket,
) {
    use axum::extract::ws::Message;

    let (mut sink, mut stream) = socket.split();
    let (noise, remote_static) = match noise_handshake_responder(&mut sink, &mut stream).await {
        Ok(v) => v,
        Err(e) => {
            tracing::debug!("远程控制握手失败: {e}");
            return;
        }
    };

    // ---- 认证（一条连接最多 3 次尝试，锁定状态直接拒绝）----
    let mut authed = false;
    let mut one_time_used = false;
    let mut peer_name = "电脑".to_string();
    let mut controller_machine = String::new();
    for _ in 0..3 {
        let msg = match tokio::time::timeout(Duration::from_secs(15), stream.next()).await {
            Ok(Some(Ok(Message::Binary(b)))) => b,
            _ => return,
        };
        let wire = match open_msg(&noise, &msg) {
            Ok(RemoteIn::Control(w)) => w,
            _ => return,
        };
        let RemoteWire::RemoteAuth {
            mode: _,
            password,
            name,
            machine_id,
        } = wire
        else {
            return;
        };
        peer_name = if name.trim().is_empty() {
            "电脑".into()
        } else {
            name.trim().to_string()
        };
        controller_machine = machine_id;
        // 防打扰：未信任设备凭临时/固定密码连入 → 拒绝；一次性密码是用户现场同意的凭据，不受限
        let dnd_untrusted =
            st.dnd.load(Ordering::Relaxed) && !crate::pigeon::is_pinned(&remote_static);
        let (ok, reason, onetime) = match st.remote.lock().unwrap().auth.verify(peer_ip, &password)
        {
            Ok(onetime) if !(dnd_untrusted && !onetime) => (true, None, onetime),
            Ok(_) => (false, Some("对方开启了防打扰".to_string()), false),
            Err(AuthError::Locked(secs)) => (
                false,
                Some(format!("密码错误次数过多，请 {secs} 秒后再试")),
                false,
            ),
            Err(AuthError::Wrong) => (false, Some("密码错误".to_string()), false),
        };
        let reply = RemoteWire::RemoteAuthResult {
            ok,
            reason,
            pc_name: ok.then(|| st.cfg.pc_name.clone()),
            screen: None,
        };
        let frame = match seal_msg(&noise, &RemoteOut::Control(reply)) {
            Ok(f) => f,
            Err(_) => return,
        };
        if sink.send(Message::Binary(frame.into())).await.is_err() {
            return;
        }
        if ok {
            authed = true;
            one_time_used = onetime;
            break;
        }
        tracing::info!("远程控制认证失败（{peer_name}，来自 {peer_ip}）");
    }
    if !authed {
        return;
    }

    // ---- 单会话限制（原子登记，防两路并发同时通过检查）----
    let killed = Arc::new(AtomicBool::new(false));
    let reject_frame = {
        let mut remote = st.remote.lock().unwrap();
        if remote.controlled.is_some() {
            Some(
                seal_msg(
                    &noise,
                    &RemoteOut::Control(RemoteWire::RemoteEnd {
                        reason: Some("本机已有控制会话进行中".into()),
                    }),
                )
                .unwrap_or_default(),
            )
        } else {
            remote.controlled = Some(ControlledSession {
                peer_id: controller_machine.clone(),
                peer_name: peer_name.clone(),
                killed: killed.clone(),
            });
            None
        }
    };
    if let Some(frame) = reject_frame {
        let _ = sink.send(Message::Binary(frame.into())).await;
        return;
    }

    // ---- 屏幕探测（顺带验证采集权限）----
    let mut source = match make_screen_source() {
        Ok(s) => s,
        Err(e) => {
            let frame = seal_msg(
                &noise,
                &RemoteOut::Control(RemoteWire::RemoteEnd {
                    reason: Some(e.clone()),
                }),
            )
            .unwrap_or_default();
            let _ = sink.send(Message::Binary(frame.into())).await;
            end_session(&st, killed, controller_machine, peer_name, Some(e));
            return;
        }
    };
    let bounds = source.input_bounds();
    let Some(probe) = source.capture() else {
        let reason = "屏幕采集失败（macOS 需在系统设置授予「屏幕录制」权限后重启）".to_string();
        let frame = seal_msg(
            &noise,
            &RemoteOut::Control(RemoteWire::RemoteEnd {
                reason: Some(reason.clone()),
            }),
        )
        .unwrap_or_default();
        let _ = sink.send(Message::Binary(frame.into())).await;
        end_session(&st, killed, controller_machine, peer_name, Some(reason));
        return;
    };
    let params = Arc::new(Mutex::new(QualityParams::default()));
    let first = scale_frame(&probe, params.lock().unwrap().max_width);
    let screen = ScreenInfo {
        width: first.width,
        height: first.height,
        logical_w: bounds.0,
        logical_h: bounds.1,
    };
    let frame = match seal_msg(
        &noise,
        &RemoteOut::Control(RemoteWire::RemoteAuthResult {
            ok: true,
            reason: None,
            pc_name: Some(st.cfg.pc_name.clone()),
            screen: Some(screen.clone()),
        }),
    ) {
        Ok(f) => f,
        Err(_) => {
            end_session(&st, killed, controller_machine, peer_name, None);
            return;
        }
    };
    if sink.send(Message::Binary(frame.into())).await.is_err() {
        end_session(&st, killed, controller_machine, peer_name, None);
        return;
    }

    tracing::info!(
        "「{peer_name}」{}连接远程控制",
        if one_time_used {
            "凭一次性密码"
        } else {
            "凭密码"
        }
    );
    st.emit(CoreEvent::RemoteSessionStarted {
        peer_id: controller_machine.clone(),
        name: peer_name.clone(),
        side: "controlled".into(),
    });

    let (out_tx, mut out_rx) = tokio::sync::mpsc::channel::<RemoteOut>(256);
    let input_tx = spawn_input_thread(bounds);

    // ---- 采集线程：截屏 → 缩放 → JPEG → 分块投递（慢链路丢帧保实时）----
    {
        let cap_killed = killed.clone();
        let cap_params = params.clone();
        let (cap_tx, cap_rx) = std::sync::mpsc::sync_channel::<RemoteOut>(64);
        let capture_thread = std::thread::Builder::new()
            .name("remote-capture".into())
            .spawn(move || {
                let mut last_hash = 0u64;
                let mut seq = 0u32;
                loop {
                    if cap_killed.load(Ordering::Relaxed) {
                        break;
                    }
                    let t0 = Instant::now();
                    let (max_side, quality, interval) = {
                        let p = cap_params.lock().unwrap();
                        (p.max_width, p.quality, p.interval_ms)
                    };
                    if let Some(f) = source.capture() {
                        let scaled = scale_frame(&f, max_side);
                        let hash = fnv_hash(&scaled.rgba);
                        if hash != last_hash
                            && let Some(jpeg) = encode_jpeg(&scaled, quality)
                        {
                            seq = seq.wrapping_add(1);
                            let total = jpeg.len().div_ceil(CHUNK_SIZE) as u16;
                            let mut sent_any = false;
                            for (i, part) in jpeg.chunks(CHUNK_SIZE).enumerate() {
                                let msg = RemoteOut::FrameChunk {
                                    seq,
                                    idx: i as u16,
                                    total,
                                    data: part.to_vec(),
                                };
                                match cap_tx.try_send(msg) {
                                    Ok(()) => sent_any = true,
                                    Err(_) => break, // 通道满：丢掉当前帧剩余分块，等下一帧
                                }
                            }
                            if sent_any {
                                last_hash = hash;
                            } else {
                                // 整帧被丢：指纹回退，下次仍尝试发送
                                last_hash = hash.wrapping_add(1);
                            }
                        }
                    }
                    let spent = t0.elapsed();
                    let wait = Duration::from_millis(interval)
                        .saturating_sub(spent)
                        .max(Duration::from_millis(5));
                    std::thread::sleep(wait);
                }
            });
        match capture_thread {
            Ok(h) => drop(h), // 会话结束时由 killed 标志置位退出
            Err(e) => {
                end_session(
                    &st,
                    killed,
                    controller_machine,
                    peer_name,
                    Some(format!("采集线程启动失败: {e}")),
                );
                return;
            }
        }
        let out_tx2 = out_tx.clone();
        tokio::spawn(async move {
            while let Ok(msg) = cap_rx.recv() {
                if out_tx2.send(msg).await.is_err() {
                    break;
                }
            }
        });
    }

    // ---- 受控端剪贴板监视：变化即推给控制端 ----
    if !fake_screen_enabled() {
        let out_tx2 = out_tx.clone();
        let clip_killed = killed.clone();
        tokio::spawn(async move {
            let mut last_sent = crate::clipboard::read_text()
                .map(|t| fnv_hash(t.as_bytes()))
                .unwrap_or(0);
            loop {
                tokio::time::sleep(Duration::from_millis(800)).await;
                if clip_killed.load(Ordering::Relaxed) {
                    break;
                }
                let Some(text) = crate::clipboard::read_text() else {
                    continue;
                };
                let h = fnv_hash(text.as_bytes());
                if h != last_sent && text.len() <= CLIPBOARD_MAX {
                    last_sent = h;
                    if out_tx2
                        .send(RemoteOut::Control(RemoteWire::RemoteClipboard { text }))
                        .await
                        .is_err()
                    {
                        break;
                    }
                }
            }
        });
    }

    // ---- 主循环：收（输入/画质/剪贴板/结束）发（帧）----
    let mut end_reason = "对端断开".to_string();
    loop {
        tokio::select! {
            msg = stream.next() => match msg {
                Some(Ok(Message::Binary(b))) => match open_msg(&noise, &b) {
                    Ok(RemoteIn::Control(RemoteWire::RemoteInput { ev })) => {
                        let _ = apply_input(&input_tx, ev);
                    }
                    Ok(RemoteIn::Control(RemoteWire::RemoteQuality { max_width, quality })) => {
                        let mut p = params.lock().unwrap();
                        p.max_width = max_width.clamp(480, 3840);
                        p.quality = quality.clamp(30, 95);
                    }
                    Ok(RemoteIn::Control(RemoteWire::RemoteClipboard { text })) => {
                        if text.len() <= CLIPBOARD_MAX {
                            crate::clipboard::copy_text(&text);
                        }
                    }
                    Ok(RemoteIn::Control(RemoteWire::RemotePaste { text })) => {
                        let _ = input_tx.send(InputCmd::Paste { text });
                    }
                    Ok(RemoteIn::Control(RemoteWire::RemoteEnd { reason })) => {
                        end_reason = reason.unwrap_or_else(|| "对方结束了会话".into());
                        break;
                    }
                    Ok(_) => {}
                    Err(e) => {
                        tracing::warn!("远程控制解密失败，断开会话: {e}");
                        break;
                    }
                },
                Some(Ok(Message::Close(_))) | None => break,
                Some(Ok(_)) => {}
                Some(Err(_)) => break,
            },
            out = out_rx.recv() => match out {
                Some(msg) => match seal_msg(&noise, &msg) {
                    Ok(frame) => {
                        if sink.send(Message::Binary(frame.into())).await.is_err() {
                            end_reason = "发送失败".into();
                            break;
                        }
                    }
                    Err(e) => {
                        tracing::warn!("远程控制加密失败: {e}");
                        break;
                    }
                },
                None => {
                    end_reason = "内部通道关闭".into();
                    break;
                }
            },
            _ = tokio::time::sleep(Duration::from_millis(200)) => {
                if killed.load(Ordering::Relaxed) {
                    end_reason = "本机断开了会话".into();
                    break;
                }
            }
        }
    }

    let _ = sink.close().await;
    drop(input_tx); // 注入线程退出（Enigo drop 自动释放按住的修饰键）
    end_session(
        &st,
        killed,
        controller_machine,
        peer_name.clone(),
        Some(end_reason.clone()),
    );
    tracing::info!("与「{peer_name}」的远程控制会话结束（{end_reason}）");
}

fn apply_input(tx: &std::sync::mpsc::Sender<InputCmd>, ev: InputEvent) -> Result<(), String> {
    let cmd = match ev {
        InputEvent::MouseMove { x, y } => InputCmd::Move { x, y },
        InputEvent::MouseDown { btn } => InputCmd::Button { btn, down: true },
        InputEvent::MouseUp { btn } => InputCmd::Button { btn, down: false },
        InputEvent::Wheel { dy } => InputCmd::Wheel { dy },
        InputEvent::KeyDown { key } => InputCmd::Key {
            name: key,
            down: true,
        },
        InputEvent::KeyUp { key } => InputCmd::Key {
            name: key,
            down: false,
        },
    };
    tx.send(cmd).map_err(|_| "注入线程已退出".to_string())
}

// ---------- 控制端（发起方） ----------

pub async fn remote_connect(st: Arc<AppState>, machine: String, password: String) {
    if let Err(e) = remote_connect_inner(&st, &machine, &password).await {
        tracing::info!("远程控制连接失败：{e}");
        st.emit(CoreEvent::RemoteConnectFailed {
            peer_id: machine,
            reason: e,
        });
    }
}

async fn remote_connect_inner(
    st: &Arc<AppState>,
    machine: &str,
    password: &str,
) -> Result<(), String> {
    if st.remote.lock().unwrap().viewing.is_some() {
        return Err("已有远程控制会话进行中".into());
    }
    let (ip, port, name) = {
        let peers = st.pigeon_peers.lock().unwrap();
        match peers.values().find(|p| p.machine_id == machine) {
            Some(p) => (p.ip, p.tcp_port, p.name.clone()),
            None => return Err(format!("未发现机器 ID 为 {machine} 的设备（对方需在线）")),
        }
    };
    st.emit(CoreEvent::RemoteConnecting {
        peer_id: machine.to_string(),
        name: name.clone(),
    });

    let tcp = tokio::time::timeout(Duration::from_secs(5), TcpStream::connect((ip, port)))
        .await
        .map_err(|_| "连接超时".to_string())?
        .map_err(|e| e.to_string())?;
    let (ws, _resp) =
        tokio_tungstenite::client_async(format!("ws://{ip}:{port}{REMOTE_WS_PATH}"), tcp)
            .await
            .map_err(|e| e.to_string())?;
    let (mut sink, mut stream) = ws.split();

    // Noise XX 握手（initiator）
    let mut hs = crate::pigeon::new_handshake(true)?;
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
    let tokio_tungstenite::tungstenite::Message::Binary(m2) = m2 else {
        return Err("握手消息格式错误".into());
    };
    hs.read_message(&m2, &mut buf).map_err(|e| e.to_string())?;
    let n = hs.write_message(&[], &mut buf).map_err(|e| e.to_string())?;
    sink.send(tokio_tungstenite::tungstenite::Message::binary(
        buf[..n].to_vec(),
    ))
    .await
    .map_err(|e| e.to_string())?;
    let noise = Arc::new(Mutex::new(
        hs.into_transport_mode().map_err(|e| e.to_string())?,
    ));

    // 认证
    let auth = RemoteWire::RemoteAuth {
        mode: "temp".into(),
        password: password.to_string(),
        name: st.cfg.pc_name.clone(),
        machine_id: machine_id(),
    };
    let frame = seal_msg(&noise, &RemoteOut::Control(auth))?;
    sink.send(tokio_tungstenite::tungstenite::Message::binary(frame))
        .await
        .map_err(|e| e.to_string())?;
    let reply = tokio::time::timeout(Duration::from_secs(20), stream.next())
        .await
        .map_err(|_| "认证超时".to_string())?
        .ok_or("对方断开")?
        .map_err(|e| e.to_string())?;
    let tokio_tungstenite::tungstenite::Message::Binary(reply) = reply else {
        return Err("认证消息格式错误".into());
    };
    let RemoteIn::Control(RemoteWire::RemoteAuthResult {
        ok,
        reason,
        pc_name,
        screen: _,
    }) = open_msg(&noise, &reply)?
    else {
        return Err("认证应答格式错误".into());
    };
    if !ok {
        return Err(reason.unwrap_or_else(|| "认证失败".into()));
    }

    // 第二条应答：屏幕信息（或 RemoteEnd——如对方采集失败）
    let detail = tokio::time::timeout(Duration::from_secs(20), stream.next())
        .await
        .map_err(|_| "等待屏幕信息超时".to_string())?
        .ok_or("对方断开")?
        .map_err(|e| e.to_string())?;
    let tokio_tungstenite::tungstenite::Message::Binary(detail) = detail else {
        return Err("应答格式错误".into());
    };
    let screen = match open_msg(&noise, &detail)? {
        RemoteIn::Control(RemoteWire::RemoteAuthResult {
            ok: true, screen, ..
        }) => screen.ok_or("对方未提供屏幕信息")?,
        RemoteIn::Control(RemoteWire::RemoteEnd { reason }) => {
            return Err(reason.unwrap_or_else(|| "对方无法提供画面".into()));
        }
        _ => return Err("应答格式错误".into()),
    };
    let pc_name = pc_name.unwrap_or_else(|| name.clone());
    tracing::info!(
        "已连上「{pc_name}」的远程控制（{}x{}）",
        screen.width,
        screen.height
    );

    // ---- 观看会话登记 + 本地桥 ----
    let token = uuid::Uuid::new_v4().simple().to_string();
    let closed = Arc::new(AtomicBool::new(false));
    let (bridge_tx, _) = tokio::sync::broadcast::channel::<BridgeMsg>(48);
    let (out_tx, mut out_rx) = tokio::sync::mpsc::channel::<RemoteOut>(256);
    st.remote.lock().unwrap().viewing = Some(ViewingSession {
        peer_id: machine.to_string(),
        name: pc_name.clone(),
        token: token.clone(),
        closed: closed.clone(),
        out: out_tx.clone(),
        bridge: bridge_tx.clone(),
    });
    st.emit(CoreEvent::RemoteViewReady {
        peer_id: machine.to_string(),
        name: pc_name.clone(),
        token: token.clone(),
        port: st.cfg.port,
        width: screen.width,
        height: screen.height,
    });

    // 首条桥消息：hello（观看页据此初始化画布比例）
    let _ = bridge_tx.send(BridgeMsg::Json(
        serde_json::json!({ "t": "hello", "name": pc_name, "width": screen.width, "height": screen.height }).to_string(),
    ));

    // ---- 主循环：收帧转发观看桥 / 发输入 ----
    let mut assembler = FrameAssembler::default();
    let mut end_reason = "观看已结束".to_string();
    loop {
        tokio::select! {
            msg = stream.next() => match msg {
                Some(Ok(tokio_tungstenite::tungstenite::Message::Binary(b))) => match open_msg(&noise, &b) {
                    Ok(RemoteIn::FrameChunk { seq, idx, total, data }) => {
                        if let Some(jpeg) = assembler.feed(seq, idx, total, &data) {
                            let _ = bridge_tx.send(BridgeMsg::Frame(jpeg));
                        }
                    }
                    Ok(RemoteIn::Control(RemoteWire::RemoteClipboard { text })) => {
                        if !fake_screen_enabled() {
                            crate::clipboard::copy_text(&text);
                        }
                        let _ = bridge_tx.send(BridgeMsg::Json(
                            serde_json::json!({"t": "clipboard", "len": text.chars().count()}).to_string(),
                        ));
                    }
                    Ok(RemoteIn::Control(RemoteWire::RemoteEnd { reason })) => {
                        end_reason = reason.unwrap_or_else(|| "对方结束了会话".into());
                        break;
                    }
                    Ok(_) => {}
                    Err(e) => {
                        tracing::warn!("远程控制解密失败: {e}");
                        end_reason = "连接异常".into();
                        break;
                    }
                },
                Some(Ok(tokio_tungstenite::tungstenite::Message::Close(_))) | None => {
                    end_reason = "对方断开连接".into();
                    break;
                }
                Some(Ok(_)) => {}
                Some(Err(_)) => {
                    end_reason = "连接异常".into();
                    break;
                }
            },
            out = out_rx.recv() => match out {
                Some(msg) => match seal_msg(&noise, &msg) {
                    Ok(frame) => {
                        if sink.send(tokio_tungstenite::tungstenite::Message::binary(frame)).await.is_err() {
                            end_reason = "发送失败".into();
                            break;
                        }
                    }
                    Err(e) => {
                        tracing::warn!("远程控制加密失败: {e}");
                        end_reason = "连接异常".into();
                        break;
                    }
                },
                None => {
                    end_reason = "内部通道关闭".into();
                    break;
                }
            },
            _ = tokio::time::sleep(Duration::from_millis(200)) => {
                if closed.load(Ordering::Relaxed) {
                    break;
                }
            }
        }
    }

    // ---- 清理 ----
    closed.store(true, Ordering::Relaxed);
    let _ = sink.close().await;
    st.remote.lock().unwrap().viewing = None;
    let _ = bridge_tx.send(BridgeMsg::Json(
        serde_json::json!({"t": "end", "reason": end_reason}).to_string(),
    ));
    st.emit(CoreEvent::RemoteSessionEnded {
        peer_id: machine.to_string(),
        name: pc_name,
        side: "controller".into(),
        reason: Some(end_reason),
    });
    Ok(())
}

// ---------- 本地观看桥（/remote-view-ws，token 鉴权） ----------

pub async fn handle_view_socket(
    st: Arc<AppState>,
    token: Option<String>,
    socket: axum::extract::ws::WebSocket,
) {
    use axum::extract::ws::Message;

    let session = {
        let remote = st.remote.lock().unwrap();
        match remote
            .viewing
            .as_ref()
            .filter(|v| Some(v.token.as_str()) == token.as_deref())
        {
            Some(v) => (v.out.clone(), v.bridge.subscribe(), v.token.clone()),
            None => return,
        }
    };
    let (out_tx, mut bridge_rx, session_token) = session;

    let (mut sink, mut stream) = socket.split();
    // 观看桥写入任务
    let writer = tokio::spawn(async move {
        loop {
            match bridge_rx.recv().await {
                Ok(BridgeMsg::Frame(data)) => {
                    if sink.send(Message::Binary(data.into())).await.is_err() {
                        break;
                    }
                }
                Ok(BridgeMsg::Json(text)) => {
                    if sink.send(Message::text(text)).await.is_err() {
                        break;
                    }
                }
                Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
            }
        }
    });

    // 观看页上行：输入/画质/粘贴/结束
    while let Some(Ok(msg)) = stream.next().await {
        let Message::Text(text) = msg else { continue };
        let Ok(v) = serde_json::from_str::<serde_json::Value>(&text) else {
            continue;
        };
        match v.get("t").and_then(|t| t.as_str()) {
            Some("input") => {
                if let Ok(ev) = serde_json::from_value::<InputEvent>(v) {
                    let _ = out_tx.try_send(RemoteOut::Control(RemoteWire::RemoteInput { ev }));
                }
            }
            Some("clip_paste") => {
                if let Some(text) = crate::clipboard::read_text()
                    && text.len() <= CLIPBOARD_MAX
                {
                    let _ = out_tx.try_send(RemoteOut::Control(RemoteWire::RemotePaste { text }));
                }
            }
            Some("quality") => {
                let max_width = v.get("max_width").and_then(|x| x.as_u64()).unwrap_or(1920) as u32;
                let quality = v.get("quality").and_then(|x| x.as_u64()).unwrap_or(70) as u8;
                let _ = out_tx.try_send(RemoteOut::Control(RemoteWire::RemoteQuality {
                    max_width,
                    quality,
                }));
            }
            Some("end") => {
                if let Some(view) = st.remote.lock().unwrap().viewing.as_ref()
                    && view.token == session_token
                {
                    view.closed.store(true, Ordering::Relaxed);
                }
            }
            _ => {}
        }
    }
    writer.abort();
    // 观看页关闭 = 会话结束
    if let Some(view) = st.remote.lock().unwrap().viewing.as_ref()
        && view.token == session_token
    {
        view.closed.store(true, Ordering::Relaxed);
    }
}

// ---------- 反向协助 ----------

/// 收到飞鸽链路转来的协助请求（B 端）
pub fn on_assist_request(st: &Arc<AppState>, peer_id: &str, peer_name: &str) {
    st.remote.lock().unwrap().assist_pending = Some(peer_id.to_string());
    st.emit(CoreEvent::RemoteAssistRequest {
        peer_id: peer_id.to_string(),
        name: peer_name.to_string(),
    });
}

/// 对方对协助请求的答复（A 端）：同意则拿一次性密码自动建连
pub fn on_assist_response(
    st: &Arc<AppState>,
    peer_id: &str,
    accepted: bool,
    password: Option<String>,
) {
    if !accepted {
        st.emit(CoreEvent::Toast {
            text: "对方拒绝了远程协助请求".into(),
        });
        return;
    }
    let Some(password) = password else {
        st.emit(CoreEvent::Toast {
            text: "对方同意了协助请求但未携带密码".into(),
        });
        return;
    };
    let (machine, name) = {
        let peers = st.pigeon_peers.lock().unwrap();
        match peers.get(peer_id) {
            Some(p) => (p.machine_id.clone(), p.name.clone()),
            None => (String::new(), "电脑".into()),
        }
    };
    if machine.is_empty() {
        st.emit(CoreEvent::Toast {
            text: "对方版本过旧，无法发起远程控制".into(),
        });
        return;
    }
    st.emit(CoreEvent::Toast {
        text: format!("「{name}」已同意，正在连接远程控制…"),
    });
    let st2 = st.clone();
    tokio::spawn(async move {
        remote_connect(st2, machine, password).await;
    });
}

// ---------- UI 请求处理（server.rs 转发） ----------

pub fn handle_ui_request(st: &Arc<AppState>, req: UiRequest) {
    match req {
        UiRequest::RemoteConnect {
            machine_id,
            password,
        } => {
            // 本函数运行在 UI 请求线程（非 runtime 线程），必须经 rt_handle 投递回 runtime
            let st2 = st.clone();
            st.rt_handle.spawn(async move {
                remote_connect(st2, machine_id, password).await;
            });
        }
        UiRequest::RemoteDisconnect => {
            let remote = st.remote.lock().unwrap();
            if let Some(view) = remote.viewing.as_ref() {
                view.closed.store(true, Ordering::Relaxed);
                tracing::info!("用户断开了远程观看会话");
            } else if let Some(ctl) = remote.controlled.as_ref() {
                ctl.killed.store(true, Ordering::Relaxed);
                tracing::info!("用户断开了来自「{}」的远程控制", ctl.peer_name);
            } else {
                drop(remote);
                st.emit(CoreEvent::Toast {
                    text: "当前没有远程控制会话".into(),
                });
            }
        }
        UiRequest::RemoteAssistRequest { peer_id } => {
            let ok = crate::pigeon::try_push(
                st,
                &peer_id,
                crate::protocol::PigeonWire::RemoteAssistRequest {},
            );
            if !ok {
                st.emit(CoreEvent::Toast {
                    text: "对方已断开，无法发起协助请求".into(),
                });
            } else {
                let name = st
                    .pigeon_peers
                    .lock()
                    .unwrap()
                    .get(&peer_id)
                    .map(|p| p.name.clone())
                    .unwrap_or_else(|| "电脑".into());
                st.emit(CoreEvent::Toast {
                    text: format!("已向「{name}」发起协助请求，等待对方同意"),
                });
            }
        }
        UiRequest::RemoteAssistRespond { peer_id, accepted } => {
            let mut remote = st.remote.lock().unwrap();
            remote.assist_pending = None;
            let response = if accepted {
                let pw = gen_onetime_password();
                remote.auth.set_one_time(pw.clone());
                tracing::info!("已同意「{peer_id}」的协助请求，一次性密码已生成");
                crate::protocol::PigeonWire::RemoteAssistResponse {
                    accepted: true,
                    password: Some(pw),
                }
            } else {
                crate::protocol::PigeonWire::RemoteAssistResponse {
                    accepted: false,
                    password: None,
                }
            };
            let ok = crate::pigeon::try_push(st, &peer_id, response);
            if !ok {
                st.emit(CoreEvent::Toast {
                    text: "对方已断开，未能送达答复".into(),
                });
            }
        }
        UiRequest::SetRemoteFixedPassword { password } => {
            // None 或空串 = 清除；否则设置
            let cleared = password
                .as_deref()
                .map(str::trim)
                .map(str::is_empty)
                .unwrap_or(true);
            let hash = (!cleared).then(|| hash_password(password.as_deref().unwrap()));
            st.remote.lock().unwrap().auth.set_fixed_hash(hash.clone());
            let mut s = crate::settings::Settings::load_or_default();
            s.remote_fixed_password_hash = hash;
            match s.save() {
                Ok(()) => st.emit(CoreEvent::Toast {
                    text: if !cleared {
                        "远程控制固定密码已保存（重启后仍有效）".into()
                    } else {
                        "远程控制固定密码已清除".into()
                    },
                }),
                Err(e) => {
                    tracing::error!("固定密码落盘失败: {e}");
                    st.emit(CoreEvent::Toast {
                        text: format!("固定密码保存失败：{e}"),
                    });
                }
            }
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn password_hash_roundtrip() {
        let h = hash_password("abc123");
        assert!(h.starts_with("pbkdf2-sha256$"));
        assert!(verify_password_hash("abc123", &h));
        assert!(!verify_password_hash("abc124", &h));
        // 两次哈希盐不同
        assert_ne!(h, hash_password("abc123"));
        // 非法格式安全返回 false
        assert!(!verify_password_hash("x", "garbage"));
        assert!(!verify_password_hash("x", "pbkdf2-sha256$zz$00$00"));
    }

    #[test]
    fn auth_temp_and_lockout() {
        let mut auth = RemoteAuthState::new(None);
        let ip: IpAddr = "192.168.1.9".parse().unwrap();
        let temp = auth.temp_password().to_string();
        assert_eq!(auth.verify(ip, &temp), Ok(false));
        // 错 5 次 → 锁定
        for _ in 0..MAX_FAILS {
            assert_eq!(auth.verify(ip, "000000"), Err(AuthError::Wrong));
        }
        assert!(matches!(auth.verify(ip, &temp), Err(AuthError::Locked(_))));
        // 换 IP 不受影响
        let ip2: IpAddr = "192.168.1.10".parse().unwrap();
        assert_eq!(auth.verify(ip2, &temp), Ok(false));
    }

    #[test]
    fn auth_fixed_and_onetime() {
        let hash = hash_password("fixed-pw");
        let mut auth = RemoteAuthState::new(Some(hash));
        let ip: IpAddr = "10.0.0.2".parse().unwrap();
        assert_eq!(auth.verify(ip, "fixed-pw"), Ok(false));
        // 一次性密码命中后即焚
        auth.set_one_time("AB2CD3EF".into());
        assert_eq!(auth.verify(ip, "AB2CD3EF"), Ok(true));
        assert_eq!(
            auth.verify(ip, "AB2CD3EF"),
            Err(AuthError::Wrong),
            "一次性密码应即焚"
        );
    }

    #[test]
    fn frame_assembler_reassembles_and_drops_stale() {
        let mut a = FrameAssembler::default();
        // 空组装器收到非首块 → 残尾丢弃
        assert!(a.feed(7, 1, 3, b"bb").is_none());
        assert!(a.feed(7, 0, 3, b"aa").is_none());
        assert!(a.feed(7, 2, 3, b"cc").is_none());
        // 重复 idx=0 → 重建缓冲
        assert!(a.feed(7, 0, 3, b"aa").is_none());
        assert!(a.feed(7, 1, 3, b"bb").is_none());
        // 三块齐 → 出完整帧
        assert_eq!(
            a.feed(7, 2, 3, b"cc").as_deref(),
            Some(b"aabbcc".as_slice())
        );
        // 新 seq 的 idx=0 重建
        assert!(a.feed(8, 0, 2, b"xx").is_none());
        assert_eq!(a.feed(8, 1, 2, b"yy").as_deref(), Some(b"xxyy".as_slice()));
        // 旧 seq 残尾丢弃
        assert!(a.feed(7, 1, 3, b"zz").is_none());
    }

    #[test]
    fn scale_frame_dims() {
        let f = Frame {
            width: 800,
            height: 600,
            rgba: vec![0; 800 * 600 * 4],
        };
        // 不超上限原样
        let s = scale_frame(&f, 1920);
        assert_eq!((s.width, s.height), (800, 600));
        // 缩到上限，比例保持
        let s = scale_frame(&f, 400);
        assert_eq!((s.width, s.height), (400, 300));
        assert_eq!(s.rgba.len(), 400 * 300 * 4);
    }

    #[test]
    fn machine_id_is_8hex_and_stable() {
        let id = machine_id();
        assert_eq!(id.len(), 8);
        assert!(id.chars().all(|c| c.is_ascii_hexdigit()));
        assert_eq!(id, machine_id());
    }

    #[test]
    fn wire_serde_roundtrip() {
        let w = RemoteWire::RemoteInput {
            ev: InputEvent::MouseMove { x: 0.5, y: 0.25 },
        };
        let json = serde_json::to_string(&w).unwrap();
        assert!(json.contains(r#""type":"remote_input""#));
        let back: RemoteWire = serde_json::from_str(&json).unwrap();
        assert!(matches!(back, RemoteWire::RemoteInput { .. }));

        let auth = RemoteWire::RemoteAuth {
            mode: "temp".into(),
            password: "123456".into(),
            name: "A".into(),
            machine_id: "abcd1234".into(),
        };
        let back: RemoteWire =
            serde_json::from_str(&serde_json::to_string(&auth).unwrap()).unwrap();
        match back {
            RemoteWire::RemoteAuth {
                password,
                machine_id,
                ..
            } => {
                assert_eq!(password, "123456");
                assert_eq!(machine_id, "abcd1234");
            }
            _ => panic!("应为 RemoteAuth"),
        }
    }

    #[test]
    fn onetime_password_alphabet() {
        for _ in 0..20 {
            let pw = gen_onetime_password();
            assert_eq!(pw.len(), 8);
            assert!(pw.chars().all(|c| c.is_ascii_alphanumeric() && !matches!(c, '0' | 'O' | '1' | 'I' | 'L')));
        }
    }
}
