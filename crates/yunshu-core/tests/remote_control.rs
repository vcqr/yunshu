//! 远程控制全链路集成测试：
//! 起 serve（YUNSHU_FAKE_SCREEN=1 合成屏幕源，不碰真实屏幕/剪贴板/输入）→
//! 裸 WS + Noise XX 客户端连 /remote-ws →
//! ① 正确临时密码认证 → 收屏幕信息 → 收 JPEG 视频帧 → 发输入 → 结束会话；
//! ② 连续错 5 次密码 → 第 6 次连入被锁定（防爆破）。
//! 带超时，任何环节卡住即失败。

use futures_util::{SinkExt, StreamExt};
use snow::TransportState;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::time::timeout;
use tokio_tungstenite::tungstenite::Message;
use yunshu_core::{CoreConfig, CoreEvent, UiRequest};

const PORT: u16 = 38280;
const HTTPS_PORT: u16 = 38281;
const PATTERN: &str = "Noise_XX_25519_ChaChaPoly_BLAKE2s";

/// 独立 Noise 密钥（不动真实配置目录）；配置目录整体指到临时目录
fn init_test_env() -> tempfile_guard::Guard {
    let dir = std::env::temp_dir().join(format!(
        "yunshu-remote-test-{}",
        uuid::Uuid::new_v4().simple()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    // 集成测试进程独占：HOME/XDG 重定向后 load_machine_key 等写临时目录
    // SAFETY: 测试进程启动早期（单线程 tokio test 尚未拉起业务线程前）调用，
    // 且本测试二进制内没有并发读取这些变量的其他测试
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

fn auth_plain(password: &str) -> Vec<u8> {
    let mut p = vec![b'C'];
    p.extend_from_slice(
        serde_json::json!({
            "type": "remote_auth",
            "mode": "temp",
            "password": password,
            "name": "e2e控制端",
            "machine_id": "deadbeef",
        })
        .to_string()
        .as_bytes(),
    );
    p
}

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

/// 建连 + Noise 握手（initiator），返回 WS 与传输态
async fn connect_remote() -> (
    tokio_tungstenite::WebSocketStream<tokio::net::TcpStream>,
    Arc<Mutex<TransportState>>,
) {
    let tcp = tokio::net::TcpStream::connect(("127.0.0.1", PORT))
        .await
        .unwrap();
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
    let Message::Binary(m2) = m2 else {
        panic!("握手消息格式")
    };
    hs.read_message(&m2, &mut buf).unwrap();
    let n = hs.write_message(&[], &mut buf).unwrap();
    ws.send(Message::binary(buf[..n].to_vec())).await.unwrap();
    (ws, Arc::new(Mutex::new(hs.into_transport_mode().unwrap())))
}

/// 发送 auth 明文帧并读取第一条应答（认证判定）
async fn send_auth(
    ws: &mut tokio_tungstenite::WebSocketStream<tokio::net::TcpStream>,
    noise: &Arc<Mutex<TransportState>>,
    password: &str,
) -> serde_json::Value {
    ws.send(Message::binary(seal(noise, &auth_plain(password))))
        .await
        .unwrap();
    let reply = timeout(Duration::from_secs(10), ws.next())
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    let Message::Binary(reply) = reply else {
        panic!("应答格式")
    };
    let mut buf = vec![0u8; reply.len()];
    let n = noise
        .lock()
        .unwrap()
        .read_message(&reply, &mut buf)
        .unwrap();
    assert_eq!(buf[0], b'C', "应为控制 JSON");
    serde_json::from_slice(&buf[1..n]).unwrap()
}

#[tokio::test(flavor = "multi_thread")]
async fn remote_control_auth_stream_and_lockout() {
    let _guard = init_test_env();

    let (event_tx, event_rx) = std::sync::mpsc::channel::<CoreEvent>();
    let (ui_tx, ui_rx) = std::sync::mpsc::channel::<yunshu_core::UiRequest>();
    let cfg = CoreConfig {
        port: PORT,
        https_port: HTTPS_PORT,
        save_dir: std::env::temp_dir(),
        pc_name: "e2e受控端".into(),
        dnd_mode: false,
    };
    tokio::spawn(yunshu_core::server::serve(cfg, event_tx, ui_rx));

    // std → tokio 事件桥（eprintln 留痕：stderr 无缓冲，便于死锁排查）
    let (ev_tx, mut ev_rx) = tokio::sync::mpsc::unbounded_channel::<CoreEvent>();
    std::thread::spawn(move || {
        for ev in event_rx {
            let _ = ev_tx.send(ev);
        }
    });

    // 等临时密码事件（serve 启动即发）
    wait_port(PORT).await;
    let temp_password = loop {
        let ev = timeout(Duration::from_secs(10), ev_rx.recv())
            .await
            .unwrap()
            .unwrap();
        if let CoreEvent::RemoteTempPassword { password } = ev {
            assert_eq!(password.len(), 6, "临时密码应为 6 位数字");
            break password;
        }
    };

    // ---- ① 错误密码：应答密码错误（本连接内重试 3 次机会，每次都是独立连接）----
    for round in 0..5 {
        let (mut ws, noise) = connect_remote().await;
        let v = send_auth(&mut ws, &noise, &format!("9999{round:02}")).await;
        assert_eq!(v["type"], "remote_auth_result", "{v}");
        assert_eq!(v["ok"], false, "错误密码不应通过: {v}");
        assert!(
            v["reason"]
                .as_str()
                .unwrap_or_default()
                .contains("密码错误"),
            "{v}"
        );
        let _ = ws.close(None).await;
    }

    // ---- ② 连错 5 次后，正确密码也被锁定 ----
    {
        let (mut ws, noise) = connect_remote().await;
        let v = send_auth(&mut ws, &noise, &temp_password).await;
        assert_eq!(v["ok"], false, "锁定期间正确密码也不应通过: {v}");
        assert!(
            v["reason"]
                .as_str()
                .unwrap_or_default()
                .contains("密码错误次数过多"),
            "{v}"
        );
        let _ = ws.close(None).await;
        panic_if_session_started(&mut ev_rx).await;
    }

    // 重启一个全新 serve 以绕开锁定（锁定状态在进程内）：
    // 直接在同一个 state 上不清锁定——改为通过一次性密码路径验证认证成功？
    // 一次性密码需要经飞鸽链路回传，此处直接注入：用 SetRemoteFixedPassword 设置固定密码，
    // 固定密码同样被 IP 锁定拦截。因此先等待锁定窗口不可行（10 分钟）。
    // 解法：锁定按 IP 计，测试客户端换用回环别名地址不可行（同一 TCP 来源）。
    // 所以认证成功路径用第二个 serve 实例（新 state，锁定表为空）验证。
    let port2 = PORT + 10;
    let (event_tx2, event_rx2) = std::sync::mpsc::channel::<CoreEvent>();
    let (ui_tx2, ui_rx2) = std::sync::mpsc::channel::<yunshu_core::UiRequest>();
    tokio::spawn(yunshu_core::server::serve(
        CoreConfig {
            port: port2,
            https_port: HTTPS_PORT + 10,
            save_dir: std::env::temp_dir(),
            pc_name: "e2e受控端2".into(),
            dnd_mode: false,
        },
        event_tx2,
        ui_rx2,
    ));
    let (ev2_tx, mut ev2_rx) = tokio::sync::mpsc::unbounded_channel::<CoreEvent>();
    std::thread::spawn(move || {
        for ev in event_rx2 {
            let _ = ev2_tx.send(ev);
        }
    });
    wait_port(port2).await;
    let _ = &ui_tx; // 第一个实例的 ui 通道保持存活
    let _ = &ui_tx2;
    let temp2 = loop {
        let ev = timeout(Duration::from_secs(10), ev2_rx.recv())
            .await
            .unwrap()
            .unwrap();
        if let CoreEvent::RemoteTempPassword { password } = ev {
            break password;
        }
    };

    // ---- ③ 正确临时密码：认证通过 + 屏幕信息 + JPEG 视频帧 ----
    {
        let (mut ws, noise) = connect_remote_at(port2).await;
        let v = send_auth(&mut ws, &noise, &temp2).await;
        assert_eq!(v["type"], "remote_auth_result", "{v}");
        assert_eq!(v["ok"], true, "正确临时密码应通过: {v}");
        assert_eq!(v["pc_name"], "e2e受控端2", "{v}");

        // 第二条应答：屏幕信息（合成源 320x200）
        let detail = timeout(Duration::from_secs(10), ws.next())
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        let Message::Binary(detail) = detail else {
            panic!("应答格式")
        };
        let mut buf = vec![0u8; detail.len()];
        let n = noise
            .lock()
            .unwrap()
            .read_message(&detail, &mut buf)
            .unwrap();
        assert_eq!(buf[0], b'C');
        let v: serde_json::Value = serde_json::from_slice(&buf[1..n]).unwrap();
        assert_eq!(v["type"], "remote_auth_result", "{v}");
        assert_eq!(v["screen"]["width"], 320, "合成源宽度: {v}");
        assert_eq!(v["screen"]["height"], 200, "合成源高度: {v}");
        assert_eq!(v["screen"]["logical_w"], 320, "{v}");

        // 收视频帧分块 → 重组 → JPEG SOI 校验
        let (codec, jpeg) = collect_frame(&mut ws, &noise).await;
        assert_eq!(codec, 0, "初始编码应为 JPEG");
        assert!(jpeg.len() > 100, "JPEG 帧太小: {} 字节", jpeg.len());
        assert_eq!(&jpeg[..2], &[0xFF, 0xD8], "应为 JPEG SOI 开头");
        assert_eq!(&jpeg[jpeg.len() - 2..], &[0xFF, 0xD9], "应为 JPEG EOI 结尾");

        // 发输入事件 + 结束会话
        let mut input = vec![b'C'];
        input.extend_from_slice(
            serde_json::json!({
                "type": "remote_input",
                "ev": { "t": "mouse_move", "x": 0.5, "y": 0.5 }
            })
            .to_string()
            .as_bytes(),
        );
        ws.send(Message::binary(seal(&noise, &input)))
            .await
            .unwrap();
        let mut end = vec![b'C'];
        end.extend_from_slice(
            serde_json::json!({"type": "remote_end", "reason": "e2e 完毕"})
                .to_string()
                .as_bytes(),
        );
        ws.send(Message::binary(seal(&noise, &end))).await.unwrap();

        // 受控端应发会话结束事件
        let ended = loop {
            let ev = timeout(Duration::from_secs(10), ev2_rx.recv())
                .await
                .unwrap()
                .unwrap();
            if let CoreEvent::RemoteSessionEnded { side, reason, .. } = ev {
                assert_eq!(side, "controlled");
                assert!(reason.is_some_and(|r| r.contains("e2e")));
                break;
            }
        };
        let _ = ended;
        let _ = ws.close(None).await;
    }
    panic_if_session_started(&mut ev_rx).await;

    // ---- ④ 控制端全链路：UDP 注入假发现节点（指向实例 2）→ UiRequest::RemoteConnect →
    //        实例 1 发起连接（覆盖 UI 线程 spawn 投递）→ RemoteViewReady → 双端会话事件 → 断开 ----
    {
        let sock = std::net::UdpSocket::bind("127.0.0.1:0").unwrap();
        let announce = serde_json::json!({
            "v": 1, "kind": "entry", "id": "e2efake0001", "name": "e2e假节点",
            "port": port2, "mid": "cafe5678"
        })
        .to_string();
        // 共享 UDP 端口存在负载均衡投递，多发几遍确保实例 1 的发现表收到
        for _ in 0..20 {
            let _ = sock.send_to(announce.as_bytes(), ("127.0.0.1", 34255u16));
            tokio::time::sleep(Duration::from_millis(60)).await;
        }
        ui_tx
            .send(UiRequest::RemoteConnect {
                machine_id: "cafe5678".into(),
                password: temp2.clone(),
            })
            .unwrap();
        // 实例 1（控制端）：连接成功事件（失败则带原因快速失败）
        let mut main_token = String::new();
        let mut view_token = String::new();
        loop {
            let ev = timeout(Duration::from_secs(20), ev_rx.recv())
                .await
                .unwrap()
                .unwrap();
            match ev {
                CoreEvent::RemoteViewReady {
                    peer_id,
                    token,
                    view_token: vt,
                    port,
                    ..
                } => {
                    assert_eq!(peer_id, "cafe5678");
                    assert_eq!(port, PORT, "观看桥端口应为本机 HTTP 端口");
                    assert!(!token.is_empty());
                    assert_eq!(token.len(), 32, "主凭证 32 位 hex");
                    assert_ne!(token, vt, "只读凭证应与主凭证不同");
                    main_token = token;
                    view_token = vt;
                    break;
                }
                CoreEvent::RemoteConnectFailed { reason, .. } => {
                    panic!("远程连接失败：{reason}");
                }
                _ => {}
            }
        }
        // 实例 2（受控端）：会话开始
        loop {
            let ev = timeout(Duration::from_secs(10), ev2_rx.recv())
                .await
                .unwrap()
                .unwrap();
            if let CoreEvent::RemoteSessionStarted { side, name, .. } = ev {
                assert_eq!(side, "controlled");
                assert_eq!(name, "e2e受控端", "应为实例 1 的机器名");
                break;
            }
        }

        // ---- ⑤ 编码协商：观看桥接入 + 上报 H.264 能力 → 受控端切 H.264 → 收 Annex-B 帧 → openh264 解码回环 ----
        {
            let (mut bridge, _) = tokio_tungstenite::connect_async(format!(
                "ws://127.0.0.1:{PORT}/remote-view-ws?token={main_token}"
            ))
            .await
            .unwrap();
            bridge
                .send(Message::text(
                    serde_json::json!({"t": "caps", "h264": true}).to_string(),
                ))
                .await
                .unwrap();
            // 收帧直到 H.264 关键帧（切换后首个 IDR），Annex-B 起始码 + openh264 解码出 320x200
            let mut decoder = openh264::decoder::Decoder::new().unwrap();
            let deadline = tokio::time::Instant::now() + Duration::from_secs(20);
            let mut decoded_ok = false;
            while tokio::time::Instant::now() < deadline {
                let msg = timeout(Duration::from_secs(10), bridge.next())
                    .await
                    .unwrap()
                    .unwrap()
                    .unwrap();
                let Message::Binary(bin) = msg else { continue };
                assert!(bin.len() > 2, "帧载荷过短");
                if bin[0] != 1 {
                    continue; // 切换前的 JPEG 帧
                }
                let payload = &bin[1..];
                assert_eq!(&payload[..4], &[0, 0, 0, 1], "H.264 应为 Annex-B 起始码");
                if let Ok(Some(yuv)) = decoder.decode(payload) {
                    let (w, h) = openh264::formats::YUVSource::dimensions(&yuv);
                    assert_eq!((w, h), (320, 200), "H.264 解码回环分辨率");
                    decoded_ok = true;
                    break;
                }
            }
            assert!(decoded_ok, "20 秒内未解出 H.264 帧");
            let _ = bridge.close(None).await;
        }

        // ---- ⑥ 文件回传：控制端 → 受控端（自动接受 → 分块 → SHA-256 校验 → 落盘）----
        {
            let dir =
                std::env::temp_dir().join(format!("yunshu-push-{}", uuid::Uuid::new_v4().simple()));
            std::fs::create_dir_all(&dir).unwrap();
            let path = dir.join("回传测试.bin");
            let data: Vec<u8> = (0..500_000u32).map(|i| (i % 253) as u8).collect();
            std::fs::write(&path, &data).unwrap();
            ui_tx
                .send(UiRequest::RemoteSendFiles {
                    paths: vec![path.clone()],
                })
                .unwrap();
            // 受控端：批次完成事件
            let mut done = false;
            loop {
                let ev = timeout(Duration::from_secs(20), ev2_rx.recv())
                    .await
                    .unwrap()
                    .unwrap();
                if let CoreEvent::TransferUpdate { t } = ev {
                    // 受控端行的 peer 是控制端真实机器 ID（④ 段经 remote_connect 连入），
                    // 以批次名 + 状态判定
                    if t.name == "回传测试.bin" && t.status == yunshu_core::TransferStatus::Done
                    {
                        assert_eq!(t.transferred, data.len() as u64, "受控端应收满 500KB");
                        done = true;
                        break;
                    }
                }
            }
            assert!(done);
            // 控制端：发送行完成
            loop {
                let ev = timeout(Duration::from_secs(10), ev_rx.recv())
                    .await
                    .unwrap()
                    .unwrap();
                if let CoreEvent::TransferUpdate { t } = ev {
                    if t.direction == yunshu_core::Direction::ToPhone
                        && t.peer.as_deref() == Some("cafe5678")
                        && t.status == yunshu_core::TransferStatus::Done
                    {
                        assert_eq!(t.transferred, data.len() as u64);
                        break;
                    }
                }
            }
            // 磁盘内容逐字节校验（存「远程回传/<id8>/」目录）
            let base = std::env::temp_dir().join("远程回传");
            let mut found = None;
            for _ in 0..30 {
                let direct = base.join("回传测试.bin");
                if direct.exists() {
                    found = Some(direct);
                    break;
                }
                if let Ok(entries) = std::fs::read_dir(&base) {
                    for e in entries.flatten() {
                        let inner = e.path().join("回传测试.bin");
                        if inner.exists() {
                            found = Some(inner);
                            break;
                        }
                    }
                }
                if found.is_some() {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(200)).await;
            }
            let found = found.expect("远程回传文件未落盘");
            assert_eq!(std::fs::read(&found).unwrap(), data, "回传内容逐字节一致");
            let _ = std::fs::remove_dir_all(&dir);
            let _ = std::fs::remove_dir_all(&base);
        }

        // 断开：两端都应收尾
        ui_tx.send(UiRequest::RemoteDisconnect).unwrap();
        loop {
            let ev = timeout(Duration::from_secs(10), ev_rx.recv())
                .await
                .unwrap()
                .unwrap();
            if let CoreEvent::RemoteSessionEnded { side, .. } = ev {
                assert_eq!(side, "controller");
                break;
            }
        }
        loop {
            let ev = timeout(Duration::from_secs(10), ev2_rx.recv())
                .await
                .unwrap()
                .unwrap();
            if let CoreEvent::RemoteSessionEnded { side, .. } = ev {
                assert_eq!(side, "controlled");
                break;
            }
        }
    }
}

/// 第一个实例在整个测试期间不应有会话开始事件（它处于锁定状态）
async fn panic_if_session_started(ev_rx: &mut tokio::sync::mpsc::UnboundedReceiver<CoreEvent>) {
    // 非阻塞检查：确认缓冲里没有会话开始事件
    while let Ok(ev) = ev_rx.try_recv() {
        if let CoreEvent::RemoteSessionStarted { .. } = ev {
            panic!("锁定实例不应有会话开始");
        }
    }
}

async fn connect_remote_at(
    port: u16,
) -> (
    tokio_tungstenite::WebSocketStream<tokio::net::TcpStream>,
    Arc<Mutex<TransportState>>,
) {
    let tcp = tokio::net::TcpStream::connect(("127.0.0.1", port))
        .await
        .unwrap();
    let (mut ws, _) =
        tokio_tungstenite::client_async(format!("ws://127.0.0.1:{port}/remote-ws"), tcp)
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
    let Message::Binary(m2) = m2 else {
        panic!("握手消息格式")
    };
    hs.read_message(&m2, &mut buf).unwrap();
    let n = hs.write_message(&[], &mut buf).unwrap();
    ws.send(Message::binary(buf[..n].to_vec())).await.unwrap();
    (ws, Arc::new(Mutex::new(hs.into_transport_mode().unwrap())))
}

/// 从 WS 读取视频帧分块并重组出一个完整帧（返回 codec 与数据）
async fn collect_frame(
    ws: &mut tokio_tungstenite::WebSocketStream<tokio::net::TcpStream>,
    noise: &Arc<Mutex<TransportState>>,
) -> (u8, Vec<u8>) {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(15);
    let mut seq = 0u32;
    let mut total = 0u16;
    let mut frame_codec = 0u8;
    let mut got: Vec<bool> = Vec::new();
    let mut buf_all = Vec::new();
    let mut got_count = 0usize;
    loop {
        assert!(tokio::time::Instant::now() < deadline, "等视频帧超时");
        let msg = timeout(Duration::from_secs(5), ws.next())
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        let Message::Binary(cipher) = msg else {
            continue;
        };
        let mut plain = vec![0u8; cipher.len()];
        let n = noise
            .lock()
            .unwrap()
            .read_message(&cipher, &mut plain)
            .unwrap();
        assert_eq!(plain[0], b'V', "应为视频帧分块: {}", plain[0]);
        let s = u32::from_le_bytes(plain[1..5].try_into().unwrap());
        let idx = u16::from_le_bytes(plain[5..7].try_into().unwrap());
        let t = u16::from_le_bytes(plain[7..9].try_into().unwrap());
        let codec = plain[9];
        if idx == 0 {
            seq = s;
            total = t;
            frame_codec = codec;
            got = vec![false; t as usize];
            buf_all.clear();
            got_count = 0;
        } else if s != seq || t != total {
            continue;
        }
        let i = idx as usize;
        if i < got.len() && !got[i] {
            got[i] = true;
            got_count += 1;
            buf_all.extend_from_slice(&plain[10..n]);
        }
        if got_count == got.len() && !got.is_empty() {
            return (frame_codec, buf_all);
        }
    }
}
