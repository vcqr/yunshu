//! 云书 Tauri 壳：窗口 + 托盘 + 核心（yunshu-core）桥接。
//! 业务全部在 yunshu_core；本壳只做：事件转发（core → 前端）、命令（前端 → core）、托盘与窗口管理。

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use serde_json::json;
use std::sync::{Arc, Mutex};
use tauri::{Emitter, Manager};
use yunshu_core::{CoreEvent, UiRequest};

struct AppState {
    ui_tx: std::sync::mpsc::Sender<UiRequest>,
    cache: Arc<Mutex<serde_json::Value>>,
}

fn state_snapshot() -> serde_json::Value {
    json!({
        "addr": null, "error": null, "qr_url": null,
        "pending": null, "connected": null,
        "peers": [], "transfers": [], "toasts": []
    })
}

fn apply_to_cache(cache: &Mutex<serde_json::Value>, ev: &CoreEvent) {
    let mut c = cache.lock().unwrap();
    let obj = c.as_object_mut().unwrap();
    match ev {
        CoreEvent::ServerStarted { addr } => { obj.insert("addr".into(), json!(addr)); }
        CoreEvent::ServerError { reason } => { obj.insert("error".into(), json!(reason)); }
        CoreEvent::QrUrl(url) => { obj.insert("qr_url".into(), json!(url)); }
        CoreEvent::PairPending { code, device } => { obj.insert("pending".into(), json!({ "code": code, "device": device })); }
        CoreEvent::PairOk { device } => {
            obj.insert("pending".into(), json!(null));
            obj.insert("connected".into(), json!(device));
        }
        CoreEvent::PeerDisconnected => { obj.insert("connected".into(), json!(null)); }
        CoreEvent::PigeonPeers { peers } => { obj.insert("peers".into(), serde_json::to_value(peers).unwrap_or_default()); }
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
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(path) {
        use std::io::Write;
        let _ = f.write_all(format!("{}
", text).as_bytes());
    }
}

#[tauri::command]
fn get_state(state: tauri::State<AppState>) -> serde_json::Value {
    state.cache.lock().unwrap().clone()
}

#[tauri::command]
fn send_paths(state: tauri::State<AppState>, kind: String, peer_id: Option<String>, paths: Vec<String>) {
    let mut files = Vec::new();
    for p in paths {
        let path = std::path::PathBuf::from(&p);
        if path.is_dir() {
            match yunshu_core::util::zip_folder(&path) {
                Ok(z) => files.push(z.to_string_lossy().to_string()),
                Err(e) => {
                    let name = path.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
                    let _ = state.ui_tx.send(UiRequest::Toast(format!("打包「{name}」失败：{e}")));
                }
            }
        } else {
            files.push(p);
        }
    }
    if files.is_empty() {
        return;
    }
    let paths: Vec<std::path::PathBuf> = files.into_iter().map(std::path::PathBuf::from).collect();
    let _ = match kind.as_str() {
        "phone" => state.ui_tx.send(UiRequest::SendFiles(paths)),
        "pc" => {
            if let Some(peer_id) = peer_id {
                state.ui_tx.send(UiRequest::PigeonSendFiles { peer_id, paths })
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
    let _ = state.ui_tx.send(UiRequest::PigeonSendText { peer_id, text });
}

#[tauri::command]
fn broadcast_text(state: tauri::State<AppState>, text: String) {
    let _ = state.ui_tx.send(UiRequest::BroadcastText(text));
}

#[tauri::command]
fn pigeon_trust(state: tauri::State<AppState>, peer_id: String, fingerprint: String) {
    let _ = state.ui_tx.send(UiRequest::PigeonTrust { peer_id, fingerprint });
}

#[tauri::command]
fn pigeon_reject_trust(state: tauri::State<AppState>, peer_id: String) {
    let _ = state.ui_tx.send(UiRequest::PigeonRejectTrust { peer_id });
}

#[tauri::command]
fn pigeon_accept(state: tauri::State<AppState>, peer_id: String, offer_id: String) {
    let _ = state.ui_tx.send(UiRequest::PigeonAcceptFile { peer_id, offer_id });
}

#[tauri::command]
fn pigeon_reject(state: tauri::State<AppState>, peer_id: String, offer_id: String) {
    let _ = state.ui_tx.send(UiRequest::PigeonRejectFile { peer_id, offer_id });
}

#[tauri::command]
fn pigeon_cancel(state: tauri::State<AppState>, peer_id: String, offer_id: String) {
    let _ = state.ui_tx.send(UiRequest::PigeonCancelFile { peer_id, offer_id });
}

/// 忘记一条传输记录（仅 UI 列表；已收文件用 delete_received_file 删）
#[tauri::command]
fn forget_transfer(state: tauri::State<AppState>, id: String) {
    let _ = state.ui_tx.send(UiRequest::ForgetTransfer { id });
}

/// 删除已接收的文件（仅允许保存目录内的路径，防止误删任意文件）
#[tauri::command]
fn delete_received_file(path: String) -> Result<(), String> {
    let p = std::path::PathBuf::from(&path);
    let save_dir = yunshu_core::settings::Settings::load_or_default().save_dir_path();
    let in_save_dir = match (p.parent().and_then(|d| d.canonicalize().ok()), save_dir.canonicalize().ok()) {
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

#[tauri::command]
fn open_folder(path: String) {
    let _ = std::process::Command::new("explorer").arg(format!("/select,{path}")).spawn();
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
        .map(|c| if c.is_ascii_control() || c == '\\' || c == '/' || c == ':' || c == '*' || c == '?' || c == '"' || c == '<' || c == '>' || c == '|' { '_' } else { c })
        .collect();
    let path = dir.join(format!("{stamp}-{seq}-{safe}"));
    std::fs::write(&path, &bytes).map_err(|e| e.to_string())?;
    Ok(path.to_string_lossy().to_string())
}

#[tauri::command]
fn get_settings() -> serde_json::Value {
    serde_json::to_value(yunshu_core::settings::Settings::load_or_default()).unwrap_or_default()
}

#[tauri::command]
fn save_settings(
    state: tauri::State<AppState>,
    device_name: String,
    port: u16,
    save_dir: String,
    autostart: bool,
    dnd_mode: bool,
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
    s.save()
}

/// 防打扰开关（头部铃铛）：立即生效并持久化
#[tauri::command]
fn set_dnd(state: tauri::State<AppState>, enabled: bool) -> Result<(), String> {
    let _ = state.ui_tx.send(UiRequest::SetDnd(enabled));
    let mut s = yunshu_core::settings::Settings::load_or_default();
    s.dnd_mode = enabled;
    s.save()
}

fn main() {
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
        .manage(AppState { ui_tx, cache: Arc::new(Mutex::new(state_snapshot())) })
        .setup(move |app| {
            let handle = app.handle().clone();
            let cache = app.state::<AppState>().cache.clone();
            // 核心事件 → 前端（同时维护初始状态快照）
            std::thread::spawn(move || {
                while let Ok(ev) = event_rx.recv() {
                    apply_to_cache(&cache, &ev);
                    if let Ok(v) = serde_json::to_value(&ev) {
                        let _ = handle.emit("core-event", v);
                    }
                }
            });

            // 托盘
            use tauri::menu::{Menu, MenuItem};
            let open = MenuItem::with_id(app, "open", "打开云书", true, None::<&str>)?;
            let quit = MenuItem::with_id(app, "quit", "退出", true, None::<&str>)?;
            let menu = Menu::with_items(app, &[&open, &quit])?;
            tauri::tray::TrayIconBuilder::new()
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
            forget_transfer,
            delete_received_file,
            pigeon_disconnect,
            set_save_dir,
            open_folder,
            stage_blob,
            get_settings,
            save_settings,
            set_dnd
        ])
        .on_window_event(|window, event| {
            // 关闭 = 隐藏进托盘；退出走托盘菜单
            if let tauri::WindowEvent::CloseRequested { api, .. } = event {
                let _ = window.hide();
                api.prevent_close();
            }
        })
        .run(tauri::generate_context!())
        .expect("云书运行出错");
}
