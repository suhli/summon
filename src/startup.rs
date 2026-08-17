use std::path::PathBuf;

use tracing::{error, info};
use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::{ERROR_FILE_NOT_FOUND, ERROR_SUCCESS, WIN32_ERROR};
use windows::Win32::System::Registry::{
    RegCloseKey, RegCreateKeyExW, RegDeleteValueW, RegQueryValueExW, RegSetValueExW, HKEY,
    HKEY_CURRENT_USER, KEY_READ, KEY_WRITE, REG_OPTION_NON_VOLATILE, REG_SZ,
};

const RUN_SUBKEY: PCWSTR = w!("Software\\Microsoft\\Windows\\CurrentVersion\\Run");
const VALUE_NAME: PCWSTR = w!("Summon");

pub fn is_enabled() -> bool {
    match current_exe() {
        Ok(exe) => read_run_value().is_some_and(|value| paths_match(&value, &exe)),
        Err(_) => read_run_value().is_some(),
    }
}

pub fn set_enabled(enabled: bool) -> Result<(), String> {
    if enabled {
        let exe = current_exe()?;
        let quoted = format!("\"{}\"", exe.display());
        write_run_value(&quoted)?;
        info!(path = %exe.display(), "startup enabled");
    } else {
        delete_run_value()?;
        info!("startup disabled");
    }
    Ok(())
}

fn current_exe() -> Result<PathBuf, String> {
    std::env::current_exe().map_err(|error| format!("Failed to resolve executable: {error}"))
}

fn paths_match(stored: &str, exe: &std::path::Path) -> bool {
    let trimmed = stored.trim().trim_matches('"');
    PathBuf::from(trimmed) == exe
}

fn ok(status: WIN32_ERROR) -> bool {
    status == ERROR_SUCCESS
}

fn read_run_value() -> Option<String> {
    unsafe {
        let mut key = HKEY::default();
        if !ok(RegCreateKeyExW(
            HKEY_CURRENT_USER,
            RUN_SUBKEY,
            None,
            None,
            REG_OPTION_NON_VOLATILE,
            KEY_READ,
            None,
            &mut key,
            None,
        )) {
            return None;
        }
        let mut size = 0u32;
        let query = RegQueryValueExW(key, VALUE_NAME, None, None, None, Some(&mut size));
        if !ok(query) || size == 0 {
            let _ = RegCloseKey(key);
            return None;
        }
        let mut buffer = vec![0u16; (size as usize / 2).max(1)];
        let mut bytes = size;
        let status = RegQueryValueExW(
            key,
            VALUE_NAME,
            None,
            None,
            Some(buffer.as_mut_ptr() as *mut u8),
            Some(&mut bytes),
        );
        let _ = RegCloseKey(key);
        if !ok(status) {
            return None;
        }
        Some(String::from_utf16_lossy(
            buffer.split(|c| *c == 0).next().unwrap_or(&[]),
        ))
    }
}

fn write_run_value(value: &str) -> Result<(), String> {
    unsafe {
        let mut key = HKEY::default();
        let status = RegCreateKeyExW(
            HKEY_CURRENT_USER,
            RUN_SUBKEY,
            None,
            None,
            REG_OPTION_NON_VOLATILE,
            KEY_WRITE,
            None,
            &mut key,
            None,
        );
        if !ok(status) {
            error!(?status, "startup registry open failed");
            return Err(format!("Failed to open startup registry key: {status:?}"));
        }
        let wide: Vec<u16> = value.encode_utf16().chain(std::iter::once(0)).collect();
        let bytes = std::slice::from_raw_parts(wide.as_ptr() as *const u8, wide.len() * 2);
        let result = RegSetValueExW(key, VALUE_NAME, None, REG_SZ, Some(bytes));
        let _ = RegCloseKey(key);
        if !ok(result) {
            error!(?result, "startup registry write failed");
            return Err(format!("Failed to write startup value: {result:?}"));
        }
        Ok(())
    }
}

fn delete_run_value() -> Result<(), String> {
    unsafe {
        let mut key = HKEY::default();
        let status = RegCreateKeyExW(
            HKEY_CURRENT_USER,
            RUN_SUBKEY,
            None,
            None,
            REG_OPTION_NON_VOLATILE,
            KEY_WRITE,
            None,
            &mut key,
            None,
        );
        if !ok(status) {
            return Err(format!("Failed to open startup registry key: {status:?}"));
        }
        let result = RegDeleteValueW(key, VALUE_NAME);
        let _ = RegCloseKey(key);
        if result == ERROR_SUCCESS || result == ERROR_FILE_NOT_FOUND {
            Ok(())
        } else {
            error!(?result, "startup registry delete failed");
            Err(format!("Failed to remove startup value: {result:?}"))
        }
    }
}
