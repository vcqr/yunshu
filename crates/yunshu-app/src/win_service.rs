//! Windows 服务化（调研文档三期）：免登录自启 + 登录会话内拉起 UI 代理。
//!
//! 模型对齐 RustDesk：`yunshu service install` 注册 SYSTEM 服务（开机自启）；
//! 服务运行后轮询活动控制台会话，检测到用户登录即以该用户身份在会话内
//! 拉起 yunshu.exe（托盘/采集/注入均在用户会话中，UAC 提权窗口可见）。
//! 服务退出（Stop/Shutdown）时不再拉起；agent 进程退出自动重启。
//!
//! 仅 Windows；其余平台不编译本模块。

#![cfg(windows)]

use windows::Win32::Foundation::{CloseHandle, HANDLE, WAIT_OBJECT_0};
use windows::Win32::Security::{
    DuplicateTokenEx, SetTokenInformation, SecurityImpersonation, TokenPrimary, TokenSessionId,
    TOKEN_ADJUST_SESSIONID, TOKEN_ASSIGN_PRIMARY, TOKEN_DUPLICATE, TOKEN_QUERY,
};
use windows::Win32::System::Environment::{CreateEnvironmentBlock, DestroyEnvironmentBlock};
use windows::Win32::System::RemoteDesktop::{WTSGetActiveConsoleSessionId, WTSQueryUserToken};
use windows::Win32::System::Threading::{
    GetCurrentProcess, OpenProcessToken, CREATE_NO_WINDOW, CREATE_SUSPENDED,
    CREATE_UNICODE_ENVIRONMENT, CreateProcessAsUserW, DETACHED_PROCESS, PROCESS_INFORMATION,
    STARTUPINFOW, WaitForSingleObject,
};
use windows::core::PWSTR;
use windows_service::service::{
    ServiceControl, ServiceControlAccept, ServiceErrorControl, ServiceExitCode, ServiceState,
    ServiceStatus, ServiceType,
};
use windows_service::service_control_handler::{self, ServiceControlHandlerResult};
use windows_service::{define_windows_service, service_dispatcher};

pub const SERVICE_NAME: &str = "yunshu";

define_windows_service!(ffi_service_main, service_main);

/// main() 入口：命令行匹配 `service run` 时进入服务分发器（阻塞），返回 true 表示已处理
pub fn try_dispatch() -> bool {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.len() == 2 && args[0] == "service" {
        match args[1].as_str() {
            "run" => {
                // 服务模式失败仅记日志（服务上下文无 UI）
                if let Err(e) = service_dispatcher::start(SERVICE_NAME, ffi_service_main) {
                    eprintln!("服务运行失败: {e}");
                }
                return true;
            }
            "install" => {
                match install() {
                    Ok(()) => println!("Yunshu 服务已安装（开机自启，登录会话内自动拉起）"),
                    Err(e) => {
                        eprintln!("安装失败: {e}");
                        std::process::exit(1);
                    }
                }
                return true;
            }
            "uninstall" => {
                match uninstall() {
                    Ok(()) => println!("Yunshu 服务已卸载"),
                    Err(e) => {
                        eprintln!("卸载失败: {e}");
                        std::process::exit(1);
                    }
                }
                return true;
            }
            _ => {}
        }
    }
    false
}

fn service_main(args: Vec<std::ffi::OsString>) {
    if let Err(e) = run_service(args) {
        eprintln!("服务异常退出: {e}");
    }
}

fn run_service(_args: Vec<std::ffi::OsString>) -> windows_service::Result<()> {
    let (stop_tx, stop_rx) = std::sync::mpsc::channel::<()>();
    let handler = service_control_handler::register(SERVICE_NAME, move |control| match control {
        ServiceControl::Stop | ServiceControl::Shutdown => {
            let _ = stop_tx.send(());
            ServiceControlHandlerResult::NoError
        }
        ServiceControl::Interrogate => ServiceControlHandlerResult::NoError,
        _ => ServiceControlHandlerResult::NotImplemented,
    })?;
    handler.set_service_status(ServiceStatus {
        service_type: ServiceType::OWN_PROCESS,
        current_state: ServiceState::Running,
        controls_accepted: ServiceControlAccept::STOP | ServiceControlAccept::SHUTDOWN,
        exit_code: ServiceExitCode::NO_ERROR,
        checkpoint: 0,
        wait_hint: std::time::Duration::from_secs(5),
        process_id: None,
    })?;

    agent_loop(stop_rx);

    handler.set_service_status(ServiceStatus {
        service_type: ServiceType::OWN_PROCESS,
        current_state: ServiceState::Stopped,
        controls_accepted: ServiceControlAccept::empty(),
        exit_code: ServiceExitCode::NO_ERROR,
        checkpoint: 0,
        wait_hint: std::time::Duration::from_secs(0),
        process_id: None,
    })?;
    Ok(())
}

/// 服务主循环：每 3 秒检查活动控制台会话，agent 未运行则以登录用户身份拉起
fn agent_loop(stop_rx: std::sync::mpsc::Receiver<()>) {
    let mut child: Option<HANDLE> = None;
    let mut helper: Option<HANDLE> = None;
    // 三期：接管令牌每次服务启动随机生成，分别下发给助手与 agent
    let secure_token = yunshu_core::remote::secure::random_token();
    let exe = match std::env::current_exe() {
        Ok(p) => p,
        Err(_) => return,
    };
    loop {
        // Stop 请求（带 3 秒超时的等待 = 轮询间隔）
        if stop_rx
            .recv_timeout(std::time::Duration::from_secs(3))
            .is_ok()
        {
            break;
        }
        // 回收已退出的 agent / 安全助手
        if let Some(h) = child {
            unsafe {
                if WaitForSingleObject(h, 0) == WAIT_OBJECT_0 {
                    let _ = CloseHandle(h);
                    child = None;
                }
            }
        }
        if let Some(h) = helper {
            unsafe {
                if WaitForSingleObject(h, 0) == WAIT_OBJECT_0 {
                    let _ = CloseHandle(h);
                    helper = None;
                }
            }
        }
        let session = unsafe { WTSGetActiveConsoleSessionId() };
        // 0xFFFFFFFF = 无活动会话（未登录/锁屏前一瞬间）
        if session == u32::MAX {
            continue;
        }
        // 三期：SYSTEM 安全助手（控制台会话内），锁屏采集/输入的执行者。
        // 登录界面即可拉起（不依赖用户令牌）；agent 死亡不影响助手存活
        if helper.is_none() {
            match unsafe { spawn_secure_helper(&exe, session, &secure_token) } {
                Ok(h) => helper = Some(h),
                Err(e) => eprintln!("安全助手拉起失败（session {session}）: {e}"),
            }
        }
        if child.is_some() {
            continue;
        }
        match unsafe { spawn_agent_in_session(&exe, session, &secure_token) } {
            Ok(h) => child = Some(h),
            // 会话刚登录令牌未就绪等情况：静默重试
            Err(e) => eprintln!("拉起会话代理失败（session {session}）: {e}"),
        }
    }
    // 退出时不杀 agent：用户可能正用着，agent 随下次注销/关机退出
}

/// 以 SYSTEM 身份在控制台会话内启动安全助手（yunshu.exe secure --token …）。
/// SYSTEM + 控制台会话 ⇒ 可采集安全桌面（锁屏）、SendInput 可达安全桌面。
/// SAFETY: 运行于 SYSTEM 服务上下文；令牌/句柄由 RAII guard 释放
unsafe fn spawn_secure_helper(
    exe: &std::path::Path,
    session: u32,
    token_hex: &str,
) -> windows::core::Result<HANDLE> {
    unsafe {
        let mut cur = HANDLE::default();
        OpenProcessToken(
            GetCurrentProcess(),
            TOKEN_DUPLICATE | TOKEN_ASSIGN_PRIMARY | TOKEN_QUERY | TOKEN_ADJUST_SESSIONID,
            &mut cur,
        )?;
        let _cur_guard = HandleGuard(cur);

        let mut dup = HANDLE::default();
        DuplicateTokenEx(
            cur,
            TOKEN_DUPLICATE | TOKEN_ASSIGN_PRIMARY | TOKEN_QUERY | TOKEN_ADJUST_SESSIONID,
            None,
            SecurityImpersonation,
            TokenPrimary,
            &mut dup,
        )?;
        let _dup_guard = HandleGuard(dup);

        // 会话重定向到活动控制台会话（服务本体在会话 0）
        SetTokenInformation(
            dup,
            TokenSessionId,
            &(session as i32) as *const i32 as *const core::ffi::c_void,
            4,
        )?;

        let args = format!(""{}" secure --token {token_hex}", exe.display());
        let args_w: Vec<u16> = args.encode_utf16().chain(std::iter::once(0)).collect();
        let si = STARTUPINFOW {
            cb: std::mem::size_of::<STARTUPINFOW>() as u32,
            ..Default::default()
        };
        let mut pi = PROCESS_INFORMATION::default();
        CreateProcessAsUserW(
            Some(dup),
            PWSTR::null(),
            Some(PWSTR(args_w.as_mut_ptr())),
            None,
            None,
            false,
            CREATE_NO_WINDOW | CREATE_SUSPENDED | CREATE_UNICODE_ENVIRONMENT,
            None,
            None,
            &si,
            &mut pi,
        )?;
        windows::Win32::System::Threading::ResumeThread(pi.hThread);
        let _ = CloseHandle(pi.hThread);
        Ok(pi.hProcess)
    }
}

/// 以会话用户身份在控制台会话内启动 agent（yunshu.exe）
/// SAFETY: 运行于 SYSTEM 服务上下文；token/env 句柄由 RAII guard 释放
unsafe fn spawn_agent_in_session(
    exe: &std::path::Path,
    session: u32,
    secure_token: &str,
) -> windows::core::Result<HANDLE> {
    unsafe {
        let mut token = HANDLE::default();
        WTSQueryUserToken(session, &mut token)?;
        let token_guard = HandleGuard(token);

        let mut env: *mut core::ffi::c_void = std::ptr::null_mut();
        CreateEnvironmentBlock(&mut env, Some(token_guard.0), false)?;
        // 追加 YUNSHU_SECURE_TOKEN（三期）：agent 据此与 SYSTEM 安全助手互信。
        // 环境块 = 双 0 结尾的字符串序列，把新变量拼在结尾
        let extra: Vec<u16> = format!("YUNSHU_SECURE_TOKEN={secure_token} ")
            .encode_utf16()
            .collect();
        let mut n = 0usize;
        loop {
            let p = env as *const u16;
            if *p.add(n) == 0 && *p.add(n + 1) == 0 {
                n += 2;
                break;
            }
            n += 1;
        }
        let mut merged: Vec<u16> = Vec::with_capacity(n + extra.len() + 1);
        merged.extend_from_slice(std::slice::from_raw_parts(env as *const u16, n - 1));
        merged.extend_from_slice(&extra);
        merged.push(0);
        struct EnvGuard(*mut core::ffi::c_void, Vec<u16>);
        impl Drop for EnvGuard {
            fn drop(&mut self) {
                unsafe {
                    let _ = DestroyEnvironmentBlock(self.0);
                }
            }
        }
        let _env_guard = EnvGuard(env, merged);

        let exe_w: Vec<u16> = exe
            .to_string_lossy()
            .encode_utf16()
            .chain(std::iter::once(0))
            .collect();
        let si = STARTUPINFOW {
            cb: size_of::<STARTUPINFOW>() as u32,
            ..Default::default()
        };
        let mut pi = PROCESS_INFORMATION::default();
        let merged_env = _env_guard.1.as_ptr();
        CreateProcessAsUserW(
            Some(token_guard.0),
            PWSTR(exe_w.as_ptr() as *mut u16),
            None,
            None,
            None,
            false,
            CREATE_UNICODE_ENVIRONMENT | CREATE_SUSPENDED | DETACHED_PROCESS,
            Some(merged_env as *mut core::ffi::c_void),
            None,
            &si,
            &mut pi,
        )?;
        // CREATE_SUSPENDED 启动：环境块生命周期内绝对安全，随后恢复执行
        windows::Win32::System::Threading::ResumeThread(pi.hThread);
        let _ = CloseHandle(pi.hThread);
        Ok(pi.hProcess)
    }
}

struct HandleGuard(HANDLE);
impl Drop for HandleGuard {
    fn drop(&mut self) {
        unsafe {
            let _ = CloseHandle(self.0);
        }
    }
}

// ---------- 安装 / 卸载（需管理员；前端经 PowerShell -Verb RunAs 提权调用） ----------

fn install() -> Result<(), String> {
    use windows_service::service::ServiceAccess;
    use windows_service::service_manager::{ServiceManager, ServiceManagerAccess};
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    let manager =
        ServiceManager::local_computer(None::<&str>, ServiceManagerAccess::CREATE_SERVICE)
            .map_err(|e| e.to_string())?;
    let info = windows_service::service::ServiceInfo {
        name: SERVICE_NAME.into(),
        display_name: "Yunshu 远程控制服务".into(),
        service_type: ServiceType::OWN_PROCESS,
        start_type: windows_service::service::ServiceStartType::AutoStart,
        error_control: ServiceErrorControl::Normal,
        executable_path: exe,
        launch_arguments: vec!["service".into(), "run".into()],
        dependencies: vec![],
        account_name: None, // LocalSystem
        account_password: None,
    };
    let service = manager
        .create_service(&info, ServiceAccess::CHANGE_CONFIG)
        .map_err(|e| e.to_string())?;
    // 失败自动重启（崩溃/被杀后 30 秒拉起）
    let actions = vec![
        windows_service::service::ServiceAction {
            action_type: windows_service::service::ServiceActionType::Restart,
            delay: std::time::Duration::from_secs(30),
        },
        windows_service::service::ServiceAction {
            action_type: windows_service::service::ServiceActionType::Restart,
            delay: std::time::Duration::from_secs(30),
        },
        windows_service::service::ServiceAction {
            action_type: windows_service::service::ServiceActionType::Restart,
            delay: std::time::Duration::from_secs(60),
        },
    ];
    service
        .update_failure_actions(windows_service::service::ServiceFailureActions {
            reset_period: windows_service::service::ServiceFailureResetPeriod::After(
                std::time::Duration::from_secs(86_400),
            ),
            reboot_msg: None,
            command: None,
            actions: Some(actions),
        })
        .map_err(|e| e.to_string())?;
    Ok(())
}

fn uninstall() -> Result<(), String> {
    use windows_service::service::ServiceAccess;
    use windows_service::service_manager::{ServiceManager, ServiceManagerAccess};
    let manager = ServiceManager::local_computer(None::<&str>, ServiceManagerAccess::CONNECT)
        .map_err(|e| e.to_string())?;
    let service = manager
        .open_service(
            SERVICE_NAME,
            ServiceAccess::DELETE | windows_service::service::ServiceAccess::STOP,
        )
        .map_err(|e| e.to_string())?;
    // 先尝试停止（未运行则忽略）
    let _ = service.stop();
    service.delete().map_err(|e| e.to_string())?;
    Ok(())
}
