//! Windows 推送式屏幕采集（Windows.Graphics.Capture，DXGI 桌面复制管线）。
//!
//! 对比 xcap 的 GDI 逐帧 BitBlt：WGC 由系统按屏幕更新推送帧（60fps、自带光标、
//! 无全屏同步拷贝），发送侧只做"取最新帧"——与 macOS 的 AVCaptureSession 推送
//! 采集同构。元数据（显示器列表/坐标空间）复用 XcapSource；WGC 启动失败或
//! Win10 1903 以下自动回退 xcap 同步截屏。

use super::{Frame, ScreenSource, XcapSource};
use std::sync::{Arc, Mutex};
use windows_capture::{
    capture::{CaptureControl, Context, GraphicsCaptureApiHandler},
    frame::Frame,
    monitor::Monitor,
    settings::{
        ColorFormat, CursorCaptureSettings, DirtyRegionSettings, DrawBorderSettings,
        MinimumUpdateIntervalSettings, SecondaryWindowSettings, Settings,
    },
};

/// 帧回调与采集线程共享的"最新帧"槽：width、height、RGBA
type LatestSlot = Arc<Mutex<Option<(u32, u32, Vec<u8>)>>>;

struct WgcHandler {
    latest: LatestSlot,
}

impl GraphicsCaptureApiHandler for WgcHandler {
    type Flags = LatestSlot;
    type Error = Box<dyn std::error::Error + Send + Sync>;

    fn new(ctx: Context<Self::Flags>) -> Result<Self, Self::Error> {
        Ok(WgcHandler { latest: ctx.flags })
    }

    fn on_frame_arrived(
        &mut self,
        frame: &mut Frame,
        _capture_control: windows_capture::capture::InternalCaptureControl,
    ) -> Result<(), Self::Error> {
        let (w, h) = (frame.width.max(0) as u32, frame.height.max(0) as u32);
        if w == 0 || h == 0 {
            return Ok(());
        }
        let mut row_padded = Vec::new();
        let rgba = frame.buffer()?.as_nopadding_buffer(&mut row_padded).to_vec();
        if rgba.len() == (w * h * 4) as usize {
            *self.latest.lock().unwrap() = Some((w, h, rgba));
        }
        Ok(())
    }
}

/// 推送式采集源：显示器元数据/坐标空间/光标回退全部委托内部 XcapSource，
/// 帧则优先取 WGC 最新槽（Rgba8，含光标），槽空（未出帧/异常）回退同步截屏
pub struct WgcSource {
    xcap: XcapSource,
    latest: LatestSlot,
    control: Option<CaptureControl<WgcHandler, Box<dyn std::error::Error + Send + Sync>>>,
    current: usize,
}

impl WgcSource {
    /// 主屏；WGC 会话启动失败返回 None（调用方回退 xcap）
    pub fn new() -> Option<Self> {
        let xcap = XcapSource::new()?;
        let mut s = WgcSource {
            xcap,
            latest: Arc::new(Mutex::new(None)),
            control: None,
            current: 0,
        };
        s.current = s.xcap.current;
        if !s.start_capture() {
            return None;
        }
        Some(s)
    }

    /// 为当前选中的显示器启动 WGC 会话（旧会话先停）。
    /// windows-capture 的 Monitor::from_index 是 1 基序号，与 xcap 的显示器枚举
    /// 同源（系统枚举顺序），直接用内部当前索引对齐，保证坐标空间与画面同屏
    fn start_capture(&mut self) -> bool {
        // 旧会话必须显式停止：CaptureControl 无 Drop 停止语义，泄漏会继续往槽里推旧屏帧
        if let Some(c) = self.control.take() {
            let _ = c.stop();
        }
        let Ok(monitor) = Monitor::from_index(self.xcap.current + 1) else {
            return false;
        };
        let settings = Settings::new(
            monitor,
            CursorCaptureSettings::WithCursor,
            DrawBorderSettings::WithoutBorder,
            SecondaryWindowSettings::Default,
            MinimumUpdateIntervalSettings::Default,
            DirtyRegionSettings::Default,
            ColorFormat::Rgba8,
            self.latest.clone(),
        );
        match WgcHandler::start_free_threaded(settings) {
            Ok(control) => {
                self.control = Some(control);
                true
            }
            Err(_) => false,
        }
    }
}

impl Drop for WgcSource {
    fn drop(&mut self) {
        if let Some(c) = self.control.take() {
            let _ = c.stop();
        }
    }
}

impl ScreenSource for WgcSource {
    fn input_bounds(&self) -> (u32, u32) {
        self.xcap.input_bounds()
    }

    fn capture(&mut self) -> Option<Frame> {
        // 取 WGC 最新帧；槽空（刚启动/异常）回退 xcap 同步截屏
        if let Some((w, h, rgba)) = self.latest.lock().unwrap().take() {
            return Some(Frame {
                width: w,
                height: h,
                rgba,
            });
        }
        self.xcap.capture()
    }

    fn monitors(&self) -> Vec<super::MonitorInfo> {
        self.xcap.monitors()
    }

    fn set_monitor(&mut self, index: u32) -> bool {
        if self.xcap.set_monitor(index) {
            self.current = index as usize;
            self.latest.lock().unwrap().take();
            self.start_capture();
            true
        } else {
            false
        }
    }

    // WGC 帧已含系统光标，不再叠加软光标（macOS 推送采集同理）
    fn cursor(&self) -> Option<(f32, f32)> {
        None
    }
}
