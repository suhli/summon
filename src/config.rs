use std::fmt;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use tracing::{error, info, warn};

use crate::model::Entry;

pub const CONFIG_VERSION: u32 = 2;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    #[serde(default = "default_version")]
    pub version: u32,
    #[serde(default)]
    pub launcher: LauncherSettings,
    #[serde(default)]
    pub entries: Vec<Entry>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LauncherSettings {
    #[serde(default = "default_hotkey")]
    pub hotkey: String,
    #[serde(default = "default_true")]
    pub hide_on_focus_lost: bool,
    #[serde(default)]
    pub launch_at_startup: bool,
}

impl Default for LauncherSettings {
    fn default() -> Self {
        Self {
            hotkey: default_hotkey(),
            hide_on_focus_lost: true,
            launch_at_startup: false,
        }
    }
}

impl Default for Config {
    fn default() -> Self {
        Self {
            version: CONFIG_VERSION,
            launcher: LauncherSettings::default(),
            entries: default_entries(),
        }
    }
}

fn default_version() -> u32 {
    CONFIG_VERSION
}

fn default_hotkey() -> String {
    "Alt+Space".into()
}

fn default_true() -> bool {
    true
}

pub fn config_dir() -> PathBuf {
    appdata_dir().join("Summon")
}

pub fn config_path() -> PathBuf {
    config_dir().join("config.toml")
}

pub fn log_path() -> PathBuf {
    config_dir().join("summon.log")
}

fn appdata_dir() -> PathBuf {
    std::env::var_os("APPDATA")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."))
}

pub fn ensure_config_dir() -> Result<PathBuf, String> {
    let dir = config_dir();
    if let Err(error) = std::fs::create_dir_all(&dir) {
        error!(path = %dir.display(), %error, "failed to create config directory");
        return Err(format!("Failed to create config directory: {error}"));
    }
    Ok(dir)
}

pub fn load() -> Config {
    let path = config_path();
    info!(path = %path.display(), "config path");

    if let Err(error) = ensure_config_dir() {
        warn!(%error, "using in-memory default config");
        return Config::default();
    }

    if !path.exists() {
        let config = Config::default();
        info!("config not found, writing defaults");
        let _ = save(&config);
        return config;
    }

    match std::fs::read_to_string(&path) {
        Ok(text) => match toml::from_str::<Config>(&text) {
            Ok(mut config) => {
                sanitize(&mut config);
                if migrate(&mut config) {
                    let _ = save(&config);
                }
                info!(entries = config.entries.len(), "config loaded");
                config
            }
            Err(error) => {
                error!(%error, "config parse failed, using defaults");
                Config::default()
            }
        },
        Err(error) => {
            error!(%error, "config read failed, using defaults");
            Config::default()
        }
    }
}

pub fn save(config: &Config) -> Result<(), String> {
    let path = config_path();
    if let Err(error) = ensure_config_dir() {
        return Err(error);
    }

    let serialized = match toml::to_string_pretty(config) {
        Ok(text) => text,
        Err(error) => {
            error!(%error, "config serialize failed");
            return Err(format!("Failed to serialize config: {error}"));
        }
    };

    let tmp = path.with_extension("toml.tmp");
    if let Err(error) = std::fs::write(&tmp, serialized) {
        error!(path = %tmp.display(), %error, "config save failed");
        return Err(format!("Failed to save config: {error}"));
    }
    if let Err(error) = std::fs::rename(&tmp, &path) {
        let _ = std::fs::remove_file(&tmp);
        error!(path = %path.display(), %error, "config replace failed");
        return Err(format!("Failed to save config: {error}"));
    }

    info!(path = %path.display(), "config saved");
    Ok(())
}

fn sanitize(config: &mut Config) {
    if config.version == 0 {
        config.version = CONFIG_VERSION;
    }
    if config.launcher.hotkey.trim().is_empty() {
        config.launcher.hotkey = default_hotkey();
    }

    let mut seen = std::collections::HashSet::new();
    for entry in &mut config.entries {
        if entry.id.trim().is_empty() || !seen.insert(entry.id.clone()) {
            entry.id = crate::model::new_entry_id();
            seen.insert(entry.id.clone());
        }
        entry.name = entry.name.trim().to_string();
        if entry.name.is_empty() {
            entry.name = "Untitled".into();
        }
    }
}

fn migrate(config: &mut Config) -> bool {
    if config.version >= CONFIG_VERSION {
        return false;
    }

    if config.version < 2 {
        const STOCK_IDS: &[&str] = &[
            "notepad",
            "explorer",
            "downloads",
            "calculator",
            "settings",
            "powershell",
            "terminal",
            "chatgpt",
            "paint",
        ];
        config
            .entries
            .retain(|entry| !STOCK_IDS.contains(&entry.id.as_str()));
        info!("removed built-in starter entries");
    }

    config.version = CONFIG_VERSION;
    true
}

fn default_entries() -> Vec<Entry> {
    Vec::new()
}

pub fn expand_path(path: &Path) -> PathBuf {
    let raw = path.to_string_lossy();
    if let Some(rest) = raw.strip_prefix("%USERPROFILE%") {
        if let Some(home) = std::env::var_os("USERPROFILE") {
            return PathBuf::from(home).join(rest.trim_start_matches(['\\', '/']));
        }
    }
    path.to_path_buf()
}

impl fmt::Display for Config {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Summon config v{} ({} entries)", self.version, self.entries.len())
    }
}
