//! 冒烟：真实截屏 → 缩放 → JPEG 编码（验证 TCC 权限与依赖可用性）
use yunshu_core::remote::{make_screen_source, scale_frame};

fn main() {
    let t0 = std::time::Instant::now();
    let mut src = match make_screen_source() {
        Ok(s) => s,
        Err(e) => {
            eprintln!("屏幕源创建失败: {e}");
            std::process::exit(1);
        }
    };
    println!("屏幕源就绪（{}ms）", t0.elapsed().as_millis());
    let bounds = src.input_bounds();
    println!("输入坐标空间: {}x{}", bounds.0, bounds.1);
    let t1 = std::time::Instant::now();
    let Some(frame) = src.capture() else {
        eprintln!("截屏失败——macOS 需在系统设置 → 隐私与安全性 → 屏幕录制中授权");
        std::process::exit(2);
    };
    println!(
        "截屏 {}x{}（{}ms，{} 字节 RGBA）",
        frame.width,
        frame.height,
        t1.elapsed().as_millis(),
        frame.rgba.len()
    );
    let t2 = std::time::Instant::now();
    let scaled = scale_frame(&frame, 1920);
    println!(
        "缩放 → {}x{}（{}ms）",
        scaled.width,
        scaled.height,
        t2.elapsed().as_millis()
    );
    let t3 = std::time::Instant::now();
    let jpeg = yunshu_core::remote::encode_jpeg(&scaled, 70).expect("JPEG 编码");
    println!("JPEG {} 字节（{}ms）", jpeg.len(), t3.elapsed().as_millis());
    std::fs::write(std::env::temp_dir().join("yunshu-xcap-smoke.jpg"), &jpeg).unwrap();
    println!(
        "已写出 {}",
        std::env::temp_dir().join("yunshu-xcap-smoke.jpg").display()
    );

    // 模拟采集线程真实负载：连续 10 帧 JPEG + 切 H.264（超清档参数）
    println!("---- 连续 10 帧负载（JPEG q75 → H.264）----");
    let mut enc: Option<Box<dyn yunshu_core::encoder::VideoEncoder>> = None;
    for i in 0..10 {
        let t = std::time::Instant::now();
        let Some(f) = src.capture() else {
            eprintln!("第 {i} 帧截屏失败");
            std::process::exit(3);
        };
        let s = scale_frame(&f, 1920);
        let codec = if i < 5 {
            yunshu_core::encoder::CODEC_JPEG
        } else {
            yunshu_core::encoder::CODEC_H264
        };
        let rebuild = enc.as_ref().map_or(true, |e| e.codec() != codec);
        if rebuild {
            enc = yunshu_core::encoder::make_encoder(codec, s.width, s.height, 20, 75);
            if enc.is_none() {
                eprintln!("codec {codec} 编码器创建失败");
                std::process::exit(4);
            }
        }
        match enc.as_mut().and_then(|e| e.encode(&s, rebuild)) {
            Some(out) => println!(
                "帧{i} codec={codec} 编码 {} 字节（采集+编码 {}ms）",
                out.data.len(),
                t.elapsed().as_millis()
            ),
            None => println!("帧{i} codec={codec} 无输出（skip/err，非致命）"),
        }
    }
}
