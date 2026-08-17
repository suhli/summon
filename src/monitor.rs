use slint::{LogicalPosition, PhysicalPosition};
use tracing::debug;
use windows::Win32::Foundation::{HWND, POINT, RECT};
use windows::Win32::Graphics::Gdi::{
    GetMonitorInfoW, MonitorFromPoint, MonitorFromWindow, HMONITOR, MONITORINFO,
    MONITOR_DEFAULTTONEAREST, MONITOR_DEFAULTTOPRIMARY,
};
use windows::Win32::UI::WindowsAndMessaging::{GetCursorPos, GetForegroundWindow};

use crate::window::hwnd_from_slint;

pub fn center_launcher(window: &slint::Window, width_px: f32, height_px: f32) {
    let scale = window.scale_factor();
    let physical_width = (width_px * scale).round() as i32;
    let physical_height = (height_px * scale).round() as i32;
    let work = work_area_for_launcher(window);

    let x = work.left + (work.right - work.left - physical_width).max(0) / 2;
    let y = work.top + (work.bottom - work.top - physical_height).max(0) / 2;

    debug!(x, y, "positioning launcher");
    window.set_position(LogicalPosition::from_physical(
        PhysicalPosition::new(x, y),
        scale,
    ));
}

fn work_area_for_launcher(window: &slint::Window) -> RECT {
    let monitor = preferred_monitor(window);
    monitor_work_area(monitor)
}

fn preferred_monitor(window: &slint::Window) -> HMONITOR {
    unsafe {
        let foreground = GetForegroundWindow();
        if !foreground.0.is_null() {
            if let Some(self_hwnd) = hwnd_from_slint(window) {
                if foreground.0 != self_hwnd.0 {
                    return MonitorFromWindow(foreground, MONITOR_DEFAULTTONEAREST);
                }
            } else {
                return MonitorFromWindow(foreground, MONITOR_DEFAULTTONEAREST);
            }
        }

        let mut cursor = POINT::default();
        if GetCursorPos(&mut cursor).is_ok() {
            return MonitorFromPoint(cursor, MONITOR_DEFAULTTONEAREST);
        }

        MonitorFromWindow(HWND::default(), MONITOR_DEFAULTTOPRIMARY)
    }
}

fn monitor_work_area(monitor: HMONITOR) -> RECT {
    let mut info = MONITORINFO {
        cbSize: std::mem::size_of::<MONITORINFO>() as u32,
        ..Default::default()
    };
    unsafe {
        if GetMonitorInfoW(monitor, &mut info).as_bool() {
            info.rcWork
        } else {
            RECT {
                left: 0,
                top: 0,
                right: 1920,
                bottom: 1080,
            }
        }
    }
}
