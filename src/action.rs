use std::os::windows::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use tracing::{error, info};
use windows::core::PCWSTR;
use windows::Win32::System::Threading::CREATE_NO_WINDOW;
use windows::Win32::UI::Shell::ShellExecuteW;
use windows::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;

use crate::config::expand_path;
use crate::model::Action;

const CREATE_NEW_CONSOLE: u32 = 0x00000010;

pub fn execute(action: &Action) -> Result<(), String> {
    match action {
        Action::App {
            path,
            args,
            working_dir,
        } => {
            info!(path = %path.display(), "launch app");
            shell_open(path, args, working_dir.as_deref())
        }
        Action::File { path } => {
            info!(path = %path.display(), "open file");
            shell_open(path, &[], None)
        }
        Action::Directory { path } => {
            info!(path = %path.display(), "open directory");
            shell_open(path, &[], None)
        }
        Action::Url { url } => {
            info!(url, "open url");
            shell_open_str(url, &[], None)
        }
        Action::Command {
            command,
            args,
            working_dir,
        } => {
            info!(command, "run command");
            spawn_command(command, args, working_dir.as_deref(), false)
        }
        Action::PowerShell { script } => {
            info!("run powershell");
            run_powershell(script)
        }
    }
}

fn shell_open(path: &Path, args: &[String], working_dir: Option<&Path>) -> Result<(), String> {
    let expanded = expand_path(path);
    shell_open_str(&expanded.to_string_lossy(), args, working_dir)
}

fn shell_open_str(target: &str, args: &[String], working_dir: Option<&Path>) -> Result<(), String> {
    let file = wide(target);
    let params = if args.is_empty() {
        None
    } else {
        Some(wide(&join_args(args)))
    };
    let dir = working_dir.map(|path| wide(&expand_path(path).to_string_lossy()));
    let verb = wide("open");

    let result = unsafe {
        ShellExecuteW(
            None,
            PCWSTR(verb.as_ptr()),
            PCWSTR(file.as_ptr()),
            params
                .as_ref()
                .map(|value| PCWSTR(value.as_ptr()))
                .unwrap_or_else(|| PCWSTR::null()),
            dir.as_ref()
                .map(|value| PCWSTR(value.as_ptr()))
                .unwrap_or_else(|| PCWSTR::null()),
            SW_SHOWNORMAL,
        )
    };

    let code = result.0 as usize;
    if code <= 32 {
        let message = format!("Windows Shell could not open '{target}' (code {code})");
        error!(code, target, "action failed");
        Err(message)
    } else {
        Ok(())
    }
}

fn spawn_command(
    command: &str,
    args: &[String],
    working_dir: Option<&Path>,
    hidden: bool,
) -> Result<(), String> {
    let mut cmd = Command::new(command);
    cmd.args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    if let Some(dir) = working_dir {
        cmd.current_dir(expand_path(dir));
    }
    if hidden {
        cmd.creation_flags(CREATE_NO_WINDOW.0);
    } else {
        cmd.creation_flags(CREATE_NEW_CONSOLE);
    }

    match cmd.spawn() {
        Ok(mut child) => {
            std::thread::spawn(move || {
                let _ = child.wait();
            });
            Ok(())
        }
        Err(error) => {
            error!(command, %error, "command spawn failed");
            Err(format!("Failed to run '{command}': {error}"))
        }
    }
}

fn run_powershell(script: &str) -> Result<(), String> {
    let trimmed = script.trim();
    let path = PathBuf::from(trimmed);
    let mut args = vec![
        "-NoProfile".into(),
        "-ExecutionPolicy".into(),
        "Bypass".into(),
    ];
    if path.extension().and_then(|e| e.to_str()) == Some("ps1") && path.exists() {
        args.push("-File".into());
        args.push(path.to_string_lossy().into_owned());
    } else {
        args.push("-Command".into());
        args.push(trimmed.to_string());
    }
    spawn_command("powershell.exe", &args, None, false)
}

fn join_args(args: &[String]) -> String {
    args.iter()
        .map(|arg| quote_arg(arg))
        .collect::<Vec<_>>()
        .join(" ")
}

fn quote_arg(arg: &str) -> String {
    if arg.contains([' ', '"']) {
        format!("\"{}\"", arg.replace('"', "\\\""))
    } else {
        arg.to_string()
    }
}

fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(std::iter::once(0)).collect()
}
