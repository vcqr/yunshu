//! 飞鸽节点调试工具：起一个 serve 实例（自动信任所有新设备），事件以 JSON 行打印到
//! stdout；stdin 每行一条命令：`send <peer_id> <文本>`（发聊天消息）。
//! 双实例联调：用 HOME/XDG_CONFIG_HOME/YUNSHU_PORT/YUNSHU_HTTPS_PORT 隔离实例。

use std::io::BufRead;
use yunshu_core::{CoreConfig, CoreEvent, UiRequest};

#[tokio::main(flavor = "multi_thread")]
async fn main() {
    let _ = tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::new("info"))
        .try_init();
    let port: u16 = std::env::var("YUNSHU_PORT")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(34256);
    let https_port: u16 = std::env::var("YUNSHU_HTTPS_PORT")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(34257);
    let name = std::env::var("YUNSHU_NAME").unwrap_or_else(|_| "node".into());

    let (event_tx, event_rx) = std::sync::mpsc::channel::<CoreEvent>();
    let (ui_tx, ui_rx) = std::sync::mpsc::channel::<UiRequest>();
    let cfg = CoreConfig {
        port,
        https_port,
        save_dir: std::env::temp_dir(),
        pc_name: name,
        dnd_mode: false,
        stealth_mode: false,
    };
    tokio::spawn(yunshu_core::server::serve(cfg, event_tx, ui_rx));

    // 事件泵：JSON 行输出；信任请求自动同意（联调免人工确认）
    let ui_tx2 = ui_tx.clone();
    std::thread::spawn(move || {
        for ev in event_rx {
            match &ev {
                CoreEvent::PigeonTrustRequest {
                    peer_id,
                    fingerprint,
                    ..
                } => {
                    let _ = ui_tx2.send(UiRequest::PigeonTrust {
                        peer_id: peer_id.clone(),
                        fingerprint: fingerprint.clone(),
                    });
                }
                CoreEvent::RemoteTempPassword { .. } => {}
                _ => {}
            }
            if let Ok(line) = serde_json::to_string(&ev) {
                println!("EVENT {line}");
            }
        }
    });

    // 命令泵：stdin 每行 `send <peer_id> <文本>`（独立线程——nohup 下 stdin 立即
    // EOF 也不能让 main 返回，否则整个 runtime 关停、所有后台任务被杀）
    {
        let ui_tx = ui_tx.clone();
        std::thread::spawn(move || {
            let stdin = std::io::stdin();
            for line in stdin.lock().lines() {
                let Ok(line) = line else { break };
                let mut parts = line.splitn(3, ' ');
                match (parts.next(), parts.next(), parts.next()) {
                    (Some("send"), Some(peer), Some(text)) => {
                        let _ = ui_tx.send(UiRequest::PigeonSendText {
                            peer_id: peer.to_string(),
                            text: text.to_string(),
                        });
                    }
                    _ => println!("EVENT {{\"unknown_cmd\":{line:?}}}"),
                }
            }
        });
    }
    // 单播信标：同机多实例时广播会被多个 SO_REUSEPORT 套接字分拣丢失；
    // YUNSHU_UNICAST_PEERS="ip:port,..." 每 2s 直接单播一份 entry 信标，
    // 模拟真实局域网的可靠发现（报文格式与 run_discovery 的 Announce 一致）
    if let Ok(peers) = std::env::var("YUNSHU_UNICAST_PEERS") {
        let name = std::env::var("YUNSHU_NAME").unwrap_or_else(|_| "node".into());
        let id = yunshu_core::remote::machine_id();
        let beacon = serde_json::json!({
            "v": 1, "kind": "entry", "id": id,
            "name": name, "port": port, "mid": id,
        })
        .to_string();
        tokio::spawn(async move {
            let sock = tokio::net::UdpSocket::bind("0.0.0.0:0").await.ok();
            let Some(sock) = sock else { return };
            let targets: Vec<&str> = peers.split(',').map(str::trim).collect();
            loop {
                for t in &targets {
                    let _ = sock.send_to(beacon.as_bytes(), t).await;
                }
                tokio::time::sleep(std::time::Duration::from_secs(2)).await;
            }
        });
    }

    // 常驻
    futures_util::future::pending::<()>().await;
}
