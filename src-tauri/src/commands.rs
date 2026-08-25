use std::sync::atomic::Ordering;

use tauri::{AppHandle, Emitter, State};
use tauri_plugin_autostart::ManagerExt;

use crate::overlay;
use crate::state::{AppState, OverlayState};
use crate::{
    FORCE_CLOSE_SHORTCUT_ENABLED, MEDIA_PAUSE_ON_BREAK_ENABLED, OVERLAY_AUTO_CLOSE_MINUTES,
    POMODORO_ENABLED,
};

#[tauri::command]
pub fn get_overlay_state(state: State<AppState>) -> OverlayState {
    state.overlay.lock().unwrap().clone()
}

#[tauri::command]
pub fn is_dev_mode(state: State<AppState>) -> bool {
    state.dev_mode
}

/// Pushes the breakit challenge length/charset (loaded by the frontend from
/// app_setting) into Rust state. Called once on app boot and again whenever
/// Settings saves, so SQLite stays the source of truth while the scheduler
/// still has a fast in-memory copy to use when it spawns a fresh overlay.
#[tauri::command]
pub fn sync_breakit_config(state: State<AppState>, length: u32, include_special: bool) {
    let mut cfg = state.breakit_config.lock().unwrap();
    cfg.length = length.clamp(4, 64);
    cfg.include_special = include_special;
}

#[tauri::command]
pub fn mark_reflection_entered(app: AppHandle, state: State<AppState>) -> OverlayState {
    {
        let mut overlay = state.overlay.lock().unwrap();
        overlay.reflection_entered = true;
    }
    overlay::try_close_if_unlocked(&app);
    state.overlay.lock().unwrap().clone()
}

#[tauri::command]
pub fn breakit_attempt(app: AppHandle, state: State<AppState>, input: String) -> OverlayState {
    {
        let mut overlay = state.overlay.lock().unwrap();
        if overlay.open && input == overlay.breakit_challenge {
            overlay.breakit_matched = true;
        }
    }
    overlay::try_close_if_unlocked(&app);
    state.overlay.lock().unwrap().clone()
}

#[tauri::command]
pub fn get_enabled() -> bool {
    POMODORO_ENABLED.load(Ordering::SeqCst)
}

#[tauri::command]
pub fn set_enabled(app: AppHandle, enabled: bool) {
    POMODORO_ENABLED.store(enabled, Ordering::SeqCst);
    let _ = app.emit("pomodoro://enabled-changed", enabled);
}

/// Checked once by the main window on every app boot. Returns the start of
/// the most recently completed break slot (RFC3339, local offset) if `now`
/// is in a work slot, so the frontend can check whether it was ever
/// reflected on -- catches the case where the app was closed/killed/the
/// machine restarted with a break left unlogged. `None` while a break is
/// live; that case is already owned by the normal scheduler.
#[tauri::command]
pub fn get_startup_catchup_slot() -> Option<String> {
    crate::grid::preceding_break_start(chrono::Local::now()).map(|dt| dt.to_rfc3339())
}

/// Opens the small top-of-screen catch-up window for a break slot that has
/// already elapsed unreflected (see `get_startup_catchup_slot`). Stores the
/// slot in state for the window's `get_catchup_slot` call on its very first
/// mount, and also emits it as an event for every show after that -- the
/// window is hidden rather than destroyed when dismissed (see overlay.rs),
/// so its page only mounts once per app run and needs a signal to reset
/// itself (clear the textarea, re-check for missed slots) each time it's
/// reused for a different occurrence.
#[tauri::command]
pub async fn open_catchup_window(
    app: AppHandle,
    state: State<'_, AppState>,
    slot_start_iso: String,
) -> Result<(), ()> {
    {
        let mut slot = state.catchup_slot.lock().unwrap();
        *slot = Some(slot_start_iso.clone());
    }
    // This runs at app boot, often within the webview startup race window
    // (see overlay::WEBVIEW_WARMUP) -- wait it out before showing.
    crate::overlay::wait_for_webview_warmup(&app).await;
    crate::overlay::spawn_catchup_window(&app);
    let _ = app.emit("catchup://slot", slot_start_iso);
    Ok(())
}

/// Read by the catch-up window on mount to learn which slot triggered it.
#[tauri::command]
pub fn get_catchup_slot(state: State<AppState>) -> Option<String> {
    state.catchup_slot.lock().unwrap().clone()
}

/// Triggers the wellness check-in popup for a slot whose reflection was just
/// saved. Called by the `catchup` window after "Save & close"; the live break
/// overlay's own close path calls `overlay::open_checkin_for_slot` directly
/// instead of going through this command.
#[tauri::command]
pub fn open_checkin_window(app: AppHandle, slot_start_iso: String) {
    overlay::open_checkin_for_slot(&app, slot_start_iso);
}

/// Read by the check-in window on mount to learn which slot triggered it.
#[tauri::command]
pub fn get_checkin_slot(state: State<AppState>) -> Option<String> {
    state.checkin_slot.lock().unwrap().clone()
}

/// Backs the Settings "Data" export/import feature. Plain `std::fs` rather
/// than tauri-plugin-fs: app-defined commands need no capability entry at
/// all, sidestepping that plugin's path-scope config entirely (the same
/// class of silent-permission trap already hit twice with sql/window
/// capabilities -- see CLAUDE.md). The path always comes from the native
/// dialog picker, not arbitrary user text input.
#[tauri::command]
pub fn read_text_file(path: String) -> Result<String, String> {
    std::fs::read_to_string(&path).map_err(|e| format!("failed to read {path}: {e}"))
}

#[tauri::command]
pub fn write_text_file(path: String, contents: String) -> Result<(), String> {
    std::fs::write(&path, contents).map_err(|e| format!("failed to write {path}: {e}"))
}

/// Reflects the actual OS registration state (Windows Run registry key /
/// equivalent elsewhere) -- this is the single source of truth for on/off,
/// not anything stored in app_setting. See `ensureDefaultAutostart` in
/// db.ts for why a *separate* DB flag exists to track whether the user has
/// ever made an explicit choice at all.
#[tauri::command]
pub fn get_autostart_enabled(app: AppHandle) -> bool {
    app.autolaunch().is_enabled().unwrap_or(false)
}

#[tauri::command]
pub fn set_autostart_enabled(app: AppHandle, enabled: bool) -> Result<(), String> {
    let auto = app.autolaunch();
    let result = if enabled { auto.enable() } else { auto.disable() };
    result.map_err(|e| e.to_string())
}

/// Mirrors app_setting.force_close_shortcut_enabled -- loaded and pushed here
/// by the frontend on boot and on every Settings save (see
/// loadAndSyncForceCloseShortcutSetting in db.ts).
#[tauri::command]
pub fn get_force_close_shortcut_enabled() -> bool {
    FORCE_CLOSE_SHORTCUT_ENABLED.load(Ordering::SeqCst)
}

#[tauri::command]
pub fn set_force_close_shortcut_enabled(enabled: bool) {
    FORCE_CLOSE_SHORTCUT_ENABLED.store(enabled, Ordering::SeqCst);
}

/// Mirrors app_setting.overlay_auto_close_minutes -- loaded and pushed here
/// by the frontend on boot and on every Settings save (see
/// loadAndSyncOverlayAutoClose in db.ts).
#[tauri::command]
pub fn get_overlay_auto_close_minutes() -> u32 {
    OVERLAY_AUTO_CLOSE_MINUTES.load(Ordering::SeqCst)
}

#[tauri::command]
pub fn set_overlay_auto_close_minutes(minutes: u32) {
    OVERLAY_AUTO_CLOSE_MINUTES.store(minutes.max(1), Ordering::SeqCst);
}

/// Mirrors app_setting.media_pause_on_break_enabled -- loaded and pushed here
/// by the frontend on boot and on every Settings save (see
/// loadAndSyncMediaPauseOnBreakSetting in db.ts).
#[tauri::command]
pub fn get_media_pause_on_break_enabled() -> bool {
    MEDIA_PAUSE_ON_BREAK_ENABLED.load(Ordering::SeqCst)
}

#[tauri::command]
pub fn set_media_pause_on_break_enabled(enabled: bool) {
    MEDIA_PAUSE_ON_BREAK_ENABLED.store(enabled, Ordering::SeqCst);
}

/// Only available when dev_mode is on -- bypasses the unlock formula entirely.
/// A permanent, non-dev-gated safety net also exists via the global shortcut
/// (see lib.rs) and the tray Quit item.
#[tauri::command]
pub fn dev_force_close(app: AppHandle, state: State<AppState>) -> Result<(), String> {
    if !state.dev_mode {
        return Err("dev mode is not enabled".into());
    }
    overlay::close_overlay(&app);
    Ok(())
}
