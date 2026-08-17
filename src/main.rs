#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod action;
mod app;
mod config;
mod dialog;
mod hotkey;
mod icon;
mod model;
mod monitor;
mod nav;
mod search;
mod startup;
mod window;

use std::fs::OpenOptions;
use std::sync::atomic::{AtomicIsize, Ordering};
use std::sync::Mutex;

use tracing::{error, info};
use tracing_subscriber::fmt::writer::MakeWriterExt;
use tracing_subscriber::EnvFilter;
use windows::core::PCWSTR;
use windows::Win32::Foundation::{GetLastError, ERROR_ALREADY_EXISTS};
use windows::Win32::System::Threading::CreateMutexW;

static INSTANCE_MUTEX: AtomicIsize = AtomicIsize::new(0);

fn main() {
    if let Err(error) = config::ensure_config_dir() {
        eprintln!("{error}");
    }
    init_tracing();

    if !acquire_single_instance() {
        error!("Summon is already running");
        return;
    }

    info!("process start");
    if let Err(error) = app::run() {
        error!(%error, "application error");
    }
}

fn init_tracing() {
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("summon=info"));
    let file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(config::log_path())
        .ok();

    let subscriber = tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_target(false)
        .with_ansi(cfg!(debug_assertions));

    if let Some(file) = file {
        let writer = Mutex::new(file).and(std::io::stderr);
        let _ = subscriber.with_writer(writer).try_init();
    } else {
        let _ = subscriber.with_writer(std::io::stderr).try_init();
    }
}

fn acquire_single_instance() -> bool {
    let name = wide("Local\\Summon.Launcher.SingleInstance");
    unsafe {
        match CreateMutexW(None, true, PCWSTR(name.as_ptr())) {
            Ok(handle) => {
                let already = GetLastError() == ERROR_ALREADY_EXISTS;
                INSTANCE_MUTEX.store(handle.0 as isize, Ordering::SeqCst);
                !already
            }
            Err(_) => true,
        }
    }
}

fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(std::iter::once(0)).collect()
}
