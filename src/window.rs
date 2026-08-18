use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::OnceLock;

use raw_window_handle::{HasWindowHandle, RawWindowHandle};
use tracing::{debug, warn};
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, WPARAM, WIN32_ERROR, ERROR_SUCCESS};
use windows::Win32::Graphics::Dwm::{
    DwmExtendFrameIntoClientArea, DwmSetWindowAttribute, DWMWA_SYSTEMBACKDROP_TYPE,
    DWMWA_USE_IMMERSIVE_DARK_MODE, DWMWA_WINDOW_CORNER_PREFERENCE, DWM_SYSTEMBACKDROP_TYPE,
    DWM_WINDOW_CORNER_PREFERENCE,
};
use windows::Win32::System::Threading::{AttachThreadInput, GetCurrentThreadId};
use windows::Win32::UI::Accessibility::SetWinEventHook;
use windows::Win32::UI::Input::KeyboardAndMouse::{
    GetAsyncKeyState, SendInput, SetFocus, INPUT, INPUT_0, INPUT_KEYBOARD, KEYBDINPUT,
    KEYEVENTF_KEYUP, VK_LBUTTON, VK_MENU, VK_RBUTTON,
};
use windows::Win32::UI::Shell::{
    DefSubclassProc, DragAcceptFiles, DragFinish, DragQueryFileW, SetWindowSubclass, HDROP,
};
use windows::Win32::UI::WindowsAndMessaging::{
    AllowSetForegroundWindow, BringWindowToTop, ChangeWindowMessageFilterEx, GetAncestor,
    GetForegroundWindow, GetWindowLongPtrW, GetWindowThreadProcessId, SetForegroundWindow,
    SetWindowLongPtrW, SetWindowPos, ShowWindow, EVENT_SYSTEM_FOREGROUND, GA_ROOT, GWL_EXSTYLE,
    HWND_TOPMOST, MSGFLT_ALLOW, SWP_FRAMECHANGED, SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOSIZE,
    SWP_SHOWWINDOW, SW_SHOW, WA_INACTIVE, WINEVENT_OUTOFCONTEXT, WM_ACTIVATE, WM_DROPFILES,
    WS_EX_ACCEPTFILES, WS_EX_TOOLWINDOW, WS_EX_TOPMOST,
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
        let updated = current
            | WS_EX_TOOLWINDOW.0 as isize
            | WS_EX_TOPMOST.0 as isize
            | WS_EX_ACCEPTFILES.0 as isize;
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
        enable_file_drop(hwnd);

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
static DROP_FILES: OnceLock<Box<dyn Fn(Vec<PathBuf>) + Send + Sync>> = OnceLock::new();
static WINEVENT_HOOKED: AtomicBool = AtomicBool::new(false);
const DROP_SUBCLASS_ID: usize = 0x534D4E31;

pub fn set_drop_callback(callback: impl Fn(Vec<PathBuf>) + Send + Sync + 'static) {
    let _ = DROP_FILES.set(Box::new(callback));
}

pub fn attach_focus_lost_handler(hwnd: HWND, callback: impl Fn() + Send + Sync + 'static) {
    let _ = FOCUS_LOST.set(Box::new(callback));
    subclass_launcher(hwnd);
    install_foreground_hook();
}

pub fn attach_drop_handler(hwnd: HWND) {
    subclass_launcher(hwnd);
    enable_file_drop(hwnd);
}

pub fn enable_file_drop(hwnd: HWND) {
    if hwnd.0.is_null() {
        return;
    }
    unsafe {
        DragAcceptFiles(hwnd, true);
        let _ = ChangeWindowMessageFilterEx(hwnd, WM_DROPFILES, MSGFLT_ALLOW, None);
        let _ = ChangeWindowMessageFilterEx(hwnd, 0x0049, MSGFLT_ALLOW, None);
        let root = GetAncestor(hwnd, GA_ROOT);
        if !root.0.is_null() && root.0 != hwnd.0 {
            DragAcceptFiles(root, true);
            let _ = ChangeWindowMessageFilterEx(root, WM_DROPFILES, MSGFLT_ALLOW, None);
            subclass_launcher(root);
        }
    }
}

fn install_foreground_hook() {
    if WINEVENT_HOOKED.swap(true, Ordering::SeqCst) {
        return;
    }
    unsafe {
        let hook = SetWinEventHook(
            EVENT_SYSTEM_FOREGROUND,
            EVENT_SYSTEM_FOREGROUND,
            None,
            Some(foreground_hook),
            0,
            0,
            WINEVENT_OUTOFCONTEXT,
        );
        if hook.is_invalid() {
            warn!("SetWinEventHook failed; falling back to window subclass and polling");
            WINEVENT_HOOKED.store(false, Ordering::SeqCst);
        }
    }
}

fn subclass_launcher(hwnd: HWND) {
    if hwnd.0.is_null() {
        return;
    }
    unsafe {
        let _ = SetWindowSubclass(hwnd, Some(launcher_subclass), DROP_SUBCLASS_ID, 0);
    }
}

unsafe extern "system" fn launcher_subclass(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
    _id: usize,
    _data: usize,
) -> LRESULT {
    if msg == WM_DROPFILES {
        notify_drop_files(HDROP(wparam.0 as *mut std::ffi::c_void));
        return LRESULT(0);
    }
    if msg == WM_ACTIVATE {
        let state = (wparam.0 as u32) & 0xffff;
        if state == WA_INACTIVE {
            notify_focus_lost();
        }
    }
    DefSubclassProc(hwnd, msg, wparam, lparam)
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
    notify_focus_lost();
}

fn notify_focus_lost() {
    if let Some(callback) = FOCUS_LOST.get() {
        callback();
    }
}

fn notify_drop_files(hdrop: HDROP) {
    let paths = unsafe { collect_drop_paths(hdrop) };
    dispatch_dropped_paths(paths);
}

pub fn dispatch_dropped_paths(paths: Vec<PathBuf>) {
    if paths.is_empty() {
        warn!("drop contained no paths");
        return;
    }
    if let Some(callback) = DROP_FILES.get() {
        callback(paths);
    } else {
        warn!("drop callback missing");
    }
}

pub fn paths_from_wparam(wparam: WPARAM) -> Vec<PathBuf> {
    unsafe { collect_drop_paths(HDROP(wparam.0 as *mut std::ffi::c_void)) }
}

unsafe fn collect_drop_paths(hdrop: HDROP) -> Vec<PathBuf> {
    let count = DragQueryFileW(hdrop, 0xFFFF_FFFF, None);
    let mut paths = Vec::with_capacity(count as usize);
    for index in 0..count {
        let needed = DragQueryFileW(hdrop, index, None) as usize;
        if needed == 0 {
            continue;
        }
        let mut buffer = vec![0u16; needed + 1];
        let written = DragQueryFileW(hdrop, index, Some(&mut buffer));
        if written == 0 {
            continue;
        }
        let end = buffer.iter().position(|c| *c == 0).unwrap_or(written as usize);
        paths.push(PathBuf::from(String::from_utf16_lossy(&buffer[..end])));
    }
    DragFinish(hdrop);
    paths
}

pub fn primary_button_down() -> bool {
    mouse_button_down(VK_LBUTTON.0 as i32)
}

pub fn pointing_button_down() -> bool {
    mouse_button_down(VK_LBUTTON.0 as i32) || mouse_button_down(VK_RBUTTON.0 as i32)
}

fn mouse_button_down(vk: i32) -> bool {
    unsafe { GetAsyncKeyState(vk) as u16 & 0x8000 != 0 }
}

pub fn force_foreground(hwnd: HWND, aggressive: bool) {
    unsafe {
        let _ = AllowSetForegroundWindow(u32::MAX);
        let _ = ShowWindow(hwnd, SW_SHOW);
        let _ = SetWindowPos(
            hwnd,
            Some(HWND_TOPMOST),
            0,
            0,
            0,
            0,
            SWP_NOMOVE | SWP_NOSIZE | SWP_SHOWWINDOW,
        );

        let foreground = GetForegroundWindow();
        let mut foreground_pid = 0;
        let foreground_tid = GetWindowThreadProcessId(foreground, Some(&mut foreground_pid));
        let current_tid = GetCurrentThreadId();
        let attached = foreground_tid != 0 && foreground_tid != current_tid;
        if attached {
            let _ = AttachThreadInput(foreground_tid, current_tid, true);
        }

        if !SetForegroundWindow(hwnd).as_bool() {
            if aggressive {
                pulse_alt_key();
            }
            if !SetForegroundWindow(hwnd).as_bool() {
                warn!("SetForegroundWindow failed");
            }
        }
        let _ = BringWindowToTop(hwnd);
        let _ = SetFocus(Some(hwnd));

        if attached {
            let _ = AttachThreadInput(foreground_tid, current_tid, false);
        }
    }
}

fn pulse_alt_key() {
    let mut inputs = [
        INPUT {
            r#type: INPUT_KEYBOARD,
            Anonymous: INPUT_0 {
                ki: KEYBDINPUT {
                    wVk: VK_MENU,
                    wScan: 0,
                    dwFlags: Default::default(),
                    time: 0,
                    dwExtraInfo: 0,
                },
            },
        },
        INPUT {
            r#type: INPUT_KEYBOARD,
            Anonymous: INPUT_0 {
                ki: KEYBDINPUT {
                    wVk: VK_MENU,
                    wScan: 0,
                    dwFlags: KEYEVENTF_KEYUP,
                    time: 0,
                    dwExtraInfo: 0,
                },
            },
        },
    ];
    unsafe {
        SendInput(&mut inputs, std::mem::size_of::<INPUT>() as i32);
    }
}

pub fn is_our_hwnd(hwnd: HWND, ours: &[HWND]) -> bool {
    if hwnd.0.is_null() {
        return false;
    }
    unsafe {
        let fg_root = GetAncestor(hwnd, GA_ROOT);
        ours.iter().any(|candidate| {
            if candidate.0.is_null() {
                return false;
            }
            let our_root = GetAncestor(*candidate, GA_ROOT);
            candidate.0 == hwnd.0
                || candidate.0 == fg_root.0
                || our_root.0 == hwnd.0
                || (!our_root.0.is_null() && our_root.0 == fg_root.0)
        })
    }
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
