use std::collections::{HashMap, HashSet};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};

use tracing::{error, info, warn};
use windows::core::PCWSTR;
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Threading::GetCurrentThreadId;
use windows::Win32::UI::Input::KeyboardAndMouse::{
    RegisterHotKey, UnregisterHotKey, HOT_KEY_MODIFIERS, MOD_ALT, MOD_CONTROL, MOD_NOREPEAT,
    MOD_SHIFT, MOD_WIN, VIRTUAL_KEY, VK_ADD, VK_BACK, VK_DELETE, VK_DOWN, VK_END, VK_ESCAPE,
    VK_F1, VK_F12, VK_F24, VK_HOME, VK_INSERT, VK_LEFT, VK_NEXT, VK_OEM_1, VK_OEM_2, VK_OEM_3,
    VK_OEM_4, VK_OEM_5, VK_OEM_6, VK_OEM_7, VK_OEM_COMMA, VK_OEM_MINUS, VK_OEM_PERIOD, VK_OEM_PLUS,
    VK_PAUSE, VK_PRIOR, VK_RETURN, VK_RIGHT, VK_SNAPSHOT, VK_SPACE, VK_SUBTRACT, VK_TAB, VK_UP,
};
use windows::Win32::UI::WindowsAndMessaging::{
    AllowSetForegroundWindow, CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW,
    GetMessageW, PostThreadMessageW, RegisterClassW, TranslateMessage, CS_HREDRAW, CS_VREDRAW,
    HWND_MESSAGE, MSG, WM_APP, WM_HOTKEY, WM_QUIT, WNDCLASSW,
};

const WM_RELOAD: u32 = WM_APP + 1;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Hotkey {
    pub ctrl: bool,
    pub alt: bool,
    pub shift: bool,
    pub win: bool,
    pub key: String,
}

impl Hotkey {
    pub fn parse(input: &str) -> Option<Self> {
        let mut ctrl = false;
        let mut alt = false;
        let mut shift = false;
        let mut win = false;
        let mut key = None;

        for part in input.split('+') {
            let part = part.trim();
            if part.is_empty() {
                continue;
            }
            match normalize_token(part).as_str() {
                "CTRL" | "CONTROL" => ctrl = true,
                "ALT" => alt = true,
                "SHIFT" => shift = true,
                "WIN" | "WINDOWS" | "SUPER" | "META" => win = true,
                token => {
                    if key.is_some() {
                        return None;
                    }
                    key = Some(pretty_key(token));
                }
            }
        }

        let key = key.filter(|value| !value.is_empty())?;
        if !ctrl && !alt && !shift && !win {
            return None;
        }
        Some(Self {
            ctrl,
            alt,
            shift,
            win,
            key,
        })
    }

    pub fn display(&self) -> String {
        let mut parts = Vec::new();
        if self.ctrl {
            parts.push("Ctrl");
        }
        if self.alt {
            parts.push("Alt");
        }
        if self.shift {
            parts.push("Shift");
        }
        if self.win {
            parts.push("Win");
        }
        parts.push(self.key.as_str());
        parts.join("+")
    }

    pub fn modifiers(&self) -> HOT_KEY_MODIFIERS {
        let mut modifiers = MOD_NOREPEAT;
        if self.ctrl {
            modifiers |= MOD_CONTROL;
        }
        if self.alt {
            modifiers |= MOD_ALT;
        }
        if self.shift {
            modifiers |= MOD_SHIFT;
        }
        if self.win {
            modifiers |= MOD_WIN;
        }
        modifiers
    }

    pub fn virtual_key(&self) -> Option<VIRTUAL_KEY> {
        virtual_key(&self.key)
    }
}

#[derive(Debug, Clone)]
pub enum HotkeyTarget {
    Launcher,
    Entry(String),
}

#[derive(Debug, Clone)]
pub struct HotkeyBinding {
    pub id: u32,
    pub target: HotkeyTarget,
    pub hotkey: Hotkey,
}

#[derive(Debug, Clone)]
pub struct RegistrationStatus {
    #[allow(dead_code)]
    pub display: String,
    pub registered: bool,
}

pub type HotkeyHandler = Arc<Mutex<Option<Box<dyn Fn(HotkeyTarget) + Send>>>>;
pub type ReloadHandler = Arc<Mutex<Option<Box<dyn Fn() + Send>>>>;

pub struct HotkeyManager {
    thread_id: u32,
    _join: JoinHandle<()>,
    command_tx: Sender<Vec<HotkeyBinding>>,
    handler: HotkeyHandler,
    reload_handler: ReloadHandler,
    statuses: Arc<Mutex<HashMap<u32, RegistrationStatus>>>,
}

impl HotkeyManager {
    pub fn start() -> Result<Self, String> {
        let (ready_tx, ready_rx) = mpsc::channel();
        let (command_tx, command_rx) = mpsc::channel::<Vec<HotkeyBinding>>();
        let statuses = Arc::new(Mutex::new(HashMap::new()));
        let statuses_thread = statuses.clone();
        let handler: HotkeyHandler = Arc::new(Mutex::new(None));
        let handler_thread = handler.clone();
        let reload_handler: ReloadHandler = Arc::new(Mutex::new(None));
        let reload_handler_thread = reload_handler.clone();

        let join = thread::Builder::new()
            .name("summon-hotkeys".into())
            .spawn(move || {
                if let Err(error) = run_hotkey_thread(
                    ready_tx,
                    command_rx,
                    handler_thread,
                    reload_handler_thread,
                    statuses_thread,
                ) {
                    error!(%error, "hotkey thread failed");
                }
            })
            .map_err(|error| format!("failed to start hotkey thread: {error}"))?;

        let thread_id = ready_rx
            .recv()
            .map_err(|_| "hotkey thread exited before becoming ready".to_string())?;

        Ok(Self {
            thread_id,
            _join: join,
            command_tx,
            handler,
            reload_handler,
            statuses,
        })
    }

    pub fn set_handler(&self, callback: impl Fn(HotkeyTarget) + Send + 'static) {
        if let Ok(mut guard) = self.handler.lock() {
            *guard = Some(Box::new(callback));
        }
    }

    pub fn set_reload_handler(&self, callback: impl Fn() + Send + 'static) {
        if let Ok(mut guard) = self.reload_handler.lock() {
            *guard = Some(Box::new(callback));
        }
    }

    pub fn reload(&self, bindings: Vec<HotkeyBinding>) {
        if self.command_tx.send(bindings).is_err() {
            error!("hotkey command channel closed");
            return;
        }
        unsafe {
            if let Err(error) = PostThreadMessageW(self.thread_id, WM_RELOAD, WPARAM(0), LPARAM(0)) {
                error!(%error, "failed to wake hotkey thread");
            }
        }
    }

    pub fn statuses(&self) -> HashMap<u32, RegistrationStatus> {
        self.statuses
            .lock()
            .map(|guard| guard.clone())
            .unwrap_or_default()
    }

    pub fn shutdown(&self) {
        unsafe {
            let _ = PostThreadMessageW(self.thread_id, WM_QUIT, WPARAM(0), LPARAM(0));
        }
    }
}

fn run_hotkey_thread(
    ready_tx: Sender<u32>,
    command_rx: Receiver<Vec<HotkeyBinding>>,
    handler: HotkeyHandler,
    reload_handler: ReloadHandler,
    statuses: Arc<Mutex<HashMap<u32, RegistrationStatus>>>,
) -> Result<(), String> {
    let class_name = wide("Summon.Hotkey.MessageWindow");
    let class = WNDCLASSW {
        style: CS_HREDRAW | CS_VREDRAW,
        lpfnWndProc: Some(hotkey_wnd_proc),
        hInstance: unsafe { GetModuleHandleW(None).map(|h| h.into()).unwrap_or_default() },
        lpszClassName: PCWSTR(class_name.as_ptr()),
        ..Default::default()
    };

    unsafe {
        RegisterClassW(&class);
    }

    let hwnd = unsafe {
        CreateWindowExW(
            Default::default(),
            PCWSTR(class_name.as_ptr()),
            PCWSTR(class_name.as_ptr()),
            Default::default(),
            0,
            0,
            0,
            0,
            Some(HWND_MESSAGE),
            None,
            None,
            None,
        )
    }
    .map_err(|error| format!("failed to create hotkey window: {error}"))?;

    let thread_id = unsafe { GetCurrentThreadId() };
    let _ = ready_tx.send(thread_id);
    info!("hotkey thread ready");

    let mut registered_ids: HashSet<u32> = HashSet::new();
    let mut map: HashMap<u32, HotkeyTarget> = HashMap::new();
    let mut msg = MSG::default();

    loop {
        let ok = unsafe { GetMessageW(&mut msg, None, 0, 0) };
        if !ok.as_bool() {
            break;
        }

        if msg.message == WM_RELOAD {
            let bindings = drain_bindings(&command_rx);
            reregister(
                hwnd,
                bindings,
                &mut registered_ids,
                &mut map,
                &statuses,
                &reload_handler,
            );
            continue;
        }

        if msg.message == WM_HOTKEY {
            let id = msg.wParam.0 as u32;
            if let Some(target) = map.get(&id).cloned() {
                match &target {
                    HotkeyTarget::Launcher => info!(id, "launcher hotkey"),
                    HotkeyTarget::Entry(entry_id) => {
                        info!(id, entry_id, "entry hotkey")
                    }
                }
                unsafe {
                    let _ = AllowSetForegroundWindow(u32::MAX);
                }
                if let Ok(guard) = handler.lock() {
                    if let Some(callback) = guard.as_ref() {
                        callback(target);
                    }
                }
            }
            continue;
        }

        unsafe {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }

    unregister_all(hwnd, &registered_ids);
    unsafe {
        let _ = DestroyWindow(hwnd);
    }
    Ok(())
}

fn drain_bindings(rx: &Receiver<Vec<HotkeyBinding>>) -> Vec<HotkeyBinding> {
    let mut latest = rx.try_recv().unwrap_or_default();
    while let Ok(next) = rx.try_recv() {
        latest = next;
    }
    latest
}

fn reregister(
    hwnd: HWND,
    bindings: Vec<HotkeyBinding>,
    registered_ids: &mut HashSet<u32>,
    map: &mut HashMap<u32, HotkeyTarget>,
    statuses: &Arc<Mutex<HashMap<u32, RegistrationStatus>>>,
    reload_handler: &ReloadHandler,
) {
    unregister_all(hwnd, registered_ids);
    registered_ids.clear();
    map.clear();

    let mut next_status = HashMap::new();
    for binding in bindings {
        let label = binding.hotkey.display();
        let Some(vk) = binding.hotkey.virtual_key() else {
            warn!(hotkey = %label, "unsupported hotkey");
            next_status.insert(
                binding.id,
                RegistrationStatus {
                    display: label,
                    registered: false,
                },
            );
            continue;
        };

        let ok = unsafe {
            RegisterHotKey(
                Some(hwnd),
                binding.id as i32,
                binding.hotkey.modifiers(),
                vk.0 as u32,
            )
        };
        let registered = ok.is_ok();
        if registered {
            info!(id = binding.id, hotkey = %label, "hotkey registered");
            registered_ids.insert(binding.id);
            map.insert(binding.id, binding.target);
        } else {
            warn!(id = binding.id, hotkey = %label, "hotkey conflict");
        }
        next_status.insert(
            binding.id,
            RegistrationStatus {
                display: label,
                registered,
            },
        );
    }

    if let Ok(mut guard) = statuses.lock() {
        *guard = next_status;
    }
    if let Ok(guard) = reload_handler.lock() {
        if let Some(callback) = guard.as_ref() {
            callback();
        }
    }
}

fn unregister_all(hwnd: HWND, ids: &HashSet<u32>) {
    for id in ids {
        unsafe {
            let _ = UnregisterHotKey(Some(hwnd), *id as i32);
        }
    }
}

unsafe extern "system" fn hotkey_wnd_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) }
}

pub fn probe_available(hotkey: &Hotkey) -> bool {
    let Some(vk) = hotkey.virtual_key() else {
        return false;
    };
    unsafe {
        match RegisterHotKey(None, 0x7FFF, hotkey.modifiers(), vk.0 as u32) {
            Ok(()) => {
                let _ = UnregisterHotKey(None, 0x7FFF);
                true
            }
            Err(_) => false,
        }
    }
}

fn normalize_token(part: &str) -> String {
    part.trim().to_ascii_uppercase()
}

fn pretty_key(token: &str) -> String {
    match token {
        "ESC" => "Escape".into(),
        "RETURN" => "Enter".into(),
        "PGUP" => "PageUp".into(),
        "PGDN" | "PAGEDOWN" => "PageDown".into(),
        "INS" => "Insert".into(),
        "DEL" => "Delete".into(),
        "PLUS" | "=" => "Plus".into(),
        "MINUS" | "_" => "Minus".into(),
        other => {
            if other.len() == 1 {
                other.to_string()
            } else {
                let mut chars = other.chars();
                match chars.next() {
                    Some(first) => {
                        first.to_ascii_uppercase().to_string() + &chars.as_str().to_ascii_lowercase()
                    }
                    None => other.to_string(),
                }
            }
        }
    }
}

fn virtual_key(key: &str) -> Option<VIRTUAL_KEY> {
    let upper = key.to_ascii_uppercase();
    Some(match upper.as_str() {
        "SPACE" => VK_SPACE,
        "ENTER" | "RETURN" => VK_RETURN,
        "ESCAPE" | "ESC" => VK_ESCAPE,
        "TAB" => VK_TAB,
        "BACKSPACE" => VK_BACK,
        "DELETE" | "DEL" => VK_DELETE,
        "INSERT" | "INS" => VK_INSERT,
        "HOME" => VK_HOME,
        "END" => VK_END,
        "PAGEUP" | "PGUP" => VK_PRIOR,
        "PAGEDOWN" | "PGDN" => VK_NEXT,
        "LEFT" => VK_LEFT,
        "RIGHT" => VK_RIGHT,
        "UP" => VK_UP,
        "DOWN" => VK_DOWN,
        "PLUS" | "=" => VK_OEM_PLUS,
        "MINUS" | "_" => VK_OEM_MINUS,
        "ADD" => VK_ADD,
        "SUBTRACT" => VK_SUBTRACT,
        "PAUSE" => VK_PAUSE,
        "PRINTSCREEN" => VK_SNAPSHOT,
        "," => VK_OEM_COMMA,
        "." => VK_OEM_PERIOD,
        ";" => VK_OEM_1,
        "/" => VK_OEM_2,
        "`" => VK_OEM_3,
        "[" => VK_OEM_4,
        "\\" => VK_OEM_5,
        "]" => VK_OEM_6,
        "'" => VK_OEM_7,
        other if other.len() == 1 => {
            let ch = other.as_bytes()[0];
            if ch.is_ascii_alphanumeric() {
                VIRTUAL_KEY(ch as u16)
            } else {
                return None;
            }
        }
        other if other.starts_with('F') => {
            let number: u16 = other[1..].parse().ok()?;
            if (1..=24).contains(&number) {
                VIRTUAL_KEY(VK_F1.0 + (number - 1))
            } else {
                return None;
            }
        }
        _ => return None,
    })
}

fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(std::iter::once(0)).collect()
}

pub const LAUNCHER_HOTKEY_ID: u32 = 1;
pub const ENTRY_HOTKEY_BASE: u32 = 100;

#[allow(dead_code)]
fn _vk_range() {
    let _ = (VK_F12, VK_F24);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_alt_space() {
        let hotkey = Hotkey::parse("Alt+Space").unwrap();
        assert!(hotkey.alt);
        assert_eq!(hotkey.key, "Space");
        assert_eq!(hotkey.display(), "Alt+Space");
    }

    #[test]
    fn parses_ctrl_alt_s() {
        let hotkey = Hotkey::parse("ctrl + alt + s").unwrap();
        assert!(hotkey.ctrl && hotkey.alt);
        assert_eq!(hotkey.display(), "Ctrl+Alt+S");
    }

    #[test]
    fn rejects_key_only() {
        assert!(Hotkey::parse("S").is_none());
    }
}
