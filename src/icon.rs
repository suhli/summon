use std::collections::HashMap;
use std::path::{Path, PathBuf};

use slint::{Image, Rgba8Pixel, SharedPixelBuffer};
use tracing::{debug, warn};
use windows::core::PCWSTR;
use windows::Win32::Foundation::{COLORREF, HWND};
use windows::Win32::Graphics::Gdi::{
    CreateCompatibleDC, DeleteDC, DeleteObject, GetDC, GetDIBits, GetObjectW, ReleaseDC,
    SelectObject, BITMAP, BITMAPINFO, BITMAPINFOHEADER, BI_RGB, DIB_RGB_COLORS, HBITMAP, HDC,
};
use windows::Win32::Storage::FileSystem::FILE_FLAGS_AND_ATTRIBUTES;
use windows::Win32::UI::Shell::{SHGetFileInfoW, SHFILEINFOW, SHGFI_ICON, SHGFI_LARGEICON};
use windows::Win32::UI::WindowsAndMessaging::{
    DestroyIcon, GetIconInfo, DrawIconEx, ICONINFO, DI_NORMAL, HICON,
};

use crate::model::{Action, Entry};

#[derive(Clone)]
pub struct IconPixels {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
}

#[derive(Default)]
pub struct IconCache {
    pixels: HashMap<String, IconPixels>,
    defaults: Option<DefaultIcons>,
}

struct DefaultIcons {
    app: IconPixels,
    file: IconPixels,
    directory: IconPixels,
    url: IconPixels,
    command: IconPixels,
    powershell: IconPixels,
}

impl IconCache {
    pub fn new() -> Self {
        Self {
            pixels: HashMap::new(),
            defaults: Some(DefaultIcons::generate()),
        }
    }

    pub fn image_for(&mut self, entry: &Entry) -> Image {
        let key = cache_key(entry);
        if let Some(pixels) = self.pixels.get(&key) {
            return image_from_pixels(pixels);
        }

        if let Some(pixels) = resolve_pixels(entry) {
            let image = image_from_pixels(&pixels);
            self.pixels.insert(key, pixels);
            return image;
        }

        image_from_pixels(self.default_for(&entry.action))
    }

    pub fn refresh_changed(&mut self, entries: &[Entry]) {
        let live: std::collections::HashSet<String> = entries.iter().map(cache_key).collect();
        self.pixels.retain(|key, _| live.contains(key));
        for entry in entries {
            let key = cache_key(entry);
            if !self.pixels.contains_key(&key) {
                if let Some(pixels) = resolve_pixels(entry) {
                    self.pixels.insert(key, pixels);
                }
            }
        }
    }

    fn default_for(&self, action: &Action) -> &IconPixels {
        let defaults = self.defaults.as_ref().expect("defaults");
        match action {
            Action::App { .. } => &defaults.app,
            Action::File { .. } => &defaults.file,
            Action::Directory { .. } => &defaults.directory,
            Action::Url { .. } => &defaults.url,
            Action::Command { .. } => &defaults.command,
            Action::PowerShell { .. } => &defaults.powershell,
        }
    }
}

fn cache_key(entry: &Entry) -> String {
    if let Some(icon) = entry.icon.as_deref().filter(|value| !value.is_empty()) {
        return format!("user:{icon}");
    }
    match &entry.action {
        Action::App { path, .. } | Action::File { path } | Action::Directory { path } => {
            format!("shell:{}", path.to_string_lossy())
        }
        Action::Url { .. } => "default:url".into(),
        Action::Command { .. } => "default:command".into(),
        Action::PowerShell { script } => {
            let path = Path::new(script);
            if path.extension().and_then(|e| e.to_str()) == Some("ps1") {
                format!("shell:{}", path.to_string_lossy())
            } else {
                "default:powershell".into()
            }
        }
    }
}

fn resolve_pixels(entry: &Entry) -> Option<IconPixels> {
    if let Some(icon) = entry.icon.as_deref().filter(|value| !value.is_empty()) {
        if let Some(pixels) = load_user_icon(Path::new(icon)) {
            return Some(pixels);
        }
        warn!(icon, entry_id = %entry.id, "user icon failed, falling back");
    }
    if let Some(path) = entry.action.icon_source_path() {
        if let Some(pixels) = extract_shell_icon(path) {
            return Some(pixels);
        }
        warn!(path = %path.display(), entry_id = %entry.id, "shell icon failed");
    }
    None
}

fn load_user_icon(path: &Path) -> Option<IconPixels> {
    if !path.exists() {
        return None;
    }
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    match ext.as_str() {
        "png" | "jpg" | "jpeg" | "bmp" | "gif" | "webp" | "svg" => load_image_file(path),
        "ico" | "exe" | "dll" | "lnk" => extract_shell_icon(path),
        _ => extract_shell_icon(path).or_else(|| load_image_file(path)),
    }
}

fn load_image_file(path: &Path) -> Option<IconPixels> {
    match Image::load_from_path(path) {
        Ok(image) => image.to_rgba8().map(|buffer| IconPixels {
            width: buffer.width(),
            height: buffer.height(),
            rgba: buffer.as_bytes().to_vec(),
        }),
        Err(error) => {
            debug!(path = %path.display(), ?error, "image load failed");
            None
        }
    }
}

fn extract_shell_icon(path: &Path) -> Option<IconPixels> {
    let wide = wide_path(path);
    let mut info = SHFILEINFOW::default();
    let result = unsafe {
        SHGetFileInfoW(
            PCWSTR(wide.as_ptr()),
            FILE_FLAGS_AND_ATTRIBUTES(0),
            Some(&mut info),
            std::mem::size_of::<SHFILEINFOW>() as u32,
            SHGFI_ICON | SHGFI_LARGEICON,
        )
    };
    if result == 0 || info.hIcon.is_invalid() {
        return None;
    }
    let pixels = hicon_to_pixels(info.hIcon);
    unsafe {
        let _ = DestroyIcon(info.hIcon);
    }
    pixels
}

fn hicon_to_pixels(icon: HICON) -> Option<IconPixels> {
    unsafe {
        let mut icon_info = ICONINFO::default();
        GetIconInfo(icon, &mut icon_info).ok()?;

        let mut bitmap = BITMAP::default();
        if GetObjectW(
            icon_info.hbmColor.into(),
            std::mem::size_of::<BITMAP>() as i32,
            Some(&mut bitmap as *mut BITMAP as *mut _),
        ) == 0
        {
            cleanup_icon_info(&icon_info);
            return draw_icon_fallback(icon);
        }

        let width = bitmap.bmWidth;
        let height = bitmap.bmHeight.abs();
        if width <= 0 || height <= 0 {
            cleanup_icon_info(&icon_info);
            return draw_icon_fallback(icon);
        }

        let mut bmi = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: width,
                biHeight: -height,
                biPlanes: 1,
                biBitCount: 32,
                biCompression: BI_RGB.0,
                ..Default::default()
            },
            ..Default::default()
        };

        let mut bgra = vec![0u8; (width * height * 4) as usize];
        let hdc = GetDC(None);
        let copied = GetDIBits(
            hdc,
            icon_info.hbmColor,
            0,
            height as u32,
            Some(bgra.as_mut_ptr() as *mut _),
            &mut bmi,
            DIB_RGB_COLORS,
        );
        ReleaseDC(None, hdc);
        cleanup_icon_info(&icon_info);

        if copied == 0 {
            return draw_icon_fallback(icon);
        }

        let mut rgba = vec![0u8; bgra.len()];
        let mut has_alpha = false;
        for (src, dst) in bgra.chunks_exact(4).zip(rgba.chunks_exact_mut(4)) {
            dst[0] = src[2];
            dst[1] = src[1];
            dst[2] = src[0];
            dst[3] = src[3];
            if src[3] != 0 {
                has_alpha = true;
            }
        }

        if !has_alpha {
            apply_mask(&mut rgba, icon, width as u32, height as u32);
        }

        Some(IconPixels {
            width: width as u32,
            height: height as u32,
            rgba,
        })
    }
}

fn apply_mask(rgba: &mut [u8], icon: HICON, width: u32, height: u32) {
    if let Some(fallback) = draw_icon_fallback(icon) {
        if fallback.rgba.len() == rgba.len() {
            rgba.copy_from_slice(&fallback.rgba);
            return;
        }
    }
    for pixel in rgba.chunks_exact_mut(4) {
        if pixel[0] == 0 && pixel[1] == 0 && pixel[2] == 0 {
            pixel[3] = 0;
        } else {
            pixel[3] = 255;
        }
    }
    let _ = (width, height);
}

fn draw_icon_fallback(icon: HICON) -> Option<IconPixels> {
    unsafe {
        let size = 32i32;
        let hdc_screen = GetDC(None);
        let hdc = CreateCompatibleDC(Some(hdc_screen));
        let bmi = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: size,
                biHeight: -size,
                biPlanes: 1,
                biBitCount: 32,
                biCompression: BI_RGB.0,
                ..Default::default()
            },
            ..Default::default()
        };
        let mut bits: *mut std::ffi::c_void = std::ptr::null_mut();
        let dib = windows::Win32::Graphics::Gdi::CreateDIBSection(
            Some(hdc),
            &bmi,
            DIB_RGB_COLORS,
            &mut bits,
            None,
            0,
        )
        .ok()?;
        let old = SelectObject(hdc, dib.into());
        let _ = windows::Win32::Graphics::Gdi::GdiFlush();
        let _ = windows::Win32::Graphics::Gdi::PatBlt(
            hdc,
            0,
            0,
            size,
            size,
            windows::Win32::Graphics::Gdi::BLACKNESS,
        );
        let _ = DrawIconEx(
            hdc,
            0,
            0,
            icon,
            size,
            size,
            0,
            None,
            DI_NORMAL,
        );
        let mut bgra = vec![0u8; (size * size * 4) as usize];
        if !bits.is_null() {
            std::ptr::copy_nonoverlapping(bits as *const u8, bgra.as_mut_ptr(), bgra.len());
        }
        SelectObject(hdc, old);
        let _ = DeleteObject(dib.into());
        let _ = DeleteDC(hdc);
        ReleaseDC(None, hdc_screen);

        let mut rgba = vec![0u8; bgra.len()];
        for (src, dst) in bgra.chunks_exact(4).zip(rgba.chunks_exact_mut(4)) {
            dst[0] = src[2];
            dst[1] = src[1];
            dst[2] = src[0];
            dst[3] = if src[3] == 0 { 255 } else { src[3] };
        }
        Some(IconPixels {
            width: size as u32,
            height: size as u32,
            rgba,
        })
    }
}

unsafe fn cleanup_icon_info(info: &ICONINFO) {
    unsafe {
        if !info.hbmColor.is_invalid() {
            let _ = DeleteObject(info.hbmColor.into());
        }
        if !info.hbmMask.is_invalid() {
            let _ = DeleteObject(info.hbmMask.into());
        }
    }
}

fn image_from_pixels(pixels: &IconPixels) -> Image {
    let buffer = SharedPixelBuffer::<Rgba8Pixel>::clone_from_slice(
        &pixels.rgba,
        pixels.width,
        pixels.height,
    );
    Image::from_rgba8(buffer)
}

impl DefaultIcons {
    fn generate() -> Self {
        Self {
            app: svg_icon(APP_SVG),
            file: svg_icon(FILE_SVG),
            directory: svg_icon(FOLDER_SVG),
            url: svg_icon(GLOBE_SVG),
            command: svg_icon(TERMINAL_SVG),
            powershell: svg_icon(POWERSHELL_SVG),
        }
    }
}

fn svg_icon(svg: &str) -> IconPixels {
    match Image::load_from_svg_data(svg.as_bytes()) {
        Ok(image) => image
            .to_rgba8()
            .map(|buffer| IconPixels {
                width: buffer.width(),
                height: buffer.height(),
                rgba: buffer.as_bytes().to_vec(),
            })
            .unwrap_or_else(solid_fallback),
        Err(_) => solid_fallback(),
    }
}

fn solid_fallback() -> IconPixels {
    IconPixels {
        width: 48,
        height: 48,
        rgba: (0..48 * 48)
            .flat_map(|_| [0x00u8, 0x78, 0xD4, 0xFF])
            .collect(),
    }
}

fn wide_path(path: &Path) -> Vec<u16> {
    path.to_string_lossy()
        .encode_utf16()
        .chain(std::iter::once(0))
        .collect()
}

const APP_SVG: &str = r##"<svg xmlns="http://www.w3.org/2000/svg" width="48" height="48" viewBox="0 0 48 48">
  <rect x="6" y="8" width="36" height="32" rx="6" fill="#0078D4"/>
  <rect x="10" y="14" width="28" height="18" rx="3" fill="#F3F3F3"/>
  <circle cx="14" cy="12" r="1.4" fill="#F3F3F3"/>
  <circle cx="18" cy="12" r="1.4" fill="#F3F3F3"/>
</svg>"##;

const FILE_SVG: &str = r##"<svg xmlns="http://www.w3.org/2000/svg" width="48" height="48" viewBox="0 0 48 48">
  <path d="M14 6h14l10 10v26a4 4 0 0 1-4 4H14a4 4 0 0 1-4-4V10a4 4 0 0 1 4-4z" fill="#E8EAED"/>
  <path d="M28 6v10h10" fill="#C5C9CE"/>
  <rect x="16" y="24" width="16" height="2.4" rx="1.2" fill="#5F6368"/>
  <rect x="16" y="29" width="12" height="2.4" rx="1.2" fill="#5F6368"/>
</svg>"##;

const FOLDER_SVG: &str = r##"<svg xmlns="http://www.w3.org/2000/svg" width="48" height="48" viewBox="0 0 48 48">
  <path d="M8 14a4 4 0 0 1 4-4h8l4 4h12a4 4 0 0 1 4 4v16a4 4 0 0 1-4 4H12a4 4 0 0 1-4-4V14z" fill="#FFB900"/>
  <path d="M8 22h32v12a4 4 0 0 1-4 4H12a4 4 0 0 1-4-4V22z" fill="#FFD335"/>
</svg>"##;

const GLOBE_SVG: &str = r##"<svg xmlns="http://www.w3.org/2000/svg" width="48" height="48" viewBox="0 0 48 48">
  <circle cx="24" cy="24" r="16" fill="#13A10E"/>
  <ellipse cx="24" cy="24" rx="8" ry="16" fill="none" stroke="#F3F3F3" stroke-width="2"/>
  <path d="M8 24h32M12 16h24M12 32h24" fill="none" stroke="#F3F3F3" stroke-width="2"/>
</svg>"##;

const TERMINAL_SVG: &str = r##"<svg xmlns="http://www.w3.org/2000/svg" width="48" height="48" viewBox="0 0 48 48">
  <rect x="6" y="10" width="36" height="28" rx="6" fill="#1A1A1A"/>
  <path d="M14 20l6 4-6 4" fill="none" stroke="#3A96DD" stroke-width="2.4" stroke-linecap="round"/>
  <path d="M24 28h10" stroke="#F3F3F3" stroke-width="2.4" stroke-linecap="round"/>
</svg>"##;

const POWERSHELL_SVG: &str = r##"<svg xmlns="http://www.w3.org/2000/svg" width="48" height="48" viewBox="0 0 48 48">
  <rect x="6" y="10" width="36" height="28" rx="6" fill="#012456"/>
  <path d="M14 18l12 6-12 6" fill="none" stroke="#5DC9F5" stroke-width="2.6" stroke-linecap="round"/>
  <path d="M24 30h10" stroke="#F3F3F3" stroke-width="2.4" stroke-linecap="round"/>
</svg>"##;

#[allow(dead_code)]
fn _unused_types(_p: PathBuf, _c: COLORREF, _h: HWND, _d: HDC, _b: HBITMAP) {}
