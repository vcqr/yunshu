//! 交互式终端：stdin 命令操作本机无头实例。
//! 信任确认与来件接收在此由人决策——刻意不做成 MCP 工具（安全门）。

use crate::shared::{self, PendingOffer, Shared};
use std::io::Write as _;
use std::sync::Arc;
use yunshu_core::UiRequest;

const HELP: &str = "命令：
  peers                        查看飞鸽主机列表
  status                       本机状态（服务/传输/待确认事项/最近聊天）
  log [条数]                   查看最近聊天记录（SQLite 落库，收发双向）
  send <peer> <文本…>           发加密文本（peer 支持 id/机器ID 前缀或精确名字）
  sendfile <peer> <路径…>       发文件/文件夹（空格分隔多个，路径不能含空格）
  accept <offer_id前缀>         接受来件批次
  reject <offer_id前缀>         拒绝来件批次
  trust <peer>                 信任新设备（指纹见确认提示）
  rejecttrust <peer>           拒绝新设备
  addhost <host:port> [名称]    手动添加并连接主机
  connect <host:port>          重连已添加的主机";

/// 微信通道是隐藏功能（YUNSHU_WEIXIN=1）：开启时才在 help 里展示
const HELP_WX: &str = "  wx login                     微信通道：扫码登录（终端出二维码）
  wx status | logout           微信通道：状态 / 退出登录
  wx send <文本…>               微信通道：发文本（默认扫码绑定的微信账号）
  wx sendto <user_id> <文本…>   微信通道：发文本给指定用户
  wx sendfile <路径…>           微信通道：发文件/图片/视频（空格分隔多个）
  wx inbox [条数]              微信通道：最近收到的消息与媒体";

const HELP_QUIT: &str = "  quit                         广播下线并退出";

fn help_text() -> String {
    if yunshu_core::weixin::enabled() {
        format!("{HELP}\n{HELP_WX}\n{HELP_QUIT}")
    } else {
        format!("{HELP}\n{HELP_QUIT}")
    }
}

pub fn run(rt: &tokio::runtime::Runtime, shared: Arc<Shared>) {
    println!("Yunshu 无头节点（交互模式）。help 查看命令。");
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
            "help" | "?" => println!("{}", help_text()),
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
            "wx" => wx(rt, rest),
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
    // 优先读 SQLite 聊天记录库（收发双向、跨重启留存）；库不存在退回本会话内存镜像
    let db = yunshu_core::config_dir()
        .map(|d| d.join("chat.db"))
        .and_then(|p| yunshu_core::chatlog::ChatLog::open_readonly(&p));
    if let Some(l) = db {
        let entries = l.recent(None, n);
        if entries.is_empty() {
            println!("（聊天记录为空）");
            return;
        }
        for e in entries {
            let time = chrono::DateTime::from_timestamp_millis(e.ts)
                .map(|t| t.with_timezone(&chrono::Local).format("%m-%d %H:%M").to_string())
                .unwrap_or_default();
            let dir = if e.direction == "out" { "→" } else { "←" };
            let chan = match e.channel.as_str() {
                "broadcast" => "（广播）",
                "phone" => "（手机）",
                _ => "",
            };
            println!("{time} {dir}{chan}「{}」{}", e.peer_name, e.text);
        }
        return;
    }
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

// ── 微信通道（wx …）──

const WX_HELP: &str = "wx 子命令：
  wx login                  扫码登录（终端出二维码，如需配对数字按提示输入）
  wx status                 登录/轮询/配对状态
  wx logout                 退出登录（清除凭证）
  wx send <文本…>            发文本（默认推给扫码绑定的微信账号）
  wx sendto <user_id> <文本…> 发文本给指定用户
  wx sendfile <路径…>        发文件/图片/视频（图片直接显示，其余作附件）
  wx inbox [条数]           最近收到的消息与媒体（默认 20 条）";

fn wx(rt: &tokio::runtime::Runtime, rest: &str) {
    use yunshu_core::weixin;
    // 隐藏功能：环境变量未开启时整套 wx 命令拒答
    if !weixin::enabled() {
        println!("微信通道未开启（设置环境变量 YUNSHU_WEIXIN=1 后重启 yunshu-cli 生效）");
        return;
    }
    let (sub, arg) = rest.split_once(' ').unwrap_or((rest, ""));
    let arg = arg.trim();
    match sub {
        "" | "help" | "?" => println!("{WX_HELP}"),
        "login" => wx_login(rt),
        "status" => wx_status(),
        "logout" => match weixin::logout() {
            Ok(()) => println!("✓ 已退出微信登录"),
            Err(e) => println!("✗ {e}"),
        },
        "send" => {
            if arg.is_empty() {
                println!("用法: wx send <文本…>");
            } else {
                match rt.block_on(weixin::send_text(None, arg)) {
                    Ok(n) => println!("✓ 已发送（{n} 条）"),
                    Err(e) => println!("✗ {e}"),
                }
            }
        }
        "sendto" => match arg.split_once(' ') {
            Some((to, text)) if !text.trim().is_empty() => {
                match rt.block_on(weixin::send_text(Some(to.trim()), text.trim())) {
                    Ok(n) => println!("✓ 已发送（{n} 条）"),
                    Err(e) => println!("✗ {e}"),
                }
            }
            _ => println!("用法: wx sendto <user_id> <文本…>"),
        },
        "sendfile" => {
            if arg.is_empty() {
                println!("用法: wx sendfile <路径…>");
            } else {
                let paths: Vec<std::path::PathBuf> =
                    arg.split_whitespace().map(std::path::PathBuf::from).collect();
                for (p, r) in rt.block_on(weixin::send_files(None, &paths, None)) {
                    match r {
                        Ok(desc) => println!("✓ {p}：已发送 {desc}"),
                        Err(e) => println!("✗ {p}：{e}"),
                    }
                }
            }
        }
        "inbox" => {
            let n = arg.parse().unwrap_or(20);
            let items = weixin::inbox_recent(n);
            if items.is_empty() {
                println!("（收件箱为空）");
            }
            for m in items {
                let at = fmt_time(m.at_ms);
                match (&m.saved_path, &m.error) {
                    (Some(p), _) => println!("← {at}「{}」{} → {p}", m.from, m.text),
                    (None, Some(e)) => println!("← {at}「{}」{}（失败：{e}）", m.from, m.text),
                    _ => println!("← {at}「{}」{}", m.from, m.text),
                }
            }
        }
        _ => println!("未知子命令。{WX_HELP}"),
    }
}

/// 扫码登录：终端打印二维码，跟随状态直到终态（need_verifycode 时交互输入）
fn wx_login(rt: &tokio::runtime::Runtime) {
    use yunshu_core::weixin::{self, LoginStatus};
    match rt.block_on(weixin::login_start()) {
        Err(e) => println!("✗ 取二维码失败：{e}"),
        Ok(v) => {
            if let Some(qr) = &v.qr_url {
                print_qr(qr);
                println!("（若二维码扫不出来，用手机微信访问：{qr}）");
            }
            let mut last_msg = String::new();
            loop {
                std::thread::sleep(std::time::Duration::from_millis(1500));
                let view = weixin::login_view();
                if let Some(m) = &view.message {
                    if *m != last_msg {
                        last_msg = m.clone();
                        println!("· {m}");
                    }
                }
                match view.status {
                    LoginStatus::Waiting | LoginStatus::Scanned => {}
                    LoginStatus::NeedVerifycode => {
                        print!("请输入手机微信上显示的配对数字: ");
                        let _ = std::io::stdout().flush();
                        let mut code = String::new();
                        if std::io::stdin().read_line(&mut code).unwrap_or(0) == 0 {
                            weixin::login_cancel();
                            println!();
                            return;
                        }
                        weixin::login_submit_code(code.trim());
                    }
                    LoginStatus::Confirmed => {
                        println!("✓ 已连接微信，收到的消息会实时显示");
                        return;
                    }
                    LoginStatus::Binded => {
                        println!("✓ 该微信已绑定过，凭证仍有效（wx status 查看）");
                        return;
                    }
                    LoginStatus::Failed => {
                        println!("✗ 登录失败（见上方提示），可重试 wx login");
                        return;
                    }
                    LoginStatus::Idle => return,
                }
            }
        }
    }
}

fn wx_status() {
    use yunshu_core::weixin;
    let s = weixin::status_view();
    if !s.logged_in {
        println!("✗ 微信未登录（wx login 扫码登录）");
        return;
    }
    println!(
        "✓ 已登录 bot_id:{}（绑定于 {}）",
        s.bot_id,
        fmt_time(s.saved_at)
    );
    println!(
        "轮询：{}",
        if s.polling {
            "运行中".to_string()
        } else if s.paused_remaining_ms > 0 {
            format!("冷却中（约 {} 分钟，重新扫码可解除）", s.paused_remaining_ms / 60_000 + 1)
        } else {
            "已停止".to_string()
        }
    );
    if let Some(e) = &s.last_error {
        println!("最近错误：{e}");
    }
    println!("默认推送目标：{}", if s.user_id.is_empty() { "（未定，配对后取最近）" } else { &s.user_id });
    for p in s.peers.iter().take(5) {
        println!(
            "  配对 {} 「{}」 {}",
            p.user_id,
            p.last_text,
            fmt_time(p.updated_at)
        );
    }
    println!(
        "收件箱 {} 条，媒体目录：{}",
        s.inbox_count,
        s.media_dir.as_deref().unwrap_or("（无法创建）")
    );
}

fn print_qr(content: &str) {
    use qrcode::render::unicode;
    use qrcode::QrCode;
    match QrCode::new(content.as_bytes()) {
        Ok(c) => {
            let img = c
                .render::<unicode::Dense1x2>()
                .dark_color(unicode::Dense1x2::Dark)
                .light_color(unicode::Dense1x2::Light)
                .build();
            println!("{img}");
        }
        Err(e) => println!("（终端二维码生成失败：{e}）"),
    }
}

/// 毫秒时间戳 → 本地 "MM-DD HH:MM"
fn fmt_time(ms: u64) -> String {
    if ms == 0 {
        return "—".into();
    }
    let secs = (ms / 1000) as i64;
    let days = secs.div_euclid(86400);
    let tod = secs.rem_euclid(86400);
    // 从 Unix 纪元推算年月日（民用算法，够展示用）
    let (y, m, d) = {
        let z = days + 719_468;
        let era = z.div_euclid(146_097);
        let doe = z.rem_euclid(146_097);
        let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
        let y = yoe + era * 400;
        let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
        let mp = (5 * doy + 2) / 153;
        let d = doy - (153 * mp + 2) / 5 + 1;
        let m = if mp < 10 { mp + 3 } else { mp - 9 };
        (if m <= 2 { y + 1 } else { y }, m, d)
    };
    format!(
        "{y:04}-{m:02}-{d:02} {:02}:{:02}",
        tod / 3600,
        (tod % 3600) / 60
    )
}
