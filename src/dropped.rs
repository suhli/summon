use std::path::{Path, PathBuf};

use tracing::warn;
use windows::core::{Interface, PCWSTR};
use windows::Win32::Foundation::HWND;
use windows::Win32::System::Com::{
    CoCreateInstance, CLSCTX_INPROC_SERVER, IPersistFile, STGM_READ,
};
use windows::Win32::UI::Shell::{IShellLinkW, ShellLink, SLR_NO_UI, SLR_NOUPDATE};

use crate::model::{new_entry_id, Action, Entry};

pub fn entry_from_path(path: PathBuf) -> Entry {
    let name = display_name(&path);
    let action = action_from_path(&path);
    Entry {
        id: new_entry_id(),
        name,
        description: Some(path.to_string_lossy().into_owned()),
        icon: None,
        keywords: Vec::new(),
        hotkey: None,
        action,
    }
}

pub fn same_target(left: &Action, right: &Action) -> bool {
    match (left, right) {
        (Action::App { path: a, .. }, Action::App { path: b, .. })
        | (Action::File { path: a }, Action::File { path: b })
        | (Action::Directory { path: a }, Action::Directory { path: b }) => paths_equal(a, b),
        (Action::Url { url: a }, Action::Url { url: b }) => a.eq_ignore_ascii_case(b),
        _ => false,
    }
}

fn action_from_path(path: &Path) -> Action {
    if path.is_dir() {
        return Action::Directory {
            path: path.to_path_buf(),
        };
    }

    let ext = path
        .extension()
        .and_then(|ext| ext.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();

    match ext.as_str() {
        "lnk" => action_from_shortcut(path),
        "url" => action_from_url_file(path).unwrap_or_else(|| Action::File {
            path: path.to_path_buf(),
        }),
        "exe" | "bat" | "cmd" | "com" | "msc" | "cpl" => Action::App {
            path: path.to_path_buf(),
            args: Vec::new(),
            working_dir: path.parent().map(Path::to_path_buf),
        },
        _ => Action::File {
            path: path.to_path_buf(),
        },
    }
}

fn action_from_shortcut(path: &Path) -> Action {
    match resolve_shortcut(path) {
        Ok(resolved) if resolved.target.as_os_str().is_empty() => Action::File {
            path: path.to_path_buf(),
        },
        Ok(resolved) if resolved.target.is_dir() => Action::Directory {
            path: resolved.target,
        },
        Ok(resolved) if is_launchable(&resolved.target) => Action::App {
            path: resolved.target,
            args: resolved.args,
            working_dir: resolved.working_dir,
        },
        Ok(resolved) => Action::File {
            path: resolved.target,
        },
        Err(error) => {
            warn!(path = %path.display(), %error, "shortcut resolve failed");
            Action::File {
                path: path.to_path_buf(),
            }
        }
    }
}

struct ResolvedShortcut {
    target: PathBuf,
    args: Vec<String>,
    working_dir: Option<PathBuf>,
}

fn resolve_shortcut(path: &Path) -> Result<ResolvedShortcut, String> {
    unsafe {
        let link: IShellLinkW = CoCreateInstance(&ShellLink, None, CLSCTX_INPROC_SERVER)
            .map_err(|error| error.to_string())?;
        let persist: IPersistFile = link.cast().map_err(|error| error.to_string())?;
        let wide = wide(path);
        persist
            .Load(PCWSTR(wide.as_ptr()), STGM_READ)
            .map_err(|error| error.to_string())?;
        let _ = link.Resolve(HWND::default(), (SLR_NO_UI.0 | SLR_NOUPDATE.0) as u32);

        let mut target = [0u16; 32768];
        link.GetPath(&mut target, std::ptr::null_mut(), 0)
            .map_err(|error| error.to_string())?;
        let mut args = [0u16; 2048];
        let _ = link.GetArguments(&mut args);
        let mut dir = [0u16; 32768];
        let _ = link.GetWorkingDirectory(&mut dir);

        let target = PathBuf::from(from_wide(&target));
        let args_text = from_wide(&args);
        let dir_text = from_wide(&dir);
        Ok(ResolvedShortcut {
            target,
            args: split_args(&args_text),
            working_dir: if dir_text.is_empty() {
                None
            } else {
                Some(PathBuf::from(dir_text))
            },
        })
    }
}

fn action_from_url_file(path: &Path) -> Option<Action> {
    let text = std::fs::read_to_string(path).ok()?;
    parse_internet_shortcut(&text).map(|url| Action::Url { url })
}

fn parse_internet_shortcut(text: &str) -> Option<String> {
    for line in text.lines() {
        let line = line.trim();
        if let Some(url) = line.strip_prefix("URL=") {
            let url = url.trim();
            if !url.is_empty() {
                return Some(url.to_string());
            }
        }
    }
    None
}

fn display_name(path: &Path) -> String {
    path.file_stem()
        .or_else(|| path.file_name())
        .map(|name| name.to_string_lossy().into_owned())
        .filter(|name| !name.is_empty())
        .unwrap_or_else(|| "Untitled".into())
}

fn is_launchable(path: &Path) -> bool {
    let ext = path
        .extension()
        .and_then(|ext| ext.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    matches!(ext.as_str(), "exe" | "bat" | "cmd" | "com" | "msc" | "cpl")
}

fn paths_equal(left: &Path, right: &Path) -> bool {
    match (std::fs::canonicalize(left), std::fs::canonicalize(right)) {
        (Ok(a), Ok(b)) => a == b,
        _ => left == right,
    }
}

fn split_args(text: &str) -> Vec<String> {
    let mut args = Vec::new();
    let mut current = String::new();
    let mut in_quotes = false;
    for ch in text.chars() {
        match ch {
            '"' => in_quotes = !in_quotes,
            c if c.is_whitespace() && !in_quotes => {
                if !current.is_empty() {
                    args.push(std::mem::take(&mut current));
                }
            }
            c => current.push(c),
        }
    }
    if !current.is_empty() {
        args.push(current);
    }
    args
}

fn wide(path: &Path) -> Vec<u16> {
    path.to_string_lossy()
        .encode_utf16()
        .chain(std::iter::once(0))
        .collect()
}

fn from_wide(buf: &[u16]) -> String {
    let end = buf.iter().position(|c| *c == 0).unwrap_or(buf.len());
    String::from_utf16_lossy(&buf[..end])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_internet_shortcut() {
        let text = "[InternetShortcut]\r\nURL=https://chatgpt.com\r\n";
        assert_eq!(
            parse_internet_shortcut(text).as_deref(),
            Some("https://chatgpt.com")
        );
    }

    #[test]
    fn splits_quoted_args() {
        assert_eq!(
            split_args(r#"--profile "User Data" --flag"#),
            ["--profile", "User Data", "--flag"]
        );
    }
}
