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
}

impl Default for Settings {
    fn default() -> Self {
        let device_name = std::env::var("COMPUTERNAME").unwrap_or_else(|_| "我的电脑".into());
        let save_dir = default_save_dir().to_string_lossy().to_string();
        Settings { device_name, port: 34256, https_port: 34257, save_dir, autostart: false, dnd_mode: false }
    }
}

fn default_save_dir() -> PathBuf {
    dirs::download_dir()
        .unwrap_or_else(|| dirs::home_dir().unwrap_or_else(|| PathBuf::from(".")))
        .join("云书")
}

impl Settings {
    fn config_path() -> Option<PathBuf> {
        dirs::config_dir().map(|d| d.join("yunshu").join("config.toml"))
    }

    pub fn load_or_default() -> Self {
        let Some(path) = Self::config_path() else { return Self::default() };
        match std::fs::read_to_string(&path) {
            Ok(text) => toml::from_str(&text).unwrap_or_default(),
            Err(_) => Self::default(),
        }
    }

    pub fn save(&self) -> Result<(), String> {
        let Some(path) = Self::config_path() else { return Err("无法定位配置目录".into()) };
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        }
        let text = toml::to_string_pretty(self).map_err(|e| e.to_string())?;
        std::fs::write(&path, text).map_err(|e| e.to_string())
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
        }
    }

    /// 写/删 HKCU\...\Run 键。返回描述结果的文本。
    #[cfg(windows)]
    pub fn apply_autostart(enable: bool) -> Result<(), String> {
        use winreg::enums::*;
        let exe = std::env::current_exe().map_err(|e| e.to_string())?;
        let hkcu = winreg::RegKey::predef(HKEY_CURRENT_USER);
        let run = hkcu
            .open_subkey_with_flags(r"Software\Microsoft\Windows\CurrentVersion\Run", KEY_SET_VALUE | KEY_QUERY_VALUE)
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

    /// 非 Windows 平台暂未实现开机自启
    #[cfg(not(windows))]
    pub fn apply_autostart(_enable: bool) -> Result<(), String> {
        Err("开机自启暂不支持当前平台".into())
    }
}
