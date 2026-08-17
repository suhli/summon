use std::cell::RefCell;
use std::rc::Rc;
use std::time::Duration;

use slint::{
    CloseRequestResponse, ComponentHandle, ModelRc, SharedString, Timer, TimerMode, VecModel,
};
use tracing::{error, info, warn};
use windows::Win32::Foundation::HWND;

use crate::config::Config;
use crate::hotkey::{
    Hotkey, HotkeyBinding, HotkeyManager, HotkeyTarget, ENTRY_HOTKEY_BASE, LAUNCHER_HOTKEY_ID,
};
use crate::icon::IconCache;
use crate::model::{new_entry_id, Action, Entry};
use crate::nav::{self, Direction};
use crate::window::hwnd_from_slint;
use crate::{action, config, dialog, monitor, search, startup, window};

slint::include_modules!();

const LAUNCHER_WIDTH: f32 = 760.0;
const LAUNCHER_HEIGHT: f32 = 540.0;

struct Ui {
    launcher: LauncherWindow,
    settings: SettingsWindow,
    tray: AppTray,
}

struct State {
    config: Config,
    icons: IconCache,
    query: String,
    selected: i32,
    filtered_ids: Vec<String>,
    columns: i32,
    launcher_visible: bool,
    settings_visible: bool,
    launcher_hwnd: HWND,
    settings_hwnd: HWND,
    style_applied: bool,
    focus_hooked: bool,
    recording_target: Option<String>,
    toast_timer: Timer,
    settings_toast_timer: Timer,
}

struct App {
    ui: Ui,
    state: Rc<RefCell<State>>,
    hotkeys: HotkeyManager,
}

thread_local! {
    static APP: RefCell<Option<Rc<App>>> = RefCell::new(None);
}

fn with_app(func: impl FnOnce(&Rc<App>)) {
    APP.with(|slot| {
        if let Some(app) = slot.borrow().clone() {
            func(&app);
        }
    });
}

pub fn run() -> Result<(), slint::PlatformError> {
    info!(path = %config::config_path().display(), "starting Summon");

    let loaded = config::load();
    if let Err(error) = startup::set_enabled(loaded.launcher.launch_at_startup) {
        warn!(%error, "failed to apply startup setting");
    }
    info!(enabled = startup::is_enabled(), "launch at startup");

    let launcher = LauncherWindow::new()?;
    let settings = SettingsWindow::new()?;
    let tray = match AppTray::new() {
        Ok(tray) => tray,
        Err(error) => {
            error!(%error, "tray initialization failed");
            return Err(error);
        }
    };

    let hotkeys = HotkeyManager::start().unwrap_or_else(|error| {
        error!(%error, "hotkey thread failed");
        panic!("Summon cannot start without a hotkey message thread: {error}");
    });

    let state = Rc::new(RefCell::new(State {
        config: loaded,
        icons: IconCache::new(),
        query: String::new(),
        selected: 0,
        filtered_ids: Vec::new(),
        columns: 6,
        launcher_visible: false,
        settings_visible: false,
        launcher_hwnd: HWND::default(),
        settings_hwnd: HWND::default(),
        style_applied: false,
        focus_hooked: false,
        recording_target: None,
        toast_timer: Timer::default(),
        settings_toast_timer: Timer::default(),
    }));

    let app = Rc::new(App {
        ui: Ui {
            launcher,
            settings,
            tray,
        },
        state,
        hotkeys,
    });
    APP.with(|slot| *slot.borrow_mut() = Some(app.clone()));

    wire(&app);
    apply_all(&app, true);
    sync_general_controls(&app);

    if let Err(error) = app.ui.tray.show() {
        error!(%error, "tray show failed, keeping launcher visible as fallback");
        show_launcher(&app);
    }

    info!("Summon ready");
    slint::run_event_loop_until_quit()?;
    app.hotkeys.shutdown();
    info!("Summon exited");
    Ok(())
}

fn wire(app: &Rc<App>) {
    app.hotkeys.set_handler(move |target| {
        let _ = slint::invoke_from_event_loop(move || {
            with_app(|app| match target {
                HotkeyTarget::Launcher => toggle_launcher(app),
                HotkeyTarget::Entry(id) => run_entry_by_id(app, &id, false),
            });
        });
    });

    app.hotkeys.set_reload_handler(move || {
        let _ = slint::invoke_from_event_loop(|| {
            with_app(sync_hotkey_status);
        });
    });

    {
        let app_cb = app.clone();
        app.ui.launcher.on_query_changed(move |text| {
            app_cb.state.borrow_mut().query = text.to_string();
            app_cb.state.borrow_mut().selected = 0;
            refresh_launcher(&app_cb);
        });
    }
    {
        let app_cb = app.clone();
        app.ui.launcher.on_execute_selected(move || execute_selected(&app_cb));
    }
    {
        let app_cb = app.clone();
        app.ui.launcher.on_execute_id(move |id| {
            run_entry_by_id(&app_cb, &id, true);
        });
    }
    {
        let app_cb = app.clone();
        app.ui.launcher.on_hide_requested(move || hide_launcher(&app_cb));
    }
    {
        let app_cb = app.clone();
        app.ui.launcher.on_move_left(move || move_sel(&app_cb, Direction::Left));
    }
    {
        let app_cb = app.clone();
        app.ui.launcher.on_move_right(move || move_sel(&app_cb, Direction::Right));
    }
    {
        let app_cb = app.clone();
        app.ui.launcher.on_move_up(move || move_sel(&app_cb, Direction::Up));
    }
    {
        let app_cb = app.clone();
        app.ui.launcher.on_move_down(move || move_sel(&app_cb, Direction::Down));
    }
    {
        let app_cb = app.clone();
        app.ui.launcher.on_width_changed(move |width| {
            let columns = columns_for_width(width);
            let mut inner = app_cb.state.borrow_mut();
            if inner.columns != columns {
                inner.columns = columns;
                drop(inner);
                app_cb.ui.launcher.set_columns(columns);
            }
        });
    }

    let launcher_for_close = app.clone();
    app.ui.launcher.window().on_close_requested(move || {
        hide_launcher(&launcher_for_close);
        CloseRequestResponse::KeepWindowShown
    });

    {
        let app_cb = app.clone();
        app.ui.settings.on_hide_focus_toggled(move |checked| {
            app_cb.state.borrow_mut().config.launcher.hide_on_focus_lost = checked;
            persist(&app_cb);
        });
    }
    {
        let app_cb = app.clone();
        app.ui.settings.on_startup_toggled(move |checked| set_startup(&app_cb, checked));
    }
    {
        let app_cb = app.clone();
        app.ui.settings.on_start_record(move |target| start_record(&app_cb, &target));
    }
    {
        let app_cb = app.clone();
        app.ui.settings.on_cancel_record(move || cancel_record(&app_cb));
    }
    {
        let app_cb = app.clone();
        app.ui.settings.on_clear_hotkey(move |target| clear_hotkey(&app_cb, &target));
    }
    {
        let app_cb = app.clone();
        app.ui.settings.on_record_key(move |text, ctrl, alt, shift, meta| {
            record_key(&app_cb, &text, ctrl, alt, shift, meta);
        });
    }
    {
        let app_cb = app.clone();
        app.ui.settings.on_select_entry(move |index| select_settings_entry(&app_cb, index));
    }
    {
        let app_cb = app.clone();
        app.ui.settings.on_add_entry(move || add_entry(&app_cb));
    }
    {
        let app_cb = app.clone();
        app.ui.settings.on_delete_entry(move || delete_entry(&app_cb));
    }
    {
        let app_cb = app.clone();
        app.ui.settings.on_save_entry(move || save_entry(&app_cb));
    }
    {
        let app_cb = app.clone();
        app.ui.settings.on_browse_icon(move || browse_path(&app_cb, BrowseKind::Icon));
    }
    {
        let app_cb = app.clone();
        app.ui.settings.on_browse_path(move || browse_path(&app_cb, BrowseKind::ActionPath));
    }
    {
        let app_cb = app.clone();
        app.ui.settings.on_browse_working_dir(move || browse_path(&app_cb, BrowseKind::WorkingDir));
    }

    let settings_for_close = app.clone();
    app.ui.settings.window().on_close_requested(move || {
        hide_settings(&settings_for_close);
        CloseRequestResponse::KeepWindowShown
    });

    {
        let app_cb = app.clone();
        app.ui.tray.on_open_launcher(move || show_launcher(&app_cb));
    }
    {
        let app_cb = app.clone();
        app.ui.tray.on_open_settings(move || show_settings(&app_cb));
    }
    {
        let app_cb = app.clone();
        app.ui.tray.on_reload_config(move || reload_from_disk(&app_cb));
    }
    {
        let app_cb = app.clone();
        app.ui.tray.on_startup_toggled(move |checked| set_startup(&app_cb, checked));
    }
    {
        let app_cb = app.clone();
        app.ui.tray.on_exit_app(move || {
            info!("exit requested");
            app_cb.hotkeys.shutdown();
            let _ = app_cb.ui.launcher.hide();
            let _ = app_cb.ui.settings.hide();
            let _ = app_cb.ui.tray.hide();
            slint::quit_event_loop().ok();
        });
    }
}

fn apply_all(app: &Rc<App>, reset_selection: bool) {
    {
        let mut inner = app.state.borrow_mut();
        let entries = inner.config.entries.clone();
        inner.icons.refresh_changed(&entries);
        inner.query.clear();
        if reset_selection {
            inner.selected = 0;
        }
    }
    reregister_hotkeys(app);
    refresh_launcher(app);
    refresh_settings_list(app);
    sync_general_controls(app);
}

fn reregister_hotkeys(app: &Rc<App>) {
    let inner = app.state.borrow();
    let mut bindings = Vec::new();
    if let Some(hotkey) = Hotkey::parse(&inner.config.launcher.hotkey) {
        bindings.push(HotkeyBinding {
            id: LAUNCHER_HOTKEY_ID,
            target: HotkeyTarget::Launcher,
            hotkey,
        });
    } else {
        warn!(hotkey = %inner.config.launcher.hotkey, "invalid launcher hotkey");
    }
    for (index, entry) in inner.config.entries.iter().enumerate() {
        if let Some(hotkey) = entry.parsed_hotkey() {
            bindings.push(HotkeyBinding {
                id: ENTRY_HOTKEY_BASE + index as u32,
                target: HotkeyTarget::Entry(entry.id.clone()),
                hotkey,
            });
        }
    }
    drop(inner);
    app.hotkeys.reload(bindings);
}

fn sync_hotkey_status(app: &Rc<App>) {
    let statuses = app.hotkeys.statuses();
    let launcher_ok = statuses
        .get(&LAUNCHER_HOTKEY_ID)
        .map(|status| status.registered)
        .unwrap_or(false);
    app.ui.settings.set_launcher_hotkey_ok(launcher_ok);

    let inner = app.state.borrow();
    let rows: Vec<SettingsEntryRow> = inner
        .config
        .entries
        .iter()
        .enumerate()
        .map(|(index, entry)| {
            let hotkey = entry.hotkey.clone().unwrap_or_default();
            let hotkey_ok = if hotkey.is_empty() {
                true
            } else {
                statuses
                    .get(&(ENTRY_HOTKEY_BASE + index as u32))
                    .map(|status| status.registered)
                    .unwrap_or(false)
            };
            SettingsEntryRow {
                id: entry.id.clone().into(),
                name: entry.name.clone().into(),
                hotkey: hotkey.into(),
                hotkey_ok,
                action_label: SharedString::from(entry.action.display_label()),
            }
        })
        .collect();
    let selected = app.ui.settings.get_selected_entry();
    drop(inner);
    app.ui.settings.set_entries(ModelRc::new(VecModel::from(rows)));
    app.ui.settings.set_selected_entry(selected);
    if selected >= 0 {
        let ok = statuses
            .get(&(ENTRY_HOTKEY_BASE + selected as u32))
            .map(|status| status.registered)
            .unwrap_or(true);
        app.ui.settings.set_edit_hotkey_ok(ok);
    }
}

fn sync_general_controls(app: &Rc<App>) {
    let inner = app.state.borrow();
    let hotkey = inner.config.launcher.hotkey.clone();
    let hide = inner.config.launcher.hide_on_focus_lost;
    let startup_enabled = inner.config.launcher.launch_at_startup;
    drop(inner);
    app.ui.settings.set_launcher_hotkey(hotkey.into());
    app.ui.settings.set_hide_on_focus_lost(hide);
    app.ui.settings.set_launch_at_startup(startup_enabled);
    app.ui.tray.set_launch_at_startup(startup_enabled);
}

fn refresh_launcher(app: &Rc<App>) {
    let mut inner = app.state.borrow_mut();
    let query = inner.query.clone();
    let entries = inner.config.entries.clone();
    let filtered = search::filter_entries(&entries, &query);
    let mut tiles = Vec::with_capacity(filtered.len());
    let mut ids = Vec::with_capacity(filtered.len());
    for entry in filtered {
        ids.push(entry.id.clone());
        let icon = inner.icons.image_for(entry);
        tiles.push(TileData {
            id: entry.id.clone().into(),
            name: entry.name.clone().into(),
            icon,
        });
    }
    if inner.selected >= ids.len() as i32 {
        inner.selected = if ids.is_empty() { -1 } else { 0 };
    }
    if inner.selected < 0 && !ids.is_empty() {
        inner.selected = 0;
    }
    inner.filtered_ids = ids;
    let selected = inner.selected;
    let empty = inner.filtered_ids.is_empty();
    let columns = inner.columns;
    drop(inner);

    app.ui.launcher.set_tiles(ModelRc::new(VecModel::from(tiles)));
    app.ui.launcher.set_selected_index(selected);
    app.ui.launcher.set_empty(empty);
    app.ui.launcher.set_columns(columns);
    app.ui.launcher.set_query(app.state.borrow().query.clone().into());
}

fn refresh_settings_list(app: &Rc<App>) {
    sync_hotkey_status(app);
}

fn toggle_launcher(app: &Rc<App>) {
    if app.state.borrow().launcher_visible {
        hide_launcher(app);
    } else {
        show_launcher(app);
    }
}

fn show_launcher(app: &Rc<App>) {
    reset_launcher_view(app);
    monitor::center_launcher(app.ui.launcher.window(), LAUNCHER_WIDTH, LAUNCHER_HEIGHT);
    if let Err(error) = app.ui.launcher.show() {
        error!(%error, "failed to show launcher");
        return;
    }
    ensure_launcher_style(app);
    if let Some(hwnd) = hwnd_from_slint(app.ui.launcher.window()) {
        app.state.borrow_mut().launcher_hwnd = hwnd;
        window::force_foreground(hwnd);
    }
    app.state.borrow_mut().launcher_visible = true;
    app.ui.launcher.set_request_focus(true);
}

fn hide_launcher(app: &Rc<App>) {
    let _ = app.ui.launcher.hide();
    app.state.borrow_mut().launcher_visible = false;
    reset_launcher_view(app);
}

fn reset_launcher_view(app: &Rc<App>) {
    app.state.borrow_mut().query.clear();
    app.state.borrow_mut().selected = 0;
    refresh_launcher(app);
}

fn ensure_launcher_style(app: &Rc<App>) {
    let Some(hwnd) = hwnd_from_slint(app.ui.launcher.window()) else {
        return;
    };
    let mut inner = app.state.borrow_mut();
    if !inner.style_applied || inner.launcher_hwnd.0 != hwnd.0 {
        window::apply_launcher_style(hwnd, window::apps_use_dark_mode());
        inner.launcher_hwnd = hwnd;
        inner.style_applied = true;
    }
    if !inner.focus_hooked {
        window::attach_focus_lost_handler(hwnd, || {
            let _ = slint::invoke_from_event_loop(|| {
                with_app(maybe_hide_on_focus_lost);
            });
        });
        inner.focus_hooked = true;
    }
}

fn maybe_hide_on_focus_lost(app: &Rc<App>) {
    let inner = app.state.borrow();
    if !inner.config.launcher.hide_on_focus_lost || !inner.launcher_visible {
        return;
    }
    let foreground = window::current_foreground();
    let ours = [inner.launcher_hwnd, inner.settings_hwnd];
    if window::is_our_hwnd(foreground, &ours) {
        return;
    }
    drop(inner);
    hide_launcher(app);
}

fn show_settings(app: &Rc<App>) {
    hide_launcher(app);
    sync_general_controls(app);
    refresh_settings_list(app);
    if let Err(error) = app.ui.settings.show() {
        error!(%error, "failed to show settings");
        return;
    }
    if let Some(hwnd) = hwnd_from_slint(app.ui.settings.window()) {
        app.state.borrow_mut().settings_hwnd = hwnd;
        window::force_foreground(hwnd);
    }
    app.state.borrow_mut().settings_visible = true;
}

fn hide_settings(app: &Rc<App>) {
    cancel_record(app);
    let _ = app.ui.settings.hide();
    app.state.borrow_mut().settings_visible = false;
}

fn move_sel(app: &Rc<App>, direction: Direction) {
    let mut inner = app.state.borrow_mut();
    let next = nav::move_selection(
        inner.selected,
        inner.filtered_ids.len() as i32,
        inner.columns,
        direction,
    );
    inner.selected = next;
    drop(inner);
    app.ui.launcher.set_selected_index(next);
}

fn execute_selected(app: &Rc<App>) {
    let id = {
        let inner = app.state.borrow();
        let index = inner.selected;
        if index < 0 {
            return;
        }
        inner.filtered_ids.get(index as usize).cloned()
    };
    if let Some(id) = id {
        run_entry_by_id(app, &id, true);
    }
}

fn run_entry_by_id(app: &Rc<App>, id: &str, from_launcher: bool) {
    let action = app
        .state
        .borrow()
        .config
        .entries
        .iter()
        .find(|entry| entry.id == id)
        .map(|entry| entry.action.clone());
    let Some(action) = action else {
        warn!(id, "entry not found");
        return;
    };
    if from_launcher {
        hide_launcher(app);
    }
    std::thread::spawn(move || match action::execute(&action) {
        Ok(()) => {}
        Err(message) => {
            error!(%message, "action failed");
            let _ = slint::invoke_from_event_loop(move || {
                with_app(|app| {
                    if from_launcher {
                        show_launcher(app);
                    }
                    toast(app, &message);
                });
            });
        }
    });
}

fn set_startup(app: &Rc<App>, enabled: bool) {
    match startup::set_enabled(enabled) {
        Ok(()) => {
            app.state.borrow_mut().config.launcher.launch_at_startup = enabled;
            persist(app);
            app.ui.settings.set_launch_at_startup(enabled);
            app.ui.tray.set_launch_at_startup(enabled);
        }
        Err(error) => {
            app.ui.settings.set_launch_at_startup(!enabled);
            app.ui.tray.set_launch_at_startup(!enabled);
            settings_toast(app, &error);
        }
    }
}

fn persist(app: &Rc<App>) {
    let config = app.state.borrow().config.clone();
    if let Err(error) = config::save(&config) {
        settings_toast(app, &error);
        toast(app, &error);
    }
}

fn reload_from_disk(app: &Rc<App>) {
    let loaded = config::load();
    app.state.borrow_mut().config = loaded;
    let enabled = app.state.borrow().config.launcher.launch_at_startup;
    if let Err(error) = startup::set_enabled(enabled) {
        warn!(%error, "startup sync after reload failed");
    }
    apply_all(app, true);
    settings_toast(app, "Config reloaded");
}

fn start_record(app: &Rc<App>, target: &str) {
    app.state.borrow_mut().recording_target = Some(target.to_string());
    app.ui.settings.set_recording(true);
    app.ui.settings.set_record_target(target.into());
    app.ui.settings.set_record_preview("Press a shortcut".into());
}

fn cancel_record(app: &Rc<App>) {
    app.state.borrow_mut().recording_target = None;
    app.ui.settings.set_recording(false);
    app.ui.settings.set_record_target(SharedString::default());
}

fn clear_hotkey(app: &Rc<App>, target: &str) {
    cancel_record(app);
    if target == "launcher" {
        app.state.borrow_mut().config.launcher.hotkey = "Alt+Space".into();
        persist(app);
        reregister_hotkeys(app);
        sync_general_controls(app);
        return;
    }
    app.ui.settings.set_edit_hotkey(SharedString::default());
    app.ui.settings.set_edit_hotkey_ok(true);
}

fn record_key(app: &Rc<App>, text: &str, ctrl: bool, alt: bool, shift: bool, meta: bool) {
    if app.state.borrow().recording_target.is_none() {
        return;
    }
    let Some(key) = slint_key_name(text) else {
        let preview = preview_modifiers(ctrl, alt, shift, meta);
        app.ui.settings.set_record_preview(preview.into());
        return;
    };
    if !ctrl && !alt && !shift && !meta {
        app.ui.settings.set_record_preview("Add Ctrl, Alt, Shift or Win".into());
        return;
    }
    let hotkey = Hotkey {
        ctrl,
        alt,
        shift,
        win: meta,
        key,
    };
    let display = hotkey.display();
    let target = app.state.borrow().recording_target.clone().unwrap_or_default();
    cancel_record(app);

    if target == "launcher" {
        app.state.borrow_mut().config.launcher.hotkey = display.clone();
        persist(app);
        reregister_hotkeys(app);
        sync_general_controls(app);
        let available = crate::hotkey::probe_available(&hotkey)
            || app
                .hotkeys
                .statuses()
                .get(&LAUNCHER_HOTKEY_ID)
                .is_some_and(|status| status.registered);
        app.ui.settings.set_launcher_hotkey_ok(available);
        return;
    }

    app.ui.settings.set_edit_hotkey(display.into());
    app.ui.settings.set_edit_hotkey_ok(crate::hotkey::probe_available(&hotkey));
}

fn select_settings_entry(app: &Rc<App>, index: i32) {
    cancel_record(app);
    let inner = app.state.borrow();
    let Some(entry) = inner.config.entries.get(index as usize) else {
        return;
    };
    fill_editor(&app.ui.settings, entry);
    drop(inner);
    app.ui.settings.set_selected_entry(index);
    app.ui.settings.set_inline_error(SharedString::default());
    sync_hotkey_status(app);
}

fn fill_editor(settings: &SettingsWindow, entry: &Entry) {
    settings.set_edit_id(entry.id.clone().into());
    settings.set_edit_name(entry.name.clone().into());
    settings.set_edit_keywords(entry.keywords.join(", ").into());
    settings.set_edit_icon(entry.icon.clone().unwrap_or_default().into());
    settings.set_edit_hotkey(entry.hotkey.clone().unwrap_or_default().into());
    settings.set_edit_action_index(action_index(&entry.action));
    settings.set_edit_path(SharedString::default());
    settings.set_edit_args(SharedString::default());
    settings.set_edit_working_dir(SharedString::default());
    settings.set_edit_url(SharedString::default());
    settings.set_edit_command(SharedString::default());
    settings.set_edit_script(SharedString::default());
    match &entry.action {
        Action::App {
            path,
            args,
            working_dir,
        } => {
            settings.set_edit_path(path.to_string_lossy().into_owned().into());
            settings.set_edit_args(args.join(" ").into());
            settings.set_edit_working_dir(
                working_dir
                    .as_ref()
                    .map(|path| path.to_string_lossy().into_owned())
                    .unwrap_or_default()
                    .into(),
            );
        }
        Action::File { path } | Action::Directory { path } => {
            settings.set_edit_path(path.to_string_lossy().into_owned().into());
        }
        Action::Url { url } => settings.set_edit_url(url.clone().into()),
        Action::Command {
            command,
            args,
            working_dir,
        } => {
            settings.set_edit_command(command.clone().into());
            settings.set_edit_args(args.join(" ").into());
            settings.set_edit_working_dir(
                working_dir
                    .as_ref()
                    .map(|path| path.to_string_lossy().into_owned())
                    .unwrap_or_default()
                    .into(),
            );
        }
        Action::PowerShell { script } => settings.set_edit_script(script.clone().into()),
    }
}

fn add_entry(app: &Rc<App>) {
    let entry = Entry {
        id: new_entry_id(),
        name: "New Entry".into(),
        description: None,
        icon: None,
        keywords: Vec::new(),
        hotkey: None,
        action: Action::App {
            path: Default::default(),
            args: Vec::new(),
            working_dir: None,
        },
    };
    let index;
    {
        let mut inner = app.state.borrow_mut();
        inner.config.entries.push(entry.clone());
        index = inner.config.entries.len() as i32 - 1;
    }
    persist(app);
    apply_all(app, false);
    app.ui.settings.set_page(1);
    select_settings_entry(app, index);
}

fn delete_entry(app: &Rc<App>) {
    let id = app.ui.settings.get_edit_id().to_string();
    if id.is_empty() {
        return;
    }
    {
        let mut inner = app.state.borrow_mut();
        inner.config.entries.retain(|entry| entry.id != id);
    }
    persist(app);
    apply_all(app, true);
    clear_editor(&app.ui.settings);
    app.ui.settings.set_selected_entry(-1);
}

fn save_entry(app: &Rc<App>) {
    let settings = &app.ui.settings;
    let id = settings.get_edit_id().to_string();
    if id.is_empty() {
        return;
    }
    let name = settings.get_edit_name().trim().to_string();
    if name.is_empty() {
        settings.set_inline_error("Name is required".into());
        return;
    }
    let action = match action_from_editor(settings) {
        Ok(action) => action,
        Err(error) => {
            settings.set_inline_error(error.into());
            return;
        }
    };
    let keywords = settings
        .get_edit_keywords()
        .split(',')
        .map(|part| part.trim().to_string())
        .filter(|part| !part.is_empty())
        .collect();
    let icon = nonempty(settings.get_edit_icon());
    let hotkey = nonempty(settings.get_edit_hotkey());
    if let Some(raw) = hotkey.as_deref() {
        if Hotkey::parse(raw).is_none() {
            settings.set_inline_error("Invalid hotkey".into());
            return;
        }
    }

    {
        let mut inner = app.state.borrow_mut();
        let Some(entry) = inner.config.entries.iter_mut().find(|entry| entry.id == id) else {
            settings.set_inline_error("Entry not found".into());
            return;
        };
        entry.name = name;
        entry.keywords = keywords;
        entry.icon = icon;
        entry.hotkey = hotkey;
        entry.action = action;
    }

    settings.set_inline_error(SharedString::default());
    persist(app);
    apply_all(app, false);
    settings_toast(app, "Entry saved");
}

fn action_from_editor(settings: &SettingsWindow) -> Result<Action, String> {
    match settings.get_edit_action_index() {
        0 => Ok(Action::App {
            path: required_path(settings.get_edit_path(), "Executable")?.into(),
            args: split_args(&settings.get_edit_args()),
            working_dir: nonempty(settings.get_edit_working_dir()).map(Into::into),
        }),
        1 => Ok(Action::File {
            path: required_path(settings.get_edit_path(), "File Path")?.into(),
        }),
        2 => Ok(Action::Directory {
            path: required_path(settings.get_edit_path(), "Directory Path")?.into(),
        }),
        3 => {
            let url = settings.get_edit_url().trim().to_string();
            if url.is_empty() {
                Err("URL is required".into())
            } else {
                Ok(Action::Url { url })
            }
        }
        4 => {
            let command = settings.get_edit_command().trim().to_string();
            if command.is_empty() {
                Err("Command is required".into())
            } else {
                Ok(Action::Command {
                    command,
                    args: split_args(&settings.get_edit_args()),
                    working_dir: nonempty(settings.get_edit_working_dir()).map(Into::into),
                })
            }
        }
        5 => {
            let script = settings.get_edit_script().trim().to_string();
            if script.is_empty() {
                Err("Script is required".into())
            } else {
                Ok(Action::PowerShell { script })
            }
        }
        _ => Err("Unknown action type".into()),
    }
}

fn action_index(action: &Action) -> i32 {
    match action {
        Action::App { .. } => 0,
        Action::File { .. } => 1,
        Action::Directory { .. } => 2,
        Action::Url { .. } => 3,
        Action::Command { .. } => 4,
        Action::PowerShell { .. } => 5,
    }
}

fn clear_editor(settings: &SettingsWindow) {
    settings.set_edit_id(SharedString::default());
    settings.set_edit_name(SharedString::default());
    settings.set_edit_keywords(SharedString::default());
    settings.set_edit_icon(SharedString::default());
    settings.set_edit_hotkey(SharedString::default());
    settings.set_edit_path(SharedString::default());
    settings.set_edit_args(SharedString::default());
    settings.set_edit_working_dir(SharedString::default());
    settings.set_edit_url(SharedString::default());
    settings.set_edit_command(SharedString::default());
    settings.set_edit_script(SharedString::default());
}

enum BrowseKind {
    Icon,
    ActionPath,
    WorkingDir,
}

fn browse_path(app: &Rc<App>, kind: BrowseKind) {
    let owner = hwnd_from_slint(app.ui.settings.window());
    let folders = matches!(kind, BrowseKind::WorkingDir)
        || app.ui.settings.get_edit_action_index() == 2 && matches!(kind, BrowseKind::ActionPath);
    let title = match kind {
        BrowseKind::Icon => "Choose icon",
        BrowseKind::WorkingDir => "Choose working directory",
        BrowseKind::ActionPath if folders => "Choose folder",
        BrowseKind::ActionPath => "Choose file",
    };
    let picked = if folders {
        dialog::pick_folder(owner, title)
    } else {
        dialog::pick_file(owner, title)
    };
    if let Some(path) = picked {
        let value = path.to_string_lossy().into_owned();
        match kind {
            BrowseKind::Icon => app.ui.settings.set_edit_icon(value.into()),
            BrowseKind::ActionPath => app.ui.settings.set_edit_path(value.into()),
            BrowseKind::WorkingDir => app.ui.settings.set_edit_working_dir(value.into()),
        }
    }
}

fn toast(app: &Rc<App>, message: &str) {
    app.ui.launcher.set_toast_text(message.into());
    app.ui.launcher.set_toast_visible(true);
    let launcher = app.ui.launcher.as_weak();
    app.state.borrow().toast_timer.start(
        TimerMode::SingleShot,
        Duration::from_secs(3),
        move || {
            if let Some(launcher) = launcher.upgrade() {
                launcher.set_toast_visible(false);
            }
        },
    );
}

fn settings_toast(app: &Rc<App>, message: &str) {
    app.ui.settings.set_toast_text(message.into());
    app.ui.settings.set_toast_visible(true);
    let settings = app.ui.settings.as_weak();
    app.state.borrow().settings_toast_timer.start(
        TimerMode::SingleShot,
        Duration::from_secs(3),
        move || {
            if let Some(settings) = settings.upgrade() {
                settings.set_toast_visible(false);
            }
        },
    );
}

fn columns_for_width(width: f32) -> i32 {
    let cols = ((width - 48.0) / 116.0).floor() as i32;
    cols.clamp(5, 7)
}

fn nonempty(value: SharedString) -> Option<String> {
    let text = value.trim();
    if text.is_empty() {
        None
    } else {
        Some(text.to_string())
    }
}

fn required_path(value: SharedString, label: &str) -> Result<String, String> {
    nonempty(value).ok_or_else(|| format!("{label} is required"))
}

fn split_args(value: &SharedString) -> Vec<String> {
    value
        .split_whitespace()
        .map(str::to_string)
        .collect()
}

fn preview_modifiers(ctrl: bool, alt: bool, shift: bool, meta: bool) -> String {
    let mut parts = Vec::new();
    if ctrl {
        parts.push("Ctrl");
    }
    if alt {
        parts.push("Alt");
    }
    if shift {
        parts.push("Shift");
    }
    if meta {
        parts.push("Win");
    }
    if parts.is_empty() {
        "Press a shortcut".into()
    } else {
        parts.join("+") + "+"
    }
}

fn slint_key_name(text: &str) -> Option<String> {
    if text.is_empty() {
        return None;
    }
    let ch = text.chars().next()?;
    const SHIFT: char = '\u{10}';
    const CONTROL: char = '\u{11}';
    const ALT: char = '\u{12}';
    const META: char = '\u{F000}';
    match ch {
        '\u{8}' => Some("Backspace".into()),
        '\t' => Some("Tab".into()),
        '\n' | '\r' => Some("Enter".into()),
        '\u{1b}' => Some("Escape".into()),
        ' ' => Some("Space".into()),
        '\u{7f}' => Some("Delete".into()),
        SHIFT | CONTROL | ALT | META => None,
        c if c.is_ascii_graphic() => Some(c.to_ascii_uppercase().to_string()),
        c if ('\u{F700}'..='\u{F71B}').contains(&c) => {
            let n = c as u32 - 0xF700 + 1;
            Some(format!("F{n}"))
        }
        '\u{F702}' => Some("Left".into()),
        '\u{F703}' => Some("Right".into()),
        '\u{F700}' => Some("Up".into()),
        '\u{F701}' => Some("Down".into()),
        _ => None,
    }
}
