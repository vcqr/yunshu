//! 手机端下载全链路集成测试：
//! 起 serve → WS 配对 → UI 通道触发 SendFiles → GET /file/{id} 逐字节比对。
//! 覆盖「下载一直卡住无数据」回归：带超时，卡住即测试失败。

use futures_util::{SinkExt, StreamExt};
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::time::timeout;
use tokio_tungstenite::tungstenite::Message;
use yunshu_core::{CoreConfig, CoreEvent, UiRequest};

const PORT: u16 = 38180;
const HTTPS_PORT: u16 = 38181;

/// 裸 TCP HTTP/1.1 GET（可带 Range），读到 EOF（Connection: close）。
/// 返回 (状态码, 响应头原文, body)。
async fn fetch(path: &str, port: u16, extra: &str) -> std::io::Result<(u16, String, Vec<u8>)> {
    let mut s = tokio::net::TcpStream::connect(("127.0.0.1", port)).await?;
    s.write_all(
        format!("GET {path} HTTP/1.1\r\nHost: 127.0.0.1\r\n{extra}Connection: close\r\n\r\n")
            .as_bytes(),
    )
    .await?;
    let mut buf = Vec::new();
    s.read_to_end(&mut buf).await?;
    let idx = buf
        .windows(4)
        .position(|w| w == b"\r\n\r\n")
        .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::InvalidData, "无响应头"))?;
    let head = String::from_utf8_lossy(&buf[..idx]).to_string();
    let status: u16 = head
        .split_whitespace()
        .nth(1)
        .and_then(|s| s.parse().ok())
        .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::InvalidData, "无状态码"))?;
    Ok((status, head, buf[idx + 4..].to_vec()))
}

/// 等 HTTP 端口就绪（QrUrl 事件早于 axum bind 发出）
async fn wait_port(port: u16) {
    for _ in 0..50 {
        if tokio::net::TcpStream::connect(("127.0.0.1", port))
            .await
            .is_ok()
        {
            return;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    panic!("端口 {port} 未就绪");
}

#[tokio::test(flavor = "multi_thread")]
async fn phone_file_download_stream_completes() {
    let (event_tx, event_rx) = std::sync::mpsc::channel::<CoreEvent>();
    let (ui_tx, ui_rx) = std::sync::mpsc::channel::<UiRequest>();
    let cfg = CoreConfig {
        port: PORT,
        https_port: HTTPS_PORT,
        save_dir: std::env::temp_dir(),
        pc_name: "e2e下载测试".into(),
        dnd_mode: false,
        stealth_mode: false,
    };
    tokio::spawn(yunshu_core::server::serve(cfg, event_tx, ui_rx));

    // std channel → tokio channel 桥接
    let (ev_tx, mut ev_rx) = tokio::sync::mpsc::unbounded_channel::<CoreEvent>();
    std::thread::spawn(move || {
        for ev in event_rx {
            let _ = ev_tx.send(ev);
        }
    });

    // 等首个二维码 token
    let token = loop {
        let ev = timeout(Duration::from_secs(5), ev_rx.recv())
            .await
            .unwrap()
            .unwrap();
        if let CoreEvent::QrUrl(url) = ev {
            break url.split("?t=").nth(1).expect("QrUrl 应含 ?t=").to_string();
        }
    };

    // 手机 WS：hello → 配对码 → pair
    wait_port(PORT).await;
    let (mut ws, _) = tokio_tungstenite::connect_async(format!("ws://127.0.0.1:{PORT}/ws"))
        .await
        .unwrap();
    ws.send(Message::text(
        serde_json::json!({"type":"hello","token":token,"device":"e2e手机"}).to_string(),
    ))
    .await
    .unwrap();

    let code = loop {
        let ev = timeout(Duration::from_secs(5), ev_rx.recv())
            .await
            .unwrap()
            .unwrap();
        if let CoreEvent::PairPending { code, .. } = ev {
            break code;
        }
    };
    ws.send(Message::text(
        serde_json::json!({"type":"pair","code":code}).to_string(),
    ))
    .await
    .unwrap();
    let msg = timeout(Duration::from_secs(5), ws.next())
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    let v: serde_json::Value = serde_json::from_str(msg.to_text().unwrap()).unwrap();
    assert_eq!(v["type"], "pair_result", "应为配对结果: {v}");
    assert_eq!(v["ok"], true, "配对应成功: {v}");
    assert_eq!(v["http_port"], PORT, "应携带 HTTP 备用端口");
    let session = v["session"].as_str().unwrap().to_string();

    // 造一个 3MB 测试文件
    let dir = std::env::temp_dir().join(format!("yunshu-dl-{}", uuid::Uuid::new_v4().simple()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("sample.bin");
    let data: Vec<u8> = (0..3_000_000u32).map(|i| (i % 251) as u8).collect();
    std::fs::write(&path, &data).unwrap();

    // PC → 手机发送（真实 UI 通道）
    ui_tx
        .send(UiRequest::SendFiles(vec![path.clone()]))
        .unwrap();
    let msg = timeout(Duration::from_secs(5), ws.next())
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    let v: serde_json::Value = serde_json::from_str(msg.to_text().unwrap()).unwrap();
    assert_eq!(v["type"], "pc_file", "应收到文件卡片: {v}");
    let id = v["id"].as_str().unwrap().to_string();
    let size = v["size"].as_u64().unwrap();
    assert_eq!(size, data.len() as u64);

    // 下载（带 session）——超时即「一直下载中」回归
    let (status, _, body) = timeout(
        Duration::from_secs(15),
        fetch(&format!("/file/{id}?s={session}"), PORT, ""),
    )
    .await
    .expect("下载超时（15 秒无完成）——复现「一直下载中」")
    .unwrap();
    assert_eq!(status, 200);
    assert_eq!(body.len() as u64, size, "HTTP 下载字节数不符");
    assert_eq!(body, data, "HTTP 下载内容不符");

    // 下载（不带 session）——修复后文件 id 本身即凭证
    let (status, _, body) = timeout(
        Duration::from_secs(15),
        fetch(&format!("/file/{id}"), PORT, ""),
    )
    .await
    .expect("无 session 下载超时")
    .unwrap();
    assert_eq!(status, 200);
    assert_eq!(body, data, "无 session 下载内容不符");

    // Range 探测（iOS Safari 下载器的行为）：bytes=0- 应答 206 + 正确 Content-Range
    let (status, head, body) = timeout(
        Duration::from_secs(15),
        fetch(&format!("/file/{id}"), PORT, "Range: bytes=0-\r\n"),
    )
    .await
    .expect("Range 探测超时")
    .unwrap();
    assert_eq!(status, 206, "Range 请求应答 206: {head}");
    assert!(
        head.contains("content-range: bytes 0-"),
        "Content-Range 不符: {head}"
    );
    assert_eq!(body, data, "Range 全量段内容不符");

    // Range 中段：bytes=1000-1999 应答对应片段
    let (status, head, body) = timeout(
        Duration::from_secs(15),
        fetch(&format!("/file/{id}"), PORT, "Range: bytes=1000-1999\r\n"),
    )
    .await
    .expect("Range 中段超时")
    .unwrap();
    assert_eq!(status, 206);
    assert!(
        head.contains("content-range: bytes 1000-1999/3000000"),
        "Content-Range 不符: {head}"
    );
    assert_eq!(body, &data[1000..2000], "Range 中段内容不符");

    // Accept-Ranges 声明存在
    let (_, head, _) = fetch(&format!("/file/{id}"), PORT, "").await.unwrap();
    assert!(
        head.to_lowercase().contains("accept-ranges: bytes"),
        "缺 Accept-Ranges: {head}"
    );

    // HTTPS（手机实际走 34257 的通道）——用系统 curl 忽略自签证书，硬断言
    let out = std::process::Command::new("curl")
        .args([
            "-k",
            "-sS",
            "--max-time",
            "15",
            &format!("https://127.0.0.1:{HTTPS_PORT}/file/{id}"),
        ])
        .output()
        .expect("curl 不可用");
    assert!(
        out.status.success(),
        "HTTPS 下载失败: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(out.stdout.len() as u64, size, "HTTPS 下载字节数不符");
    assert_eq!(out.stdout, data, "HTTPS 下载内容不符");

    let _ = std::fs::remove_dir_all(&dir);
}
