//! yunshu-cli：无头 Yunshu 节点。
//! 内嵌 yunshu-core（与桌面版同一套核心实现），一个二进制两种身份：
//!   直接运行   交互式终端（peers/send/sendfile/trust/accept…），信任与来件由人决策
//!   --mcp      MCP stdio server，把能力暴露为工具供 AI agent 调用
//! 配置目录默认 ~/.yunshu-cli，与桌面实例身份隔离（独立机器 ID/端口/信任表），
//! 避免同开时互抢身份；--home 指向 ~/.yunshu 可刻意与桌面版共用身份。
//!
//! 用法：yunshu-cli [--mcp] [--home <目录>] [--port <端口>] [--name <名称>] [--dnd|--no-dnd] [--stealth]
//! 覆盖只影响本次运行，不写入 config.toml。

mod cli;
mod mcp;
mod shared;

use std::path::PathBuf;
use yunshu_core::{CoreEvent, UiRequest};

struct Opts {
    mcp: bool,
    home: PathBuf,
    port: Option<u16>,
    name: Option<String>,
    dnd: Option<bool>,
    stealth: bool,
}

fn usage() {
    println!(
        "yunshu-cli — 无头 Yunshu 节点\n\
         用法: yunshu-cli [--mcp] [--home <目录>] [--port <端口>] [--name <名称>] [--dnd] [--no-dnd] [--stealth]\n\
         \n\
         直接运行是交互式终端；--mcp 以 MCP stdio server 运行（供 AI agent 调用）。\n\
         配置目录默认 ~/.yunshu-cli（与桌面实例身份隔离）；--home 指向 ~/.yunshu 可共用身份。"
    );
}

fn parse_opts() -> Opts {
    let mut o = Opts {
        mcp: false,
        home: dirs::home_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join(".yunshu-cli"),
        port: None,
        name: None,
        dnd: None,
        stealth: false,
    };
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        match a.as_str() {
            "--mcp" => o.mcp = true,
            "--home" => {
                if let Some(v) = args.next() {
                    o.home = PathBuf::from(v);
                }
            }
            "--port" => o.port = args.next().and_then(|v| v.parse().ok()),
            "--name" => o.name = args.next(),
            "--dnd" => o.dnd = Some(true),
            "--no-dnd" => o.dnd = Some(false),
            "--stealth" => o.stealth = true,
            "-h" | "--help" => {
                usage();
                std::process::exit(0);
            }
            _ => {}
        }
    }
    o
}

fn main() {
    let mut opts = parse_opts();
    yunshu_core::set_config_dir(opts.home.clone());

    // 日志进 stderr：MCP 模式下 stdout 是协议通道（core::run 里的 try_init 会静默让位）
    let _ = tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .with_writer(std::io::stderr)
        .try_init();

    let mut cfg = yunshu_core::settings::Settings::load_or_default().core_config();
    if let Some(p) = opts.port {
        cfg.port = p;
    }
    if let Some(n) = opts.name.take() {
        cfg.pc_name = n;
    }
    if let Some(d) = opts.dnd {
        cfg.dnd_mode = d;
    }
    if opts.stealth {
        cfg.stealth_mode = true;
    }

    let (event_tx, event_rx) = std::sync::mpsc::channel::<CoreEvent>();
    let (ui_tx, ui_rx) = std::sync::mpsc::channel::<UiRequest>();
    // 微信通道入站消息走 CoreEvent::WeixinInbound 进事件泵
    yunshu_core::weixin::set_event_sink(event_tx.clone());
    yunshu_core::run(cfg, event_tx, ui_rx);
    let shared = shared::Shared::new(ui_tx);

    // 泵线程：std mpsc → 镜像更新 →（交互模式）打印 → 广播给等待方
    let cli_echo = !opts.mcp;
    {
        let shared = shared.clone();
        std::thread::spawn(move || pump(shared, event_rx, cli_echo));
    }

    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("tokio runtime 创建失败");
    // 有微信凭证则恢复长轮询（收消息 + 保活；通道未开启时 core 内部直接跳过）
    rt.block_on(async {
        yunshu_core::weixin::init_polling();
    });
    if yunshu_core::weixin::enabled()
        && yunshu_core::weixin::load_config()
            .map(|c| c.logged_in())
            .unwrap_or(false)
    {
        eprintln!("[yunshu-cli] 微信通道已恢复（长轮询运行中）");
    }
    if opts.mcp {
        let tools = if yunshu_core::weixin::enabled() {
            "pigeon_list_peers / pigeon_send_text / pigeon_send_files / weixin_status / weixin_send_text / weixin_send_files / weixin_inbox"
        } else {
            "pigeon_list_peers / pigeon_send_text / pigeon_send_files"
        };
        eprintln!("[yunshu-cli] MCP stdio server 已启动（工具: {tools}）");
        if let Err(e) = rt.block_on(mcp::run(shared)) {
            eprintln!("[yunshu-cli] MCP server 退出: {e}");
            std::process::exit(1);
        }
    } else {
        cli::run(&rt, shared);
    }
}

/// 核心线程事件泵：更新镜像 → 打印 → 广播
fn pump(
    shared: std::sync::Arc<shared::Shared>,
    event_rx: std::sync::mpsc::Receiver<CoreEvent>,
    cli_echo: bool,
) {
    let mut seen: std::collections::HashMap<String, &'static str> =
        std::collections::HashMap::new();
    while let Ok(ev) = event_rx.recv() {
        shared::apply(&shared, &ev);
        if cli_echo {
            shared::print_event(&ev, &mut seen);
        }
        let _ = shared.events.send(ev);
    }
}
