//! 无人值守三期：Windows 安全助手（SYSTEM、控制台会话内）+ 本地命名管道协议。
//!
//! 背景：agent 跑在用户会话，锁屏后 Windows 切到安全桌面（WinSta0\Winlogon），
//! 用户级进程既采不到画面，SendInput 也被 UIPI 拦截。本模块引入一个由服务以
//! SYSTEM 身份在控制台会话内拉起的助手进程（`yunshu secure --token …`）：
//!   * SYSTEM + 控制台会话 ⇒ 可以 GDI 采集安全桌面（锁屏画面，经典 VNC 手法），
//!     SendInput 也能到达安全桌面（可远程输密码解锁）；
//!   * agent 与助手经本地命名管道通信（ACL 限 SYSTEM + 会话用户，外启动令牌
//!     握手）：锁屏时 agent 把采集/输入路由到助手，解锁自动切回本地 WGC/enigo。
//!
//! 网络会话、认证、编码协商全部留在 agent；助手只做"眼睛和手"，仅监听本机
//! 命名管道，无任何网络面。安全边界与已知限制见 docs/远程控制功能调研.md 三期。

use serde::{Deserialize, Serialize};
#[cfg(any(windows, target_os = "macos"))]
use std::sync::Mutex;
#[cfg(any(windows, target_os = "macos"))]
use std::sync::atomic::AtomicBool;

// ===== 协议层（跨平台，单测覆盖）=====

/// 服务每次启动随机生成的接管令牌（分别下发给助手与 agent）
pub fn random_token() -> String {
    (0..32).map(|_| format!("{:02x}", rand::random::<u8>())).collect()
}

/// 恒时字符串比较（令牌握手用）
fn constant_eq(a: &str, b: &str) -> bool {
    if a.len() != b.len() {
        return false;
    }
    a.bytes().zip(b.bytes()).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

/// 本地命名管道名（按控制台会话区分）
pub fn pipe_name(session: u32) -> String {
    format!(r"\\.\pipe\yunshu-secure-{session}")
}

/// 帧类型标记：助手帧固定 JPEG（锁屏画面静态，JPEG 足够；解锁后走本地 WGC+H.264）
pub const CODEC_JPEG_RELAY: u8 = 0;

/// JSON 控制消息
pub const KIND_JSON: u8 = 1;
/// 编码帧负载
pub const KIND_FRAME: u8 = 2;

/// 4 字节 LE 长度 + 1 字节类型 + 载荷。上限 32MB 防畸形流拖爆内存
pub fn write_frame(w: &mut impl std::io::Write, kind: u8, payload: &[u8]) -> std::io::Result<()> {
    let len = (payload.len() + 1) as u32;
    if len > 33_554_432 {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "帧超限",
        ));
    }
    w.write_all(&len.to_le_bytes())?;
    w.write_all(&[kind])?;
    w.write_all(payload)
}

pub fn read_frame(r: &mut impl std::io::Read) -> std::io::Result<(u8, Vec<u8>)> {
    let mut lbuf = [0u8; 4];
    r.read_exact(&mut lbuf)?;
    let len = u32::from_le_bytes(lbuf) as usize;
    if len == 0 || len > 33_554_432 {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "帧长度非法",
        ));
    }
    let mut buf = vec![0u8; len];
    r.read_exact(&mut buf)?;
    Ok((buf[0], buf[1..].to_vec()))
}

/// agent → 助手 请求
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum SecureRequest {
    Hello { token: String },
    Start { max_width: u32, quality: u8 },
    Params { max_width: u32, quality: u8 },
    Input { ev: crate::remote::InputEvent },
    Stop,
}

/// 助手 → agent 应答
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum SecureResponse {
    Ok,
    Busy,
    Bounds { width: u32, height: u32 },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frame_roundtrip() {
        let mut buf = Vec::new();
        write_frame(&mut buf, KIND_JSON, br#"{"a":1}"#).unwrap();
        let (kind, payload) = read_frame(&mut buf.as_slice()).unwrap();
        assert_eq!(kind, KIND_JSON);
        assert_eq!(payload, br#"{"a":1}"#);
    }

    #[test]
    fn frame_rejects_oversize() {
        let mut buf = Vec::new();
        buf.extend_from_slice(&u32::MAX.to_le_bytes());
        assert!(read_frame(&mut buf.as_slice()).is_err());
    }

    #[test]
    fn request_serde_roundtrip() {
        let req = SecureRequest::Input {
            ev: crate::remote::InputEvent::MouseMove { x: 0.5, y: 0.25 },
        };
        let s = serde_json::to_string(&req).unwrap();
        let back: SecureRequest = serde_json::from_str(&s).unwrap();
        assert!(matches!(back, SecureRequest::Input { .. }));
    }
}

/// 接管路由：输入线程据此把注入改道到平台助手（锁屏期间）。
/// Windows = 命名管道（采集帧 + 输入）；macOS = unix 套接字（仅输入，采集保持本地）。
/// 仅三期平台（Windows/macOS）存在：依赖的平台助手与管道/套接字客户端 Linux 上没有
#[cfg(any(windows, target_os = "macos"))]
pub struct Route {
    pub locked: AtomicBool,
    #[cfg(windows)]
    pub pipe: Mutex<Option<win::PipeClient>>,
    #[cfg(target_os = "macos")]
    pub sock: Mutex<Option<macos::UnixInputClient>>,
}

#[cfg(any(windows, target_os = "macos"))]
impl Route {
    pub fn new() -> Self {
        Route {
            locked: AtomicBool::new(false),
            #[cfg(windows)]
            pipe: Mutex::new(None),
            #[cfg(target_os = "macos")]
            sock: Mutex::new(None),
        }
    }
}

#[cfg(any(windows, target_os = "macos"))]
impl Default for Route {
    fn default() -> Self {
        Self::new()
    }
}

// ===== Windows 实现 =====
#[cfg(windows)]
mod win {
    use super::*;
    use crate::remote::InputEvent;
    use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU8, Ordering};
    use std::os::windows::io::FromRawHandle;
    use std::sync::{Arc, Mutex};
    use windows_sys::Win32::Foundation::{CloseHandle, GENERIC_ALL, HANDLE, INVALID_HANDLE_VALUE};
    use windows_sys::Win32::Graphics::Gdi::{
        BitBlt, CreateCompatibleBitmap, CreateCompatibleDC, CreateDCA, DeleteDC, DeleteObject,
        GetDIBits, SelectObject, BITMAPINFO, BITMAPINFOHEADER, CAPTUREBLT, DIB_RGB_COLORS, SRCCOPY,
    };
    use windows_sys::Win32::Security::{
        AddAccessAllowedAce, ACL, GetTokenInformation, InitializeSecurityDescriptor,
        SECURITY_ATTRIBUTES, SetSecurityDescriptorDacl, TokenUser, WinLocalSystemSid, ACL_REVISION,
    };
    use windows_sys::Win32::Storage::FileSystem::PIPE_ACCESS_DUPLEX;
    use windows_sys::Win32::System::Pipes::{
        ConnectNamedPipe, CreateNamedPipeW, DisconnectNamedPipe, PIPE_READMODE_BYTE,
        PIPE_TYPE_BYTE, PIPE_WAIT,
    };
    use windows_sys::Win32::System::RemoteDesktop::{
        WTSFreeMemory, WTSGetActiveConsoleSessionId, WTSQuerySessionInformationW, WTSINFOEXW,
        WTS_CURRENT_SERVER_HANDLE, WTS_CURRENT_SESSION, WTSSessionInfoEx,
    };
    use windows_sys::Win32::System::StationsAndDesktops::{
        CloseDesktop, OpenInputDesktop, SetThreadDesktop, DESKTOP_READOBJECTS,
        DESKTOP_SWITCHDESKTOP, DESKTOP_WRITEOBJECTS,
    };
    use windows_sys::Win32::UI::WindowsAndMessaging::{GetSystemMetrics, SM_CXSCREEN, SM_CYSCREEN};
    use windows_sys::core::PWSTR;

    /// 当前控制台会话 id
    pub fn console_session() -> u32 {
        unsafe { WTSGetActiveConsoleSessionId() }
    }

    /// 控制台会话是否锁定。Win8+ 语义：flags==1 即锁定（Win7 值反转，本应用
    /// 要求 Win10 1903+，不处理旧系统）
    pub fn console_locked() -> bool {
        unsafe {
            let mut buf: PWSTR = std::ptr::null_mut();
            let mut len = 0u32;
            if WTSQuerySessionInformationW(
                WTS_CURRENT_SERVER_HANDLE,
                WTS_CURRENT_SESSION,
                WTSSessionInfoEx,
                &mut buf,
                &mut len,
            ) == 0
            {
                return false; // 查询失败按未锁定处理（宁可继续本地采集）
            }
            let info = buf as *const WTSINFOEXW;
            // WTSINFOEXW = Level(u32) + union{Level1{SessionId,SessionState,SessionFlags…}}
            // SessionFlags 位于偏移 12 字节（Level 4 + SessionId 4 + SessionState 4）
            let flags = (info as *const u8).add(12) as *const i32;
            let locked = *flags == 1;
            WTSFreeMemory(buf as _);
            locked
        }
    }

    fn handle_guard(h: HANDLE) -> HandleGuard {
        HandleGuard(h)
    }

    struct HandleGuard(HANDLE);
    impl Drop for HandleGuard {
        fn drop(&mut self) {
            unsafe {
                CloseHandle(self.0);
            }
        }
    }

    /// 管道安全描述符：DACL 授予 SYSTEM 与控制台会话用户完全访问。
    /// 生命周期内自持 SD/ACL/SID 缓冲，sa() 返回给 CreateNamedPipeW
    struct PipeSec {
        sa: SECURITY_ATTRIBUTES,
        sd: [u8; 40],
        // 被安全描述符以裸指针引用（ACE→SID），本体无直接读取，须保活
        #[allow(dead_code)]
        acl: [u8; 256],
        #[allow(dead_code)]
        sid: Vec<u8>,
    }

    impl PipeSec {
        fn build(session: u32) -> Option<Self> {
            unsafe {
            let mut user_tok = HANDLE::default();
            if windows_sys::Win32::System::RemoteDesktop::WTSQueryUserToken(
                session,
                &mut user_tok,
            ) == 0
            {
                return None;
            }
            let _tok_guard = handle_guard(user_tok);
            let mut len = 0u32;
            GetTokenInformation(user_tok, TokenUser, std::ptr::null_mut(), 0, &mut len);
            let mut sid = vec![0u8; len.max(68) as usize];
            if GetTokenInformation(
                user_tok,
                TokenUser,
                sid.as_mut_ptr() as _,
                sid.len() as u32,
                &mut len,
            ) == 0
            {
                return None;
            }
            // TOKEN_USER.User.Sid 位于结构体偏移 0（SID_AND_ATTRIBUTES 首字段）
            let user_sid = sid.as_ptr() as *mut core::ffi::c_void;

            let mut system_sid = [0u8; 68];
            let mut cb = 68u32;
            if windows_sys::Win32::Security::CreateWellKnownSid(
                WinLocalSystemSid,
                std::ptr::null_mut(),
                system_sid.as_mut_ptr() as _,
                &mut cb,
            ) == 0
            {
                return None;
            }

            let mut sd = [0u8; 40];
            let mut acl = [0u8; 256];
            let acl_ptr = acl.as_mut_ptr() as *mut ACL;
            InitializeSecurityDescriptor(sd.as_mut_ptr() as _, ACL_REVISION);
            AddAccessAllowedAce(acl_ptr, ACL_REVISION, GENERIC_ALL, system_sid.as_mut_ptr() as _);
            AddAccessAllowedAce(acl_ptr, ACL_REVISION, GENERIC_ALL, user_sid);
            SetSecurityDescriptorDacl(sd.as_mut_ptr() as _, 1, acl_ptr, 0);

            Some(PipeSec {
                sa: SECURITY_ATTRIBUTES {
                    nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
                    lpSecurityDescriptor: sd.as_mut_ptr() as _,
                    bInheritHandle: 0,
                },
                sd,
                acl,
                sid,
            })
            }
        }

        fn as_sa(&self) -> SECURITY_ATTRIBUTES {
            SECURITY_ATTRIBUTES {
                nLength: self.sa.nLength,
                lpSecurityDescriptor: self.sd.as_ptr() as _,
                bInheritHandle: 0,
            }
        }
    }

    /// GDI 采集活动桌面（锁屏 = 安全桌面）。每次采集切桌面：线程桌面随
    /// 活动桌面变化需要跟随。SYSTEM + 控制台会话即可打开安全桌面
    fn capture_active_desktop() -> Option<crate::remote::Frame> {
        unsafe {
            let desk = OpenInputDesktop(
                0,
                0,
                DESKTOP_READOBJECTS | DESKTOP_WRITEOBJECTS | DESKTOP_SWITCHDESKTOP,
            );
            if desk.is_null() {
                return None;
            }
            // HDESK 用 CloseDesktop 释放（不能用 CloseHandle），所有路径都要关
            if SetThreadDesktop(desk) == 0 {
                CloseDesktop(desk);
                return None;
            }
            let w = GetSystemMetrics(SM_CXSCREEN).max(1) as u32;
            let h = GetSystemMetrics(SM_CYSCREEN).max(1) as u32;
            let hdc = CreateDCA(b"display\0".as_ptr(), std::ptr::null(), std::ptr::null(), std::ptr::null());
            if hdc.is_null() {
                return None;
            }
            let mem = CreateCompatibleDC(hdc);
            let bmp = CreateCompatibleBitmap(hdc, w as i32, h as i32);
            let old = SelectObject(mem, bmp as _);

            let mut bmi = BITMAPINFO {
                bmiHeader: BITMAPINFOHEADER {
                    biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                    biWidth: w as i32,
                    biHeight: -(h as i32), // 负高 = 自上而下行序
                    biPlanes: 1,
                    biBitCount: 32,
                    biCompression: 0,
                    biSizeImage: (w * h * 4) as u32,
                    ..Default::default()
                },
                ..Default::default()
            };
            let mut bgra = vec![0u8; (w * h * 4) as usize];
            let ok = BitBlt(mem, 0, 0, w as i32, h as i32, hdc, 0, 0, SRCCOPY | CAPTUREBLT) != 0
                && GetDIBits(
                    mem,
                    bmp,
                    0,
                    h,
                    bgra.as_mut_ptr() as _,
                    &mut bmi,
                    DIB_RGB_COLORS,
                ) != 0;

            SelectObject(mem, old);
            DeleteObject(bmp as _);
            DeleteDC(mem);
            CloseDesktop(desk);
            if !ok {
                return None;
            }
            // BGRA → RGBA
            for px in bgra.chunks_exact_mut(4) {
                px.swap(0, 2);
            }
            Some(crate::remote::Frame {
                width: w,
                height: h,
                rgba: bgra,
            })
        }
    }

    struct CaptureParams {
        max_width: AtomicU32,
        quality: AtomicU8,
    }

    /// 助手采集线程：活动桌面 GDI 采集 → 缩放 → JPEG → 管道帧。
    /// 静止跳帧 + 1s 心跳，节奏上限 ~12fps（锁屏画面静态，够用）
    fn spawn_capture_thread(
        write: Arc<Mutex<std::fs::File>>,
        stop: Arc<AtomicBool>,
        params: Arc<CaptureParams>,
        bounds: Arc<Mutex<(u32, u32)>>,
    ) -> std::thread::JoinHandle<()> {
        std::thread::Builder::new()
            .name("secure-capture".into())
            .spawn(move || {
                let mut last_hash = 0u64;
                let mut last_sent = std::time::Instant::now();
                while !stop.load(Ordering::Relaxed) {
                    let t0 = std::time::Instant::now();
                    let Some(frame) = capture_active_desktop() else {
                        std::thread::sleep(std::time::Duration::from_millis(250));
                        continue;
                    };
                    {
                        let mut b = bounds.lock().unwrap();
                        if *b != (frame.width, frame.height) {
                            *b = (frame.width, frame.height);
                            let msg = SecureResponse::Bounds {
                                width: frame.width,
                                height: frame.height,
                            };
                            let _ = write_frame(
                                &mut *write.lock().unwrap(),
                                KIND_JSON,
                                &serde_json::to_vec(&msg).unwrap_or_default(),
                            );
                        }
                    }
                    let scaled =
                        crate::remote::scale_frame(&frame, params.max_width.load(Ordering::Relaxed));
                    let hash = crate::remote::fnv_hash(&scaled.rgba);
                    let heartbeat = last_sent.elapsed() >= std::time::Duration::from_secs(1);
                    if hash != last_hash || heartbeat {
                        if let Some(jpeg) =
                            crate::remote::encode_jpeg(&scaled, params.quality.load(Ordering::Relaxed))
                        {
                            let payload = {
                                let mut p = vec![CODEC_JPEG_RELAY];
                                p.extend_from_slice(&(scaled.width).to_le_bytes());
                                p.extend_from_slice(&(scaled.height).to_le_bytes());
                                p.extend_from_slice(&jpeg);
                                p
                            };
                            let mut w = write.lock().unwrap();
                            if write_frame(&mut *w, KIND_FRAME, &payload).is_err() {
                                break; // 管道断开（agent 退出/解锁切回）
                            }
                            last_hash = hash;
                            last_sent = std::time::Instant::now();
                        }
                    }
                    let spent = t0.elapsed();
                    let wait = std::time::Duration::from_millis(83).saturating_sub(spent);
                    if !wait.is_zero() {
                        std::thread::sleep(wait);
                    }
                }
            })
            .expect("助手采集线程启动失败")
    }

    /// 服务单个客户端连接：令牌握手 → 请求循环
    fn serve_client(
        pipe: HANDLE,
        file: std::fs::File,
        token: &str,
        bounds: Arc<Mutex<(u32, u32)>>,
    ) {
        let mut reader = match file.try_clone() {
            Ok(f) => f,
            Err(_) => return,
        };
        let write = Arc::new(Mutex::new(match file.try_clone() {
            Ok(f) => f,
            Err(_) => return,
        }));

        // 握手：Hello {token} 不匹配即断（防误连）
        let ok = (|| -> Option<()> {
            let (kind, payload) = read_frame(&mut reader).ok()?;
            if kind != KIND_JSON {
                return None;
            }
            match serde_json::from_slice::<SecureRequest>(&payload).ok()? {
                SecureRequest::Hello { token: t } if constant_eq(&t, token) => Some(()),
                _ => None,
            }
        })();
        if ok.is_none() {
            let _ = write_frame(
                &mut *write.lock().unwrap(),
                KIND_JSON,
                &serde_json::to_vec(&SecureResponse::Busy).unwrap_or_default(),
            );
            return;
        }
        let _ = write_frame(
            &mut *write.lock().unwrap(),
            KIND_JSON,
            &serde_json::to_vec(&SecureResponse::Ok).unwrap_or_default(),
        );

        let input_tx = crate::remote::spawn_input_thread(bounds.clone(), None);
        let stop = Arc::new(AtomicBool::new(false));
        let mut capture: Option<std::thread::JoinHandle<()>> = None;
        let params = Arc::new(CaptureParams {
            max_width: AtomicU32::new(1920),
            quality: AtomicU8::new(60),
        });

        loop {
            let (kind, payload) = match read_frame(&mut reader) {
                Ok(v) => v,
                Err(_) => break, // 客户端断开（agent 结束/解锁切回本地）
            };
            if kind != KIND_JSON {
                continue;
            }
            match serde_json::from_slice::<SecureRequest>(&payload) {
                Ok(SecureRequest::Start { max_width, quality }) => {
                    params.max_width.store(max_width, Ordering::Relaxed);
                    params.quality.store(quality, Ordering::Relaxed);
                    if capture.is_none() {
                        stop.store(false, Ordering::Relaxed);
                        capture = Some(spawn_capture_thread(
                            write.clone(),
                            stop.clone(),
                            params.clone(),
                            bounds.clone(),
                        ));
                    }
                }
                Ok(SecureRequest::Params { max_width, quality }) => {
                    params.max_width.store(max_width, Ordering::Relaxed);
                    params.quality.store(quality, Ordering::Relaxed);
                }
                Ok(SecureRequest::Input { ev }) => {
                    // 坐标为归一化值，由注入线程按助手侧 bounds 映射
                    let _ = crate::remote::apply_input(&input_tx, ev);
                }
                Ok(SecureRequest::Stop) => {
                    stop.store(true, Ordering::Relaxed);
                    if let Some(h) = capture.take() {
                        let _ = h.join();
                    }
                }
                Ok(_) | Err(_) => {}
            }
        }
        stop.store(true, Ordering::Relaxed);
        if let Some(h) = capture.take() {
            let _ = h.join();
        }
        unsafe { DisconnectNamedPipe(pipe) };
        // file drop 关闭句柄
    }

    /// 助手主循环：创建管道 → 等客户端 → 服务 → 断开后重建。永不返回
    pub fn run_helper(token: &str) -> ! {
        let session = console_session();
        let name = pipe_name(session);
        let name_w: Vec<u16> = name.encode_utf16().chain(std::iter::once(0)).collect();
        let bounds = Arc::new(Mutex::new((0u32, 0u32)));
        tracing::info!("安全助手启动（会话 {session}，管道 {name}）");
        loop {
            let Some(sec) = PipeSec::build(session) else {
                std::thread::sleep(std::time::Duration::from_secs(3));
                continue;
            };
            unsafe {
                let handle = CreateNamedPipeW(
                    name_w.as_ptr(),
                    PIPE_ACCESS_DUPLEX,
                    PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_WAIT,
                    1,
                    4 * 1024 * 1024,
                    64 * 1024,
                    0,
                    &sec.as_sa(),
                );
                if handle.is_null() || handle == INVALID_HANDLE_VALUE {
                    std::thread::sleep(std::time::Duration::from_secs(3));
                    continue;
                }
                let _pipe_guard = handle_guard(handle);
                // 阻塞等 agent 接入（锁屏触发；服务停时会杀本进程）
                if ConnectNamedPipe(handle, std::ptr::null_mut()) == 0 {
                    let err = windows_sys::Win32::Foundation::GetLastError();
                    // ERROR_PIPE_CONNECTED：连接竞态下客户端已连上，直接服务
                    if err != 535 {
                        std::thread::sleep(std::time::Duration::from_millis(300));
                        continue;
                    }
                }
                let file = std::fs::File::from_raw_handle(handle as _);
                serve_client(handle, file, token, bounds.clone());
                // serve_client 结束时已 DisconnectNamedPipe 并关闭句柄
            }
        }
    }

    // ===== agent 侧管道客户端 =====

    #[derive(Debug, Clone)]
    pub struct SecureFrame {
        pub codec: u8,
        pub width: u32,
        pub height: u32,
        pub data: Vec<u8>,
    }

    /// agent 侧锁屏接管器：检测锁状态，管理与助手的管道
    pub struct AgentSecure {
        token: Option<String>,
        route: Arc<Route>,
        last_check: std::time::Instant,
        sent_params: Option<(u32, u8)>,
    }

    impl AgentSecure {
        /// 令牌经服务以 env 下发；无 env（手动运行/未装服务）= 三期能力不可用
        pub fn new() -> Self {
            AgentSecure {
                token: std::env::var("YUNSHU_SECURE_TOKEN").ok(),
                route: Arc::new(Route::new()),
                last_check: std::time::Instant::now() - std::time::Duration::from_secs(10),
                sent_params: None,
            }
        }

        pub fn route(&self) -> Arc<Route> {
            self.route.clone()
        }

        /// 每次采集迭代调用（2s 节流锁检测）。返回待转发的助手编码帧；
        /// 画质参数变化时下发 Params 给助手
        pub fn poll(
            &mut self,
            max_width: u32,
            quality: u8,
        ) -> Option<SecureFrame> {
            let Some(token) = &self.token else {
                return None;
            };
            if self.last_check.elapsed() < std::time::Duration::from_secs(2) {
                // 已接管：从助手取帧
                if let Some(p) = self.route.pipe.lock().unwrap().as_ref() {
                    return p.try_recv_frame();
                }
                return None;
            }
            self.last_check = std::time::Instant::now();
            let locked = console_locked();
            let was = self.route.locked.swap(locked, Ordering::Relaxed);
            if locked && !was {
                match PipeClient::connect(token) {
                    Ok(p) => {
                        p.send_start(max_width, quality);
                        self.sent_params = Some((max_width, quality));
                        *self.route.pipe.lock().unwrap() = Some(p);
                        tracing::info!("屏幕已锁定：采集/输入切换到 SYSTEM 安全助手");
                    }
                    Err(e) => {
                        tracing::info!("锁屏接管失败（助手未就绪？）: {e}");
                    }
                }
            } else if !locked && was {
                if let Some(p) = self.route.pipe.lock().unwrap().take() {
                    p.stop();
                }
                tracing::info!("屏幕已解锁：切回本地采集/注入");
            }
            // 画质参数变化下发（Start 只在接管时发一次）
            if let Some(p) = self.route.pipe.lock().unwrap().as_ref() {
                if self.sent_params != Some((max_width, quality)) {
                    p.send_params(max_width, quality);
                    self.sent_params = Some((max_width, quality));
                }
                return p.try_recv_frame();
            }
            None
        }
    }

    pub struct PipeClient {
        file: std::fs::File,
        rx: std::sync::mpsc::Receiver<SecureFrame>,
    }

    impl PipeClient {
        pub fn connect(token: &str) -> std::io::Result<Self> {
            let file = std::fs::OpenOptions::new()
                .read(true)
                .write(true)
                .open(pipe_name(console_session()))?;
            let mut w = file.try_clone()?;
            write_frame(
                &mut w,
                KIND_JSON,
                &serde_json::to_vec(&SecureRequest::Hello {
                    token: token.to_string(),
                })
                .unwrap_or_default(),
            )?;
            // 握手应答（助手即时返回）
            let (kind, payload) = read_frame(&mut w)?;
            if kind != KIND_JSON {
                return Err(std::io::Error::new(std::io::ErrorKind::InvalidData, "握手格式"));
            }
            match serde_json::from_slice::<SecureResponse>(&payload) {
                Ok(SecureResponse::Ok) => {}
                _ => {
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::PermissionDenied,
                        "助手拒绝",
                    ))
                }
            }
            let (tx, rx) = std::sync::mpsc::channel::<SecureFrame>();
            let mut r = file.try_clone()?;
            std::thread::Builder::new()
                .name("secure-pipe-rx".into())
                .spawn(move || loop {
                    let (kind, payload) = match read_frame(&mut r) {
                        Ok(v) => v,
                        Err(_) => break, // 管道断（助手停/解锁切回）
                    };
                    if kind != KIND_FRAME {
                        continue;
                    }
                    if payload.len() < 9 {
                        continue;
                    }
                    let frame = SecureFrame {
                        codec: payload[0],
                        width: u32::from_le_bytes(payload[1..5].try_into().unwrap()),
                        height: u32::from_le_bytes(payload[5..9].try_into().unwrap()),
                        data: payload[9..].to_vec(),
                    };
                    if tx.send(frame).is_err() {
                        break;
                    }
                })
                .expect("管道读线程启动失败");
            Ok(PipeClient { file, rx })
        }

        pub fn send_start(&self, max_width: u32, quality: u8) {
            self.send_json(&SecureRequest::Start { max_width, quality });
        }

        pub fn send_params(&self, max_width: u32, quality: u8) {
            self.send_json(&SecureRequest::Params { max_width, quality });
        }

        pub fn send_input(&self, ev: &InputEvent) {
            self.send_json(&SecureRequest::Input { ev: ev.clone() });
        }

        fn send_json(&self, req: &SecureRequest) {
            if let Ok(payload) = serde_json::to_vec(req) {
                let mut w = self.file.try_clone().expect("管道句柄克隆失败");
                let _ = write_frame(&mut w, KIND_JSON, &payload);
            }
        }

        pub fn try_recv_frame(&self) -> Option<SecureFrame> {
            self.rx.try_recv().ok()
        }

        pub fn stop(&self) {
            self.send_json(&SecureRequest::Stop);
        }
    }
}

#[cfg(windows)]
pub use win::{AgentSecure, PipeClient};
#[cfg(target_os = "macos")]
pub use macos::{AgentSecure, plist_installed};


// ===== macOS：锁检测 + root 输入助手（LaunchDaemon）+ 本地 unix 套接字 =====
//
// 平台差异：macOS 锁屏画面**用户级权限即可采集**（TCC 屏幕录制授权后，登录窗口
// 出现在录屏流里），采集保持本地不动；真正被挡的是**输入**——loginwindow 属
// root，用户会话的合成事件（CGEventPost）到不了安全桌面。因此 macOS 的三期
// 助手是 root **输入助手**（LaunchDaemon 常驻），只做输入注入，不做采集。
#[cfg(target_os = "macos")]
mod macos {
    use super::*;
    use crate::remote::InputEvent;
    use std::os::unix::net::UnixListener;
    use std::sync::atomic::Ordering;
    use std::sync::{Arc, Mutex};

    pub const SOCK_PATH: &str = "/var/run/yunshu-secure.sock";
    const LAUNCHD_PLIST: &str = "/Library/LaunchDaemons/com.yunshu.secure.plist";

    unsafe extern "C" {
        fn CGSessionCopyCurrentDictionary() -> *mut std::ffi::c_void;
        fn CFDictionaryGetValue(dict: *const std::ffi::c_void, key: *const std::ffi::c_void)
            -> *const std::ffi::c_void;
        fn CFBooleanGetValue(boolean: *const std::ffi::c_void) -> bool;
        fn CFRelease(cf: *const std::ffi::c_void);
        fn CFStringCreateWithCString(
            alloc: *const std::ffi::c_void,
            c_str: *const std::ffi::c_char,
            encoding: u32,
        ) -> *const std::ffi::c_void;
    }

    /// 控制台会话是否锁定：CGSessionCopyCurrentDictionary 里
    /// kCGSSessionOnConsoleKey 为假（或缺失）= 本会话已不持有控制台 = 已锁屏
    pub fn console_locked() -> bool {
        let dict = unsafe { CGSessionCopyCurrentDictionary() };
        if dict.is_null() {
            return false;
        }
        let key = unsafe {
            CFStringCreateWithCString(
                std::ptr::null(),
                c"kCGSSessionOnConsoleKey".as_ptr(),
                0x08000100, // kCFStringEncodingUTF8
            )
        };
        let val = unsafe { CFDictionaryGetValue(dict, key) };
        let on_console =
            !val.is_null() && unsafe { CFBooleanGetValue(val) };
        unsafe {
            CFRelease(key);
            CFRelease(dict);
        }
        !on_console
    }

    /// 从 LaunchDaemon 配置里取接管令牌（plist 全局可读）
    pub fn token_from_plist() -> Option<String> {
        let raw = std::fs::read_to_string(LAUNCHD_PLIST).ok()?;
        let i = raw.find("--token")?;
        let rest = raw[i + "--token".len()..].trim();
        let tok: String = rest
            .chars()
            .skip_while(|c| c.is_whitespace() || *c == '<' || *c == '/' || *c == 's')
            .take_while(|c| c.is_ascii_hexdigit())
            .collect();
        if tok.len() >= 16 {
            Some(tok)
        } else {
            None
        }
    }

    pub fn plist_installed() -> bool {
        std::path::Path::new(LAUNCHD_PLIST).exists()
    }

    /// root 输入助手主循环：unix 套接字收 InputEvent → enigo 注入（root 可达登录窗口）
    pub fn run_helper(token: &str) -> ! {
        let _ = std::fs::remove_file(SOCK_PATH);
        let listener = UnixListener::bind(SOCK_PATH).expect("助手套接字绑定失败");
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(SOCK_PATH, std::fs::Permissions::from_mode(0o666));
        tracing::info!("macOS 输入助手就绪（{SOCK_PATH}）");
        loop {
            let (stream, _) = match listener.accept() {
                Ok(v) => v,
                Err(_) => continue,
            };
            serve_client(stream, token);
        }
    }

    fn serve_client(stream: std::os::unix::net::UnixStream, token: &str) {
        let mut reader = match stream.try_clone() {
            Ok(f) => f,
            Err(_) => return,
        };
        let write = match stream.try_clone() {
            Ok(f) => f,
            Err(_) => return,
        };
        // 令牌握手
        let ok = (|| -> Option<()> {
            let (kind, payload) = read_frame(&mut reader).ok()?;
            if kind != KIND_JSON {
                return None;
            }
            match serde_json::from_slice::<SecureRequest>(&payload).ok()? {
                SecureRequest::Hello { token: t } if constant_eq(&t, token) => Some(()),
                _ => None,
            }
        })();
        if ok.is_none() {
            let _ = write_frame(
                &mut { write },
                KIND_JSON,
                &serde_json::to_vec(&SecureResponse::Busy).unwrap_or_default(),
            );
            return;
        }
        let _ = write_frame(
            &mut { write },
            KIND_JSON,
            &serde_json::to_vec(&SecureResponse::Ok).unwrap_or_default(),
        );

        let bounds = Arc::new(Mutex::new((0u32, 0u32)));
        let input_tx = crate::remote::spawn_input_thread(bounds, None);
        loop {
            let (kind, payload) = match read_frame(&mut reader) {
                Ok(v) => v,
                Err(_) => break, // agent 断开（解锁切回本地）
            };
            if kind != KIND_JSON {
                continue;
            }
            if let Ok(SecureRequest::Input { ev }) =
                serde_json::from_slice::<SecureRequest>(&payload)
            {
                let _ = crate::remote::apply_input(&input_tx, ev);
            }
        }
        let _ = write;
    }

    // ===== agent 侧接管器（仅输入路由；采集保持本地用户级）=====
    pub struct AgentSecure {
        route: Arc<Route>,
        token: Option<String>,
        last_check: std::time::Instant,
    }

    impl AgentSecure {
        pub fn new() -> Self {
            AgentSecure {
                route: Arc::new(Route::new()),
                token: token_from_plist(),
                last_check: std::time::Instant::now() - std::time::Duration::from_secs(10),
            }
        }

        pub fn route(&self) -> Arc<Route> {
            self.route.clone()
        }

        /// 每次采集迭代调用（2s 节流）。锁屏时连接 root 输入助手（唤醒显示器）
        pub fn poll(&mut self) {
            if self.last_check.elapsed() < std::time::Duration::from_secs(2) {
                return;
            }
            self.last_check = std::time::Instant::now();
            let locked = console_locked();
            let was = self.route.locked.swap(locked, Ordering::Relaxed);
            if locked && !was {
                let Some(token) = &self.token else {
                    tracing::info!("屏幕已锁定：未安装输入助手（设置页安装后可远程解锁）");
                    return;
                };
                match UnixInputClient::connect(token) {
                    Ok(c) => {
                        *self.route.sock.lock().unwrap() = Some(c);
                        // caffeinate -u 模拟用户活动唤醒显示器
                        let _ = std::process::Command::new("/usr/bin/caffeinate")
                            .args(["-u", "-t", "2"])
                            .spawn();
                        tracing::info!("屏幕已锁定：输入切换到 root 助手（可远程输密码解锁）");
                    }
                    Err(e) => tracing::info!("锁屏输入助手连接失败: {e}"),
                }
            } else if !locked && was {
                if let Some(c) = self.route.sock.lock().unwrap().take() {
                    c.send_stop();
                }
                tracing::info!("屏幕已解锁：切回本地注入");
            }
        }
    }

    // ===== agent 侧客户端 =====

    pub struct UnixInputClient {
        stream: std::os::unix::net::UnixStream,
    }

    impl UnixInputClient {
        pub fn connect(token: &str) -> std::io::Result<Self> {
            use std::os::unix::net::UnixStream;
            let stream = UnixStream::connect(SOCK_PATH)?;
            let mut w = stream.try_clone()?;
            write_frame(
                &mut w,
                KIND_JSON,
                &serde_json::to_vec(&SecureRequest::Hello {
                    token: token.to_string(),
                })
                .unwrap_or_default(),
            )?;
            let (kind, payload) = read_frame(&mut w)?;
            if kind != KIND_JSON {
                return Err(std::io::Error::new(std::io::ErrorKind::InvalidData, "握手格式"));
            }
            match serde_json::from_slice::<SecureResponse>(&payload) {
                Ok(SecureResponse::Ok) => Ok(UnixInputClient { stream }),
                _ => Err(std::io::Error::new(
                    std::io::ErrorKind::PermissionDenied,
                    "助手拒绝",
                )),
            }
        }

        pub fn send_input(&self, ev: &InputEvent) {
            if let Ok(payload) = serde_json::to_vec(&SecureRequest::Input { ev: ev.clone() }) {
                let mut w = match self.stream.try_clone() {
                    Ok(w) => w,
                    Err(_) => return,
                };
                let _ = write_frame(&mut w, KIND_JSON, &payload);
            }
        }

        pub fn send_stop(&self) {
            if let Ok(payload) = serde_json::to_vec(&SecureRequest::Stop) {
                let mut w = match self.stream.try_clone() {
                    Ok(w) => w,
                    Err(_) => return,
                };
                let _ = write_frame(&mut w, KIND_JSON, &payload);
            }
        }
    }
}

// ===== 平台入口分发 =====
/// `yunshu secure --token <hex>`：进入 SYSTEM 安全助手（阻塞，永不返回）。
/// 仅 Windows 且由服务拉起，其余情况返回 false 走正常启动流程
pub fn try_run_helper_command() -> bool {
    let mut args = std::env::args().skip(1);
    if args.next().as_deref() != Some("secure") {
        return false;
    }
    let token = args
        .next()
        .filter(|a| a == "--token")
        .and_then(|_| args.next())
        .unwrap_or_default();
    #[cfg(windows)]
    {
        win::run_helper(&token)
    }
    #[cfg(target_os = "macos")]
    {
        macos::run_helper(&token)
    }
    #[cfg(not(any(windows, target_os = "macos")))]
    false
}
