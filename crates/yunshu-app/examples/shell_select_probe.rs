//! 冒烟：进程内 SHOpenFolderAndSelectItems 打开父目录并选中目标。
//! 用法：shell_select_probe <路径>；弹出资源管理器即成功。
#[cfg(windows)]
fn main() {
    let Some(path) = std::env::args().nth(1) else {
        eprintln!("用法：shell_select_probe <路径>");
        std::process::exit(1);
    };
    let target = std::path::PathBuf::from(&path);
    if let Err(e) = shell_open_and_select(&target) {
        eprintln!("失败：{e}");
        std::process::exit(2);
    }
    println!("OK：已请求打开并选中 {}", target.display());
}

#[cfg(windows)]
fn shell_open_and_select(target: &std::path::Path) -> Result<(), String> {
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
        let com_initialized = hr.is_ok();
        let result = (|| -> Result<(), String> {
            if !target.exists() {
                return Err(format!("路径不存在：{}", target.display()));
            }
            let folder = target.parent().unwrap_or(target).to_path_buf();
            let folder_w = wide(&folder);
            let mut pidl_folder = std::ptr::null_mut();
            SHParseDisplayName(PCWSTR(folder_w.as_ptr()), None, &mut pidl_folder, 0, None)
                .map_err(|e| format!("解析目录失败：{e}"))?;
            let item_w = wide(target);
            let mut pidl_item = std::ptr::null_mut();
            SHParseDisplayName(PCWSTR(item_w.as_ptr()), None, &mut pidl_item, 0, None)
                .map_err(|e| format!("解析文件失败：{e}"))?;
            let rel = ILFindLastID(pidl_item);
            let items = [rel as *const _];
            let r = SHOpenFolderAndSelectItems(pidl_folder, Some(&items), 0);
            CoTaskMemFree(Some(pidl_item.cast()));
            CoTaskMemFree(Some(pidl_folder.cast()));
            r.map_err(|e| format!("打开文件夹失败：{e}"))
        })();
        if com_initialized {
            CoUninitialize();
        }
        result
    }
}

#[cfg(not(windows))]
fn main() {}
