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
}
