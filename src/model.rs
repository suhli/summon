use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::hotkey::Hotkey;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Entry {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub icon: Option<String>,
    #[serde(default)]
    pub keywords: Vec<String>,
    #[serde(default)]
    pub hotkey: Option<String>,
    pub action: Action,
}

impl Entry {
    pub fn parsed_hotkey(&self) -> Option<Hotkey> {
        self.hotkey.as_deref().and_then(Hotkey::parse)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum Action {
    App {
        path: PathBuf,
        #[serde(default)]
        args: Vec<String>,
        #[serde(default)]
        working_dir: Option<PathBuf>,
    },
    File {
        path: PathBuf,
    },
    Directory {
        path: PathBuf,
    },
    Url {
        url: String,
    },
    Command {
        command: String,
        #[serde(default)]
        args: Vec<String>,
        #[serde(default)]
        working_dir: Option<PathBuf>,
    },
    PowerShell {
        script: String,
    },
}

impl Action {
    #[allow(dead_code)]
    pub fn kind_key(&self) -> &'static str {
        match self {
            Self::App { .. } => "app",
            Self::File { .. } => "file",
            Self::Directory { .. } => "directory",
            Self::Url { .. } => "url",
            Self::Command { .. } => "command",
            Self::PowerShell { .. } => "powershell",
        }
    }

    pub fn display_label(&self) -> &'static str {
        match self {
            Self::App { .. } => "Application",
            Self::File { .. } => "File",
            Self::Directory { .. } => "Directory",
            Self::Url { .. } => "URL",
            Self::Command { .. } => "Command",
            Self::PowerShell { .. } => "PowerShell",
        }
    }

    pub fn icon_source_path(&self) -> Option<&Path> {
        match self {
            Self::App { path, .. } | Self::File { path } | Self::Directory { path } => Some(path),
            Self::PowerShell { script } => {
                let path = Path::new(script);
                if path.extension().and_then(|e| e.to_str()).eq(&Some("ps1")) {
                    Some(path)
                } else {
                    None
                }
            }
            Self::Url { .. } | Self::Command { .. } => None,
        }
    }
}

pub fn new_entry_id() -> String {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    format!("entry-{nanos:x}")
}
