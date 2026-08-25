use std::sync::atomic::Ordering;

use tauri::{
    AppHandle, Emitter, Manager, WebviewUrl, WebviewWindow, WebviewWindowBuilder, WindowEvent,
};

use crate::state::{AppState, OverlayState};

pub const OVERLAY_LABEL: &str = "overlay";
pub const CATCHUP_LABEL: &str = "catchup";
pub const CHECKIN_LABEL: &str = "checkin";

/// A freshly created WebviewWindow on this machine renders permanently blank
/// if shown within roughly this long of process start (reproduced directly:
/// a window shown ~1-2s after boot stays blank forever -- no content, no
/// devtools output -- while the same window shown 6s after boot renders
/// fine). 6s is the only empirically-confirmed-safe value; there's no data
/// point between "1-2s: broken" and "6s: fine", so this doesn't shave that
/// margin down without a reason to believe it's still safe. Both special
/// windows are pre-created hidden in `precreate_windows` so they get a head
/// start on whatever WebView2/wry is doing during that window, and callers
/// await this before the first `show()`.
const WEBVIEW_WARMUP: std::time::Duration = std::time::Duration::from_millis(6000);

pub async fn wait_for_webview_warmup(app: &AppHandle) {
    let elapsed = app.state::<AppState>().started_at.elapsed();
    if elapsed < WEBVIEW_WARMUP {
        tokio::time::sleep(WEBVIEW_WARMUP - elapsed).await;
    }
}

/// Builds both special windows hidden, immediately on app start, so they
/// have as much of a head start as possible on `WEBVIEW_WARMUP` before
/// anything ever tries to show them. Safe to call more than once (e.g. after
/// a window was destroyed) -- no-ops if the label already exists.
pub fn precreate_windows(app: &AppHandle) {
    if app.get_webview_window(OVERLAY_LABEL).is_none() {
        build_overlay_window(app, false);
    }
    if app.get_webview_window(CATCHUP_LABEL).is_none() {
        build_catchup_window(app, false);
    }
    if app.get_webview_window(CHECKIN_LABEL).is_none() {
        build_checkin_window(app, false);
    }
}

fn build_overlay_window(app: &AppHandle, visible: bool) -> WebviewWindow {
    let win = WebviewWindowBuilder::new(app, OVERLAY_LABEL, WebviewUrl::App("overlay".into()))
        .title("Break")
        .fullscreen(true)
        .decorations(false)
        .always_on_top(true)
        .skip_taskbar(true)
        .resizable(false)
        .visible(visible)
        .focused(visible)
        .build()
        .expect("failed to build overlay window");

    let app_handle = app.clone();
    win.on_window_event(move |event| {
        if let WindowEvent::CloseRequested { api, .. } = event {
            api.prevent_close();
            let _ = app_handle.emit("overlay://close-blocked", ());
        }
    });

    win
}

/// Shows the (already pre-created) overlay window, or builds it fresh if it
/// was destroyed some other way -- otherwise just re-emits current state
/// (the merge case, where an already-open overlay gets a new prompt covering
/// an additional slot). Callers that are showing it for the first time this
/// break should `wait_for_webview_warmup` first; see `run_scheduler`.
pub fn spawn_or_update_overlay(app: &AppHandle) {
    let win = match app.get_webview_window(OVERLAY_LABEL) {
        Some(win) => win,
        None => build_overlay_window(app, false),
    };

    if !win.is_visible().unwrap_or(false) {
        let dev_mode = app.state::<AppState>().dev_mode;
        let _ = win.show();
        let _ = win.set_focus();
        if crate::MEDIA_PAUSE_ON_BREAK_ENABLED.load(Ordering::SeqCst) {
            crate::media::pause_playing_sessions();
        }
        if !dev_mode {
            crate::hook::install();
        }
    }

    emit_state(app);
}

pub fn emit_state(app: &AppHandle) {
    let state = app.state::<AppState>();
    let snapshot = state.overlay.lock().unwrap().clone();
    let _ = app.emit("overlay://state", snapshot);
}

/// Checks the unlock formula and closes the overlay if satisfied; otherwise
/// just broadcasts the updated state so the overlay UI can reflect progress
/// (e.g. breakit counter incrementing).
pub fn try_close_if_unlocked(app: &AppHandle) {
    let should_close = {
        let state = app.state::<AppState>();
        let overlay = state.overlay.lock().unwrap();
        overlay.unlocked()
    };
    if should_close {
        close_overlay(app);
    } else {
        emit_state(app);
    }
}

/// Force-closes the overlay `OVERLAY_AUTO_CLOSE_MINUTES` after this is
/// called (from the break->work transition in `run_scheduler`) if it's still
/// showing the *same* occurrence by then -- even without a reflection. The
/// `current_slot_start` equality check is what makes this safe to fire
/// unconditionally: if the user already resolved it (reflection/breakit) or
/// it rolled into a newer slot via the merge path before the timer elapsed,
/// this is a no-op rather than closing the wrong occurrence.
pub fn schedule_auto_close(app: &AppHandle, slot_start: String) {
    let minutes = crate::OVERLAY_AUTO_CLOSE_MINUTES.load(std::sync::atomic::Ordering::SeqCst);
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        tokio::time::sleep(std::time::Duration::from_secs(minutes as u64 * 60)).await;
        let still_pending = {
            let state = app.state::<AppState>();
            let overlay = state.overlay.lock().unwrap();
            overlay.open && overlay.current_slot_start == slot_start
        };
        if still_pending {
            close_overlay(&app);
        }
    });
}

pub fn close_overlay(app: &AppHandle) {
    let (slot_start, had_reflection) = {
        let state = app.state::<AppState>();
        let mut overlay = state.overlay.lock().unwrap();
        let slot_start = overlay.current_slot_start.clone();
        let had_reflection = overlay.reflection_entered;
        *overlay = OverlayState::closed();
        (slot_start, had_reflection)
    };
    // Hidden, not destroyed: keeps the webview warm so the *next* break
    // doesn't have to pay the creation cost (or risk the startup blank-page
    // race) all over again.
    if let Some(win) = app.get_webview_window(OVERLAY_LABEL) {
        let _ = win.hide();
    }
    crate::hook::uninstall();

    // The catch-up window (if it's up -- it can only ever be open at the
    // same time as the live overlay right after app boot, see
    // get_startup_catchup_slot) prompts for a slot that the overlay's own
    // merge logic (findMissedSlots in the frontend) may have just covered
    // too. Whatever closed the overlay just now -- reflection, breakit,
    // auto-close timeout, or a kill switch -- that catch-up prompt is stale
    // either way, so dismiss it along with the overlay rather than leaving
    // it sitting open to ask about a slot the user just handled (or that
    // timed out same as this one did).
    if let Some(win) = app.get_webview_window(CATCHUP_LABEL) {
        let _ = win.hide();
    }

    // Only prompt for the wellness check-in when a reflection was actually
    // recorded for this slot -- excludes force-closes (dev "Close (DEV)",
    // the Ctrl+Alt+Shift+F12 kill switch) where nothing was ever saved to
    // link a wellness_check row to.
    if had_reflection && !slot_start.is_empty() {
        open_checkin_for_slot(app, slot_start);
    }
}

/// Shared by the `catchup` and `checkin` popups: decorated (so the native
/// title bar close/restore controls work), maximized, and always-on-top --
/// deliberately NOT the enforcement mechanism the live break overlay is (no
/// keyboard hook, no close-requested trap): by the time either of these
/// windows is shown, there's no timer left to enforce, just an optional
/// follow-up the user can dismiss.
fn build_popup_window(app: &AppHandle, label: &str, page: &str, title: &str, visible: bool) -> WebviewWindow {
    let win = WebviewWindowBuilder::new(app, label, WebviewUrl::App(page.into()))
        .title(title)
        .maximized(true)
        .always_on_top(true)
        .visible(visible)
        .focused(visible)
        .build()
        .expect("failed to build popup window");

    // Closing (native X, or the page's own Dismiss/Skip/Save & close) hides
    // rather than destroys, so the webview stays warm for reuse next time
    // instead of risking the startup blank-page race all over again.
    let win_for_close = win.clone();
    win.on_window_event(move |event| {
        if let WindowEvent::CloseRequested { api, .. } = event {
            api.prevent_close();
            let _ = win_for_close.hide();
        }
    });

    win
}

fn build_catchup_window(app: &AppHandle, visible: bool) -> WebviewWindow {
    build_popup_window(app, CATCHUP_LABEL, "catchup", "Missed Reflectodoro", visible)
}

fn build_checkin_window(app: &AppHandle, visible: bool) -> WebviewWindow {
    build_popup_window(app, CHECKIN_LABEL, "checkin", "Wellness Check-in", visible)
}

/// Shows the (already pre-created) catch-up window, or builds it fresh if it
/// was destroyed some other way. Callers should `wait_for_webview_warmup`
/// first; see `commands::open_catchup_window`.
pub fn spawn_catchup_window(app: &AppHandle) {
    match app.get_webview_window(CATCHUP_LABEL) {
        Some(win) => {
            let _ = win.show();
            let _ = win.set_focus();
        }
        None => {
            build_catchup_window(app, true);
        }
    }
}

/// Shows the (already pre-created) check-in window, or builds it fresh if it
/// was destroyed some other way.
pub fn spawn_checkin_window(app: &AppHandle) {
    match app.get_webview_window(CHECKIN_LABEL) {
        Some(win) => {
            let _ = win.show();
            let _ = win.set_focus();
        }
        None => {
            build_checkin_window(app, true);
        }
    }
}

/// Single entry point for triggering the wellness check-in, called both from
/// `close_overlay` (the live break overlay finishing with a reflection saved)
/// and from `commands::open_checkin_window` (the startup catch-up window's
/// "Save & close"). Stores the slot for the window's own `get_checkin_slot`
/// call on first mount, shows the window, and emits an event for every reuse
/// after that -- the window is hidden rather than destroyed between uses, so
/// its page mounts once per app run and needs a signal to reset itself for
/// each new occurrence.
pub fn open_checkin_for_slot(app: &AppHandle, slot_start_iso: String) {
    {
        let state = app.state::<AppState>();
        let mut slot = state.checkin_slot.lock().unwrap();
        *slot = Some(slot_start_iso.clone());
    }
    spawn_checkin_window(app);
    let _ = app.emit("checkin://slot", slot_start_iso);
}
