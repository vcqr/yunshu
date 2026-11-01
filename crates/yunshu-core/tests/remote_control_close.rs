//! 回归测试：控制端关窗（发 WS Close 后停止收流）→ 被控端秒级结束会话。
//!
//! 被控端主循环不做任何阻塞发送（socket 写入隔离在发送任务里），对端的
//! Close/RemoteEnd 何时到都能立即被收流分支读出。本测试验证这条路径的接线；
//! TCP 背压死锁本身在回环上无法复现（内核缓冲吞数 MB），由单元测试
//! send_capped_stall_returns_in_time 与设计保证。

use futures_util::{SinkExt, StreamExt};
use snow::TransportState;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tokio::time::timeout;
use tokio_tungstenite::tungstenite::Message;
use yunshu_core::{CoreConfig, CoreEvent};

const PORT: u16 = 38295;
const HTTPS_PORT: u16 = 38296;
const PATTERN: &str = "Noise_XX_25519_ChaChaPoly_BLAKE2s";

fn init_test_env() -> tempfile_guard::Guard {
    let dir = std::env::temp_dir().join(format!(
        "yunshu-remote-close-test-{}",
        uuid::Uuid::new_v4().simple()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    // SAFETY: 测试进程启动早期调用，本测试二进制内无并发读取这些变量的其他测试
    unsafe {
        std::env::set_var("HOME", &dir);
        std::env::set_var("XDG_CONFIG_HOME", &dir);
        std::env::set_var("YUNSHU_FAKE_SCREEN", "1");
    }
    tempfile_guard::Guard(dir)
}

mod tempfile_guard {
    pub struct Guard(pub std::path::PathBuf);
    impl Drop for Guard {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
}

fn seal(noise: &Mutex<TransportState>, plain: &[u8]) -> Vec<u8> {
    let mut buf = vec![0u8; plain.len() + 64];
    let n = noise
        .lock()
        .unwrap()
        .write_message(plain, &mut buf)
        .expect("seal");
    buf.truncate(n);
    buf
}

#[tokio::test(flavor = "multi_thread")]
async fn controlled_ends_promptly_on_ws_close() {
    let _guard = init_test_env();

    let (event_tx, event_rx) = std::sync::mpsc::channel::<CoreEvent>();
    let (ui_tx, ui_rx) = std::sync::mpsc::channel::<yunshu_core::UiRequest>();
    let _ui_keepalive = ui_tx;
    tokio::spawn(yunshu_core::server::serve(
        CoreConfig {
            port: PORT,
            https_port: HTTPS_PORT,
            save_dir: std::env::temp_dir(),
            pc_name: "close-test受控端".into(),
            dnd_mode: false,
            stealth_mode: false,
        },
        event_tx,
        ui_rx,
    ));
    for _ in 0..50 {
        if tokio::net::TcpStream::connect(("127.0.0.1", PORT)).await.is_ok() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    let (ev_tx, mut ev_rx) = tokio::sync::mpsc::unbounded_channel::<CoreEvent>();
    std::thread::spawn(move || {
        for ev in event_rx {
            let _ = ev_tx.send(ev);
        }
    });
    let temp = loop {
        let ev = timeout(Duration::from_secs(10), ev_rx.recv())
            .await
            .unwrap()
            .unwrap();
        if let CoreEvent::RemoteTempPassword { password } = ev {
            break password;
        }
    };

    // 建连 + 认证
    let tcp = tokio::net::TcpStream::connect(("127.0.0.1", PORT)).await.unwrap();
    let (mut ws, _) =
        tokio_tungstenite::client_async(format!("ws://127.0.0.1:{PORT}/remote-ws"), tcp)
            .await
            .unwrap();
    let kp = snow::Builder::new(PATTERN.parse().unwrap())
        .generate_keypair()
        .unwrap();
    let mut hs = snow::Builder::new(PATTERN.parse().unwrap())
        .local_private_key(&kp.private)
        .unwrap()
        .build_initiator()
        .unwrap();
    let mut buf = vec![0u8; 65535];
    let n = hs.write_message(&[], &mut buf).unwrap();
    ws.send(Message::binary(buf[..n].to_vec())).await.unwrap();
    let m2 = timeout(Duration::from_secs(10), ws.next())
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    let Message::Binary(m2) = m2 else { panic!("握手消息格式") };
    hs.read_message(&m2, &mut buf).unwrap();
    let n = hs.write_message(&[], &mut buf).unwrap();
    ws.send(Message::binary(buf[..n].to_vec())).await.unwrap();
    let noise = Arc::new(Mutex::new(hs.into_transport_mode().unwrap()));

    let auth = {
        let mut p = vec![b'C'];
        p.extend_from_slice(
            serde_json::json!({
                "type": "remote_auth", "mode": "temp", "password": temp,
                "name": "close-test控制端", "machine_id": "deadbeef"
            })
            .to_string()
            .as_bytes(),
        );
        p
    };
    ws.send(Message::binary(seal(&noise, &auth))).await.unwrap();
    // 读掉认证应答 + 屏幕信息
    for _ in 0..2 {
        timeout(Duration::from_secs(10), ws.next())
            .await
            .unwrap()
            .unwrap()
            .unwrap();
    }
    // 至少收到一帧（会话确实在跑）
    timeout(Duration::from_secs(10), ws.next())
        .await
        .unwrap()
        .unwrap()
        .unwrap();

    // 模拟关窗：发 WS Close 后不再读流
    ws.close(None).await.unwrap();
    let t0 = Instant::now();

    // 被控端必须秒级发会话结束事件
    loop {
        let ev = timeout(Duration::from_secs(10), ev_rx.recv())
            .await
            .unwrap()
            .unwrap();
        if let CoreEvent::RemoteSessionEnded { side, reason, .. } = ev {
            assert_eq!(side, "controlled");
            let elapsed = t0.elapsed();
            assert!(
                elapsed < Duration::from_secs(5),
                "会话应秒级结束，实际 {elapsed:?}（reason={reason:?}）"
            );
            println!("✓ 关窗后 {elapsed:?} 会话结束，reason={reason:?}");
            break;
        }
    }
}
