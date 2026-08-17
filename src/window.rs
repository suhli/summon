use std::sync::OnceLock;

use raw_window_handle::{HasWindowHandle, RawWindowHandle};
use tracing::{debug, warn};
use windows::Win32::Foundation::{HWND, LPARAM, WPARAM, WIN32_ERROR, ERROR_SUCCESS};
use windows::Win32::Graphics::Dwm::{
    DwmExtendFrameIntoClientArea, DwmSetWindowAttribute, DWMWA_SYSTEMBACKDROP_TYPE,
    DWMWA_USE_IMMERSIVE_DARK_MODE, DWMWA_WINDOW_CORNER_PREFERENCE, DWM_SYSTEMBACKDROP_TYPE,
    DWM_WINDOW_CORNER_PREFERENCE,
};
use windows::Win32::System::Threading::{AttachThreadInput, GetCurrentThreadId};
use windows::Win32::UI::Accessibility::SetWinEventHook;
use windows::Win32::UI::Input::KeyboardAndMouse::SetFocus;
use windows::Win32::UI::WindowsAndMessaging::{
    BringWindowToTop, GetForegroundWindow, GetWindowLongPtrW, GetWindowThreadProcessId,
    SetForegroundWindow, SetWindowLongPtrW, SetWindowPos, EVENT_SYSTEM_FOREGROUND, GWL_EXSTYLE,
    HWND_TOPMOST, SWP_FRAMECHANGED, SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOSIZE, WINEVENT_OUTOFCONTEXT,
    WS_EX_TOOLWINDOW, WS_EX_TOPMOST,
};

const DWMWCP_ROUND: DWM_WINDOW_CORNER_PREFERENCE = DWM_WINDOW_CORNER_PREFERENCE(2);
const DWMSBT_TRANSIENTWINDOW: DWM_SYSTEMBACKDROP_TYPE = DWM_SYSTEMBACKDROP_TYPE(3);

#[repr(C)]
struct Margins {
    cx_left_width: i32,
    cx_right_width: i32,
    cy_top_height: i32,
    cy_bottom_height: i32,
}

pub fn hwnd_from_slint(window: &slint::Window) -> Option<HWND> {
    let wrapper = window.window_handle();
    let handle = wrapper.window_handle().ok()?;
    match handle.as_raw() {
        RawWindowHandle::Win32(win32) => Some(HWND(win32.hwnd.get() as *mut std::ffi::c_void)),
        _ => None,
    }
}

pub fn apply_launcher_style(hwnd: HWND, dark_mode: bool) {
    unsafe {
        let current = GetWindowLongPtrW(hwnd, GWL_EXSTYLE);
        let updated = current | WS_EX_TOOLWINDOW.0 as isize | WS_EX_TOPMOST.0 as isize;
        SetWindowLongPtrW(hwnd, GWL_EXSTYLE, updated);
        let _ = SetWindowPos(
            hwnd,
            Some(HWND_TOPMOST),
            0,
            0,
            0,
            0,
            SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE | SWP_FRAMECHANGED,
        );

        let corner = DWMWCP_ROUND;
        if let Err(error) = DwmSetWindowAttribute(
            hwnd,
            DWMWA_WINDOW_CORNER_PREFERENCE,
            &corner as *const _ as *const _,
            std::mem::size_of_val(&corner) as u32,
        ) {
            debug!(%error, "rounded corners unavailable");
        }

        let dark = i32::from(dark_mode);
        let _ = DwmSetWindowAttribute(
            hwnd,
            DWMWA_USE_IMMERSIVE_DARK_MODE,
            &dark as *const _ as *const _,
            std::mem::size_of_val(&dark) as u32,
        );

        if is_windows_11() {
            let backdrop = DWMSBT_TRANSIENTWINDOW;
            if let Err(error) = DwmSetWindowAttribute(
                hwnd,
                DWMWA_SYSTEMBACKDROP_TYPE,
                &backdrop as *const _ as *const _,
                std::mem::size_of_val(&backdrop) as u32,
            ) {
                debug!(%error, "system backdrop unavailable");
            }
            let margins = Margins {
                cx_left_width: -1,
                cx_right_width: -1,
                cy_top_height: -1,
                cy_bottom_height: -1,
            };
            let _ = DwmExtendFrameIntoClientArea(hwnd, &margins as *const Margins as *const _);
        } else {
            let margins = Margins {
                cx_left_width: 0,
                cx_right_width: 0,
                cy_top_height: 0,
                cy_bottom_height: 1,
            };
            let _ = DwmExtendFrameIntoClientArea(hwnd, &margins as *const Margins as *const _);
        }
    }
}

static FOCUS_LOST: OnceLock<Box<dyn Fn() + Send + Sync>> = OnceLock::new();

pub fn attach_focus_lost_handler(_hwnd: HWND, callback: impl Fn() + Send + Sync + 'static) {
    let _ = FOCUS_LOST.set(Box::new(callback));
    unsafe {
        let _ = SetWinEventHook(
            EVENT_SYSTEM_FOREGROUND,
            EVENT_SYSTEM_FOREGROUND,
            None,
            Some(foreground_hook),
            0,
            0,
            WINEVENT_OUTOFCONTEXT,
        );
    }
}

unsafe extern "system" fn foreground_hook(
    _hook: windows::Win32::UI::Accessibility::HWINEVENTHOOK,
    _event: u32,
    _hwnd: HWND,
    _id_object: i32,
    _id_child: i32,
    _thread: u32,
    _time: u32,
) {
    if let Some(callback) = FOCUS_LOST.get() {
        callback();
    }
}

pub fn force_foreground(hwnd: HWND) {
    unsafe {
        let foreground = GetForegroundWindow();
        let mut foreground_pid = 0;
        let foreground_tid = GetWindowThreadProcessId(foreground, Some(&mut foreground_pid));
        let current_tid = GetCurrentThreadId();

        if foreground_tid != 0 && foreground_tid != current_tid {
            let _ = AttachThreadInput(foreground_tid, current_tid, true);
            if !SetForegroundWindow(hwnd).as_bool() {
                warn!("SetForegroundWindow failed");
            }
            let _ = BringWindowToTop(hwnd);
            let _ = AttachThreadInput(foreground_tid, current_tid, false);
        } else if !SetForegroundWindow(hwnd).as_bool() {
            let _ = BringWindowToTop(hwnd);
        }

        let _ = SetFocus(Some(hwnd));
    }
}

pub fn is_our_hwnd(hwnd: HWND, ours: &[HWND]) -> bool {
    if hwnd.0.is_null() {
        return false;
    }
    ours.iter().any(|candidate| candidate.0 == hwnd.0)
}

pub fn current_foreground() -> HWND {
    unsafe { GetForegroundWindow() }
}

pub fn is_windows_11() -> bool {
    windows_build_number() >= 22000
}

pub fn windows_build_number() -> u32 {
    use windows::Win32::System::Registry::{
        RegGetValueW, HKEY_LOCAL_MACHINE, RRF_RT_REG_SZ,
    };

    let subkey = wide(r"SOFTWARE\Microsoft\Windows NT\CurrentVersion");
    let value = wide("CurrentBuildNumber");
    let mut buffer = [0u16; 32];
    let mut size = (buffer.len() * 2) as u32;
    unsafe {
        if ok_status(RegGetValueW(
            HKEY_LOCAL_MACHINE,
            windows::core::PCWSTR(subkey.as_ptr()),
            windows::core::PCWSTR(value.as_ptr()),
            RRF_RT_REG_SZ,
            None,
            Some(buffer.as_mut_ptr() as *mut _),
            Some(&mut size),
        )) {
            let text = String::from_utf16_lossy(buffer.split(|c| *c == 0).next().unwrap_or(&[]));
            if let Ok(build) = text.parse() {
                return build;
            }
        }
    }
    0
}

pub fn apps_use_dark_mode() -> bool {
    use windows::Win32::System::Registry::{
        RegGetValueW, HKEY_CURRENT_USER, RRF_RT_REG_DWORD,
    };

    let subkey = wide(r"Software\Microsoft\Windows\CurrentVersion\Themes\Personalize");
    let value = wide("AppsUseLightTheme");
    let mut data: u32 = 1;
    let mut size = std::mem::size_of::<u32>() as u32;
    unsafe {
        let result = RegGetValueW(
            HKEY_CURRENT_USER,
            windows::core::PCWSTR(subkey.as_ptr()),
            windows::core::PCWSTR(value.as_ptr()),
            RRF_RT_REG_DWORD,
            None,
            Some(&mut data as *mut u32 as *mut _),
            Some(&mut size),
        );
        if !ok_status(result) {
            return false;
        }
    }
    data == 0
}

fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(std::iter::once(0)).collect()
}

fn ok_status(status: WIN32_ERROR) -> bool {
    status == ERROR_SUCCESS
}

#[allow(dead_code)]
fn _unused(_w: WPARAM, _l: LPARAM) {}
