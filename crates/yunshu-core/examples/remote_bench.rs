//! 远程控制基准：真实屏幕起受控端 → 本地 WS 客户端收帧，分别测 JPEG / H.264 阶段的
//! fps、帧间隔、帧大小、码率。用于评估视频链路的产能与自适应行为（--release 运行）。

use futures_util::{SinkExt, StreamExt};
use snow::TransportState;
use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tokio_tungstenite::tungstenite::Message;
use yunshu_core::{CoreConfig, CoreEvent};

const PORT: u16 = 38480;
const HTTPS_PORT: u16 = 38481;
const PATTERN: &str = "Noise_XX_25519_ChaChaPoly_BLAKE2s";

fn seal(noise: &Mutex<TransportState>, plain: &[u8]) -> Vec<u8> {
    let mut buf = vec![0u8; plain.len() + 64];
    let n = noise.lock().unwrap().write_message(plain, &mut buf).expect("seal");
    buf.truncate(n);
    buf
}

/// 控制消息 = 'C' 前缀 + JSON（与服务端 open_msg 的分发约定一致）
fn seal_json(noise: &Mutex<TransportState>, json: &str) -> Vec<u8> {
    let mut plain = Vec::with_capacity(json.len() + 1);
    plain.push(b'C');
    plain.extend_from_slice(json.as_bytes());
    seal(noise, &plain)
}

fn open(noise: &Mutex<TransportState>, cipher: &[u8]) -> Option<(u8, Vec<u8>)> {
    let mut buf = vec![0u8; cipher.len()];
    let n = noise.lock().unwrap().read_message(cipher, &mut buf).ok()?;
    Some((buf[0], buf[1..n].to_vec()))
}

/// 帧重组：seq 新帧 idx=0 重建；集齐 idx 输出完整帧
struct Asm {
    seq: u32,
    total: u16,
    got: Vec<bool>,
    buf: Vec<u8>,
}
impl Asm {
    fn feed(&mut self, seq: u32, idx: u16, total: u16, data: &[u8]) -> Option<Vec<u8>> {
        if idx == 0 {
            self.seq = seq;
            self.total = total;
            self.got = vec![false; total as usize];
            self.buf = Vec::with_capacity(total as usize * 60_000);
        } else if seq != self.seq || total != self.total || idx as usize >= self.got.len() {
            return None;
        }
        let i = idx as usize;
        if self.got[i] {
            return None;
        }
        self.got[i] = true;
        self.buf.extend_from_slice(data);
        self.got.iter().all(|&g| g).then(|| std::mem::take(&mut self.buf))
    }
}

struct Stats {
    gaps: VecDeque<f64>,
    sizes: Vec<u64>,
    last: Option<Instant>,
}
impl Stats {
    fn new() -> Self {
        Stats { gaps: VecDeque::new(), sizes: Vec::new(), last: None }
    }
    fn frame(&mut self, size: usize) {
        let now = Instant::now();
        if let Some(t) = self.last {
            self.gaps.push_back((now - t).as_secs_f64() * 1000.0);
        }
        self.last = Some(now);
        self.sizes.push(size as u64);
    }
    fn report(&self, name: &str) {
        let n = self.gaps.len();
        if n == 0 {
            println!("[{name}] 无帧");
            return;
        }
        let mut gaps: Vec<f64> = self.gaps.iter().copied().collect();
        gaps.sort_by(|a, b| a.partial_cmp(b).unwrap());
        let med = gaps[n / 2];
        let p95 = gaps[(n as f64 * 0.95) as usize % n.max(1)];
        let fps = 1000.0 * n as f64 / gaps.iter().sum::<f64>();
        let bytes: u64 = self.sizes.iter().sum();
        let mbps = bytes as f64 * 8.0 / gaps.iter().sum::<f64>() / 1000.0;
        println!(
            "[{name}] 帧 {n} · fps {fps:.1} · 帧间隔中位 {med:.0}ms / p95 {p95:.0}ms · 平均帧 {:.0}KB · 码率 {mbps:.1}Mbps",
            bytes as f64 / self.sizes.len() as f64 / 1024.0
        );
    }
}

#[tokio::main(flavor = "multi_thread")]
async fn main() {
    let _ = tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::new("debug"))
        .try_init();
    // 隔离配置目录，不碰真实机器身份
    let dir = std::env::temp_dir().join(format!("yunshu-bench-{}", uuid::Uuid::new_v4().simple()));
    std::fs::create_dir_all(&dir).unwrap();
    // SAFETY: 基准进程启动早期、业务线程拉起前调用
    unsafe {
        std::env::set_var("HOME", &dir);
        std::env::set_var("XDG_CONFIG_HOME", &dir);
    }

    let (event_tx, event_rx) = std::sync::mpsc::channel::<CoreEvent>();
    let (_ui_tx, ui_rx) = std::sync::mpsc::channel::<yunshu_core::UiRequest>();
    let cfg = CoreConfig {
        port: PORT,
        https_port: HTTPS_PORT,
        save_dir: dir.clone(),
        pc_name: "bench受控端".into(),
        dnd_mode: false,
        stealth_mode: false,
    };
    tokio::spawn(yunshu_core::server::serve(cfg, event_tx, ui_rx));
    for _ in 0..50 {
        if tokio::net::TcpStream::connect(("127.0.0.1", PORT)).await.is_ok() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    let temp_password = loop {
        let ev = event_rx.recv().expect("事件通道");
        if let CoreEvent::RemoteTempPassword { password } = ev {
            break password;
        }
    };
    println!("受控端就绪，临时密码 {temp_password}");

    // ---- 连接 + Noise 握手 + 认证 ----
    let tcp = tokio::net::TcpStream::connect(("127.0.0.1", PORT)).await.unwrap();
    let (mut ws, _) =
        tokio_tungstenite::client_async(format!("ws://127.0.0.1:{PORT}/remote-ws"), tcp)
            .await
            .unwrap();
    let kp = snow::Builder::new(PATTERN.parse().unwrap()).generate_keypair().unwrap();
    let mut hs = snow::Builder::new(PATTERN.parse().unwrap())
        .local_private_key(&kp.private)
        .unwrap()
        .build_initiator()
        .unwrap();
    let mut buf = vec![0u8; 65535];
    let n = hs.write_message(&[], &mut buf).unwrap();
    ws.send(Message::binary(buf[..n].to_vec())).await.unwrap();
    let Message::Binary(m2) = ws.next().await.unwrap().unwrap() else { panic!() };
    hs.read_message(&m2, &mut buf).unwrap();
    let n = hs.write_message(&[], &mut buf).unwrap();
    ws.send(Message::binary(buf[..n].to_vec())).await.unwrap();
    let noise = Arc::new(Mutex::new(hs.into_transport_mode().unwrap()));

    let auth = format!(
        "{{\"type\":\"remote_auth\",\"mode\":\"temp\",\"password\":\"{temp_password}\",\"name\":\"bench\",\"machine_id\":\"00000000\"}}"
    );
    ws.send(Message::binary(seal_json(&noise, &auth))).await.unwrap();
    let mut screen = None;
    for _ in 0..2 {
        match ws.next().await {
            Some(Ok(Message::Binary(b))) => {
                let Some((b'C', plain)) = open(&noise, &b) else { continue };
                let v: serde_json::Value = serde_json::from_slice(&plain).unwrap();
                println!("服务端消息: {v}");
                if v["type"] == "remote_auth_result" && v["screen"].is_object() {
                    screen = Some(v["screen"].clone());
                }
            }
            other => {
                eprintln!("认证阶段连接异常: {other:?}");
                std::process::exit(2);
            }
        }
    }
    let screen = screen.expect("未收到屏幕信息");
    println!(
        "认证完成：采集 {}x{}（逻辑 {}x{}）",
        screen["width"], screen["height"], screen["logical_w"], screen["logical_h"]
    );

    // 观看端默认档（与 viewer.js 一致）：1080p / q65
    let quality = "{\"type\":\"remote_quality\",\"max_width\":1080,\"quality\":65}";
    ws.send(Message::binary(seal_json(&noise, quality))).await.unwrap();

    let mut asm = Asm { seq: 0, total: 0, got: vec![], buf: vec![] };
    let mut stats = Stats::new();
    let start = Instant::now();
    for (phase, codec_json) in [
        ("JPEG", None),
        ("H.264", Some("{\"type\":\"remote_use_codec\",\"codec\":1}")),
    ] {
        if let Some(codec) = codec_json {
            ws.send(Message::binary(seal_json(&noise, codec))).await.unwrap();
        }
        let mut asm = Asm { seq: 0, total: 0, got: vec![], buf: vec![] };
        let mut stats = Stats::new();
        let phase_end = Instant::now() + Duration::from_secs(8);
        loop {
            let wait = phase_end.checked_duration_since(Instant::now()).unwrap_or_default();
            let msg = tokio::time::timeout(wait, ws.next()).await;
            let Ok(Some(Ok(Message::Binary(b)))) = msg else { break };
            match open(&noise, &b) {
                Some((b'V', plain)) => {
                    if plain.len() < 10 {
                        continue;
                    }
                    let seq = u32::from_le_bytes(plain[0..4].try_into().unwrap());
                    let idx = u16::from_le_bytes(plain[4..6].try_into().unwrap());
                    let total = u16::from_le_bytes(plain[6..8].try_into().unwrap());
                    if let Some(frame) = asm.feed(seq, idx, total, &plain[10..]) {
                        stats.frame(frame.len());
                    }
                }
                Some((b'C', plain)) => {
                    let v: serde_json::Value = serde_json::from_slice(&plain).unwrap_or_default();
                    if v["type"] == "ping" {
                        let pong = format!("{{\"type\":\"pong\",\"t\":{},\"lost\":0}}", v["t"]);
                        ws.send(Message::binary(seal_json(&noise, &pong))).await.ok();
                    }
                }
                _ => {}
            }
        }
        stats.report(phase);
    }
    println!("总耗时 {:.1}s", start.elapsed().as_secs_f64());
    let _ = std::fs::remove_dir_all(&dir);
}
