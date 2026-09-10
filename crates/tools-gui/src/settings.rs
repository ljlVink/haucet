use serde::{Deserialize, Serialize};
use std::path::PathBuf;

use crate::i18n::Language;

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub last_dir: Option<String>,
    pub recent: Vec<String>,
    pub language: Language,
    #[serde(default = "default_dark")]
    pub dark: bool,
    pub transparent_window: bool,
    pub startup_notice_accepted: bool,
    pub last_seen_version: Option<String>,
}

fn default_dark() -> bool {
    true
}

impl Settings {
    pub fn load() -> Self {
        let mut settings: Self = std::fs::read_to_string(path())
            .ok()
            .and_then(|text| serde_json::from_str(&text).ok())
            .unwrap_or_default();
        settings.transparent_window &= crate::window::TRANSPARENCY_SUPPORTED;
        if settings.transparent_window {
            settings.dark = false;
        }
        settings
    }

    pub fn save(&self) {
        if let Some(parent) = path().parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        if let Ok(json) = serde_json::to_string_pretty(self) {
            let _ = std::fs::write(path(), json);
        }
    }

    pub fn remember_path(&mut self, path: &std::path::Path) {
        if path.is_dir() {
            self.last_dir = Some(path.display().to_string());
            return;
        }

        if let Some(parent) = path
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
        {
            self.last_dir = Some(parent.display().to_string());
        }

        if path.is_file() {
            let text = path.display().to_string();
            self.recent.retain(|entry| entry != &text);
            self.recent.insert(0, text);
            self.recent.truncate(8);
        }
    }
}

fn path() -> PathBuf {
    let mut dir = data_dir();
    dir.push("haucet-gui");
    let _ = std::fs::create_dir_all(&dir);
    dir.push("settings.json");
    dir
}

fn data_dir() -> PathBuf {
    #[cfg(windows)]
    {
        if let Some(appdata) = std::env::var_os("APPDATA") {
            return PathBuf::from(appdata);
        }
    }
    #[cfg(target_os = "macos")]
    {
        if let Some(home) = std::env::var_os("HOME") {
            return PathBuf::from(home).join("Library/Application Support");
        }
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        if let Some(xdg) = std::env::var_os("XDG_DATA_HOME") {
            return PathBuf::from(xdg);
        }
        if let Some(home) = std::env::var_os("HOME") {
            return PathBuf::from(home).join(".local/share");
        }
    }
    std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."))
}
