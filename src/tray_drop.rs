use std::sync::atomic::{AtomicBool, AtomicIsize, Ordering};

use tracing::{debug, warn};
use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::{COLORREF, HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::{
    BeginPaint, CreateSolidBrush, DrawTextW, EndPaint, FillRect, GetStockObject, SelectObject,
    SetBkMode, SetTextColor, DEFAULT_GUI_FONT, DT_CENTER, DT_SINGLELINE, DT_VCENTER, HBRUSH,
    PAINTSTRUCT, TRANSPARENT,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Accessibility::SetWinEventHook;
use windows::Win32::UI::Shell::{DragAcceptFiles, Shell_NotifyIconGetRect, NOTIFYICONIDENTIFIER};
use windows::Win32::UI::WindowsAndMessaging::{
    ChangeWindowMessageFilterEx, CreateWindowExW, DefWindowProcW, FindWindowExW, FindWindowW,
    GetClientRect, GetCursorPos, GetWindowRect, IsWindowVisible, MoveWindow, RegisterClassW,
    SetTimer, SetWindowPos, ShowWindow, CS_HREDRAW, CS_VREDRAW, EVENT_SYSTEM_DRAGDROPEND,
    EVENT_SYSTEM_DRAGDROPSTART, HWND_MESSAGE, HWND_TOPMOST, MSGFLT_ALLOW, SWP_NOACTIVATE,
    SWP_SHOWWINDOW, SW_HIDE, SW_SHOWNOACTIVATE, WINEVENT_OUTOFCONTEXT, WM_DESTROY, WM_DROPFILES,
    WM_ERASEBKGND, WM_PAINT, WM_TIMER, WNDCLASSW, WS_EX_ACCEPTFILES, WS_EX_NOACTIVATE,
    WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_POPUP,
};

use crate::window;

/// Must match Slint's `TRAY_UID` in `i-slint-core` system tray backend.
const SLINT_TRAY_UID: u32 = 1;
const CLASS_NAME: PCWSTR = w!("SummonTrayDropTarget");
const SLINT_TRAY_CLASS: PCWSTR = w!("SlintSystemTrayWindow");
const TIMER_WATCH: usize = 1;
const CHIP_W: i32 = 280;
const CHIP_H: i32 = 64;

static OVERLAY: AtomicIsize = AtomicIsize::new(0);
static LAUNCHER: AtomicIsize = AtomicIsize::new(0);
static DRAGGING: AtomicBool = AtomicBool::new(false);
static BUTTON_WAS_DOWN: AtomicBool = AtomicBool::new(false);
static PRESS_ORIGIN_X: AtomicIsize = AtomicIsize::new(0);
static PRESS_ORIGIN_Y: AtomicIsize = AtomicIsize::new(0);

pub fn install() {
    if OVERLAY.load(Ordering::SeqCst) != 0 {
        return;
    }
    match create_overlay() {
        Ok(hwnd) => {
            OVERLAY.store(hwnd.0 as isize, Ordering::SeqCst);
            install_drag_hook();
            debug!("drop target ready");
        }
        Err(error) => warn!(%error, "drop target unavailable"),
    }
}

pub fn set_launcher_hwnd(hwnd: HWND) {
    LAUNCHER.store(hwnd.0 as isize, Ordering::SeqCst);
}

fn create_overlay() -> Result<HWND, String> {
    unsafe {
        let hinstance = GetModuleHandleW(None).map_err(|error| error.to_string())?;
        let brush = CreateSolidBrush(COLORREF(0x002C2C2C));
        let class = WNDCLASSW {
            style: CS_HREDRAW | CS_VREDRAW,
            lpfnWndProc: Some(overlay_wndproc),
            hInstance: hinstance.into(),
            lpszClassName: CLASS_NAME,
            hbrBackground: HBRUSH(brush.0),
            ..Default::default()
        };
        let _ = RegisterClassW(&class);

        let hwnd = CreateWindowExW(
            WS_EX_TOOLWINDOW | WS_EX_TOPMOST | WS_EX_NOACTIVATE | WS_EX_ACCEPTFILES,
            CLASS_NAME,
            w!("Drop to add"),
            WS_POPUP,
            0,
            0,
            CHIP_W,
            CHIP_H,
            None,
            None,
            Some(hinstance.into()),
            None,
        )
        .map_err(|error| error.to_string())?;

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
            warn!("drag hook failed");
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
        let _ = slint::invoke_from_event_loop(|| {
            DRAGGING.store(true, Ordering::SeqCst);
            show_drop_target();
        });
    } else if event == EVENT_SYSTEM_DRAGDROPEND {
        let _ = slint::invoke_from_event_loop(|| {
            DRAGGING.store(false, Ordering::SeqCst);
            hide_overlay();
        });
    }
}

fn show_drop_target() {
    if launcher_rect().is_some() {
        hide_overlay();
        return;
    }
    let hwnd = overlay_hwnd();
    if hwnd.0.is_null() {
        return;
    }
    let rect = target_rect();
    unsafe {
        let _ = MoveWindow(
            hwnd,
            rect.left,
            rect.top,
            (rect.right - rect.left).max(CHIP_W),
            (rect.bottom - rect.top).max(CHIP_H),
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

pub fn hide() {
    hide_overlay();
}

fn hide_overlay() {
    let hwnd = overlay_hwnd();
    if hwnd.0.is_null() {
        return;
    }
    unsafe {
        let _ = ShowWindow(hwnd, SW_HIDE);
    }
}

fn target_rect() -> RECT {
    chip_above_tray().unwrap_or(RECT {
        left: 40,
        top: 40,
        right: 40 + CHIP_W,
        bottom: 40 + CHIP_H,
    })
}

fn launcher_rect() -> Option<RECT> {
    let hwnd = HWND(LAUNCHER.load(Ordering::SeqCst) as *mut std::ffi::c_void);
    if hwnd.0.is_null() {
        return None;
    }
    unsafe {
        if !IsWindowVisible(hwnd).as_bool() {
            return None;
        }
        let mut rect = RECT::default();
        GetWindowRect(hwnd, &mut rect).ok()?;
        if rect.right - rect.left < 80 {
            return None;
        }
        Some(rect)
    }
}

fn chip_above_tray() -> Option<RECT> {
    let icon = icon_rect().or_else(notification_area_rect)?;
    let mid_x = icon.left + (icon.right - icon.left) / 2;
    let mut left = mid_x - CHIP_W / 2;
    let mut top = icon.top - CHIP_H - 16;
    if top < 8 {
        top = icon.bottom + 16;
    }
    if left < 8 {
        left = 8;
    }
    Some(RECT {
        left,
        top,
        right: left + CHIP_W,
        bottom: top + CHIP_H,
    })
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

fn overlay_hwnd() -> HWND {
    HWND(OVERLAY.load(Ordering::SeqCst) as *mut std::ffi::c_void)
}

fn poll_drag() {
    let down = window::primary_button_down();
    let was_down = BUTTON_WAS_DOWN.swap(down, Ordering::SeqCst);
    let mut point = POINT::default();
    let _ = unsafe { GetCursorPos(&mut point) };

    if down && !was_down {
        PRESS_ORIGIN_X.store(point.x as isize, Ordering::SeqCst);
        PRESS_ORIGIN_Y.store(point.y as isize, Ordering::SeqCst);
    }

    let dragged = down
        && ((point.x as isize - PRESS_ORIGIN_X.load(Ordering::SeqCst)).abs() > 12
            || (point.y as isize - PRESS_ORIGIN_Y.load(Ordering::SeqCst)).abs() > 12);

    if DRAGGING.load(Ordering::SeqCst) || (dragged && near_tray(&point)) {
        show_drop_target();
    } else if !down {
        hide_overlay();
    }
}

fn near_tray(point: &POINT) -> bool {
    let Some(rect) = icon_rect()
        .or_else(notification_area_rect)
        .or_else(chip_above_tray)
    else {
        return false;
    };
    let pad = 96;
    point.x >= rect.left - pad
        && point.x <= rect.right + pad
        && point.y >= rect.top - pad
        && point.y <= rect.bottom + pad
}

fn paint_overlay(hwnd: HWND) {
    unsafe {
        let mut ps = PAINTSTRUCT::default();
        let hdc = BeginPaint(hwnd, &mut ps);
        let mut client = RECT::default();
        let _ = GetClientRect(hwnd, &mut client);
        let brush = CreateSolidBrush(COLORREF(0x002C2C2C));
        FillRect(hdc, &client, brush);
        SetBkMode(hdc, TRANSPARENT);
        SetTextColor(hdc, COLORREF(0x00FFFFFF));
        let font = GetStockObject(DEFAULT_GUI_FONT);
        let _ = SelectObject(hdc, font);
        let mut text: Vec<u16> = "Drop programs or shortcuts here"
            .encode_utf16()
            .chain(std::iter::once(0))
            .collect();
        DrawTextW(
            hdc,
            &mut text,
            &mut client,
            DT_CENTER | DT_VCENTER | DT_SINGLELINE,
        );
        let _ = EndPaint(hwnd, &ps);
    }
}

unsafe extern "system" fn overlay_wndproc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    match msg {
        WM_PAINT => {
            paint_overlay(hwnd);
            LRESULT(0)
        }
        WM_ERASEBKGND => LRESULT(1),
        WM_DROPFILES => {
            let paths = window::paths_from_wparam(wparam);
            hide_overlay();
            DRAGGING.store(false, Ordering::SeqCst);
            window::dispatch_dropped_paths(paths);
            LRESULT(0)
        }
        WM_TIMER => {
            if wparam.0 == TIMER_WATCH {
                poll_drag();
            }
            LRESULT(0)
        }
        WM_DESTROY => LRESULT(0),
        _ => DefWindowProcW(hwnd, msg, wparam, lparam),
    }
}
