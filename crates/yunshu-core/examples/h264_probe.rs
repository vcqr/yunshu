//! openh264 编码行为探针：
//! A. 内容变化 → delta 帧应逐帧输出
//! B. 内容静止 + force_intra → 是否仍输出 IDR（心跳补帧可行性）
//! C. 内容静止不强制 → 预期为 skip（None/空），确认冒烟测试第 6 帧 None 的成因
use openh264::encoder::{
    BitRate, Complexity, Encoder as OpenH264, EncoderConfig, FrameRate, FrameType,
    IntraFramePeriod, Profile, RateControlMode, UsageType,
};
use openh264::formats::{RgbaSliceU8, YUVBuffer};

fn make_enc(fps: u32) -> OpenH264 {
    let config = EncoderConfig::new()
        .bitrate(BitRate::from_bps(4_000_000))
        .max_frame_rate(FrameRate::from_hz(fps as f32))
        .rate_control_mode(RateControlMode::Bitrate)
        .usage_type(UsageType::ScreenContentRealTime)
        .profile(Profile::Baseline)
        .complexity(Complexity::Low)
        .intra_frame_period(IntraFramePeriod::from_num_frames(100))
        .num_threads(1);
    OpenH264::with_api_config(openh264::OpenH264API::from_source(), config).unwrap()
}

/// 生成 (i) 号帧的 RGBA：t=0 静止基线；t>0 每帧移动的条纹（内容变化）
fn gen_frame(w: usize, h: usize, t: usize) -> Vec<u8> {
    let mut rgba = vec![0u8; w * h * 4];
    for (i, px) in rgba.as_chunks_mut::<4>().0.iter_mut().enumerate() {
        let x = i % w;
        let y = i / w;
        // 静止基线 + 变化时叠加移动竖条纹
        let stripe = if t > 0 && ((x + t * 8) / 16) % 2 == 0 { 90 } else { 0 };
        px[0] = (x % 256) as u8;
        px[1] = (y % 256) as u8;
        px[2] = 128 + stripe as u8;
        px[3] = 255;
    }
    rgba
}

fn encode(enc: &mut OpenH264, rgba: &[u8], w: usize, h: usize, force: bool, tag: &str) {
    if force {
        enc.force_intra_frame();
    }
    let yuv = YUVBuffer::from_rgba8_source(RgbaSliceU8::new(rgba, (w, h)));
    match enc.encode(&yuv) {
        Ok(bs) => {
            let data = bs.to_vec();
            println!(
                "{tag}: type={:?} len={} {}",
                bs.frame_type(),
                data.len(),
                if data.is_empty() { "(空→skip)" } else { "" }
            );
            let _ = matches!(bs.frame_type(), FrameType::IDR | FrameType::I);
        }
        Err(e) => println!("{tag}: ERR {e:?}"),
    }
}

fn main() {
    let (w, h) = (1920, 1200);
    println!("==== A. 内容变化（delta 逐帧）====");
    let mut enc = make_enc(20);
    for i in 0..6 {
        let rgba = gen_frame(w, h, i);
        encode(&mut enc, &rgba, w, h, i == 0, &format!("A{i}"));
    }
    println!("==== B. 静止 + 每帧 force_intra ====");
    let mut enc = make_enc(20);
    let rgba = gen_frame(w, h, 0);
    for i in 0..6 {
        encode(&mut enc, &rgba, w, h, true, &format!("B{i}"));
    }
    println!("==== C. 静止不强制（对照冒烟第 6 帧）====");
    let mut enc = make_enc(20);
    for i in 0..6 {
        encode(&mut enc, &rgba, w, h, i == 0, &format!("C{i}"));
    }
}
