//! pigeonbot：无头飞鸽测试节点。
//! 广播自己的存在 → 等云书实例连入（Noise XX，bot 是 responder）→
//! 发一条聊天 → 推送一个随机内容批次（等对方自动/手动接受）→ 校验对方拉取。
//! 同时是完整接收端：对云书发来的批次邀请（PcFileOffer）自动接受、收块写盘、
//! 逐文件 SHA-256 校验、FileEnd 后落盘——覆盖 app→bot 方向（拖拽/右键发送）的 e2e。
//! 用法：pigeonbot [推送大小字节] [推送完成后等待来件的秒数]（默认 1MB / 30 秒，0 = 推完即退）

use futures_util::{SinkExt, StreamExt};
use sha2::Digest;
use snow::TransportState;
use std::net::{SocketAddr, UdpSocket};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tokio::net::TcpListener;
use tokio_tungstenite::tungstenite::Message;

const PATTERN: &str = "Noise_XX_25519_ChaChaPoly_BLAKE2s";
const BOT_PORT: u16 = 9000;
const CHUNK: usize = 60_000;
/// 来件持续无数据的兜底超时（秒）
const RECV_IDLE_TIMEOUT: u64 = 120;

#[derive(serde::Serialize, serde::Deserialize)]
struct OfferFile {
    name: String,
    size: u64,
}

#[derive(serde::Serialize, serde::Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum Wire {
    PigeonHello {
        id: String,
        name: String,
        port: u16,
    },
    PigeonWelcome {
        ok: bool,
        #[serde(default)]
        pc_name: Option<String>,
    },
    PcText {
        text: String,
    },
    /// 广播聊天（app 会向所有链路广播，bot 需认识该变体避免解析告警）
    BroadcastText {
        text: String,
    },
    PcFileOffer {
        id: String,
        name: String,
        total_size: u64,
        files: Vec<OfferFile>,
    },
    FileDecision {
        id: String,
        accepted: bool,
    },
    FileStart {
        id: String,
    },
    /// 接收方应答：本地已有 have 字节，发送方从断点续推
    FileResume {
        id: String,
        have: u64,
    },
    /// 单文件推送完成，携带整文件 SHA-256
    FileHash {
        id: String,
        index: u32,
        sha: String,
    },
    /// 任一方取消传输：接收方删半成品，发送方停止推送
    FileCancel {
        id: String,
    },
    /// 任一方暂停/恢复传输
    FilePause {
        id: String,
        paused: bool,
    },
    /// 批次推送结束
    FileEnd {
        id: String,
        ok: bool,
    },
}

/// 推送任务等待的两个回执（由主循环分发转发）
enum PushEvent {
    Decision(bool),
    Resume(u64),
}

fn seal(noise: &Mutex<TransportState>, plain: &[u8]) -> Vec<u8> {
    let mut buf = vec![0u8; plain.len() + 64];
    let n = noise
        .lock()
        .unwrap()
        .write_message(plain, &mut buf)
        .expect("write");
    buf.truncate(n);
    buf
}

fn open_plain(noise: &Mutex<TransportState>, cipher: &[u8]) -> Option<Vec<u8>> {
    let mut buf = vec![0u8; cipher.len()];
    let n = noise.lock().unwrap().read_message(cipher, &mut buf).ok()?;
    Some(buf[..n].to_vec())
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
    let kp = snow::Builder::new(PATTERN.parse().unwrap())
        .generate_keypair()
        .unwrap();
    let _ = std::fs::write(&path, &kp.private);
    kp.private
}

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

/// 来件批次接收状态（镜像 core 的接收语义；bot 每次全新接收，不支持断点续传）
struct Incoming {
    id: String,
    id16: [u8; 16],
    name: String,
    files: Vec<OfferFile>,
    dir: PathBuf,
    index: usize,
    written: u64,
    have: u64,
    hash_pending: bool,
    running_hash: Option<sha2::Sha256>,
    part_file: Option<std::fs::File>,
    last_activity: Instant,
}

impl Incoming {
    fn new(id: String, name: String, files: Vec<OfferFile>) -> Self {
        let mut id16 = [0u8; 16];
        for (i, b) in id.as_bytes().chunks(2).enumerate() {
            id16[i] = u8::from_str_radix(std::str::from_utf8(b).unwrap(), 16).unwrap();
        }
        let dir = std::env::temp_dir().join("pigeonbot_recv").join(&id);
        Self {
            id,
            id16,
            name,
            files,
            dir,
            index: 0,
            written: 0,
            have: 0,
            hash_pending: false,
            running_hash: None,
            part_file: None,
            last_activity: Instant::now(),
        }
    }

    /// 数据块入账：offset 是批次虚拟流偏移，与本地 have 自校验（镜像 core：越位/落后忽略）
    fn ingest(&mut self, offset: u64, data: &[u8]) {
        self.last_activity = Instant::now();
        if offset > self.have {
            println!("[bot] ⚠ 来件越位数据块（{offset} > {}），忽略", self.have);
            return;
        }
        if offset < self.have {
            println!("[bot] ⚠ 来件落后数据块（{offset} < {}），忽略", self.have);
            return;
        }
        if self.hash_pending || self.index >= self.files.len() {
            return; // 等待 FileHash 中 / 已收全
        }
        if self.part_file.is_none() {
            // 惰性打开当前文件（从文件头重写）
            let part = self.dir.join(format!("{}.part", self.index));
            let Ok(f) = std::fs::OpenOptions::new()
                .create(true)
                .write(true)
                .truncate(true)
                .open(&part)
            else {
                eprintln!("[bot] ✗ 打开半成品失败 {part:?}，中止");
                std::process::exit(1);
            };
            self.part_file = Some(f);
            self.running_hash = Some(sha2::Sha256::new());
        }
        {
            use std::io::Write;
            let f = self.part_file.as_mut().unwrap();
            if let Err(e) = f.write_all(data) {
                eprintln!("[bot] ✗ 写入半成品失败: {e}，中止");
                std::process::exit(1);
            }
        }
        if let Some(h) = self.running_hash.as_mut() {
            h.update(data);
        }
        self.written += data.len() as u64;
        self.have += data.len() as u64;
        if self.written == self.files[self.index].size {
            self.part_file = None; // 单文件写满 → 落盘关闭，等 FileHash
            self.hash_pending = true;
        }
    }

    fn file_hash(
        &mut self,
        index: u32,
        sha: &str,
        out_tx: &tokio::sync::mpsc::Sender<Vec<u8>>,
        noise: &Arc<Mutex<TransportState>>,
    ) {
        self.last_activity = Instant::now();
        if index as usize != self.index {
            println!(
                "[bot] ⚠ FileHash 序号不符（#{index} vs 当前 #{}），忽略",
                self.index
            );
            return;
        }
        let actual = match self.running_hash.take() {
            Some(h) => hex(&h.finalize()),
            None => hex(&sha2::Sha256::new().finalize()), // 空文件
        };
        self.hash_pending = false;
        if actual == sha {
            let (name, size) = {
                let f = &self.files[self.index];
                (f.name.clone(), f.size)
            };
            println!("[bot] ✓ 「{name}」校验通过（{size} 字节）");
            self.index += 1;
            self.written = 0;
        } else {
            let name = self.files[self.index].name.clone();
            println!("[bot] ✗ 「{name}」哈希校验失败（{actual}），中止");
            let _ = out_tx.try_send(control_msg(
                noise,
                &Wire::FileCancel {
                    id: self.id.clone(),
                },
            ));
            std::process::exit(1);
        }
    }

    /// FileEnd(ok=true)：镜像 core——清单未齐保留半成品，齐了按相对路径落盘
    fn finish(&mut self) -> bool {
        if self.index != self.files.len() || self.hash_pending {
            println!(
                "[bot] ⚠ FileEnd 时清单未完成（{}/{} 项），保留半成品",
                self.index,
                self.files.len()
            );
            return false;
        }
        let out = self.dir.join("out");
        for (i, f) in self.files.iter().enumerate() {
            let src = self.dir.join(format!("{i}.part"));
            let dst = out.join(&f.name);
            if let Some(p) = dst.parent()
                && let Err(e) = std::fs::create_dir_all(p)
            {
                eprintln!("[bot] ✗ 创建目录失败 {:?}: {e}", p);
                std::process::exit(1);
            }
            // 空文件没有任何数据块，part 不存在 → 补一个空 part（镜像 core）
            if !src.exists()
                && let Err(e) = std::fs::write(&src, b"")
            {
                eprintln!("[bot] ✗ 补写空文件失败 {src:?}: {e}");
                std::process::exit(1);
            }
            if let Err(e) = std::fs::rename(&src, &dst) {
                eprintln!("[bot] ✗ 落盘失败 {:?}: {e}", dst);
                std::process::exit(1);
            }
        }
        let total: u64 = self.files.iter().map(|f| f.size).sum();
        println!(
            "[bot] ✓ 批次「{}」收全 {} 项（{} 字节）→ {}",
            self.name,
            self.files.len(),
            total,
            out.display()
        );
        println!("E2E-BOT-RECV-DONE {} {}", total, self.id);
        true
    }
}

#[tokio::main]
async fn main() {
    let size: usize = std::env::args()
        .nth(1)
        .and_then(|a| a.parse().ok())
        .unwrap_or(1024 * 1024);
    // 推送完成后继续等待来件的窗口（秒）；测 app→bot 方向（拖拽/右键发送）时靠它保持在线
    let stay_secs: u64 = std::env::args()
        .nth(2)
        .and_then(|a| a.parse().ok())
        .unwrap_or(30);
    let bot_id: String = {
        let k = keypair();
        k.iter().map(|b| format!("{b:02x}")).collect::<String>()[..32].to_string()
    };
    println!(
        "[bot] 启动 id={} port={BOT_PORT} 推送={size}B 等待来件={stay_secs}s",
        &bot_id[..8]
    );

    // UDP 广播
    std::thread::spawn(move || {
        let sock = socket2::Socket::new(
            socket2::Domain::IPV4,
            socket2::Type::DGRAM,
            Some(socket2::Protocol::UDP),
        )
        .unwrap();
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
    let ws = tokio_tungstenite::accept_async(tcp)
        .await
        .expect("ws 升级失败");
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
    let remote = noise
        .lock()
        .unwrap()
        .get_remote_static()
        .map(|s| s.to_vec())
        .unwrap();
    println!(
        "[bot] ✓ Noise 握手完成，对方静态密钥 {}…",
        &hex(&remote)[..16]
    );

    // 出站统一走 writer 任务：推送任务与主循环都通过 out_tx 发帧
    let (out_tx, mut out_rx) = tokio::sync::mpsc::channel::<Vec<u8>>(64);
    let _writer = tokio::spawn(async move {
        let mut sink = sink;
        while let Some(frame) = out_rx.recv().await {
            if sink.send(Message::binary(frame)).await.is_err() {
                break;
            }
        }
    });

    // 加密 hello / welcome
    let hello = match stream.next().await {
        Some(Ok(Message::Binary(b))) => open_plain(&noise, &b).expect("hello 解密失败"),
        _ => panic!("hello 缺失"),
    };
    assert_eq!(hello[0], b'C', "hello 应为控制消息");
    let hw: Wire = serde_json::from_slice(&hello[1..]).unwrap();
    let (peer_id, peer_name) = match &hw {
        Wire::PigeonHello { id, name, .. } => (id.clone(), name.clone()),
        _ => panic!("应为 PigeonHello"),
    };
    println!("[bot] ✓ 加密 hello：{peer_name}（{}…）", &peer_id[..8]);
    let _ = out_tx
        .send(control_msg(
            &noise,
            &Wire::PigeonWelcome {
                ok: true,
                pc_name: Some("e2e测试节点".into()),
            },
        ))
        .await;

    // 推送批次（bot → app）：原 e2e 流程；FileDecision/FileResume 回执经 push_tx 转发
    let (push_tx, mut push_rx) = tokio::sync::mpsc::channel::<PushEvent>(8);

    // 稳定批次 id：sha256(逐项 名称|大小) 前 32 hex
    struct FileSpec {
        name: &'static str,
        size: u64,
        pattern: u64,
    }
    let specs = [
        FileSpec {
            name: "pigeon-a.bin",
            size: 1_048_576,
            pattern: 251,
        },
        FileSpec {
            name: "pigeon-b.bin",
            size: size as u64,
            pattern: 7,
        },
        FileSpec {
            name: "pigeon-c.bin",
            size: 0,
            pattern: 1,
        },
    ];
    let total_size: u64 = specs.iter().map(|s| s.size).sum();
    let mut idh = sha2::Sha256::new();
    idh.update((specs.len() as u64).to_le_bytes());
    for s in &specs {
        idh.update(s.name.as_bytes());
        idh.update([0]);
        idh.update(s.size.to_le_bytes());
        idh.update([0]);
    }
    let file_id = hex(&idh.finalize())[..32].to_string();
    let push_offer_id = file_id.clone();
    let push_noise = noise.clone();
    let push_out = out_tx.clone();
    let push_handle = tokio::spawn(async move {
        // 发聊天
        let text = format!(
            "来自 pigeonbot 的加密问候 {}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_millis()
        );
        let _ = push_out
            .send(control_msg(
                &push_noise,
                &Wire::PcText { text: text.clone() },
            ))
            .await;
        println!("[bot] → 已发加密聊天");

        let offer_files: Vec<OfferFile> = specs
            .iter()
            .map(|s| OfferFile {
                name: s.name.to_string(),
                size: s.size,
            })
            .collect();
        let _ = push_out
            .send(control_msg(
                &push_noise,
                &Wire::PcFileOffer {
                    id: file_id.clone(),
                    name: format!("e2e批次（{size}）"),
                    total_size,
                    files: offer_files,
                },
            ))
            .await;
        println!(
            "[bot] → 已发批次邀请（{} 项，{} 字节），等待决策…",
            specs.len(),
            total_size
        );

        // 等决策
        match tokio::time::timeout(Duration::from_secs(120), push_rx.recv()).await {
            Ok(Some(PushEvent::Decision(true))) => {
                println!("[bot] {} 对方决策：接受 ✓", &file_id[..8]);
            }
            Ok(Some(PushEvent::Decision(false))) => {
                println!("[bot] ✗ 对方拒绝，测试中止");
                std::process::exit(1);
            }
            Ok(None) => {
                println!("[bot] ✗ 连接断开，测试中止");
                std::process::exit(1);
            }
            Ok(Some(PushEvent::Resume(_))) => {
                println!("[bot] ✗ 决策前收到 FileResume，状态错乱");
                std::process::exit(1);
            }
            Err(_) => {
                println!("[bot] ✗ 等待决策超时");
                std::process::exit(1);
            }
        }

        // FileStart → 等接收方应答 FileResume（支持断点续传的接收方会带流偏移）
        let _ = push_out
            .send(control_msg(
                &push_noise,
                &Wire::FileStart {
                    id: file_id.clone(),
                },
            ))
            .await;
        let resume_from: u64 =
            match tokio::time::timeout(Duration::from_secs(10), push_rx.recv()).await {
                Ok(Some(PushEvent::Resume(have))) => have,
                _ => {
                    println!("[bot] ✗ 等待 FileResume 超时");
                    std::process::exit(1);
                }
            };
        if resume_from > 0 {
            println!("[bot] ✓ 接收方已有 {resume_from} 字节，从断点续传");
        }

        // 推送块：流偏移定位到 (文件, 文件内偏移)，逐文件推 + FileHash（整文件 SHA-256）
        let mut id16 = [0u8; 16];
        for (i, b) in file_id.as_bytes().chunks(2).enumerate() {
            id16[i] = u8::from_str_radix(std::str::from_utf8(b).unwrap(), 16).unwrap();
        }
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
        let total_blocks: usize =
            sizes.iter().map(|s| s.div_ceil(CHUNK as u64)).sum::<u64>() as usize;
        let start_block: usize = (sizes[..idx]
            .iter()
            .map(|s| s.div_ceil(CHUNK as u64))
            .sum::<u64>()
            + in_off.div_ceil(CHUNK as u64)) as usize;
        let mut pushed_blocks = 0usize;
        for (fi, spec) in specs.iter().enumerate().skip(idx) {
            // 进入大文件前演示暂停/恢复：验证接收方 FilePause 处理路径
            if fi == 1 && in_off == 0 {
                let _ = push_out
                    .send(control_msg(
                        &push_noise,
                        &Wire::FilePause {
                            id: file_id.clone(),
                            paused: true,
                        },
                    ))
                    .await;
                println!("[bot] → 已请求接收方暂停 3 秒");
                tokio::time::sleep(Duration::from_secs(3)).await;
                let _ = push_out
                    .send(control_msg(
                        &push_noise,
                        &Wire::FilePause {
                            id: file_id.clone(),
                            paused: false,
                        },
                    ))
                    .await;
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
                let _ = push_out
                    .send(chunk_msg(&push_noise, &id16, sent, &chunk))
                    .await;
                hasher.update(&chunk);
                sent += chunk.len() as u64;
                off = end;
                pushed_blocks += 1;
                if pushed_blocks.is_multiple_of(64) {
                    println!(
                        "[bot] 推送进度 {}/{} 块",
                        start_block + pushed_blocks,
                        total_blocks
                    );
                }
            }
            let digest = hasher.finalize();
            let sha: String = digest.iter().map(|x| format!("{x:02x}")).collect();
            let _ = push_out
                .send(control_msg(
                    &push_noise,
                    &Wire::FileHash {
                        id: file_id.clone(),
                        index: fi as u32,
                        sha,
                    },
                ))
                .await;
            println!("[bot] ✓ 「{}」推送完成（{} 字节）", spec.name, spec.size);
        }
        let _ = push_out
            .send(control_msg(
                &push_noise,
                &Wire::FileEnd {
                    id: file_id.clone(),
                    ok: true,
                },
            ))
            .await;
        println!(
            "[bot] ✓ 批次加密推送完成（{} 项，{} 字节）",
            specs.len(),
            sent
        );
        println!("E2E-BOT-DONE {} {}", total_size, file_id);
    });

    // 主循环：读流分发（推送回执转发 + app 发来的批次接收）
    let mut incoming: Option<Incoming> = None;
    let mut push_finished_at: Option<Instant> = None;
    let mut tick = tokio::time::interval(Duration::from_millis(250));

    loop {
        tokio::select! {
            maybe = stream.next() => {
                let msg = match maybe {
                    Some(Ok(m)) => m,
                    _ => {
                        println!("[bot] ✗ 连接断开");
                        std::process::exit(1);
                    }
                };
                let Message::Binary(b) = msg else { continue };
                let Some(plain) = open_plain(&noise, &b) else { continue };
                match plain.first() {
                    Some(b'C') => match serde_json::from_slice::<Wire>(&plain[1..]) {
                        Ok(w) => {
                            handle_control(w, &push_offer_id, &push_tx, &out_tx, &noise, &mut incoming).await;
                        }
                        Err(e) => println!("[bot] ⚠ 控制消息解析失败: {e}"),
                    },
                    Some(b'F') if plain.len() >= 25 => {
                        let mut rid = [0u8; 16];
                        rid.copy_from_slice(&plain[1..17]);
                        let offset = u64::from_le_bytes(plain[17..25].try_into().unwrap());
                        if let Some(inc) = incoming.as_mut()
                            && inc.id16 == rid
                        {
                            inc.ingest(offset, &plain[25..]);
                        }
                    }
                    _ => {}
                }
            }
            _ = tick.tick() => {
                if push_handle.is_finished() && push_finished_at.is_none() {
                    push_finished_at = Some(Instant::now());
                }
                if let Some(inc) = incoming.as_mut() {
                    // 来件进行中：只受空闲超时约束（stay 窗口不打断传输）
                    if inc.last_activity.elapsed() > Duration::from_secs(RECV_IDLE_TIMEOUT) {
                        println!("[bot] ✗ 来件 {} 秒无数据，超时中止", RECV_IDLE_TIMEOUT);
                        std::process::exit(1);
                    }
                } else if let Some(t0) = push_finished_at
                    && t0.elapsed() >= Duration::from_secs(stay_secs.max(2))
                {
                    // stay=0 时保留旧语义：推送完成后留 2 秒给对方收尾写盘再退出
                    println!("[bot] 会话结束（推送完成，{} 秒内无来件）", stay_secs);
                    tokio::time::sleep(Duration::from_secs(2)).await;
                    std::process::exit(0);
                }
            }
        }
    }
}

/// 控制消息分发：推送回执 → push_tx；来件 → 接收状态机
async fn handle_control(
    w: Wire,
    push_offer_id: &str,
    push_tx: &tokio::sync::mpsc::Sender<PushEvent>,
    out_tx: &tokio::sync::mpsc::Sender<Vec<u8>>,
    noise: &Arc<Mutex<TransportState>>,
    incoming: &mut Option<Incoming>,
) {
    match w {
        Wire::FileDecision { id, accepted } if id == push_offer_id => {
            let _ = push_tx.send(PushEvent::Decision(accepted)).await;
        }
        Wire::FileResume { id, have } if id == push_offer_id => {
            let _ = push_tx.send(PushEvent::Resume(have)).await;
        }
        Wire::FileCancel { id } if id == push_offer_id => {
            println!("[bot] ✗ 对方取消了我们的推送，测试中止");
            std::process::exit(1);
        }
        Wire::PcText { text } => println!("[bot] ← 聊天：{text}"),
        Wire::BroadcastText { text } => println!("[bot] ← 广播：{text}"),
        Wire::PcFileOffer {
            id,
            name,
            total_size,
            files,
        } => {
            if incoming.is_some() {
                println!("[bot] ⚠ 正在接收批次，拒绝新邀请「{name}」");
                let _ = out_tx
                    .send(control_msg(
                        noise,
                        &Wire::FileDecision {
                            id,
                            accepted: false,
                        },
                    ))
                    .await;
                return;
            }
            println!(
                "[bot] ← 批次邀请「{name}」（{} 项，{total_size} 字节），自动接受",
                files.len()
            );
            let _ = out_tx
                .send(control_msg(
                    noise,
                    &Wire::FileDecision {
                        id: id.clone(),
                        accepted: true,
                    },
                ))
                .await;
            *incoming = Some(Incoming::new(id, name, files));
        }
        Wire::FileStart { id } => {
            let Some(inc) = incoming.as_mut() else { return };
            if inc.id != id {
                return;
            }
            if let Err(e) = std::fs::create_dir_all(&inc.dir) {
                eprintln!("[bot] ✗ 创建接收目录失败 {:?}: {e}", inc.dir);
                std::process::exit(1);
            }
            println!("[bot] ✓ 批次开始接收，回 FileResume(have=0)");
            let _ = out_tx
                .send(control_msg(noise, &Wire::FileResume { id, have: 0 }))
                .await;
        }
        Wire::FileHash { id, index, sha } => {
            let Some(inc) = incoming.as_mut() else { return };
            if inc.id == id {
                inc.file_hash(index, &sha, out_tx, noise);
            }
        }
        Wire::FileEnd { id, ok } => {
            if !incoming.as_ref().is_some_and(|inc| inc.id == id) {
                return;
            }
            let inc = incoming.as_mut().unwrap();
            inc.last_activity = Instant::now();
            if ok {
                if inc.finish() {
                    *incoming = None;
                }
            } else {
                // 发送方中断：bot 不支持续传，丢弃半成品，等服务端重新邀请
                let dir = inc.dir.clone();
                *incoming = None;
                let _ = std::fs::remove_dir_all(dir);
                println!("[bot] ⚠ 发送方中断推送，丢弃半成品");
            }
        }
        Wire::FileCancel { id } => {
            if incoming.as_ref().is_some_and(|inc| inc.id == id) {
                let inc = incoming.as_mut().unwrap();
                let dir = inc.dir.clone();
                *incoming = None;
                let _ = std::fs::remove_dir_all(dir);
                println!("[bot] ⚠ 来件批次被对方取消，已清理半成品");
            }
        }
        Wire::FilePause { .. } => {}
        _ => {}
    }
}
