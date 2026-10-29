//! 交互式终端：stdin 命令操作本机无头实例。
//! 信任确认与来件接收在此由人决策——刻意不做成 MCP 工具（安全门）。

use crate::shared::{self, PendingOffer, Shared};
use std::io::Write as _;
use std::sync::Arc;
use yunshu_core::UiRequest;

const HELP: &str = "命令：
  peers                        查看飞鸽主机列表
  status                       本机状态（服务/传输/待确认事项/最近聊天）
  log [条数]                   查看最近收到的聊天（默认 20 条）
  send <peer> <文本…>           发加密文本（peer 支持 id/机器ID 前缀或精确名字）
  sendfile <peer> <路径…>       发文件/文件夹（空格分隔多个，路径不能含空格）
  accept <offer_id前缀>         接受来件批次
  reject <offer_id前缀>         拒绝来件批次
  trust <peer>                 信任新设备（指纹见确认提示）
  rejecttrust <peer>           拒绝新设备
  addhost <host:port> [名称]    手动添加并连接主机
  connect <host:port>          重连已添加的主机
  quit                         广播下线并退出";

pub fn run(rt: &tokio::runtime::Runtime, shared: Arc<Shared>) {
    println!("云书无头节点（交互模式）。help 查看命令。");
    let stdin = std::io::stdin();
    loop {
        print!("yunshu> ");
        let _ = std::io::stdout().flush();
        let mut line = String::new();
        if stdin.read_line(&mut line).unwrap_or(0) == 0 {
            println!();
            break; // EOF（Ctrl-D）
        }
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let (cmd, rest) = line.split_once(' ').unwrap_or((line, ""));
        let rest = rest.trim();
        match cmd {
            "help" | "?" => println!("{HELP}"),
            "peers" => peers(&shared),
            "status" => status(&shared),
            "log" => log(&shared, rest.parse().unwrap_or(20)),
            "send" => match rest.split_once(' ') {
                Some((key, text)) => match shared.find_peer(key.trim()) {
                    Ok(p) => {
                        match rt.block_on(shared::send_text(&shared, &p.id, text.trim())) {
                            Ok(_) => println!("✓ 已发送"),
                            Err(e) => println!("✗ {e}"),
                        }
                    }
                    Err(e) => println!("✗ {e}"),
                },
                None => println!("用法: send <peer> <文本…>"),
            },
            "sendfile" => match rest.split_once(' ') {
                Some((key, paths)) => {
                    let paths: Vec<std::path::PathBuf> = paths
                        .split_whitespace()
                        .map(std::path::PathBuf::from)
                        .collect();
                    sendfile(rt, &shared, key.trim(), paths);
                }
                None => println!("用法: sendfile <peer> <路径…>"),
            },
            "accept" | "reject" => match find_offer(&shared, rest) {
                Ok(o) => {
                    let sent = if cmd == "accept" {
                        shared.send(UiRequest::PigeonAcceptFile {
                            peer_id: o.peer_id.clone(),
                            offer_id: o.offer_id.clone(),
                        })
                    } else {
                        shared.send(UiRequest::PigeonRejectFile {
                            peer_id: o.peer_id.clone(),
                            offer_id: o.offer_id.clone(),
                        })
                    };
                    println!(
                        "{}",
                        if sent {
                            format!("→ 已{}「{}」", if cmd == "accept" { "接受" } else { "拒绝" }, o.name)
                        } else {
                            "✗ 核心线程已退出".into()
                        }
                    );
                }
                Err(e) => println!("✗ {e}"),
            },
            "trust" => match shared.find_peer(rest) {
                Ok(p) => {
                    let fp = shared
                        .mirror
                        .lock()
                        .unwrap()
                        .pending_trusts
                        .iter()
                        .find(|(pid, _, _)| pid == &p.id)
                        .map(|(_, _, f)| f.clone());
                    match fp {
                        Some(fingerprint) => {
                            let _ = shared.send(UiRequest::PigeonTrust {
                                peer_id: p.id.clone(),
                                fingerprint,
                            });
                            println!("✓ 已信任「{}」，等它重连", p.name);
                        }
                        None => println!("✗ 「{}」没有待确认的信任请求", p.name),
                    }
                }
                Err(e) => println!("✗ {e}"),
            },
            "rejecttrust" => match shared.find_peer(rest) {
                Ok(p) => {
                    let _ = shared.send(UiRequest::PigeonRejectTrust { peer_id: p.id });
                    println!("→ 已拒绝");
                }
                Err(e) => println!("✗ {e}"),
            },
            "addhost" => match rest.split_once(' ') {
                Some((addr, name)) => {
                    let _ = shared.send(UiRequest::PigeonAddHost {
                        addr: addr.trim().to_string(),
                        name: name.trim().to_string(),
                    });
                    println!("→ 已发起添加并连接 {addr}");
                }
                None => {
                    if rest.is_empty() {
                        println!("用法: addhost <host:port> [名称]");
                    } else {
                        let _ = shared.send(UiRequest::PigeonAddHost {
                            addr: rest.to_string(),
                            name: String::new(),
                        });
                        println!("→ 已发起添加并连接 {rest}");
                    }
                }
            },
            "connect" => {
                if rest.is_empty() {
                    println!("用法: connect <host:port>");
                } else {
                    let _ = shared.send(UiRequest::PigeonConnectHost {
                        addr: rest.to_string(),
                    });
                    println!("→ 已发起连接 {rest}");
                }
            }
            "quit" | "exit" => {
                yunshu_core::pigeon::broadcast_leave();
                println!("已广播下线，再见");
                break;
            }
            _ => println!("未知命令。help 查看用法"),
        }
    }
}

fn peers(shared: &Shared) {
    let m = shared.mirror.lock().unwrap();
    if m.peers.is_empty() {
        println!("（主机列表为空：等发现广播，或 addhost <host:port>）");
        return;
    }
    for p in &m.peers {
        println!(
            "{} 「{}」 {}:{} {}机器ID:{}",
            if p.connected { "✓已连接" } else { "·未连接" },
            p.name,
            p.ip,
            p.tcp_port,
            if p.manual { "手动 " } else { "" },
            &p.machine_id[..8.min(p.machine_id.len())]
        );
        println!("    id: {}", p.id);
    }
}

fn status(shared: &Shared) {
    let m = shared.mirror.lock().unwrap();
    println!("本机机器 ID：{}", yunshu_core::remote::machine_id());
    match (&m.addr, &m.server_error) {
        (Some(a), _) => println!("服务地址：{a}"),
        (None, Some(e)) => println!("服务启动失败：{e}"),
        (None, None) => println!("服务：启动中…"),
    }
    for (pid, name, fp) in &m.pending_trusts {
        println!(
            "待信任：「{name}」（id {pid}，指纹 {}…）—— trust {pid}",
            &fp[..8.min(fp.len())]
        );
    }
    for o in &m.offers {
        println!(
            "待接收：「{}」（{} 字节）来自「{}」—— accept {}",
            o.name,
            o.size,
            o.peer_name,
            &o.offer_id[..8.min(o.offer_id.len())]
        );
    }
    println!(
        "传输记录 {} 条，最近聊天 {} 条",
        m.transfers.len(),
        m.chats.len()
    );
    if let Some(t) = m.transfers.last() {
        println!(
            "  最近：「{}」{}（{}/{} 字节）",
            t.name,
            shared::status_zh(t.status),
            t.transferred,
            t.size
        );
    }
}

fn log(shared: &Shared, n: usize) {
    let m = shared.mirror.lock().unwrap();
    let skip = m.chats.len().saturating_sub(n);
    for c in m.chats.iter().skip(skip) {
        println!(
            "← {}「{}」{}",
            if c.broadcast { "（广播）" } else { "" },
            c.peer_name,
            c.text
        );
    }
}

fn sendfile(
    rt: &tokio::runtime::Runtime,
    shared: &Arc<Shared>,
    key: &str,
    paths: Vec<std::path::PathBuf>,
) {
    let p = match shared.find_peer(key) {
        Ok(p) => p,
        Err(e) => {
            println!("✗ {e}");
            return;
        }
    };
    for path in &paths {
        if !path.exists() {
            println!("✗ 路径不存在：{}", path.display());
            return;
        }
    }
    println!(
        "→ 向「{}」发起批次（{} 项），等待对方确认与传输…",
        p.name,
        paths.len()
    );
    match rt.block_on(shared::send_files(shared, &p.id, paths, 600)) {
        Ok(v) => println!("✓ {}", serde_json::to_string_pretty(&v).unwrap_or_default()),
        Err(e) => println!("✗ {e}"),
    }
}

/// 按 offer_id 前缀定位待接收批次
fn find_offer(shared: &Shared, key: &str) -> Result<PendingOffer, String> {
    if key.is_empty() {
        return Err("用法: accept <offer_id前缀>".into());
    }
    let offers = shared.mirror.lock().unwrap().offers.clone();
    let hits: Vec<&PendingOffer> = offers
        .iter()
        .filter(|o| o.offer_id == key || o.offer_id.starts_with(key))
        .collect();
    match hits.len() {
        1 => Ok(hits[0].clone()),
        0 => Err(format!("没有匹配「{key}」的待接收批次（status 查看待办）")),
        _ => Err(format!("「{key}」匹配到 {} 个批次，请用更长前缀", hits.len())),
    }
}
