//! 云书 Tauri 壳：窗口 + 托盘 + 核心（yunshu-core）桥接。
//! 业务全部在 yunshu_core；本壳只做：事件转发（core → 前端）、命令（前端 → core）、托盘与窗口管理。

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

#[cfg(windows)]
mod win_service;

use serde_json::json;
use std::sync::{Arc, Mutex};
use tauri::{Emitter, Manager};
use yunshu_core::{CoreEvent, UiRequest};

struct AppState {
    ui_tx: std::sync::mpsc::Sender<UiRequest>,
    cache: Arc<Mutex<serde_json::Value>>,
    /// 控制端观看会话信息（观看窗口页经 get_view_info 读取）
    remote_view: Mutex<Option<RemoteViewInfo>>,
}

#[derive(Clone)]
struct RemoteViewInfo {
    token: String,
    port: u16,
    name: String,
    peer_id: String,
    width: u32,
    height: u32,
}

fn state_snapshot() -> serde_json::Value {
    json!({
        "addr": null, "error": null, "qr_url": null,
        "pending": null, "connected": null,
        "peers": [], "transfers": [], "toasts": [],
        "remote_temp_password": null, "remote_controlled": null,
        "remote_machine_id": null,
    })
}

fn apply_to_cache(cache: &Mutex<serde_json::Value>, ev: &CoreEvent) {
    let mut c = cache.lock().unwrap();
    let obj = c.as_object_mut().unwrap();
    match ev {
        CoreEvent::ServerStarted { addr } => {
            obj.insert("addr".into(), json!(addr));
        }
        CoreEvent::ServerError { reason } => {
            obj.insert("error".into(), json!(reason));
        }
        CoreEvent::QrUrl(url) => {
            obj.insert("qr_url".into(), json!(url));
        }
        CoreEvent::PairPending { code, device } => {
            obj.insert("pending".into(), json!({ "code": code, "device": device }));
        }
        CoreEvent::PairOk { device } => {
            obj.insert("pending".into(), json!(null));
            obj.insert("connected".into(), json!(device));
        }
        CoreEvent::PeerDisconnected => {
            obj.insert("connected".into(), json!(null));
        }
        CoreEvent::PigeonPeers { peers } => {
            obj.insert(
                "peers".into(),
                serde_json::to_value(peers).unwrap_or_default(),
            );
        }
        CoreEvent::RemoteTempPassword { password } => {
            obj.insert("remote_temp_password".into(), json!(password));
        }
        CoreEvent::RemoteSessionStarted {
            peer_id,
            name,
            side,
        } => {
            if side == "controlled" {
                obj.insert(
                    "remote_controlled".into(),
                    json!({ "peer_id": peer_id, "name": name }),
                );
            }
        }
        CoreEvent::RemoteSessionEnded { side, .. } => {
            if side == "controlled" {
                obj.insert("remote_controlled".into(), json!(null));
            }
        }
        CoreEvent::TransferUpdate { t } => {
            let list = obj.entry("transfers").or_insert(json!([]));
            let arr = list.as_array_mut().unwrap();
            if let Some(pos) = arr.iter().position(|x| x["id"] == t.id) {
                arr[pos] = serde_json::to_value(t).unwrap_or_default();
            } else {
                arr.push(serde_json::to_value(t).unwrap_or_default());
            }
        }
        _ => {}
    }
}

#[tauri::command]
fn write_diag(text: String) {
    let path = std::env::temp_dir().join("yunshu_webview_diag.log");
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
    {
        use std::io::Write;
        let _ = f.write_all(
            format!(
                "{}
",
                text
            )
            .as_bytes(),
        );
    }
}

#[tauri::command]
fn get_state(state: tauri::State<AppState>) -> serde_json::Value {
    state.cache.lock().unwrap().clone()
}

#[tauri::command]
fn send_paths(
    state: tauri::State<AppState>,
    kind: String,
    peer_id: Option<String>,
    paths: Vec<String>,
) {
    let paths: Vec<std::path::PathBuf> = paths.into_iter().map(std::path::PathBuf::from).collect();
    let _ = match kind.as_str() {
        // 手机：文件夹打包 zip 后按单文件发送（手机端是浏览器下载）
        "phone" => {
            let mut files = Vec::new();
            for path in paths {
                if path.is_dir() {
                    match yunshu_core::util::zip_folder(&path) {
                        Ok(z) => files.push(z),
                        Err(e) => {
                            let name = path
                                .file_name()
                                .map(|n| n.to_string_lossy().to_string())
                                .unwrap_or_default();
                            let _ = state
                                .ui_tx
                                .send(UiRequest::Toast(format!("打包「{name}」失败：{e}")));
                        }
                    }
                } else {
                    files.push(path);
                }
            }
            if files.is_empty() {
                return;
            }
            state.ui_tx.send(UiRequest::SendFiles(files))
        }
        // 电脑：文件夹原生直传（core 递归展开为批次，一次确认）
        "pc" => {
            if let Some(peer_id) = peer_id {
                state
                    .ui_tx
                    .send(UiRequest::PigeonSendFiles { peer_id, paths })
            } else {
                Ok(())
            }
        }
        _ => Ok(()),
    };
}

#[tauri::command]
fn send_text(state: tauri::State<AppState>, text: String) {
    let _ = state.ui_tx.send(UiRequest::SendText(text));
}

#[tauri::command]
fn pigeon_send_text(state: tauri::State<AppState>, peer_id: String, text: String) {
    let _ = state
        .ui_tx
        .send(UiRequest::PigeonSendText { peer_id, text });
}

#[tauri::command]
fn broadcast_text(state: tauri::State<AppState>, text: String) {
    let _ = state.ui_tx.send(UiRequest::BroadcastText(text));
}

#[tauri::command]
fn pigeon_trust(state: tauri::State<AppState>, peer_id: String, fingerprint: String) {
    let _ = state.ui_tx.send(UiRequest::PigeonTrust {
        peer_id,
        fingerprint,
    });
}

#[tauri::command]
fn pigeon_reject_trust(state: tauri::State<AppState>, peer_id: String) {
    let _ = state.ui_tx.send(UiRequest::PigeonRejectTrust { peer_id });
}

#[tauri::command]
fn pigeon_accept(state: tauri::State<AppState>, peer_id: String, offer_id: String) {
    let _ = state
        .ui_tx
        .send(UiRequest::PigeonAcceptFile { peer_id, offer_id });
}

#[tauri::command]
fn pigeon_reject(state: tauri::State<AppState>, peer_id: String, offer_id: String) {
    let _ = state
        .ui_tx
        .send(UiRequest::PigeonRejectFile { peer_id, offer_id });
}

#[tauri::command]
fn pigeon_cancel(state: tauri::State<AppState>, peer_id: String, offer_id: String) {
    let _ = state
        .ui_tx
        .send(UiRequest::PigeonCancelFile { peer_id, offer_id });
}

/// 忘记一条传输记录（仅 UI 列表；已收文件用 delete_received_file 删）
#[tauri::command]
fn pigeon_pause(state: tauri::State<AppState>, peer_id: String, offer_id: String, paused: bool) {
    let _ = state.ui_tx.send(UiRequest::PigeonPauseFile {
        peer_id,
        offer_id,
        paused,
    });
}

#[tauri::command]
fn forget_transfer(state: tauri::State<AppState>, id: String) {
    let _ = state.ui_tx.send(UiRequest::ForgetTransfer { id });
}

/// 删除已接收的文件（仅允许保存目录内的路径，防止误删任意文件）
#[tauri::command]
fn delete_received_file(path: String) -> Result<(), String> {
    let p = std::path::PathBuf::from(&path);
    let save_dir = yunshu_core::settings::Settings::load_or_default().save_dir_path();
    let in_save_dir = match (
        p.parent().and_then(|d| d.canonicalize().ok()),
        save_dir.canonicalize().ok(),
    ) {
        (Some(parent), Some(dir)) => parent == dir,
        _ => false,
    };
    if !in_save_dir {
        return Err("只允许删除保存目录内的文件".into());
    }
    std::fs::remove_file(&p).map_err(|e| e.to_string())
}

#[tauri::command]
fn pigeon_disconnect(state: tauri::State<AppState>, peer_id: String) {
    let _ = state.ui_tx.send(UiRequest::PigeonDisconnect { peer_id });
}

#[tauri::command]
fn set_save_dir(state: tauri::State<AppState>, batch: String, dir: Option<String>) {
    let _ = state.ui_tx.send(UiRequest::SetSaveDir {
        batch,
        dir: dir.map(std::path::PathBuf::from),
    });
}

/// 打开文件所在文件夹：Windows 资源管理器选中文件，macOS 在 Finder 中显示，Linux 打开所在目录
#[tauri::command]
fn open_folder(path: String) -> Result<(), String> {
    #[cfg(target_os = "windows")]
    {
        std::process::Command::new("explorer")
            .arg(format!("/select,{path}"))
            .spawn()
            .map_err(|e| e.to_string())?;
        return Ok(());
    }
    #[cfg(target_os = "macos")]
    {
        std::process::Command::new("open")
            .args(["-R", &path])
            .spawn()
            .map_err(|e| e.to_string())?;
        return Ok(());
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        let dir = std::path::Path::new(&path)
            .parent()
            .map(|p| p.to_path_buf())
            .unwrap_or_else(|| std::path::PathBuf::from(&path));
        std::process::Command::new("xdg-open")
            .arg(dir)
            .spawn()
            .map_err(|e| e.to_string())?;
        Ok(())
    }
}

/// 粘贴的文件在浏览器侧拿不到路径：把 blob 落到临时目录，返回路径供 send_paths 使用
#[tauri::command]
fn stage_blob(name: String, bytes: Vec<u8>) -> Result<String, String> {
    static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let dir = std::env::temp_dir().join("yunshu_paste");
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);
    let seq = SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let safe: String = name
        .chars()
        .map(|c| {
            if c.is_ascii_control()
                || c == '\\'
                || c == '/'
                || c == ':'
                || c == '*'
                || c == '?'
                || c == '"'
                || c == '<'
                || c == '>'
                || c == '|'
            {
                '_'
            } else {
                c
            }
        })
        .collect();
    let path = dir.join(format!("{stamp}-{seq}-{safe}"));
    std::fs::write(&path, &bytes).map_err(|e| e.to_string())?;
    Ok(path.to_string_lossy().to_string())
}

#[tauri::command]
fn get_settings() -> serde_json::Value {
    let mut v = serde_json::to_value(yunshu_core::settings::Settings::load_or_default())
        .unwrap_or_default();
    if let Some(obj) = v.as_object_mut() {
        // 固定密码哈希不给前端（单向哈希无意义，且避免日志泄漏格式）
        obj.remove("remote_fixed_password_hash");
        obj.insert(
            "machine_id".into(),
            json!(yunshu_core::remote::machine_id()),
        );
    }
    v
}

#[tauri::command]
fn save_settings(
    state: tauri::State<AppState>,
    device_name: String,
    port: u16,
    save_dir: String,
    autostart: bool,
    dnd_mode: bool,
    remote_fixed_password: Option<String>,
) -> Result<(), String> {
    let mut s = yunshu_core::settings::Settings::load_or_default();
    s.device_name = device_name;
    s.port = port;
    s.save_dir = save_dir;
    if s.autostart != autostart {
        yunshu_core::settings::Settings::apply_autostart(autostart)?;
        s.autostart = autostart;
    }
    if s.dnd_mode != dnd_mode {
        let _ = state.ui_tx.send(UiRequest::SetDnd(dnd_mode));
        s.dnd_mode = dnd_mode;
    }
    s.save()?;
    // 固定密码走 core 通道：哈希 + 落盘 + 热更新校验器（None = 未改动）
    if let Some(pw) = remote_fixed_password {
        let _ = state.ui_tx.send(UiRequest::SetRemoteFixedPassword {
            password: (!pw.is_empty()).then_some(pw),
        });
    }
    Ok(())
}

/// 防打扰开关（头部铃铛）：立即生效并持久化
#[tauri::command]
fn set_dnd(state: tauri::State<AppState>, enabled: bool) -> Result<(), String> {
    let _ = state.ui_tx.send(UiRequest::SetDnd(enabled));
    let mut s = yunshu_core::settings::Settings::load_or_default();
    s.dnd_mode = enabled;
    s.save()
}

// ---- 远程控制 ----

#[tauri::command]
fn remote_connect(state: tauri::State<AppState>, machine_id: String, password: String) {
    let _ = state.ui_tx.send(UiRequest::RemoteConnect {
        machine_id,
        password,
    });
}

#[tauri::command]
fn remote_disconnect(state: tauri::State<AppState>) {
    let _ = state.ui_tx.send(UiRequest::RemoteDisconnect);
}

/// 观看窗口内向受控端推送文件（文件回传）
#[tauri::command]
fn remote_send_files(state: tauri::State<AppState>, paths: Vec<String>) {
    let paths: Vec<std::path::PathBuf> = paths.into_iter().map(std::path::PathBuf::from).collect();
    let _ = state.ui_tx.send(UiRequest::RemoteSendFiles { paths });
}

/// 以系统服务方式自启（Windows：提权安装；其余平台不支持）
#[tauri::command]
fn service_install() -> Result<(), String> {
    #[cfg(windows)]
    {
        let exe = std::env::current_exe().map_err(|e| e.to_string())?;
        let script = format!(
            "Start-Process -Verb RunAs -FilePath '{}' -ArgumentList 'service','install' -Wait",
            exe.display()
        );
        let status = std::process::Command::new("powershell")
            .args(["-NoProfile", "-Command", &script])
            .status()
            .map_err(|e| e.to_string())?;
        if status.success() {
            Ok(())
        } else {
            Err("服务安装未完成（需要管理员授权）".into())
        }
    }
    #[cfg(not(windows))]
    {
        Err("系统服务自启仅支持 Windows（macOS 请用「开机自启」）".into())
    }
}

/// 卸载系统服务（Windows：提权卸载）
#[tauri::command]
fn service_uninstall() -> Result<(), String> {
    #[cfg(windows)]
    {
        let exe = std::env::current_exe().map_err(|e| e.to_string())?;
        let script = format!(
            "Start-Process -Verb RunAs -FilePath '{}' -ArgumentList 'service','uninstall' -Wait",
            exe.display()
        );
        let status = std::process::Command::new("powershell")
            .args(["-NoProfile", "-Command", &script])
            .status()
            .map_err(|e| e.to_string())?;
        if status.success() {
            Ok(())
        } else {
            Err("服务卸载未完成（需要管理员授权）".into())
        }
    }
    #[cfg(not(windows))]
    {
        Err("系统服务自启仅支持 Windows".into())
    }
}

#[tauri::command]
fn remote_assist_request(state: tauri::State<AppState>, peer_id: String) {
    let _ = state.ui_tx.send(UiRequest::RemoteAssistRequest { peer_id });
}

#[tauri::command]
fn remote_assist_respond(state: tauri::State<AppState>, peer_id: String, accepted: bool) {
    let _ = state
        .ui_tx
        .send(UiRequest::RemoteAssistRespond { peer_id, accepted });
}

/// 观看窗口页获取本机会话信息（token/端口），避免往 URL 里塞凭证
#[tauri::command]
fn get_view_info(state: tauri::State<AppState>) -> Result<serde_json::Value, String> {
    let remote = state.remote_view.lock().unwrap();
    match remote.as_ref() {
        Some(v) => Ok(json!({
            "token": v.token, "port": v.port,
            "name": v.name, "peer_id": v.peer_id,
            "width": v.width, "height": v.height,
        })),
        None => Err("当前没有远程观看会话".into()),
    }
}

/// 打开远程观看窗口（观看页凭 get_view_info 拿接入凭证，凭证不进 URL）
fn open_viewer_window(app: &tauri::AppHandle, info: &RemoteViewInfo) {
    use tauri::WebviewUrl;
    if let Some(w) = app.get_webview_window("remote-viewer") {
        let _ = w.set_title(format!("远程控制 · {}", info.name).as_str());
        let _ = w.show();
        let _ = w.set_focus();
        return;
    }
    let title = format!("远程控制 · {}（{}×{}）", info.name, info.width, info.height);
    if let Err(e) = tauri::WebviewWindowBuilder::new(
        app,
        "remote-viewer",
        WebviewUrl::App("viewer.html".into()),
    )
    .title(title)
    .inner_size(1024.0, 640.0)
    .min_inner_size(560.0, 360.0)
    .build()
    {
        eprintln!("观看窗口创建失败: {e}");
    }
}

/// 被控指示：托盘图标变红 + 提示语（安全要求：被控状态必须醒目可感知）
fn set_tray_controlled(app: &tauri::AppHandle, controlled: bool, who: &str) {
    let Some(tray) = app.try_state::<TrayHandle>() else {
        return;
    };
    let tray = tray.0.lock().unwrap();
    let Some(tray) = tray.as_ref() else { return };
    if controlled {
        tray.set_icon(Some(red_tray_icon())).ok();
        tray.set_tooltip(Some(format!("云书 · 正在被 {who} 远程控制")))
            .ok();
    } else {
        if let Some(icon) = DEFAULT_ICON.get() {
            tray.set_icon(Some(icon.clone())).ok();
        }
        tray.set_tooltip(Some("云书 · 局域网传书")).ok();
    }
    // 菜单项随会话状态启用/禁用
    use tauri::menu::{Menu, MenuItem};
    let open = MenuItem::with_id(app, "open", "打开云书", true, None::<&str>);
    let disconnect = MenuItem::with_id(
        app,
        "remote-disconnect",
        "断开远程控制",
        controlled,
        None::<&str>,
    );
    let quit = MenuItem::with_id(app, "quit", "退出", true, None::<&str>);
    if let (Ok(open), Ok(disconnect), Ok(quit)) = (open, disconnect, quit)
        && let Ok(menu) = Menu::with_items(app, &[&open, &disconnect, &quit])
    {
        tray.set_menu(Some(menu)).ok();
    }
}

fn red_tray_icon() -> tauri::image::Image<'static> {
    const S: usize = 32;
    let mut rgba = vec![0u8; S * S * 4];
    let c = (S as f32 - 1.0) / 2.0;
    for y in 0..S {
        for x in 0..S {
            let dx = x as f32 - c;
            let dy = y as f32 - c;
            if dx * dx + dy * dy <= c * c {
                let o = (y * S + x) * 4;
                rgba[o] = 225;
                rgba[o + 1] = 45;
                rgba[o + 2] = 60;
                rgba[o + 3] = 255;
            }
        }
    }
    tauri::image::Image::new_owned(rgba, S as u32, S as u32)
}

static DEFAULT_ICON: std::sync::OnceLock<tauri::image::Image<'static>> = std::sync::OnceLock::new();
struct TrayHandle(Mutex<Option<tauri::tray::TrayIcon>>);

fn main() {
    // Windows 服务模式：yunshu.exe service install|uninstall|run（见 win_service.rs）
    #[cfg(windows)]
    if win_service::try_dispatch() {
        return;
    }
    // CLI 覆盖（单机双实例测试用）：yunshu.exe --port 8001 --name testa
    // 覆盖只影响本次运行，不写入 config.toml
    let mut args = std::env::args().skip(1);
    let mut cli_port: Option<u16> = None;
    let mut cli_name: Option<String> = None;
    while let Some(a) = args.next() {
        match a.as_str() {
            "--port" => cli_port = args.next().and_then(|v| v.parse().ok()),
            "--name" => cli_name = args.next(),
            _ => {}
        }
    }

    let mut core_cfg = yunshu_core::settings::Settings::load_or_default().core_config();
    if let Some(p) = cli_port {
        core_cfg.port = p;
    }
    if let Some(n) = cli_name {
        core_cfg.pc_name = n;
    }
    let (event_tx, event_rx) = std::sync::mpsc::channel::<CoreEvent>();
    let (ui_tx, ui_rx) = std::sync::mpsc::channel::<UiRequest>();
    yunshu_core::run(core_cfg, event_tx, ui_rx);

    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_notification::init())
        .manage(AppState {
            ui_tx,
            cache: Arc::new(Mutex::new(state_snapshot())),
            remote_view: Mutex::new(None),
        })
        .setup(move |app| {
            let handle = app.handle().clone();
            let cache = app.state::<AppState>().cache.clone();
            // 核心事件 → 前端（同时维护初始状态快照）
            std::thread::spawn(move || {
                let mut done_seen: std::collections::HashSet<String> =
                    std::collections::HashSet::new();
                while let Ok(ev) = event_rx.recv() {
                    apply_to_cache(&cache, &ev);
                    match &ev {
                        // 连接成功：登记观看信息并自动打开观看窗口
                        CoreEvent::RemoteViewReady {
                            peer_id,
                            name,
                            token,
                            view_token: _,
                            port,
                            width,
                            height,
                        } => {
                            let info = RemoteViewInfo {
                                token: token.clone(),
                                port: *port,
                                name: name.clone(),
                                peer_id: peer_id.clone(),
                                width: *width,
                                height: *height,
                            };
                            *handle.state::<AppState>().remote_view.lock().unwrap() =
                                Some(info.clone());
                            open_viewer_window(&handle, &info);
                        }
                        CoreEvent::RemoteSessionEnded { side, .. } if side == "controller" => {
                            *handle.state::<AppState>().remote_view.lock().unwrap() = None;
                        }
                        // 被控端醒目指示（托盘变红 + 一键断开）
                        CoreEvent::RemoteSessionStarted { name, side, .. }
                            if side == "controlled" =>
                        {
                            set_tray_controlled(&handle, true, name);
                        }
                        CoreEvent::RemoteSessionEnded { side, .. } if side == "controlled" => {
                            set_tray_controlled(&handle, false, "");
                        }
                        _ => {}
                    }
                    // 传输完成且窗口隐藏时弹系统通知
                    if let CoreEvent::TransferUpdate { t } = &ev
                        && t.status == yunshu_core::TransferStatus::Done
                        && done_seen.insert(t.id.clone())
                        && let Some(w) = handle.get_webview_window("main")
                        && let Ok(false) = w.is_visible()
                    {
                        use tauri_plugin_notification::NotificationExt;
                        let _ = handle
                            .notification()
                            .builder()
                            .title("云书")
                            .body(format!("「{}」传输完成", t.name))
                            .show();
                    }
                    if let Ok(v) = serde_json::to_value(&ev) {
                        let _ = handle.emit("core-event", v);
                    }
                }
            });

            // 托盘
            use tauri::menu::{Menu, MenuItem};
            let open = MenuItem::with_id(app, "open", "打开云书", true, None::<&str>)?;
            let disconnect = MenuItem::with_id(
                app,
                "remote-disconnect",
                "断开远程控制",
                false,
                None::<&str>,
            )?;
            let quit = MenuItem::with_id(app, "quit", "退出", true, None::<&str>)?;
            let menu = Menu::with_items(app, &[&open, &disconnect, &quit])?;
            if let Some(icon) = app.default_window_icon() {
                // 转 owned（引用生命周期绑在 app 上，OnceLock 需要 'static）
                let owned = tauri::image::Image::new_owned(
                    icon.rgba().to_vec(),
                    icon.width(),
                    icon.height(),
                );
                let _ = DEFAULT_ICON.set(owned);
            }
            let tray = tauri::tray::TrayIconBuilder::new()
                .icon(app.default_window_icon().unwrap().clone())
                .tooltip("云书 · 局域网传书")
                .menu(&menu)
                .show_menu_on_left_click(false)
                .on_menu_event(|app, ev| match ev.id().as_ref() {
                    "open" => {
                        if let Some(w) = app.get_webview_window("main") {
                            let _ = w.show();
                            let _ = w.set_focus();
                        }
                    }
                    "remote-disconnect" => {
                        app.state::<AppState>()
                            .ui_tx
                            .send(UiRequest::RemoteDisconnect)
                            .ok();
                    }
                    "quit" => app.exit(0),
                    _ => {}
                })
                .on_tray_icon_event(|tray, ev| {
                    if let tauri::tray::TrayIconEvent::Click {
                        button: tauri::tray::MouseButton::Left,
                        button_state: tauri::tray::MouseButtonState::Up,
                        ..
                    } = ev
                    {
                        let app = tray.app_handle();
                        if let Some(w) = app.get_webview_window("main") {
                            let _ = w.show();
                            let _ = w.set_focus();
                        }
                    }
                })
                .build(app)?;
            app.manage(TrayHandle(Mutex::new(Some(tray))));
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            get_state,
            write_diag,
            broadcast_text,
            pigeon_trust,
            pigeon_reject_trust,
            send_paths,
            send_text,
            pigeon_send_text,
            pigeon_accept,
            pigeon_reject,
            pigeon_cancel,
            pigeon_pause,
            forget_transfer,
            delete_received_file,
            pigeon_disconnect,
            set_save_dir,
            open_folder,
            stage_blob,
            get_settings,
            save_settings,
            set_dnd,
            remote_connect,
            remote_disconnect,
            remote_send_files,
            service_install,
            service_uninstall,
            remote_assist_request,
            remote_assist_respond,
            get_view_info
        ])
        .on_window_event(|window, event| {
            match window.label() {
                // 主窗口关闭 = 隐藏进托盘；退出走托盘菜单
                "main" => {
                    if let tauri::WindowEvent::CloseRequested { api, .. } = event {
                        let _ = window.hide();
                        api.prevent_close();
                    }
                }
                // 观看窗口关闭 = 断开远程控制会话
                "remote-viewer" => {
                    if let tauri::WindowEvent::CloseRequested { .. } = event {
                        window
                            .app_handle()
                            .state::<AppState>()
                            .ui_tx
                            .send(UiRequest::RemoteDisconnect)
                            .ok();
                    }
                }
                _ => {}
            }
        })
        .run(tauri::generate_context!())
        .expect("云书运行出错");
}
