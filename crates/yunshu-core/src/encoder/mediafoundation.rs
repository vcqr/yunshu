//! Windows MediaFoundation 硬件 H.264 编码器。
//!
//! 枚举 MFT_CATEGORY_VIDEO_ENCODER 的 HARDWARE MFT（GPU 编码器，异步 MFT），
//! 输入 ARGB32 系统内存样本，输出 Annex-B H.264（与 openh264/VideoToolbox 同
//! 规格）。关键帧用 ICodecAPI 的 CODECAPI_AVEncVideoForceKeyFrame 强推，输出
//! 样本以 MFSampleExtension_CleanPoint 判定关键帧。
//!
//! 流程：事件驱动（METransformNeedInput → ProcessInput，METransformHaveOutput
//! → ProcessOutput），encode() 内同步泵事件；任一步失败由 make_encoder 回退
//! openh264 软编。真机行为待 Windows 实测（CI 只保证编译）。

use std::ptr::null_mut;
use std::mem::ManuallyDrop;
use std::time::{Duration, Instant};

use windows::core::Interface;
use windows::Win32::Media::MediaFoundation::{
    IMFActivate, IMFMediaEventGenerator, IMFSample, IMFTransform, MFCreateMediaType,
    MFCreateMemoryBuffer, MFCreateSample, MF_E_NO_EVENTS_AVAILABLE,
    MF_E_TRANSFORM_NEED_MORE_INPUT, MF_EVENT_FLAG_NO_WAIT, MF_MT_AVG_BITRATE, MF_MT_FRAME_RATE,
    MF_MT_FRAME_SIZE, MF_MT_INTERLACE_MODE, MF_MT_MAJOR_TYPE, MF_MT_PIXEL_ASPECT_RATIO,
    MF_MT_SUBTYPE, MFMediaType_Video, MF_TRANSFORM_ASYNC_UNLOCK, MFVideoFormat_ARGB32,
    MFVideoFormat_H264, MFVideoInterlace_Progressive, MEError, METransformHaveOutput,
    METransformNeedInput, MFT_CATEGORY_VIDEO_ENCODER, MFT_ENUM_FLAG, MFT_ENUM_FLAG_HARDWARE,
    MFT_ENUM_FLAG_SORTANDFILTER, MFT_MESSAGE_COMMAND_DRAIN, MFT_MESSAGE_NOTIFY_BEGIN_STREAMING,
    MFT_MESSAGE_NOTIFY_END_OF_STREAM, MFT_MESSAGE_NOTIFY_START_OF_STREAM,
    MFT_OUTPUT_DATA_BUFFER, MFT_REGISTER_TYPE_INFO, MFSampleExtension_CleanPoint, ICodecAPI,
};
use windows::Win32::Foundation::RPC_E_CHANGED_MODE;
use windows::Win32::System::Com::{CoInitializeEx, CoTaskMemFree, COINIT_APARTMENTTHREADED};
use windows::Win32::System::Variant::{VARIANT, VARIANT_0, VARIANT_0_0, VARIANT_0_0_0, VT_UI4};
use windows::Win32::Media::MediaFoundation::{
    CODECAPI_AVEncCommonMeanBitRate, CODECAPI_AVEncVideoForceKeyFrame, CODECAPI_AVLowLatencyMode,
};

use super::{EncodedFrame, VideoEncoder, CODEC_H264};
use crate::remote::Frame;

/// MF 时间戳单位：100ns
const AVGCLOCK: i64 = 10_000_000;

pub struct MfH264Encoder {
    transform: IMFTransform,
    events: IMFMediaEventGenerator,
    codec_api: Option<ICodecAPI>,
    width: u32,
    height: u32,
    fps: u32,
    frame_count: u64,
    bitrate_bps: u32,
}

// COM 对象只在本线程使用；Send 仅为满足 trait 约束（整体移交，无并发触碰）
unsafe impl Send for MfH264Encoder {}

impl MfH264Encoder {
    pub fn new(width: u32, height: u32, fps: u32) -> Option<Self> {
        unsafe {
            // 采集线程可能已被 WGC/DXGI 初始化过 COM：S_OK/S_FALSE/CHANGED_MODE 均可继续
            let hr = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
            if hr.is_err() && hr != RPC_E_CHANGED_MODE {
                tracing::debug!("MediaFoundation: CoInitializeEx 失败 {hr:?}");
                return None;
            }
            let transform = enum_hw_h264_encoder(width, height, fps)?;
            let events = transform.cast::<IMFMediaEventGenerator>().ok()?;
            if transform
                .ProcessMessage(MFT_MESSAGE_NOTIFY_BEGIN_STREAMING, 0)
                .is_err()
                || transform
                    .ProcessMessage(MFT_MESSAGE_NOTIFY_START_OF_STREAM, 0)
                    .is_err()
            {
                tracing::debug!("MediaFoundation: 流启动失败");
                return None;
            }
            let codec_api = transform.cast::<ICodecAPI>().ok();
            // 低延迟模式尽力开启（部分 GPU MFT 不支持，忽略失败）
            if let Some(api) = &codec_api {
                let _ = api.SetValue(&CODECAPI_AVLowLatencyMode, &ui4_variant(1));
            }
            // 码率与 configure_types 同公式
            let pixels = (width.max(1) as u64) * (height.max(1) as u64);
            let bitrate =
                ((pixels * fps.max(1) as u64 * 3) / 10 / 8).clamp(1_500_000, 16_000_000) as u32;
            Some(Self {
                transform,
                events,
                codec_api,
                width,
                height,
                fps: fps.max(1),
                frame_count: 0,
                bitrate_bps: bitrate,
            })
        }
    }

    /// RGBA 帧打包为 MF 样本（BGRA，R/B 交换）
    fn make_sample(&self, frame: &Frame) -> Option<IMFSample> {
        unsafe {
            let len = (self.width as usize * self.height as usize * 4) as u32;
            let buffer = MFCreateMemoryBuffer(len).ok()?;
            let mut ptr = null_mut::<u8>();
            buffer.Lock(&mut ptr, None, None).ok()?;
            let w = self.width as usize;
            for row in 0..self.height as usize {
                let src = &frame.rgba[row * w * 4..][..w * 4];
                let dst = ptr.add(row * w * 4);
                for (i, px) in src.as_chunks::<4>().0.iter().enumerate() {
                    let d = dst.add(i * 4);
                    *d = px[2];
                    *d.add(1) = px[1];
                    *d.add(2) = px[0];
                    *d.add(3) = px[3];
                }
            }
            buffer.Unlock().ok()?;
            buffer.SetCurrentLength(len).ok()?;

            let sample = MFCreateSample().ok()?;
            sample.AddBuffer(&buffer).ok()?;
            sample
                .SetSampleTime(self.frame_count as i64 * AVGCLOCK / self.fps as i64)
                .ok()?;
            sample.SetSampleDuration(AVGCLOCK / self.fps as i64).ok()?;
            Some(sample)
        }
    }

    /// 泵事件直到等到指定事件；超时/错误返回 false
    fn wait_event(&self, kind: u32, ms: u64) -> bool {
        let deadline = Instant::now() + Duration::from_millis(ms);
        loop {
            match unsafe { self.events.GetEvent(MF_EVENT_FLAG_NO_WAIT) } {
                Ok(ev) => {
                    let Ok(t) = (unsafe { ev.GetType() }) else {
                        return false;
                    };
                    if t == kind {
                        return true;
                    }
                    if t == MEError.0 as u32 {
                        tracing::debug!("MediaFoundation: 收到 MEError");
                        return false;
                    }
                    // 无关事件继续泵
                }
                Err(e) if e.code() == MF_E_NO_EVENTS_AVAILABLE => {
                    if Instant::now() >= deadline {
                        tracing::debug!("MediaFoundation: 等事件超时");
                        return false;
                    }
                    std::thread::sleep(Duration::from_millis(2));
                }
                Err(e) => {
                    tracing::debug!("MediaFoundation: GetEvent 失败 {e:?}");
                    return false;
                }
            }
        }
    }

    fn read_output(&self) -> Option<EncodedFrame> {
        unsafe {
            let mut out = MFT_OUTPUT_DATA_BUFFER {
                dwStreamID: 0,
                pSample: ManuallyDrop::new(None),
                dwStatus: 0,
                pEvents: ManuallyDrop::new(None),
            };
            let mut status = 0u32;
            if let Err(e) = self
                .transform
                .ProcessOutput(0, std::slice::from_mut(&mut out), &mut status)
            {
                if e.code() == MF_E_TRANSFORM_NEED_MORE_INPUT {
                    return None;
                }
                tracing::debug!("MediaFoundation: ProcessOutput 失败 {e:?}");
                return None;
            }
            let sample = ManuallyDrop::take(&mut out.pSample);
            let _events = ManuallyDrop::take(&mut out.pEvents);
            let sample = sample?;
            let keyframe = sample.GetUINT32(&MFSampleExtension_CleanPoint).unwrap_or(0) != 0;
            let buffer = sample.ConvertToContiguousBuffer().ok()?;
            let mut ptr = null_mut::<u8>();
            let mut max_len = 0u32;
            let mut cur_len = 0u32;
            buffer.Lock(&mut ptr, Some(&mut max_len), Some(&mut cur_len)).ok()?;
            let data = std::slice::from_raw_parts(ptr, cur_len as usize).to_vec();
            buffer.Unlock().ok()?;
            (!data.is_empty()).then_some(EncodedFrame { data, keyframe })
        }
    }
}

impl VideoEncoder for MfH264Encoder {
    fn codec(&self) -> u8 {
        CODEC_H264
    }

    fn backend(&self) -> &'static str {
        "mediafoundation"
    }

    fn base_bitrate(&self) -> u32 {
        self.bitrate_bps
    }

    /// ICodecAPI 会话中改平均码率（部分 GPU MFT 忽略，无害）
    fn set_target(&mut self, _quality: u8, bitrate_bps: u32) {
        if bitrate_bps == 0 {
            return;
        }
        self.bitrate_bps = bitrate_bps;
        if let Some(api) = &self.codec_api {
            unsafe {
                let _ = api.SetValue(&CODECAPI_AVEncCommonMeanBitRate, &ui4_variant(bitrate_bps));
            }
        }
    }

    fn encode(&mut self, frame: &Frame, force_key: bool) -> Option<EncodedFrame> {
        if force_key {
            if let Some(api) = &self.codec_api {
                unsafe {
                    let _ = api.SetValue(&CODECAPI_AVEncVideoForceKeyFrame, &ui4_variant(1));
                }
            }
        }
        // 事件驱动：先等 NeedInput 再喂帧，再等 HaveOutput 取产物
        if !self.wait_event(METransformNeedInput.0 as u32, 500) {
            return None;
        }
        let sample = self.make_sample(frame)?;
        unsafe { self.transform.ProcessInput(0, &sample, 0).ok()? };
        if !self.wait_event(METransformHaveOutput.0 as u32, 500) {
            return None;
        }
        let out = self.read_output();
        self.frame_count += 1;
        out
    }
}

impl Drop for MfH264Encoder {
    fn drop(&mut self) {
        unsafe {
            let _ = self.transform.ProcessMessage(MFT_MESSAGE_NOTIFY_END_OF_STREAM, 0);
            let _ = self.transform.ProcessMessage(MFT_MESSAGE_COMMAND_DRAIN, 0);
        }
    }
}

/// VT_UI4=1 的 VARIANT（ICodecAPI 布尔/开关类属性惯用形态）
fn ui4_variant(v: u32) -> VARIANT {
    let mut var = VARIANT::default();
    // 整体写联合字段（VARIANT_0_0_0 为单字段联合初始化）
    var.Anonymous = VARIANT_0 {
        Anonymous: ManuallyDrop::new(VARIANT_0_0 {
            vt: VT_UI4,
            wReserved1: 0,
            wReserved2: 0,
            wReserved3: 0,
            Anonymous: VARIANT_0_0_0 { ulVal: v },
        }),
    };
    var
}

/// 枚举硬件 H.264 MFT：激活 → 异步解锁 → 配置输入输出类型，逐候选尝试；
/// 无硬件 MFT 或全部配置失败返回 None（回退软编）
fn enum_hw_h264_encoder(width: u32, height: u32, fps: u32) -> Option<IMFTransform> {
    unsafe {
        let in_type = MFT_REGISTER_TYPE_INFO {
            guidMajorType: MFMediaType_Video,
            guidSubtype: MFVideoFormat_ARGB32,
        };
        let out_type = MFT_REGISTER_TYPE_INFO {
            guidMajorType: MFMediaType_Video,
            guidSubtype: MFVideoFormat_H264,
        };
        let mut activates: *mut Option<IMFActivate> = null_mut();
        let mut count = 0u32;
        let flags = MFT_ENUM_FLAG(MFT_ENUM_FLAG_HARDWARE.0 | MFT_ENUM_FLAG_SORTANDFILTER.0);
        if windows::Win32::Media::MediaFoundation::MFTEnumEx(
            MFT_CATEGORY_VIDEO_ENCODER,
            flags,
            Some(&in_type),
            Some(&out_type),
            &mut activates,
            &mut count,
        )
        .is_err()
        {
            return None;
        }
        if activates.is_null() {
            return None;
        }
        // 槽位是 +1 引用的 COM 指针：read() 接管所有权，随后释放 MFTEnumEx 分配的数组
        let mut candidates = Vec::new();
        for i in 0..count as usize {
            if let Some(act) = activates.add(i).read() {
                candidates.push(act);
            }
        }
        CoTaskMemFree(Some(activates.cast()));

        for act in candidates {
            let Ok(t) = act.ActivateObject::<IMFTransform>() else {
                continue;
            };
            // 异步 MFT 必须先解锁再配置/处理
            let Ok(attrs) = t.GetAttributes() else {
                continue;
            };
            if attrs.SetUINT32(&MF_TRANSFORM_ASYNC_UNLOCK, 1).is_err() {
                continue;
            }
            if configure_types(&t, width, height, fps).is_some() {
                return Some(t);
            }
        }
        None
    }
}

/// 输入 ARGB32 渐进 / 输出 H.264（码率公式与软编一致：0.3 bit/px/帧，钳 1.5M..16M）
fn configure_types(t: &IMFTransform, width: u32, height: u32, fps: u32) -> Option<()> {
    unsafe {
        let in_mt = MFCreateMediaType().ok()?;
        in_mt.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video).ok()?;
        in_mt.SetGUID(&MF_MT_SUBTYPE, &MFVideoFormat_ARGB32).ok()?;
        in_mt
            .SetUINT32(&MF_MT_INTERLACE_MODE, MFVideoInterlace_Progressive.0 as u32)
            .ok()?;
        in_mt
            .SetUINT64(&MF_MT_FRAME_SIZE, ((width as u64) << 32) | height as u64)
            .ok()?;
        in_mt
            .SetUINT64(&MF_MT_FRAME_RATE, ((fps.max(1) as u64) << 32) | 1)
            .ok()?;
        in_mt.SetUINT64(&MF_MT_PIXEL_ASPECT_RATIO, (1u64 << 32) | 1).ok()?;
        t.SetInputType(0, Some(&in_mt), 0).ok()?;

        let pixels = (width.max(1) as u64) * (height.max(1) as u64);
        let bitrate =
            ((pixels * fps.max(1) as u64 * 3) / 10 / 8).clamp(1_500_000, 16_000_000) as u32;
        let out_mt = MFCreateMediaType().ok()?;
        out_mt.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video).ok()?;
        out_mt.SetGUID(&MF_MT_SUBTYPE, &MFVideoFormat_H264).ok()?;
        out_mt.SetUINT32(&MF_MT_AVG_BITRATE, bitrate).ok()?;
        out_mt
            .SetUINT64(&MF_MT_FRAME_SIZE, ((width as u64) << 32) | height as u64)
            .ok()?;
        out_mt
            .SetUINT64(&MF_MT_FRAME_RATE, ((fps.max(1) as u64) << 32) | 1)
            .ok()?;
        out_mt.SetUINT64(&MF_MT_PIXEL_ASPECT_RATIO, (1u64 << 32) | 1).ok()?;
        t.SetOutputType(0, Some(&out_mt), 0).ok()?;
        Some(())
    }
}
