//! 视频帧编码抽象（一期 JPEG / 二期 H.264）。
//!
//! 调研文档二期要求把「帧编码器」抽象成 trait，为硬编留路：JPEG 为兼容基线，
//! H.264 用 openh264（软件编码，全平台一致；Windows MediaFoundation / macOS
//! VideoToolbox 硬编可按同一 trait 后续接入，输出同为 Annex-B H.264）。
//! 关键决策：H.264 走 Baseline profile（WebCodecs 无 description 的 Annex-B
//! 解码兼容面最大），SPS/PPS 随每个 IDR 内联，观看端中途加入即可解码。

use crate::remote::Frame;
use openh264::encoder::{
    BitRate, Complexity, Encoder as OpenH264, EncoderConfig, FrameRate, FrameType,
    IntraFramePeriod, Profile, RateControlMode, UsageType,
};
use openh264::formats::{RgbaSliceU8, YUVBuffer};

#[cfg(target_vendor = "apple")]
mod videotoolbox;
#[cfg(target_vendor = "apple")]
use videotoolbox::VtH264Encoder;

/// 线上 codec 标识（'V' 帧头 / RemoteUseCodec / 桥 JSON 共用）
pub const CODEC_JPEG: u8 = 0;
pub const CODEC_H264: u8 = 1;

pub fn codec_name(codec: u8) -> &'static str {
    match codec {
        CODEC_H264 => "h264",
        _ => "jpeg",
    }
}

pub struct EncodedFrame {
    pub data: Vec<u8>,
    /// 关键帧（观看端可从此帧开始解码）
    pub keyframe: bool,
}

pub trait VideoEncoder: Send {
    fn encode(&mut self, frame: &Frame, force_key: bool) -> Option<EncodedFrame>;
    fn codec(&self) -> u8;
    /// 编码后端标识（日志/诊断）：如 "videotoolbox"、"openh264"、"jpeg"
    fn backend(&self) -> &'static str;
}

/// JPEG 编码器（一期路径，每帧独立可解，天然"全是关键帧"）
pub struct JpegVideoEncoder {
    pub quality: u8,
}

impl VideoEncoder for JpegVideoEncoder {
    fn codec(&self) -> u8 {
        CODEC_JPEG
    }
    fn backend(&self) -> &'static str {
        "jpeg"
    }
    fn encode(&mut self, frame: &Frame, _force_key: bool) -> Option<EncodedFrame> {
        crate::remote::encode_jpeg(frame, self.quality).map(|data| EncodedFrame {
            data,
            keyframe: true,
        })
    }
}

/// H.264 编码器（openh264，Annex-B；SPS/PPS 随 IDR 内联）
pub struct H264VideoEncoder {
    enc: OpenH264,
    frame_count: u64,
    /// 周期性 IDR 兜底（拥塞强推关键帧之外的可解码锚点）
    key_interval: u64,
    #[allow(dead_code)] // 诊断用
    bitrate_bps: u32,
}

impl H264VideoEncoder {
    pub fn new(width: u32, height: u32, fps: u32) -> Option<Self> {
        let pixels = (width.max(1) as u64) * (height.max(1) as u64);
        // 屏幕内容约 0.3 bit/px/帧：1080p@20 ≈ 5 Mbps，4K@30 ≈ 25M（钳到 16M）
        let bitrate =
            ((pixels * fps.max(1) as u64 * 3) / 10 / 8).clamp(1_500_000, 16_000_000) as u32;
        let key_interval = 100; // 5s @20fps
        let config = EncoderConfig::new()
            .bitrate(BitRate::from_bps(bitrate))
            .max_frame_rate(FrameRate::from_hz(fps.max(1) as f32))
            .rate_control_mode(RateControlMode::Bitrate)
            .usage_type(UsageType::ScreenContentRealTime)
            .profile(Profile::Baseline)
            .complexity(Complexity::Low)
            .intra_frame_period(IntraFramePeriod::from_num_frames(key_interval as u32))
            .num_threads(1);
        let enc = OpenH264::with_api_config(openh264::OpenH264API::from_source(), config).ok()?;
        Some(Self {
            enc,
            frame_count: 0,
            key_interval,
            bitrate_bps: bitrate,
        })
    }
}

impl VideoEncoder for H264VideoEncoder {
    fn codec(&self) -> u8 {
        CODEC_H264
    }
    fn backend(&self) -> &'static str {
        "openh264"
    }
    fn encode(&mut self, frame: &Frame, force_key: bool) -> Option<EncodedFrame> {
        if force_key || self.frame_count.is_multiple_of(self.key_interval) {
            self.enc.force_intra_frame();
        }
        let rgba = RgbaSliceU8::new(&frame.rgba, (frame.width as usize, frame.height as usize));
        let yuv = YUVBuffer::from_rgba8_source(rgba);
        let bs = self.enc.encode(&yuv).ok()?;
        let keyframe = matches!(bs.frame_type(), FrameType::IDR | FrameType::I);
        let data = bs.to_vec();
        self.frame_count += 1;
        (!data.is_empty()).then_some(EncodedFrame { data, keyframe })
    }
}

/// 创建编码器；H.264 优先硬件编码（macOS VideoToolbox / 未来 Windows
/// MediaFoundation），不可用回退 openh264 软编；H.264 全部失败返回 None
/// （调用方回退 JPEG）
pub fn make_encoder(
    codec: u8,
    width: u32,
    height: u32,
    fps: u32,
    quality: u8,
) -> Option<Box<dyn VideoEncoder>> {
    match codec {
        CODEC_H264 => {
            #[cfg(target_vendor = "apple")]
            if let Some(e) = VtH264Encoder::new(width, height, fps) {
                tracing::info!("远程控制编码器: h264 / videotoolbox（硬编）");
                return Some(Box::new(e));
            }
            let enc = H264VideoEncoder::new(width, height, fps)
                .map(|e| Box::new(e) as Box<dyn VideoEncoder>);
            if enc.is_some() {
                tracing::info!("远程控制编码器: h264 / openh264（软编）");
            }
            enc
        }
        _ => Some(Box::new(JpegVideoEncoder { quality })),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use openh264::formats::YUVSource;

    fn frame(w: u32, h: u32, seed: u8) -> Frame {
        let mut rgba = vec![0u8; (w * h * 4) as usize];
        for (i, px) in rgba.as_chunks_mut::<4>().0.iter_mut().enumerate() {
            px[0] = seed.wrapping_add(i as u8);
            px[1] = 80;
            px[2] = 160;
            px[3] = 255;
        }
        Frame {
            width: w,
            height: h,
            rgba,
        }
    }

    #[test]
    fn h264_roundtrip_through_openh264_decoder() {
        let mut enc = H264VideoEncoder::new(320, 200, 20).expect("编码器创建");
        let mut dec = openh264::decoder::Decoder::new().expect("解码器创建");

        // 关键帧（首帧必为 IDR）
        let key = enc.encode(&frame(320, 200, 1), false).expect("编码");
        assert!(key.keyframe, "首帧应为关键帧");
        assert_eq!(&key.data[..4], &[0, 0, 0, 1], "Annex-B 起始码");
        let decoded = dec
            .decode(&key.data)
            .expect("解码")
            .expect("关键帧应立即出图");
        assert_eq!(
            YUVSource::dimensions(&decoded),
            (320, 200),
            "回环分辨率一致"
        );

        // delta 帧（内容微变）
        let delta = enc.encode(&frame(320, 200, 2), false).expect("编码");
        assert!(!delta.keyframe, "第二帧应为 delta");
        assert!(
            delta.data.len() < key.data.len(),
            "delta 帧应显著小于关键帧"
        );
        let _ = dec.decode(&delta.data).expect("解码 delta");

        // 强推关键帧
        let forced = enc.encode(&frame(320, 200, 3), true).expect("编码");
        assert!(forced.keyframe, "force_key 应产出关键帧");
    }

    #[test]
    fn make_encoder_jpeg_and_fallback() {
        let mut enc = make_encoder(CODEC_JPEG, 320, 200, 20, 70).unwrap();
        assert_eq!(enc.codec(), CODEC_JPEG);
        let out = enc.encode(&frame(320, 200, 9), false).unwrap();
        assert_eq!(&out.data[..2], &[0xFF, 0xD8]);
        // 未知 codec → 回退 JPEG（协商异常时的安全兜底）
        let mut fallback = make_encoder(9, 320, 200, 20, 70).unwrap();
        assert_eq!(fallback.codec(), CODEC_JPEG);
        assert!(fallback.encode(&frame(320, 200, 9), false).is_some());
    }

    /// 硬编可用时 make_encoder 应选中 VideoToolbox，且产物能被 openh264
    /// 解码回环（Annex-B / IDR 内联 SPS/PPS 与软编同规格）
    #[cfg(target_vendor = "apple")]
    #[test]
    fn vt_hw_encoder_roundtrip_through_openh264_decoder() {
        let mut enc = match make_encoder(CODEC_H264, 320, 200, 20, 70) {
            Some(e) if e.backend() == "videotoolbox" => e,
            other => panic!("期望选中 videotoolbox 硬编，实际 {:?}", map_backend(other)),
        };

        // 首帧必为 IDR，且内联 SPS/PPS（帧首应直接是 SPS 的 start code + 0x67）
        let key = enc.encode(&frame(320, 200, 1), false).expect("编码");
        assert!(key.keyframe, "首帧应为关键帧");
        assert_eq!(&key.data[..4], &[0, 0, 0, 1], "Annex-B 起始码");
        assert_eq!(key.data[4] & 0x1f, 7, "帧首 NAL 应为 SPS");
        let start_codes = key
            .data
            .windows(4)
            .filter(|w| *w == [0, 0, 0, 1])
            .count();
        assert!(start_codes >= 3, "IDR 帧应内联 SPS/PPS（至少 SPS+PPS+IDR 三个 NAL）");

        let mut dec = openh264::decoder::Decoder::new().expect("解码器创建");
        let decoded = dec
            .decode(&key.data)
            .expect("解码")
            .expect("关键帧应立即出图");
        assert_eq!(
            openh264::formats::YUVSource::dimensions(&decoded),
            (320, 200),
            "回环分辨率一致"
        );

        // delta 帧可解、强推关键帧生效
        let delta = enc.encode(&frame(320, 200, 2), false).expect("编码");
        assert!(!delta.keyframe, "第二帧应为 delta");
        let _ = dec.decode(&delta.data).expect("解码 delta");
        let forced = enc.encode(&frame(320, 200, 3), true).expect("编码");
        assert!(forced.keyframe, "force_key 应产出关键帧");
    }

    #[cfg(target_vendor = "apple")]
    fn map_backend(e: Option<Box<dyn VideoEncoder>>) -> Option<&'static str> {
        e.map(|x| x.backend())
    }
}
