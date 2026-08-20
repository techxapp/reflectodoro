use std::sync::atomic::Ordering;

use tauri::{AppHandle, Emitter, State};

use crate::overlay;
use crate::state::{AppState, OverlayState};
use crate::POMODORO_ENABLED;

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
