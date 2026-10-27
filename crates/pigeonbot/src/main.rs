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
struct OfferFile {
    name: String,
    size: u64,
}

#[derive(serde::Serialize, serde::Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum Wire {
    PigeonHello { id: String, name: String, port: u16 },
    PigeonWelcome { ok: bool, #[serde(default)] pc_name: Option<String> },
    PcText { text: String },
    PcFileOffer {
        id: String,
        name: String,
        total_size: u64,
        files: Vec<OfferFile>,
    },
    FileDecision { id: String, accepted: bool },
    FileStart { id: String },
    /// 接收方应答：本地已有 have 字节，发送方从断点续推
    FileResume { id: String, have: u64 },
    /// 单文件推送完成，携带整文件 SHA-256
    FileHash { id: String, index: u32, sha: String },
    /// 任一方暂停/恢复传输
    FilePause { id: String, paused: bool },
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

    // 推文件：三文件批次（小文件 / 大文件 / 空文件），字节按需生成，零内存驻留
    struct FileSpec {
        name: &'static str,
        size: u64,
        pattern: u64,
    }
    let specs = [
        FileSpec { name: "pigeon-a.bin", size: 1_048_576, pattern: 251 },
        FileSpec { name: "pigeon-b.bin", size: size as u64, pattern: 7 },
        FileSpec { name: "pigeon-c.bin", size: 0, pattern: 1 },
    ];
    let total_size: u64 = specs.iter().map(|s| s.size).sum();
    // 稳定批次 id：sha256(逐项 名称|大小) 前 32 hex
    let mut idh = sha2::Sha256::new();
    use sha2::Digest;
    idh.update((specs.len() as u64).to_le_bytes());
    for s in &specs {
        idh.update(s.name.as_bytes());
        idh.update([0]);
        idh.update(s.size.to_le_bytes());
        idh.update([0]);
    }
    let file_id = hex(&idh.finalize())[..32].to_string();
    let offer_files: Vec<OfferFile> = specs
        .iter()
        .map(|s| OfferFile { name: s.name.to_string(), size: s.size })
        .collect();
    sink.send(Message::binary(control_msg(
        &noise,
        &Wire::PcFileOffer {
            id: file_id.clone(),
            name: format!("e2e批次（{}）", size),
            total_size,
            files: offer_files,
        },
    )))
    .await
    .unwrap();
    println!("[bot] → 已发批次邀请（{} 项，{} 字节），等待决策…", specs.len(), total_size);

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

    // FileStart → 等接收方应答 FileResume（支持断点续传的接收方会带流偏移）
    sink.send(Message::binary(control_msg(&noise, &Wire::FileStart { id: file_id.clone() })))
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

    // 推送块：流偏移定位到 (文件, 文件内偏移)，逐文件推 + FileHash（整文件 SHA-256）
    let sizes: Vec<u64> = specs.iter().map(|s| s.size).collect();
    let mut sent: u64 = resume_from;
    let mut loc = sent;
    let mut idx = specs.len();
    let mut in_off = 0u64;
    for (i, sz) in sizes.iter().enumerate() {
        if loc < *sz {
            idx = i;
            in_off = loc;
            break;
        }
        loc -= sz;
    }
    let total_blocks: usize = sizes.iter().map(|s| s.div_ceil(CHUNK as u64)).sum::<u64>() as usize;
    let start_block: usize = (sizes[..idx].iter().map(|s| s.div_ceil(CHUNK as u64)).sum::<u64>() + in_off.div_ceil(CHUNK as u64)) as usize;
    let mut pushed_blocks = 0usize;
    for (fi, spec) in specs.iter().enumerate().skip(idx) {
        // 进入大文件前演示暂停/恢复：验证接收方 FilePause 处理路径
        if fi == 1 && in_off == 0 {
            sink.send(Message::binary(control_msg(
                &noise,
                &Wire::FilePause { id: file_id.clone(), paused: true },
            )))
            .await
            .unwrap();
            println!("[bot] → 已请求接收方暂停 3 秒");
            tokio::time::sleep(std::time::Duration::from_secs(3)).await;
            sink.send(Message::binary(control_msg(
                &noise,
                &Wire::FilePause { id: file_id.clone(), paused: false },
            )))
            .await
            .unwrap();
            println!("[bot] → 已请求恢复");
        }
        let mut hasher = sha2::Sha256::new();
        // 续传前缀（若有）按同一模式重算，计入整文件哈希
        let mut off = in_off;
        while off > 0 {
            let start = off.saturating_sub(CHUNK as u64);
            let seg: Vec<u8> = (start..off).map(|i| (i % spec.pattern) as u8).collect();
            hasher.update(&seg);
            off = start;
        }
        off = in_off;
        while off < spec.size {
            let end = (off + CHUNK as u64).min(spec.size);
            let chunk: Vec<u8> = (off..end).map(|i| (i % spec.pattern) as u8).collect();
            sink.send(Message::binary(chunk_msg(&noise, &id16, sent, &chunk))).await.unwrap();
            hasher.update(&chunk);
            sent += chunk.len() as u64;
            off = end;
            pushed_blocks += 1;
            if pushed_blocks.is_multiple_of(64) {
                println!("[bot] 推送进度 {}/{} 块", start_block + pushed_blocks, total_blocks);
            }
        }
        let digest = hasher.finalize();
        let sha: String = digest.iter().map(|x| format!("{x:02x}")).collect();
        sink.send(Message::binary(control_msg(
            &noise,
            &Wire::FileHash { id: file_id.clone(), index: fi as u32, sha },
        )))
        .await
        .unwrap();
        println!("[bot] ✓ 「{}」推送完成（{} 字节）", spec.name, spec.size);
    }
    sink.send(Message::binary(control_msg(&noise, &Wire::FileEnd { id: file_id.clone(), ok: true })))
        .await
        .unwrap();
    println!("[bot] ✓ 批次加密推送完成（{} 项，{} 字节）", specs.len(), sent);
    println!("E2E-BOT-DONE {} {}", total_size, file_id);
    // 等一会让对方写完盘
    tokio::time::sleep(std::time::Duration::from_secs(2)).await;
}

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}


// HashMap 占位引用（保留以便扩展）
#[allow(dead_code)]
type _Unused = HashMap<String, String>;
