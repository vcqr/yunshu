//! 聊天落库集成测试：
//! 起 serve（配置目录指向临时目录，隐身模式不广播）→ WS 配对 →
//! 手机↔PC 各发一条文本 → 校验 chat.db 里收发两行都在。
//! 覆盖真实链路上的落库接线：WS 入站、UI 请求出站、SQLite 文件建表。

use futures_util::{SinkExt, StreamExt};
use std::time::Duration;
use tokio::time::timeout;
use tokio_tungstenite::tungstenite::Message;
use yunshu_core::{chatlog::ChatLog, CoreConfig, CoreEvent, UiRequest};

const PORT: u16 = 38190;
const HTTPS_PORT: u16 = 38191;

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
async fn phone_chat_messages_land_in_sqlite() {
    // 配置目录指向临时目录：chat.db / 密钥 / 证书全部隔离（须在 serve 之前设置）
    let home = std::env::temp_dir().join(format!("yunshu_chat_test_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&home);
    std::fs::create_dir_all(&home).unwrap();
    let db_path = home.join("chat.db");
    let _ = std::fs::remove_file(&db_path);
    yunshu_core::set_config_dir(home.clone());

    let (event_tx, event_rx) = std::sync::mpsc::channel::<CoreEvent>();
    let (ui_tx, ui_rx) = std::sync::mpsc::channel::<UiRequest>();
    let cfg = CoreConfig {
        port: PORT,
        https_port: HTTPS_PORT,
        save_dir: home.join("save"),
        pc_name: "聊天落库测试".into(),
        dnd_mode: false,
        // 隐身：测试不向真实局域网广播本机
        stealth_mode: true,
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
        let ev = timeout(Duration::from_secs(10), ev_rx.recv())
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
        serde_json::json!({"type":"hello","token":token,"device":"落库测试手机"}).to_string(),
    ))
    .await
    .unwrap();

    let code = loop {
        let ev = timeout(Duration::from_secs(10), ev_rx.recv())
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
    let msg = timeout(Duration::from_secs(10), ws.next())
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    let v: serde_json::Value = serde_json::from_str(msg.to_text().unwrap()).unwrap();
    assert_eq!(v["type"], "pair_result", "应为配对结果: {v}");
    assert_eq!(v["ok"], true, "配对应成功: {v}");

    // 手机 → PC：发送文本
    ws.send(Message::text(
        serde_json::json!({"type":"send_text","text":"手机发来的第一条"}).to_string(),
    ))
    .await
    .unwrap();
    // 等 TextFromPhone 事件（落库先于事件发出，收到即已写入）
    loop {
        let ev = timeout(Duration::from_secs(10), ev_rx.recv())
            .await
            .unwrap()
            .unwrap();
        if matches!(ev, CoreEvent::TextFromPhone { .. }) {
            break;
        }
    }

    // PC → 手机：UI 请求发文本
    ui_tx.send(UiRequest::SendText("电脑回复的一条".into()))
        .unwrap();
    // 等 WS 收到 pc_text（传输事件在落库之后发出，收到即已写入）
    loop {
        let ev = timeout(Duration::from_secs(10), ev_rx.recv())
            .await
            .unwrap()
            .unwrap();
        if let CoreEvent::TransferUpdate { t } = ev {
            if t.kind == yunshu_core::TransferKind::Text {
                break;
            }
        }
    }

    // 校验落库：收发两行、通道/方向/对端/文本都对
    let log = ChatLog::open_readonly(&db_path).expect("chat.db 应已创建");
    let rows = log.recent(None, 10);
    assert_eq!(rows.len(), 2, "应有两条记录: {rows:?}");
    assert_eq!(rows[0].channel, "phone");
    assert_eq!(rows[0].direction, "in");
    assert_eq!(rows[0].peer_id, "phone");
    assert_eq!(rows[0].peer_name, "手机");
    assert_eq!(rows[0].text, "手机发来的第一条");
    assert_eq!(rows[1].channel, "phone");
    assert_eq!(rows[1].direction, "out");
    assert_eq!(rows[1].text, "电脑回复的一条");

    let _ = ws.close(None).await;
    let _ = std::fs::remove_dir_all(&home);
}
