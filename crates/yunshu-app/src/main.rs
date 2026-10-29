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
        "addr": null, "error": null, "qr_url": null, "qr_alts": [],
        "pending": null, "connected": null,
        "peers": [], "transfers": [], "toasts": [],
        "remote_temp_password": null, "remote_controlled": null,
        "remote_machine_id": null,
        "weixin_inbox": [],
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
        CoreEvent::QrAlternates(urls) => {
            obj.insert("qr_alts".into(), json!(urls));
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
        CoreEvent::WeixinInbound { msg } => {
            // 最近 50 条环形缓冲，设置弹窗的微信卡片展示
            let list = obj.entry("weixin_inbox").or_insert(json!([]));
            let arr = list.as_array_mut().unwrap();
            arr.push(serde_json::to_value(msg).unwrap_or_default());
            let len = arr.len();
            if len > 50 {
                arr.drain(..len - 50);
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

/// 自助添加主机：登记并立即发起加密连接（重复添加等同重连）
#[tauri::command]
fn pigeon_add_host(state: tauri::State<AppState>, addr: String, name: String) {
    let _ = state.ui_tx.send(UiRequest::PigeonAddHost { addr, name });
}

/// 移除手动添加的主机（连接中的链路一并断开）
#[tauri::command]
fn pigeon_remove_host(state: tauri::State<AppState>, addr: String) {
    let _ = state.ui_tx.send(UiRequest::PigeonRemoveHost { addr });
}

/// 手动重连某台已添加的主机
#[tauri::command]
fn pigeon_connect_host(state: tauri::State<AppState>, addr: String) {
    let _ = state.ui_tx.send(UiRequest::PigeonConnectHost { addr });
}

#[tauri::command]
fn set_save_dir(state: tauri::State<AppState>, batch: String, dir: Option<String>) {
    let _ = state.ui_tx.send(UiRequest::SetSaveDir {
        batch,
        dir: dir.map(std::path::PathBuf::from),
    });
}

/// 打开文件所在文件夹：Windows 资源管理器选中文件，macOS 在 Finder 中显示，Linux 打开所在目录。
/// 路径已不存在（保存目录被移动/文件被清理）时向上回退到最近存在的祖先目录；
/// 整条链都不存在才报错，且错误信息附上原始路径便于诊断。
#[tauri::command]
fn open_folder(path: String) -> Result<(), String> {
    if path.trim().is_empty() {
        return Err("路径为空".into());
    }
    let target = std::path::PathBuf::from(&path);
    let Some(existing) = nearest_existing(&target) else {
        return Err(format!("路径不存在：{path}"));
    };
    #[cfg(target_os = "windows")]
    {
        // 首选：进程内 Shell API（打开父目录并选中目标，不创建子进程）。
        // 本应用进程内 CreateProcessW 会因环境异常失败（os error 50，独立
        // 探针进程均正常），Shell API 完全绕开子进程创建。
        if shell_open_and_select(&target, &existing).is_err() {
            // 回退：WINDIR 绝对路径定位 explorer.exe（不依赖 PATH 解析）
            let windir = std::env::var("WINDIR").unwrap_or_else(|_| r"C:\Windows".into());
            let explorer = std::path::Path::new(&windir).join("explorer.exe");
            let prog = if explorer.exists() {
                explorer
            } else {
                std::path::PathBuf::from("explorer")
            };
            let mut cmd = std::process::Command::new(&prog);
            cmd.arg(format!("/select,{}", existing.display()));
            if let Ok(exe) = std::env::current_exe()
                && let Some(dir) = exe.parent()
                && dir.exists()
            {
                cmd.current_dir(dir);
            }
            cmd.spawn()
                .map_err(|e| format!("启动资源管理器失败：{e}（路径：{path}）"))?;
        }
        Ok(())
    }
    #[cfg(target_os = "macos")]
    {
        std::process::Command::new("open")
            .args(["-R", &existing.to_string_lossy()])
            .spawn()
            .map_err(|e| format!("启动 Finder 失败：{e}（路径：{path}）"))?;
        Ok(())
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        // xdg-open 没有「选中」语义，退而打开所在目录
        let dir = if existing.is_dir() {
            existing
        } else {
            existing
                .parent()
                .map(|p| p.to_path_buf())
                .unwrap_or(existing)
        };
        std::process::Command::new("xdg-open")
            .arg(&dir)
            .spawn()
            .map_err(|e| format!("打开目录失败：{e}（路径：{path}）"))?;
        Ok(())
    }
}

/// 自下而上找最近存在的祖先（含自身）；连盘符/根都不存在时返回 None
fn nearest_existing(p: &std::path::Path) -> Option<std::path::PathBuf> {
    let mut cur = Some(p.to_path_buf());
    while let Some(c) = cur {
        if c.exists() {
            return Some(c);
        }
        cur = c.parent().map(|x| x.to_path_buf());
    }
    None
}

/// 进程内打开文件夹并选中目标（Shell API，不创建子进程）。
/// 目标存在 → 打开其父目录并选中；目标不存在 → 打开最近存在的祖先目录。
#[cfg(target_os = "windows")]
fn shell_open_and_select(
    target: &std::path::Path,
    existing: &std::path::Path,
) -> Result<(), String> {
    use std::os::windows::ffi::OsStrExt;
    use windows::core::PCWSTR;
    use windows::Win32::System::Com::{
        CoInitializeEx, CoTaskMemFree, CoUninitialize, COINIT_APARTMENTTHREADED,
    };
    use windows::Win32::UI::Shell::{ILFindLastID, SHOpenFolderAndSelectItems, SHParseDisplayName};

    fn wide(p: &std::path::Path) -> Vec<u16> {
        p.as_os_str().encode_wide().chain(std::iter::once(0)).collect()
    }

    unsafe {
        let hr = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
        // RPC_E_CHANGED_MODE：线程已初始化为其他套间，直接复用，不配对释放
        let com_initialized = hr.is_ok();

        let result = (|| -> Result<(), String> {
            let (folder, item) = if target.exists() {
                (
                    target.parent().unwrap_or(target).to_path_buf(),
                    Some(target.to_path_buf()),
                )
            } else {
                (existing.to_path_buf(), None)
            };

            let folder_w = wide(&folder);
            let mut pidl_folder = std::ptr::null_mut();
            SHParseDisplayName(PCWSTR(folder_w.as_ptr()), None, &mut pidl_folder, 0, None)
                .map_err(|e| format!("解析目录失败：{e}"))?;
            if pidl_folder.is_null() {
                return Err("解析目录失败：空 PIDL".into());
            }

            let select = match item {
                Some(t) => {
                    let item_w = wide(&t);
                    let mut pidl_item = std::ptr::null_mut();
                    if let Err(e) =
                        SHParseDisplayName(PCWSTR(item_w.as_ptr()), None, &mut pidl_item, 0, None)
                    {
                        CoTaskMemFree(Some(pidl_folder.cast()));
                        return Err(format!("解析文件失败：{e}"));
                    }
                    // apidl 须为相对父目录的 ITEMIDLIST：取完整 PIDL 最后一段
                    let rel = ILFindLastID(pidl_item);
                    let items = [rel as *const _];
                    let r = SHOpenFolderAndSelectItems(pidl_folder, Some(&items), 0);
                    CoTaskMemFree(Some(pidl_item.cast()));
                    r
                }
                None => SHOpenFolderAndSelectItems(pidl_folder, None, 0),
            };
            CoTaskMemFree(Some(pidl_folder.cast()));
            select.map_err(|e| format!("打开文件夹失败：{e}"))
        })();

        if com_initialized {
            CoUninitialize();
        }
        result
    }
}

#[cfg(test)]
mod open_folder_tests {
    use super::nearest_existing;

    #[test]
    fn existing_path_returns_self() {
        let dir = std::env::temp_dir().join(format!("of-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        assert_eq!(nearest_existing(&dir), Some(dir.clone()));
        // 子路径不存在 → 回退到存在的父目录
        let child = dir.join("不存在的子目录").join("x.txt");
        assert_eq!(nearest_existing(&child), Some(dir.clone()));
        std::fs::remove_dir_all(&dir).unwrap();
        // 整条链都没了 → 回退到上级（temp 目录存在）
        assert_eq!(nearest_existing(&dir), Some(std::env::temp_dir()));
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
    stealth_mode: bool,
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
    if s.stealth_mode != stealth_mode {
        let _ = state.ui_tx.send(UiRequest::SetStealth(stealth_mode));
        s.stealth_mode = stealth_mode;
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

/// 隐身模式开关（状态菜单）：立即生效并持久化
#[tauri::command]
fn set_stealth(state: tauri::State<AppState>, enabled: bool) -> Result<(), String> {
    let _ = state.ui_tx.send(UiRequest::SetStealth(enabled));
    let mut s = yunshu_core::settings::Settings::load_or_default();
    s.stealth_mode = enabled;
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

// ── 微信通道（扫码登录个人微信机器人；登录态落配置目录 weixin.json）──

#[tauri::command]
async fn weixin_login_start() -> Result<serde_json::Value, String> {
    let v = yunshu_core::weixin::login_start().await?;
    serde_json::to_value(v).map_err(|e| e.to_string())
}

#[tauri::command]
fn weixin_login_status() -> Result<serde_json::Value, String> {
    serde_json::to_value(yunshu_core::weixin::login_view()).map_err(|e| e.to_string())
}

/// need_verifycode 时提交手机微信上显示的配对数字
#[tauri::command]
fn weixin_login_submit_code(code: String) {
    yunshu_core::weixin::login_submit_code(&code);
}

#[tauri::command]
fn weixin_login_cancel() {
    yunshu_core::weixin::login_cancel();
}

#[tauri::command]
fn weixin_logout() -> Result<(), String> {
    yunshu_core::weixin::logout()
}

#[tauri::command]
fn weixin_status() -> Result<serde_json::Value, String> {
    serde_json::to_value(yunshu_core::weixin::status_view()).map_err(|e| e.to_string())
}

/// 最近收到的微信消息（文本与已落盘媒体），前端展示用
#[tauri::command]
fn weixin_inbox(limit: Option<u32>) -> Result<serde_json::Value, String> {
    let items = yunshu_core::weixin::inbox_recent(limit.unwrap_or(20).clamp(1, 100) as usize);
    serde_json::to_value(items).map_err(|e| e.to_string())
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
    // 微信通道入站消息走 CoreEvent::WeixinInbound 进事件泵（转前端 + 维护缓存）
    yunshu_core::weixin::set_event_sink(event_tx.clone());
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
            // 圆角窗口：WebView 背景强制全透明（0,0,0,0），否则 WebView 默认底色
            // 会从 CSS 圆角外的四角露出色块（窗口 transparent/shadow:false 见 tauri.conf.json）
            if let Some(win) = app.get_webview_window("main") {
                let _ = win.set_background_color(Some(tauri::webview::Color(0, 0, 0, 0)));
            }
            let handle = app.handle().clone();
            let cache = app.state::<AppState>().cache.clone();
            // 微信通道：有凭证则恢复长轮询（跑在 Tauri 自带的 tokio runtime 上，与进程同寿）
            tauri::async_runtime::spawn(async {
                yunshu_core::weixin::init_polling();
            });
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
                    "quit" => {
                        // 广播下线通知：其他设备即时从列表移除本机
                        yunshu_core::pigeon::broadcast_leave();
                        app.exit(0)
                    }
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
            pigeon_add_host,
            pigeon_remove_host,
            pigeon_connect_host,
            set_save_dir,
            open_folder,
            stage_blob,
            get_settings,
            save_settings,
            set_dnd,
            set_stealth,
            remote_connect,
            remote_disconnect,
            remote_send_files,
            service_install,
            service_uninstall,
            remote_assist_request,
            remote_assist_respond,
            get_view_info,
            weixin_login_start,
            weixin_login_status,
            weixin_login_submit_code,
            weixin_login_cancel,
            weixin_logout,
            weixin_status,
            weixin_inbox
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
