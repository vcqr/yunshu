//! 设置：TOML 持久化 + 开机自启（注册表 Run 键）。

use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub device_name: String,
    pub port: u16,
    pub https_port: u16,
    pub save_dir: String,
    pub autostart: bool,
    /// 防打扰：仅已固定指纹（信任过）的设备可连接，广播消息静音
    pub dnd_mode: bool,
    /// 隐身模式：不向局域网广播本机（其他设备的主机列表将移除本机），也不主动外连
    pub stealth_mode: bool,
    /// 远程控制固定密码（pbkdf2-sha256 加盐哈希，格式 pbkdf2-sha256$rounds$salt$hash）
    pub remote_fixed_password_hash: Option<String>,
    /// 代理模式：direct 直连（默认） | system 跟随系统 | custom 自定义
    pub proxy_mode: String,
    /// 自定义代理地址（http/https/socks5://host:port，仅 custom 模式生效）
    pub proxy_url: String,
    /// NO_PROXY 例外列表（逗号分隔，仅 custom 模式生效）
    pub no_proxy: String,
}

impl Default for Settings {
    fn default() -> Self {
        let device_name = std::env::var("COMPUTERNAME").unwrap_or_else(|_| "我的电脑".into());
        let save_dir = default_save_dir().to_string_lossy().to_string();
        Settings {
            device_name,
            port: 34256,
            https_port: 34257,
            save_dir,
            autostart: false,
            // 首次启动默认开启防打扰（仅已信任设备可连接）；已有配置文件里显式的 false 不受影响
            dnd_mode: true,
            stealth_mode: false,
            remote_fixed_password_hash: None,
            proxy_mode: "direct".into(),
            proxy_url: String::new(),
            no_proxy: String::new(),
        }
    }
}

fn default_save_dir() -> PathBuf {
    dirs::download_dir()
        .unwrap_or_else(|| dirs::home_dir().unwrap_or_else(|| PathBuf::from(".")))
        .join("Yunshu")
}

impl Settings {
    fn config_path() -> Option<PathBuf> {
        crate::ensure_config_dir();
        crate::config_dir().map(|d| d.join("config.toml"))
    }

    pub fn load_or_default() -> Self {
        let Some(path) = Self::config_path() else {
            return Self::default();
        };
        match std::fs::read_to_string(&path) {
            Ok(text) => toml::from_str(&text).unwrap_or_default(),
            Err(_) => Self::default(),
        }
    }

    pub fn save(&self) -> Result<(), String> {
        let Some(path) = Self::config_path() else {
            return Err("无法定位配置目录".into());
        };
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        }
        let text = toml::to_string_pretty(self).map_err(|e| e.to_string())?;
        std::fs::write(&path, text).map_err(|e| e.to_string())?;
        // 含远程控制固定密码哈希，权限收紧为属主可读写
        crate::pigeon::set_owner_only(&path);
        Ok(())
    }

    pub fn save_dir_path(&self) -> PathBuf {
        if self.save_dir.trim().is_empty() {
            default_save_dir()
        } else {
            PathBuf::from(&self.save_dir)
        }
    }

    pub fn core_config(&self) -> crate::CoreConfig {
        crate::CoreConfig {
            port: self.port,
            https_port: self.https_port,
            save_dir: self.save_dir_path(),
            pc_name: self.device_name.clone(),
            dnd_mode: self.dnd_mode,
            stealth_mode: self.stealth_mode,
        }
    }

    /// 写/删 HKCU\...\Run 键。返回描述结果的文本。
    #[cfg(windows)]
    pub fn apply_autostart(enable: bool) -> Result<(), String> {
        use winreg::enums::*;
        let exe = std::env::current_exe().map_err(|e| e.to_string())?;
        let hkcu = winreg::RegKey::predef(HKEY_CURRENT_USER);
        let run = hkcu
            .open_subkey_with_flags(
                r"Software\Microsoft\Windows\CurrentVersion\Run",
                KEY_SET_VALUE | KEY_QUERY_VALUE,
            )
            .map_err(|e| e.to_string())?;
        if enable {
            run.set_value("yunshu", &exe.to_string_lossy().to_string())
                .map_err(|e| e.to_string())
        } else {
            match run.delete_value("yunshu") {
                Ok(()) => Ok(()),
                Err(ref e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
                Err(e) => Err(e.to_string()),
            }
        }
    }

    /// macOS：写/删 ~/Library/LaunchAgents/com.yunshu.desktop.plist（登录后自启，配合固定密码实现无人值守）。
    /// 调研文档权限矩阵结论：macOS 无人值守 = 登录项 + 一次性 TCC 授权即可；
    /// 免登录的 LaunchDaemon 受登录窗口上下文的 TCC 限制，不做。
    #[cfg(target_os = "macos")]
    pub fn apply_autostart(enable: bool) -> Result<(), String> {
        let home = dirs::home_dir().ok_or("无法定位用户目录")?;
        let agents = home.join("Library/LaunchAgents");
        std::fs::create_dir_all(&agents).map_err(|e| e.to_string())?;
        let plist = agents.join("com.yunshu.desktop.plist");
        if !enable {
            match std::fs::remove_file(&plist) {
                Ok(()) => {}
                Err(ref e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => return Err(e.to_string()),
            }
            let _ = std::process::Command::new("launchctl")
                .args(["unload", &plist.to_string_lossy()])
                .output();
            return Ok(());
        }
        let exe = std::env::current_exe().map_err(|e| e.to_string())?;
        let label = "com.yunshu.desktop";
        let content = format!(
            r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>Label</key>
    <string>{label}</string>
    <key>ProgramArguments</key>
    <array>
        <string>{}</string>
    </array>
    <key>RunAtLoad</key>
    <true/>
    <key>ProcessType</key>
    <string>Interactive</string>
</dict>
</plist>
"#,
            exe.to_string_lossy()
        );
        std::fs::write(&plist, content).map_err(|e| e.to_string())?;
        let out = std::process::Command::new("launchctl")
            .args(["load", &plist.to_string_lossy()])
            .output()
            .map_err(|e| e.to_string())?;
        if !out.status.success() {
            return Err(format!(
                "launchctl load 失败：{}",
                String::from_utf8_lossy(&out.stderr).trim()
            ));
        }
        Ok(())
    }

    /// Linux：暂未实现开机自启
    #[cfg(all(not(windows), not(target_os = "macos")))]
    pub fn apply_autostart(_enable: bool) -> Result<(), String> {
        Err("开机自启暂不支持当前平台".into())
    }
}
