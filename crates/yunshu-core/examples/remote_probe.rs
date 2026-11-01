//! 远程控制建连探针（诊断用）：对目标 host:port 走一遍真实链路
//! TCP → WS /remote-ws → Noise XX 握手 → 身份校验 → 认证（占位密码，一次即止）。
//! 用法: cargo run -p yunshu-core --example remote_probe -- <host> <port> <期望机器ID>
//!
//! 输出的每一步对应控制端 remote_connect_inner 的失败分支，用于定位
//! 「连接超时 / 握手超时 / 身份不符 / 认证失败」分别卡在哪一层。

use futures_util::{SinkExt, StreamExt};
use std::time::Duration;
use tokio_tungstenite::tungstenite::Message;
use yunshu_core::remote::{self, RemoteIn, RemoteOut, RemoteWire};

#[tokio::main(flavor = "current_thread")]
async fn main() {
    let mut args = std::env::args().skip(1);
    let host = args.next().expect("用法: remote_probe <host> <port> <机器ID>");
    let port: u16 = args.next().expect("缺 port").parse().expect("port 非数字");
    let expect_id = args.next().expect("缺机器ID");

    println!("[1/5] TCP 连接 {host}:{port} …");
    let tcp = tokio::time::timeout(Duration::from_secs(5), tokio::net::TcpStream::connect((host.as_str(), port)))
        .await
        .map_err(|_| "连接超时（5s）——大概率被对方防火墙丢包".to_string())
        .and_then(|r| r.map_err(|e| e.to_string()));
    let tcp = match tcp {
        Ok(t) => {
            println!("      ✔ TCP 已建立");
            t
        }
        Err(e) => return eprintln!("      ✘ {e}"),
    };

    println!("[2/5] WebSocket 升级 /remote-ws …");
    let ws = tokio_tungstenite::client_async(format!("ws://{host}:{port}{}", remote::REMOTE_WS_PATH), tcp).await;
    let (mut sink, mut stream) = match ws {
        Ok((ws, _)) => {
            println!("      ✔ WS 升级完成");
            ws.split()
        }
        Err(e) => return eprintln!("      ✘ {e}"),
    };

    println!("[3/5] Noise XX 握手（发起方）…");
    let mut hs = match yunshu_core::pigeon::new_handshake(true) {
        Ok(h) => h,
        Err(e) => return eprintln!("      ✘ Noise 初始化失败: {e}"),
    };
    let mut buf = vec![0u8; 65535];
    let n = hs.write_message(&[], &mut buf).expect("msg1");
    if let Err(e) = sink.send(Message::binary(buf[..n].to_vec())).await {
        return eprintln!("      ✘ 发送 msg1 失败: {e}");
    }
    let m2 = match tokio::time::timeout(Duration::from_secs(10), stream.next()).await {
        Ok(Some(Ok(Message::Binary(b)))) => b,
        Ok(Some(Ok(other))) => return eprintln!("      ✘ 握手消息格式错误: {other:?}"),
        Ok(Some(Err(e))) => return eprintln!("      ✘ 等待 msg2 出错: {e}"),
        Err(_) => return eprintln!("      ✘ 等待 msg2 超时（10s）"),
        Ok(None) => return eprintln!("      ✘ 对方断开（等 msg2）——可能路径/版本不对"),
    };
    if let Err(e) = hs.read_message(&m2, &mut buf) {
        return eprintln!("      ✘ 解 msg2 失败: {e}");
    }
    let n = hs.write_message(&[], &mut buf).expect("msg3");
    if let Err(e) = sink.send(Message::binary(buf[..n].to_vec())).await {
        return eprintln!("      ✘ 发送 msg3 失败: {e}");
    }
    println!("      ✔ 握手完成");
    let noise = std::sync::Arc::new(std::sync::Mutex::new(match hs.into_transport_mode() {
        Ok(t) => t,
        Err(e) => return eprintln!("      ✘ 转传输态失败: {e}"),
    }));

    let derived = remote::machine_id_from_static(
        &noise.lock().unwrap().get_remote_static().map(|s| s.to_vec()).unwrap_or_default(),
    );
    println!("[4/5] 身份校验：期望 {expect_id}，实际 {derived}");
    if derived != expect_id {
        return eprintln!("      ✘ 身份不符（疑似广播伪造或对端换过身份）");
    }

    println!("[5/5] 发送认证（占位密码 000000，仅一次）…");
    let auth = RemoteWire::RemoteAuth {
        mode: "temp".into(),
        password: "000000".into(),
        name: "probe".into(),
        machine_id: "00000000".into(),
    };
    let frame = match remote::seal_msg(&noise, &RemoteOut::Control(auth)) {
        Ok(f) => f,
        Err(e) => return eprintln!("      ✘ 封包失败: {e}"),
    };
    if let Err(e) = sink.send(Message::binary(frame)).await {
        return eprintln!("      ✘ 发送认证失败: {e}");
    }
    match tokio::time::timeout(Duration::from_secs(20), stream.next()).await {
        Ok(Some(Ok(Message::Binary(b)))) => match remote::open_msg(&noise, &b) {
            Ok(RemoteIn::Control(RemoteWire::RemoteAuthResult { ok, reason, .. })) => {
                if ok {
                    println!("      ✔ 认证通过（占位密码竟然对了？请立即断开会话）");
                } else {
                    println!("      ✔ 链路完全可用；受控端答复：{}",
                        reason.unwrap_or_else(|| "密码错误".into()));
                    println!("      ⇒ 结论：TCP/WS/加密/认证交换全通，「连不上」若复现，");
                    println!("         差异只能在密码本身或对方防打扰（见 reason）。");
                }
            }
            other => eprintln!("      ✘ 意外应答: {other:?}"),
        },
        Ok(Some(Ok(other))) => eprintln!("      ✘ 认证应答格式异常: {other:?}"),
        Ok(Some(Err(e))) => eprintln!("      ✘ 认证出错: {e}"),
        Err(_) => eprintln!("      ✘ 认证超时（20s）"),
        Ok(None) => eprintln!("      ✘ 对方断开（等认证应答）"),
    }
}
