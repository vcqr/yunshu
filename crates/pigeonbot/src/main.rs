//! pigeonbot：无头飞鸽测试节点。
//! 广播自己的存在 → 等云书实例连入（Noise XX，bot 是 responder）→
//! 发一条聊天 → 推送一个随机内容文件（等对方自动/手动接受）→ 校验对方拉取后退出。
//! 用法：pigeonbot [文件大小字节]（默认 1MB）

use futures_util::{SinkExt, StreamExt};
use snow::TransportState;
use std::collections::HashMap;
use std::net::{SocketAddr, UdpSocket};
use std::sync::{Arc, Mutex};
use tokio::net::TcpListener;
use tokio_tungstenite::tungstenite::Message;

const PATTERN: &str = "Noise_XX_25519_ChaChaPoly_BLAKE2s";
const BOT_PORT: u16 = 9000;
const CHUNK: usize = 60_000;

#[derive(serde::Serialize, serde::Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum Wire {
    PigeonHello { id: String, name: String, port: u16 },
    PigeonWelcome { ok: bool, #[serde(default)] pc_name: Option<String> },
    PcText { text: String },
    PcFile { id: String, name: String, size: u64 },
    FileDecision { id: String, accepted: bool },
    FileStart { id: String, name: String, size: u64 },
    /// 接收方应答：本地已有 have 字节，发送方从断点续推
    FileResume { id: String, have: u64 },
    FileEnd { id: String, ok: bool },
}

fn seal(noise: &Mutex<TransportState>, plain: &[u8]) -> Vec<u8> {
    let mut buf = vec![0u8; plain.len() + 64];
    let n = noise.lock().unwrap().write_message(plain, &mut buf).expect("write");
    buf.truncate(n);
    buf
}

fn open(noise: &Mutex<TransportState>, cipher: &[u8]) -> Vec<u8> {
    let mut buf = vec![0u8; cipher.len()];
    let n = noise.lock().unwrap().read_message(cipher, &mut buf).expect("read");
    buf[..n].to_vec()
}

fn control_msg(noise: &Mutex<TransportState>, w: &Wire) -> Vec<u8> {
    let mut plain = vec![b'C'];
    plain.extend_from_slice(serde_json::to_vec(w).unwrap().as_slice());
    seal(noise, &plain)
}

fn chunk_msg(noise: &Mutex<TransportState>, id16: &[u8; 16], offset: u64, data: &[u8]) -> Vec<u8> {
    let mut plain = vec![b'F'];
    plain.extend_from_slice(id16);
    plain.extend_from_slice(&offset.to_le_bytes());
    plain.extend_from_slice(data);
    seal(noise, &plain)
}

fn keypair() -> Vec<u8> {
    let path = std::env::temp_dir().join("pigeonbot_key.bin");
    if let Ok(k) = std::fs::read(&path)
        && k.len() == 32
    {
        return k;
    }
    let kp = snow::Builder::new(PATTERN.parse().unwrap()).generate_keypair().unwrap();
    let _ = std::fs::write(&path, &kp.private);
    kp.private
}

#[tokio::main]
async fn main() {
    let size: usize = std::env::args().nth(1).and_then(|a| a.parse().ok()).unwrap_or(1024 * 1024);
    let bot_id: String = {
        let k = keypair();
        k.iter().map(|b| format!("{b:02x}")).collect::<String>()[..32].to_string()
    };
    println!("[bot] 启动 id={} port={BOT_PORT}", &bot_id[..8]);

    // UDP 广播
    std::thread::spawn(move || {
        let sock = socket2::Socket::new(socket2::Domain::IPV4, socket2::Type::DGRAM, Some(socket2::Protocol::UDP)).unwrap();
        let _ = sock.set_reuse_address(true);
        let _ = sock.set_nonblocking(true);
        let addr: SocketAddr = ([0, 0, 0, 0], 0).into();
        sock.bind(&addr.into()).unwrap();
        let sock: UdpSocket = sock.into();
        let _ = sock.set_broadcast(true);
        let msg = serde_json::to_vec(&serde_json::json!({
            "v": 1, "kind": "entry", "id": bot_id, "name": format!("e2e测试节点({})", size), "port": BOT_PORT
        }))
        .unwrap();
        loop {
            for addr in ["255.255.255.255", "169.254.255.255", "192.168.0.255"] {
                let _ = sock.send_to(&msg, format!("{addr}:34255"));
            }
            std::thread::sleep(std::time::Duration::from_secs(2));
        }
    });

    // TCP + WS 服务
    let listener = TcpListener::bind(("0.0.0.0", BOT_PORT)).await.unwrap();
    let (tcp, _addr) = listener.accept().await.unwrap();
    println!("[bot] 实例已连入，进行 Noise XX 握手（responder）…");
    let ws = tokio_tungstenite::accept_async(tcp).await.expect("ws 升级失败");
    let (mut sink, mut stream) = ws.split();

    // Noise 握手
    let mut hs = snow::Builder::new(PATTERN.parse().unwrap())
        .local_private_key(&keypair())
        .unwrap()
        .build_responder()
        .unwrap();
    let mut buf = vec![0u8; 65535];
    let m1 = match stream.next().await {
        Some(Ok(Message::Binary(b))) => b.to_vec(),
        _ => panic!("msg1 格式错误"),
    };
    hs.read_message(&m1, &mut buf).unwrap();
    let n = hs.write_message(&[], &mut buf).unwrap();
    sink.send(Message::binary(buf[..n].to_vec())).await.unwrap();
    let m3 = match stream.next().await {
        Some(Ok(Message::Binary(b))) => b.to_vec(),
        _ => panic!("msg3 格式错误"),
    };
    hs.read_message(&m3, &mut buf).unwrap();
    let noise = Arc::new(Mutex::new(hs.into_transport_mode().unwrap()));
    let remote = noise.lock().unwrap().get_remote_static().map(|s| s.to_vec()).unwrap();
    println!("[bot] ✓ Noise 握手完成，对方静态密钥 {}…", &hex(&remote)[..16]);

    // 加密 hello
    let hello = match stream.next().await {
        Some(Ok(Message::Binary(b))) => open(&noise, &b),
        _ => panic!("hello 缺失"),
    };
    assert_eq!(hello[0], b'C', "hello 应为控制消息");
    let hw: Wire = serde_json::from_slice(&hello[1..]).unwrap();
    let (peer_id, peer_name) = match &hw {
        Wire::PigeonHello { id, name, .. } => (id.clone(), name.clone()),
        _ => panic!("应为 PigeonHello"),
    };
    println!("[bot] ✓ 加密 hello：{peer_name}（{}…）", &peer_id[..8]);
    sink.send(Message::binary(control_msg(&noise, &Wire::PigeonWelcome { ok: true, pc_name: Some("e2e测试节点".into()) })))
        .await
        .unwrap();

    // 发聊天
    let text = format!("来自 pigeonbot 的加密问候 {}", std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_millis());
    sink.send(Message::binary(control_msg(&noise, &Wire::PcText { text: text.clone() })))
        .await
        .unwrap();
    println!("[bot] → 已发加密聊天");

    // 推文件：先 offer，等决策
    let payload: Vec<u8> = (0..size).map(|i| (i % 251) as u8).collect();
    let file_id = format!("{}{}", hex(&sha(&payload)), hex(&sha(&(payload.len() as u64).to_be_bytes())));
    sink.send(Message::binary(control_msg(
        &noise,
        &Wire::PcFile { id: file_id.clone(), name: "pigeon-e2e.bin".into(), size: payload.len() as u64 },
    )))
    .await
    .unwrap();
    println!("[bot] → 已发文件邀请（{} 字节），等待决策…", payload.len());

    // 收消息循环：等 FileDecision
    let mut id16 = [0u8; 16];
    for (i, b) in file_id.as_bytes().chunks(2).enumerate() {
        id16[i] = u8::from_str_radix(std::str::from_utf8(b).unwrap(), 16).unwrap();
    }
    let decision_ok;
    loop {
        let msg = tokio::time::timeout(std::time::Duration::from_secs(120), stream.next())
            .await
            .expect("等待决策超时")
            .expect("连接断开")
            .expect("ws 错误");
        let plain = match msg {
            Message::Binary(b) => open(&noise, &b),
            Message::Close(_) => panic!("对方关闭"),
            _ => continue,
        };
        if plain.first() != Some(&b'C') {
            continue;
        }
        let w: Wire = serde_json::from_slice(&plain[1..]).unwrap();
        match w {
            Wire::FileDecision { accepted, .. } => {
                println!("[bot] {} 对方决策：{}", &file_id[..8], if accepted { "接受 ✓" } else { "拒绝" });
                if !accepted {
                    println!("[bot] ✗ 对方拒绝，测试中止");
                    std::process::exit(1);
                }
                decision_ok = true;
                break;
            }
            Wire::PcText { .. } => {}
            _ => {}
        }
    }
    assert!(decision_ok);
    let _ = decision_ok;

    // FileStart → 等接收方应答 FileResume（支持断点续传的接收方会带本地已有字节数）
    sink.send(Message::binary(control_msg(
        &noise,
        &Wire::FileStart { id: file_id.clone(), name: "pigeon-e2e.bin".into(), size: payload.len() as u64 },
    )))
    .await
    .unwrap();
    let resume_from: u64 = loop {
        let msg = tokio::time::timeout(std::time::Duration::from_secs(10), stream.next())
            .await
            .expect("等待 FileResume 超时")
            .expect("连接断开")
            .expect("ws 错误");
        let plain = match msg {
            Message::Binary(b) => open(&noise, &b),
            Message::Close(_) => panic!("对方关闭"),
            _ => continue,
        };
        if plain.first() != Some(&b'C') {
            continue;
        }
        match serde_json::from_slice::<Wire>(&plain[1..]).unwrap() {
            Wire::FileResume { id, have } => {
                assert_eq!(id, file_id, "FileResume id 不符");
                break have;
            }
            _ => continue,
        }
    };
    if resume_from > 0 {
        println!("[bot] ✓ 接收方已有 {} 字节，从断点续传", resume_from);
    }

    // 推送块（带 offset，接收方据此校验/续传）
    let mut sent: u64 = resume_from;
    let total_blocks = payload.len().div_ceil(CHUNK);
    let start_block = (resume_from as usize) / CHUNK;
    for (i, chunk) in payload[resume_from as usize..].chunks(CHUNK).enumerate() {
        sink.send(Message::binary(chunk_msg(&noise, &id16, sent, chunk))).await.unwrap();
        sent += chunk.len() as u64;
        if (start_block + i + 1).is_multiple_of(4) {
            println!("[bot] 推送进度 {}/{} 块", start_block + i + 1, total_blocks);
        }
    }
    sink.send(Message::binary(control_msg(&noise, &Wire::FileEnd { id: file_id.clone(), ok: true })))
        .await
        .unwrap();
    println!("[bot] ✓ 加密推送完成（{} 字节，hash 前 16 位 {}）", payload.len(), &hex(&sha(&payload))[..16]);
    println!("E2E-BOT-DONE {} {}", payload.len(), hex(&sha(&payload)));
    // 等一会让对方写完盘
    tokio::time::sleep(std::time::Duration::from_secs(2)).await;
}

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

fn sha(data: &[u8]) -> [u8; 8] {
    // 简单 FNV 累计做内容指纹（e2e 用足够）
    let mut h: u64 = 0xcbf29ce484222325;
    for &b in data {
        h ^= b as u64;
        h = h.wrapping_mul(0x100000001b3);
    }
    h.to_be_bytes()
}

// HashMap 占位引用（保留以便扩展）
#[allow(dead_code)]
type _Unused = HashMap<String, String>;
