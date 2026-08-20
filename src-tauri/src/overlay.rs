use tauri::{
    AppHandle, Emitter, LogicalPosition, Manager, WebviewUrl, WebviewWindow, WebviewWindowBuilder,
    WindowEvent,
};

use crate::state::{AppState, OverlayState};

pub const OVERLAY_LABEL: &str = "overlay";
pub const CATCHUP_LABEL: &str = "catchup";

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

pub fn close_overlay(app: &AppHandle) {
    {
        let state = app.state::<AppState>();
        let mut overlay = state.overlay.lock().unwrap();
        *overlay = OverlayState::closed();
    }
    // Hidden, not destroyed: keeps the webview warm so the *next* break
    // doesn't have to pay the creation cost (or risk the startup blank-page
    // race) all over again.
    if let Some(win) = app.get_webview_window(OVERLAY_LABEL) {
        let _ = win.hide();
    }
    crate::hook::uninstall();
}

/// Small, ordinary (decorated, closable, not-fullscreen) always-on-top window
/// docked near the top of the screen, used at app startup when a break was
/// left unreflected -- deliberately NOT the enforcement mechanism the live
/// break overlay is (no keyboard hook, no close-requested trap): the break
/// it's asking about has already ended, so there's no timer left to enforce,
/// just a reminder that shouldn't be able to get the user stuck.
fn build_catchup_window(app: &AppHandle, visible: bool) -> WebviewWindow {
    let width = 720.0;
    let height = 460.0;

    let win = WebviewWindowBuilder::new(app, CATCHUP_LABEL, WebviewUrl::App("catchup".into()))
        .title("Missed Reflectodoro")
        .inner_size(width, height)
        .resizable(false)
        .always_on_top(true)
        .visible(visible)
        .focused(visible)
        .center()
        .build()
        .expect("failed to build catchup window");

    // Closing (native X, or the page's own Dismiss/Save & close) hides
    // rather than destroys, so the webview stays warm for reuse next time
    // instead of risking the startup blank-page race all over again.
    let win_for_close = win.clone();
    win.on_window_event(move |event| {
        if let WindowEvent::CloseRequested { api, .. } = event {
            api.prevent_close();
            let _ = win_for_close.hide();
        }
    });

    // .center() above centers on both axes as a safe fallback; nudge it up to
    // sit near the top of the screen instead, per the user's preference over
    // the previous fullscreen version.
    if let Ok(Some(monitor)) = win.primary_monitor() {
        let scale = monitor.scale_factor();
        let screen_x = monitor.position().x as f64 / scale;
        let screen_w = monitor.size().width as f64 / scale;
        let x = screen_x + (screen_w - width) / 2.0;
        let y = monitor.position().y as f64 / scale + 32.0;
        let _ = win.set_position(LogicalPosition::new(x, y));
    }

    win
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
