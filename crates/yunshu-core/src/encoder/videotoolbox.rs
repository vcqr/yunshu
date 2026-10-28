//! macOS VideoToolbox 硬件 H.264 编码器。
//!
//! 输出与 openh264 软编同规格：Annex-B、Baseline AutoLevel、SPS/PPS 随每个
//! IDR 内联（从样本 format description 提取拼在最前），观看端中途加入即可
//! 解码。AllowFrameReordering 关闭保证无 B 帧；MaxFrameDelayCount=0 让输出
//! 在 encode_frame 返回前回调（偶发异步路径以 park 短等兜底）。任一会话
//! 属性设置失败即整体失败，由 make_encoder 回退 openh264 软编。

use std::ffi::c_void;
use std::ptr::{null, null_mut, NonNull};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use objc2_core_foundation::{CFBoolean, CFDictionary, CFNumber, CFRetained, CFString, CFType};
use objc2_core_media::{
    CMTime, CMSampleBuffer, CMVideoFormatDescriptionGetH264ParameterSetAtIndex,
    kCMVideoCodecType_H264,
};
use objc2_core_video::{
    CVPixelBuffer, CVPixelBufferCreate, CVPixelBufferGetBaseAddress, CVPixelBufferGetBytesPerRow,
    CVPixelBufferLockBaseAddress, CVPixelBufferLockFlags, CVPixelBufferUnlockBaseAddress,
};
use objc2_video_toolbox::{
    kVTCompressionPropertyKey_AllowFrameReordering, kVTCompressionPropertyKey_AverageBitRate,
    kVTCompressionPropertyKey_ExpectedFrameRate, kVTCompressionPropertyKey_MaxFrameDelayCount,
    kVTCompressionPropertyKey_MaxKeyFrameInterval, kVTCompressionPropertyKey_ProfileLevel,
    kVTCompressionPropertyKey_RealTime, kVTProfileLevel_H264_Baseline_AutoLevel,
    kVTEncodeFrameOptionKey_ForceKeyFrame, VTCompressionSession, VTEncodeInfoFlags, VTSession,
    VTSessionSetProperty,
};

use super::{EncodedFrame, VideoEncoder, CODEC_H264};
use crate::remote::Frame;

/// Annex-B 起始码
const START_CODE: [u8; 4] = [0, 0, 0, 1];
/// kCVPixelFormatType_32BGRA 的 FourCharCode（'BGRA'）
const PIXEL_FORMAT_BGRA: u32 = 0x4247_5241;

/// 帧输出回调的收集槽：VT 在 encode_frame 返回前后把编码结果写进来
#[derive(Default)]
struct FrameSink {
    data: Option<Vec<u8>>,
    keyframe: bool,
    done: AtomicBool,
}

unsafe extern "C-unwind" fn vt_output_cb(
    _refcon: *mut c_void,
    source_refcon: *mut c_void,
    status: i32,
    info: VTEncodeInfoFlags,
    sample: *mut CMSampleBuffer,
) {
    let sink = unsafe { &mut *(source_refcon as *mut FrameSink) };
    if status == 0
        && !info.contains(VTEncodeInfoFlags::FrameDropped)
        && !sample.is_null()
    {
        // 样本只在回调期间有效，就地转 Annex-B
        let sample = unsafe { &*sample };
        if let Some((data, keyframe)) = sample_to_annexb(sample) {
            sink.data = Some(data);
            sink.keyframe = keyframe;
        }
    }
    sink.done.store(true, Ordering::Release);
}

/// 把 VT 输出的 AVCC 样本（4 字节大端长度前缀 NAL 队列）转成 Annex-B；
/// IDR 帧把 format description 里的 SPS/PPS 拼在最前，返回 (数据, 是否关键帧)
fn sample_to_annexb(sample: &CMSampleBuffer) -> Option<(Vec<u8>, bool)> {
    let block = unsafe { sample.data_buffer() }?;
    let len = unsafe { block.data_length() };
    if len < 4 {
        return None;
    }
    let mut avcc = vec![0u8; len];
    let dst = unsafe { NonNull::new_unchecked(avcc.as_mut_ptr().cast::<c_void>()) };
    if unsafe { block.copy_data_bytes(0, len, dst) } != 0 {
        return None;
    }

    // 第一遍：切分 NAL，找 IDR（type == 5）
    let mut nals = Vec::new();
    let mut pos = 0usize;
    let mut has_idr = false;
    while pos + 4 <= avcc.len() {
        let n =
            u32::from_be_bytes([avcc[pos], avcc[pos + 1], avcc[pos + 2], avcc[pos + 3]]) as usize;
        pos += 4;
        let end = (pos + n).min(avcc.len());
        if end > pos && avcc[pos] & 0x1f == 5 {
            has_idr = true;
        }
        nals.push((pos, end));
        pos = end;
    }

    let mut out = Vec::with_capacity(len + 128);
    if has_idr {
        // SPS/PPS 内联：取自样本携带的 format description
        let desc = unsafe { sample.format_description() }?;
        for i in 0.. {
            let mut ps: *const u8 = null();
            let mut ps_len = 0usize;
            let mut count = 0usize;
            let mut hdr_len: i32 = 0;
            let st = unsafe {
                CMVideoFormatDescriptionGetH264ParameterSetAtIndex(
                    &desc,
                    i,
                    &mut ps,
                    &mut ps_len,
                    &mut count,
                    &mut hdr_len,
                )
            };
            if st != 0 || ps_len == 0 {
                break;
            }
            out.extend_from_slice(&START_CODE);
            out.extend_from_slice(unsafe { std::slice::from_raw_parts(ps, ps_len) });
            if i + 1 >= count {
                break;
            }
        }
    }
    for (s, e) in nals {
        out.extend_from_slice(&START_CODE);
        out.extend_from_slice(&avcc[s..e]);
    }
    (!out.is_empty()).then_some((out, has_idr))
}

/// CF 布尔值装箱为 CFType（VTSessionSetProperty 的参数类型）
fn cf_bool(v: bool) -> CFRetained<CFType> {
    unsafe { CFRetained::cast_unchecked(CFRetained::retain(NonNull::from(CFBoolean::new(v)))) }
}

/// CF 整数装箱为 CFType
fn cf_i32(v: i32) -> CFRetained<CFType> {
    unsafe { CFRetained::cast_unchecked(CFNumber::new_i32(v)) }
}

/// CF 字符串常量装箱为 CFType
fn cf_str(s: &'static CFString) -> CFRetained<CFType> {
    unsafe { CFRetained::cast_unchecked(CFRetained::retain(NonNull::from(s))) }
}

pub struct VtH264Encoder {
    session: CFRetained<VTCompressionSession>,
    fps: u32,
    frame_count: u64,
    bitrate_bps: u32,
}

// 会话只在创建它的采集线程上使用；Send 仅为满足 trait 约束（整体移交，无并发触碰）
unsafe impl Send for VtH264Encoder {}

impl VtH264Encoder {
    pub fn new(width: u32, height: u32, fps: u32) -> Option<Self> {
        unsafe {
            let mut session: *mut VTCompressionSession = null_mut();
            let status = VTCompressionSession::create(
                None,
                width as i32,
                height as i32,
                kCMVideoCodecType_H264,
                None,
                None,
                None,
                Some(vt_output_cb),
                null_mut(),
                NonNull::from(&mut session),
            );
            if status != 0 || session.is_null() {
                tracing::debug!("VideoToolbox 会话创建失败: status={status}");
                return None;
            }
            let session = CFRetained::from_raw(NonNull::new_unchecked(session));
            // VTSessionSetProperty 的参数是父类型 VTSession：CF 对象同布局，
            // 直接按 CF 惯例转换（同一对象指针，无所有权变化）
            let vt: &VTSession = &*(std::ptr::from_ref(&*session).cast::<VTSession>());

            // 与软编同公式：屏幕内容约 0.3 bit/px/帧，钳 1.5M..16M
            let pixels = (width.max(1) as u64) * (height.max(1) as u64);
            let bitrate =
                ((pixels * fps.max(1) as u64 * 3) / 10 / 8).clamp(1_500_000, 16_000_000) as i32;

            // 任一属性设置失败都直接放弃硬编（回退软编），保证行为可预期
            let mut ok = true;
            let mut set = |key: &'static CFString, value: CFRetained<CFType>| {
                ok &= VTSessionSetProperty(vt, key, Some(&value)) == 0;
            };
            set(&kVTCompressionPropertyKey_RealTime, cf_bool(true));
            set(&kVTCompressionPropertyKey_AllowFrameReordering, cf_bool(false));
            set(&kVTCompressionPropertyKey_AverageBitRate, cf_i32(bitrate));
            set(&kVTCompressionPropertyKey_MaxKeyFrameInterval, cf_i32(100));
            set(
                &kVTCompressionPropertyKey_ExpectedFrameRate,
                cf_i32(fps.max(1) as i32),
            );
            // 0 = 输出在 encode_frame 返回前回调完成；部分硬编会话（Apple Silicon
            // 媒体引擎）不支持该属性（kVTPropertyUnsupportedErr），忽略——encode()
            // 里的回调等待逻辑兜底异步输出
            let st = VTSessionSetProperty(
                vt,
                &kVTCompressionPropertyKey_MaxFrameDelayCount,
                Some(&cf_i32(0)),
            );
            if st != 0 {
                tracing::debug!("VT MaxFrameDelayCount 不支持（{st}），走异步兜底");
            }
            set(
                &kVTCompressionPropertyKey_ProfileLevel,
                cf_str(&kVTProfileLevel_H264_Baseline_AutoLevel),
            );
            if !ok {
                tracing::debug!("VideoToolbox 会话属性设置失败，放弃硬编");
                return None;
            }
            Some(Self {
                session,
                fps: fps.max(1),
                frame_count: 0,
                bitrate_bps: bitrate as u32,
            })
        }
    }

    /// RGBA 帧写入新 CVPixelBuffer（BGRA，R/B 交换；VT 不接受 RGBA 直喂）
    fn make_pixel_buffer(&self, frame: &Frame) -> Option<CFRetained<CVPixelBuffer>> {
        unsafe {
            let mut pb: *mut CVPixelBuffer = null_mut();
            let st = CVPixelBufferCreate(
                None,
                frame.width as usize,
                frame.height as usize,
                PIXEL_FORMAT_BGRA,
                None,
                NonNull::from(&mut pb),
            );
            if st != 0 || pb.is_null() {
                return None;
            }
            let pix = CFRetained::from_raw(NonNull::new_unchecked(pb));
            if CVPixelBufferLockBaseAddress(&pix, CVPixelBufferLockFlags::empty()) != 0 {
                return None;
            }
            let base = CVPixelBufferGetBaseAddress(&pix) as *mut u8;
            let bpr = CVPixelBufferGetBytesPerRow(&pix);
            let w = frame.width as usize;
            if base.is_null() {
                CVPixelBufferUnlockBaseAddress(&pix, CVPixelBufferLockFlags::empty());
                return None;
            }
            for row in 0..frame.height as usize {
                let src = &frame.rgba[row * w * 4..][..w * 4];
                let dst = base.add(row * bpr);
                for (i, px) in src.as_chunks::<4>().0.iter().enumerate() {
                    let d = dst.add(i * 4);
                    *d = px[2];
                    *d.add(1) = px[1];
                    *d.add(2) = px[0];
                    *d.add(3) = px[3];
                }
            }
            CVPixelBufferUnlockBaseAddress(&pix, CVPixelBufferLockFlags::empty());
            Some(pix)
        }
    }
}

impl VideoEncoder for VtH264Encoder {
    fn codec(&self) -> u8 {
        CODEC_H264
    }

    fn backend(&self) -> &'static str {
        "videotoolbox"
    }

    fn base_bitrate(&self) -> u32 {
        self.bitrate_bps
    }

    /// VT 支持会话中改 AverageBitRate（下一帧生效）
    fn set_target(&mut self, _quality: u8, bitrate_bps: u32) {
        if bitrate_bps == 0 {
            return;
        }
        self.bitrate_bps = bitrate_bps;
        unsafe {
            let vt: &VTSession = &*(std::ptr::from_ref(&*self.session).cast::<VTSession>());
            VTSessionSetProperty(
                vt,
                &kVTCompressionPropertyKey_AverageBitRate,
                Some(&cf_i32(bitrate_bps as i32)),
            );
        }
    }

    fn encode(&mut self, frame: &Frame, force_key: bool) -> Option<EncodedFrame> {
        let pix = self.make_pixel_buffer(frame)?;

        // 强推关键帧：帧级属性 { kVTEncodeFrameOptionKey_ForceKeyFrame: true }
        let frame_props = force_key.then(|| unsafe {
            CFDictionary::<CFString, CFBoolean>::from_slices(
                &[&kVTEncodeFrameOptionKey_ForceKeyFrame],
                &[CFBoolean::new(true)],
            )
        });

        let sink = FrameSink::default();
        let mut info = VTEncodeInfoFlags::empty();
        let pts = unsafe { CMTime::new(self.frame_count as i64, self.fps as i32) };
        let dur = unsafe { CMTime::new(1, self.fps as i32) };
        let status = unsafe {
            self.session.encode_frame(
                &pix,
                pts,
                dur,
                frame_props.as_ref().map(|d| d.cast_unchecked()),
                &sink as *const FrameSink as *mut c_void,
                &mut info,
            )
        };
        if status != 0 {
            tracing::debug!("VT encode_frame 失败: status={status}");
            return None;
        }
        // MaxFrameDelayCount=0 下回调通常同步完成；偶发异步时短等兜底
        let deadline = Instant::now() + Duration::from_millis(200);
        while !sink.done.load(Ordering::Acquire) {
            if Instant::now() >= deadline {
                tracing::debug!("VT 输出回调超时");
                return None;
            }
            std::thread::park_timeout(Duration::from_millis(2));
        }
        self.frame_count += 1;
        sink.data.map(|data| EncodedFrame {
            data,
            keyframe: sink.keyframe,
        })
    }
}
