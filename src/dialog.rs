use std::path::PathBuf;

use tracing::{error, warn};
use windows::core::PCWSTR;
use windows::Win32::Foundation::HWND;
use windows::Win32::System::Com::{
    CoCreateInstance, CoInitializeEx, CoUninitialize, CLSCTX_INPROC_SERVER, COINIT_APARTMENTTHREADED,
};
use windows::Win32::UI::Shell::{
    FileOpenDialog, IFileOpenDialog, IShellItem, FOS_FILEMUSTEXIST, FOS_FORCEFILESYSTEM,
    FOS_PATHMUSTEXIST, FOS_PICKFOLDERS, SIGDN_FILESYSPATH,
};

pub fn pick_file(owner: Option<HWND>, title: &str) -> Option<PathBuf> {
    pick(owner, title, false)
}

pub fn pick_folder(owner: Option<HWND>, title: &str) -> Option<PathBuf> {
    pick(owner, title, true)
}

fn pick(owner: Option<HWND>, title: &str, folders: bool) -> Option<PathBuf> {
    let com_owned = unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED).is_ok() };
    let result = pick_inner(owner, title, folders);
    if com_owned {
        unsafe { CoUninitialize() };
    }
    result
}

fn pick_inner(owner: Option<HWND>, title: &str, folders: bool) -> Option<PathBuf> {
    unsafe {
        let dialog: IFileOpenDialog = match CoCreateInstance(&FileOpenDialog, None, CLSCTX_INPROC_SERVER)
        {
            Ok(dialog) => dialog,
            Err(error) => {
                error!(%error, "file dialog create failed");
                return None;
            }
        };

        let mut options = match dialog.GetOptions() {
            Ok(options) => options,
            Err(error) => {
                warn!(%error, "file dialog options failed");
                return None;
            }
        };
        options |= FOS_FORCEFILESYSTEM | FOS_PATHMUSTEXIST;
        if folders {
            options |= FOS_PICKFOLDERS;
        } else {
            options |= FOS_FILEMUSTEXIST;
        }
        if let Err(error) = dialog.SetOptions(options) {
            warn!(%error, "file dialog set options failed");
        }

        let title_wide = wide(title);
        let _ = dialog.SetTitle(PCWSTR(title_wide.as_ptr()));

        if let Err(error) = dialog.Show(owner) {
            debug_cancelled(error.to_string());
            return None;
        }

        let item: IShellItem = match dialog.GetResult() {
            Ok(item) => item,
            Err(error) => {
                warn!(%error, "file dialog result failed");
                return None;
            }
        };
        match item.GetDisplayName(SIGDN_FILESYSPATH) {
            Ok(pwstr) => {
                let path = pwstr.to_string().ok().map(PathBuf::from);
                windows::Win32::System::Com::CoTaskMemFree(Some(pwstr.0 as *const _));
                path
            }
            Err(error) => {
                warn!(%error, "file dialog path failed");
                None
            }
        }
    }
}

fn debug_cancelled(message: String) {
    if !message.contains("cancelled") && !message.contains("canceled") {
        warn!(message, "file dialog closed");
    }
}

fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(std::iter::once(0)).collect()
}
