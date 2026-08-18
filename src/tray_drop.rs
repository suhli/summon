use std::sync::atomic::{AtomicBool, AtomicIsize, Ordering};

use tracing::{debug, warn};
use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::{COLORREF, HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::{CreateSolidBrush, HBRUSH};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Accessibility::SetWinEventHook;
use windows::Win32::UI::Shell::{DragAcceptFiles, Shell_NotifyIconGetRect, NOTIFYICONIDENTIFIER};
use windows::Win32::UI::WindowsAndMessaging::{
    ChangeWindowMessageFilterEx, CreateWindowExW, DefWindowProcW, FindWindowExW, FindWindowW,
    GetWindowRect, GetCursorPos, KillTimer, MoveWindow, RegisterClassW, SetLayeredWindowAttributes, SetTimer,
    SetWindowPos, ShowWindow, EVENT_SYSTEM_DRAGDROPEND, EVENT_SYSTEM_DRAGDROPSTART, HWND_MESSAGE,
    HWND_TOPMOST, LWA_ALPHA, MSGFLT_ALLOW, SWP_NOACTIVATE, SWP_SHOWWINDOW, SW_HIDE,
    SW_SHOWNOACTIVATE, WINEVENT_OUTOFCONTEXT, WM_DESTROY, WM_DROPFILES, WM_TIMER, WNDCLASSW,
    WS_EX_LAYERED, WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_POPUP,
};

use crate::window;

/// Must match Slint's `TRAY_UID` in `i-slint-core` system tray backend.
const SLINT_TRAY_UID: u32 = 1;
const CLASS_NAME: PCWSTR = w!("SummonTrayDropTarget");
const SLINT_TRAY_CLASS: PCWSTR = w!("SlintSystemTrayWindow");
const TIMER_TRACK: usize = 1;
const TIMER_HIDE: usize = 2;
const TIMER_WATCH: usize = 3;

static OVERLAY: AtomicIsize = AtomicIsize::new(0);
static DRAGGING: AtomicBool = AtomicBool::new(false);
static BUTTON_WAS_DOWN: AtomicBool = AtomicBool::new(false);
static PRESS_STARTED_ON_TRAY: AtomicBool = AtomicBool::new(false);

pub fn install() {
    if OVERLAY.load(Ordering::SeqCst) != 0 {
        return;
    }
    match create_overlay() {
        Ok(hwnd) => {
            OVERLAY.store(hwnd.0 as isize, Ordering::SeqCst);
            install_drag_hook();
            debug!("tray drop target ready");
        }
        Err(error) => warn!(%error, "tray drop target unavailable"),
    }
}

fn create_overlay() -> Result<HWND, String> {
    unsafe {
        let hinstance = GetModuleHandleW(None).map_err(|error| error.to_string())?;
        let brush = CreateSolidBrush(COLORREF(0x00F0F0F0));
        let class = WNDCLASSW {
            lpfnWndProc: Some(overlay_wndproc),
            hInstance: hinstance.into(),
            lpszClassName: CLASS_NAME,
            hbrBackground: HBRUSH(brush.0),
            ..Default::default()
        };
        let _ = RegisterClassW(&class);

        let hwnd = CreateWindowExW(
            WS_EX_LAYERED | WS_EX_TOOLWINDOW | WS_EX_TOPMOST | WS_EX_NOACTIVATE,
            CLASS_NAME,
            w!(""),
            WS_POPUP,
            0,
            0,
            0,
            0,
            None,
            None,
            Some(hinstance.into()),
            None,
        )
        .map_err(|error| error.to_string())?;

        let _ = SetLayeredWindowAttributes(hwnd, COLORREF(0), 72, LWA_ALPHA);
        DragAcceptFiles(hwnd, true);
        let _ = ChangeWindowMessageFilterEx(hwnd, WM_DROPFILES, MSGFLT_ALLOW, None);
        let _ = ChangeWindowMessageFilterEx(hwnd, 0x0049, MSGFLT_ALLOW, None);
        let _ = SetTimer(Some(hwnd), TIMER_WATCH, 50, None);
        Ok(hwnd)
    }
}

fn install_drag_hook() {
    unsafe {
        let hook = SetWinEventHook(
            EVENT_SYSTEM_DRAGDROPSTART,
            EVENT_SYSTEM_DRAGDROPEND,
            None,
            Some(drag_hook),
            0,
            0,
            WINEVENT_OUTOFCONTEXT,
        );
        if hook.is_invalid() {
            warn!("tray drag hook failed");
        }
    }
}

unsafe extern "system" fn drag_hook(
    _hook: windows::Win32::UI::Accessibility::HWINEVENTHOOK,
    event: u32,
    _hwnd: HWND,
    _id_object: i32,
    _id_child: i32,
    _thread: u32,
    _time: u32,
) {
    if event == EVENT_SYSTEM_DRAGDROPSTART {
        let _ = slint::invoke_from_event_loop(on_drag_start);
    } else if event == EVENT_SYSTEM_DRAGDROPEND {
        let _ = slint::invoke_from_event_loop(on_drag_end);
    }
}

fn on_drag_start() {
    DRAGGING.store(true, Ordering::SeqCst);
    let hwnd = overlay_hwnd();
    if hwnd.0.is_null() {
        return;
    }
    unsafe {
        let _ = KillTimer(Some(hwnd), TIMER_HIDE);
        let _ = SetTimer(Some(hwnd), TIMER_TRACK, 50, None);
    }
    show_over_tray();
}

fn on_drag_end() {
    DRAGGING.store(false, Ordering::SeqCst);
    let hwnd = overlay_hwnd();
    if hwnd.0.is_null() {
        return;
    }
    unsafe {
        let _ = KillTimer(Some(hwnd), TIMER_TRACK);
        let _ = SetTimer(Some(hwnd), TIMER_HIDE, 250, None);
    }
}

fn show_over_tray() {
    let hwnd = overlay_hwnd();
    if hwnd.0.is_null() {
        return;
    }
    let Some(rect) = drop_rect() else {
        hide_overlay();
        return;
    };
    unsafe {
        let _ = MoveWindow(
            hwnd,
            rect.left,
            rect.top,
            (rect.right - rect.left).max(24),
            (rect.bottom - rect.top).max(24),
            true,
        );
        let _ = SetWindowPos(
            hwnd,
            Some(HWND_TOPMOST),
            0,
            0,
            0,
            0,
            SWP_NOACTIVATE | SWP_SHOWWINDOW,
        );
        let _ = ShowWindow(hwnd, SW_SHOWNOACTIVATE);
        DragAcceptFiles(hwnd, true);
    }
}

fn hide_overlay() {
    let hwnd = overlay_hwnd();
    if hwnd.0.is_null() {
        return;
    }
    unsafe {
        let _ = KillTimer(Some(hwnd), TIMER_TRACK);
        let _ = KillTimer(Some(hwnd), TIMER_HIDE);
        let _ = ShowWindow(hwnd, SW_HIDE);
    }
}

fn drop_rect() -> Option<RECT> {
    icon_rect().or_else(notification_area_rect).map(inflate)
}

fn icon_rect() -> Option<RECT> {
    let tray = slint_tray_hwnd()?;
    let identifier = NOTIFYICONIDENTIFIER {
        cbSize: std::mem::size_of::<NOTIFYICONIDENTIFIER>() as u32,
        hWnd: tray,
        uID: SLINT_TRAY_UID,
        guidItem: windows::core::GUID::zeroed(),
    };
    let rect = unsafe { Shell_NotifyIconGetRect(&identifier) }.ok()?;
    if rect.right <= rect.left || rect.bottom <= rect.top {
        return None;
    }
    Some(rect)
}

fn slint_tray_hwnd() -> Option<HWND> {
    unsafe { FindWindowExW(Some(HWND_MESSAGE), None, SLINT_TRAY_CLASS, PCWSTR::null()).ok() }
}

fn notification_area_rect() -> Option<RECT> {
    unsafe {
        let tray = FindWindowW(w!("Shell_TrayWnd"), PCWSTR::null()).ok()?;
        let notify = FindWindowExW(Some(tray), None, w!("TrayNotifyWnd"), PCWSTR::null())
            .ok()
            .or(Some(tray))?;
        let mut rect = RECT::default();
        GetWindowRect(notify, &mut rect).ok()?;
        if rect.right <= rect.left {
            return None;
        }
        Some(rect)
    }
}

fn inflate(mut rect: RECT) -> RECT {
    rect.left -= 8;
    rect.top -= 8;
    rect.right += 8;
    rect.bottom += 8;
    rect
}

fn overlay_hwnd() -> HWND {
    HWND(OVERLAY.load(Ordering::SeqCst) as *mut std::ffi::c_void)
}

fn poll_drag_onto_tray() {
    let down = window::primary_button_down();
    let over = cursor_over_drop_rect();
    let was_down = BUTTON_WAS_DOWN.swap(down, Ordering::SeqCst);

    if down && !was_down {
        PRESS_STARTED_ON_TRAY.store(over, Ordering::SeqCst);
    }
    if !down {
        PRESS_STARTED_ON_TRAY.store(false, Ordering::SeqCst);
    }

    let started_on_tray = PRESS_STARTED_ON_TRAY.load(Ordering::SeqCst);
    let ole_dragging = DRAGGING.load(Ordering::SeqCst);

    if down && over && !started_on_tray {
        show_over_tray();
    } else if !ole_dragging && (!down || !over) {
        hide_overlay();
    }
}

fn cursor_over_drop_rect() -> bool {
    let Some(rect) = drop_rect() else {
        return false;
    };
    let mut point = POINT::default();
    if unsafe { GetCursorPos(&mut point) }.is_err() {
        return false;
    }
    point.x >= rect.left && point.x < rect.right && point.y >= rect.top && point.y < rect.bottom
}

unsafe extern "system" fn overlay_wndproc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    match msg {
        WM_DROPFILES => {
            let paths = window::paths_from_wparam(wparam);
            hide_overlay();
            DRAGGING.store(false, Ordering::SeqCst);
            window::dispatch_dropped_paths(paths);
            LRESULT(0)
        }
        WM_TIMER => {
            if wparam.0 == TIMER_WATCH {
                poll_drag_onto_tray();
            } else if wparam.0 == TIMER_TRACK {
                if DRAGGING.load(Ordering::SeqCst) {
                    show_over_tray();
                }
            } else if wparam.0 == TIMER_HIDE {
                hide_overlay();
            }
            LRESULT(0)
        }
        WM_DESTROY => LRESULT(0),
        _ => DefWindowProcW(hwnd, msg, wparam, lparam),
    }
}
