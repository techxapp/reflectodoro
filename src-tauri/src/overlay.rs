use std::sync::atomic::Ordering;

use tauri::{AppHandle, Emitter, Manager};
#[cfg(desktop)]
use tauri::{WebviewUrl, WebviewWindow, WebviewWindowBuilder, WindowEvent};

use crate::state::{AppState, OverlayState};

pub const OVERLAY_LABEL: &str = "overlay";
pub const CHECKIN_LABEL: &str = "checkin";

/// A freshly created WebviewWindow on this machine renders permanently blank
/// if shown within roughly this long of process start (reproduced directly:
/// a window shown ~1-2s after boot stays blank forever -- no content, no
/// devtools output -- while the same window shown 6s after boot renders
/// fine). 6s is the only empirically-confirmed-safe value; there's no data
/// point between "1-2s: broken" and "6s: fine", so this doesn't shave that
/// margin down without a reason to believe it's still safe. The check-in
/// window is pre-created hidden in `precreate_windows` so it gets a head
/// start on whatever WebView2/wry is doing during that window, and callers
/// await this before the first `show()`.
const WEBVIEW_WARMUP: std::time::Duration = std::time::Duration::from_millis(6000);

pub async fn wait_for_webview_warmup(app: &AppHandle) {
    let elapsed = app.state::<AppState>().started_at.elapsed();
    if elapsed < WEBVIEW_WARMUP {
        tokio::time::sleep(WEBVIEW_WARMUP - elapsed).await;
    }
}

/// Builds the check-in window hidden, immediately on app start, so it
/// has as much of a head start as possible on `WEBVIEW_WARMUP` before
/// anything ever tries to show them. Safe to call more than once (e.g. after
/// a window was destroyed) -- no-ops if the label already exists.
///
/// Desktop only: on mobile there is exactly one Activity/window, and
/// empirically creating a second `WebviewWindow` doesn't layer a new
/// surface over it the way a second OS window would on desktop -- it
/// replaces the single Activity's visible content, leaving "main" empty and
/// detached. See the module-level note on `spawn_or_update_overlay`.
#[cfg(desktop)]
pub fn precreate_windows(app: &AppHandle) {
    if app.get_webview_window(OVERLAY_LABEL).is_none() {
        log::info!("precreate_windows: building {OVERLAY_LABEL}");
        build_overlay_window(app, false);
    }
    if app.get_webview_window(CHECKIN_LABEL).is_none() {
        log::info!("precreate_windows: building {CHECKIN_LABEL}");
        build_checkin_window(app, false);
    }
}

#[cfg(mobile)]
pub fn precreate_windows(_app: &AppHandle) {}

#[cfg(desktop)]
fn build_overlay_window(app: &AppHandle, visible: bool) -> WebviewWindow {
    let win = WebviewWindowBuilder::new(app, OVERLAY_LABEL, WebviewUrl::App("overlay".into()))
        .title("Break")
        .resizable(false)
        .visible(visible)
        .fullscreen(true)
        .decorations(false)
        .always_on_top(true)
        .skip_taskbar(true)
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
///
/// On mobile there's no window to show at all -- the single window's own
/// frontend reacts to the `overlay://state` event emitted below by
/// navigating itself to `/overlay` (see `+layout.svelte`), since a second
/// `WebviewWindow` doesn't layer over "main" there the way it does on
/// desktop (confirmed empirically: it replaces the single Activity's
/// visible content instead).
pub fn spawn_or_update_overlay(app: &AppHandle) {
    #[cfg(desktop)]
    {
        let win = match app.get_webview_window(OVERLAY_LABEL) {
            Some(win) => win,
            None => build_overlay_window(app, false),
        };

        if !win.is_visible().unwrap_or(false) {
            let dev_mode = app.state::<AppState>().dev_mode;
            let _ = win.show();
            let _ = win.set_focus();
            if crate::MEDIA_PAUSE_ON_BREAK_ENABLED.load(Ordering::SeqCst) {
                crate::media::pause_playing_sessions(app);
            }
            if !dev_mode {
                crate::hook::install();
            }
        }
    }

    // No "already showing" guard to mirror on mobile: this is only ever
    // called on an actual Work->Break transition (see run_scheduler), never
    // while already in Break, so there's no merge case to detect here.
    #[cfg(mobile)]
    if crate::MEDIA_PAUSE_ON_BREAK_ENABLED.load(Ordering::SeqCst) {
        crate::media::pause_playing_sessions(app);
    }

    // Posts a break notification if the app isn't already visible -- a
    // no-op (decided Kotlin-side) if it is, since the frontend's own
    // overlay://state listener (about to fire below) already handles
    // showing /overlay for an app the user is already looking at. Not a
    // full-screen-intent/auto-launch: Android only honors that over a
    // *locked* screen, and the point here is specifically to interrupt
    // active phone use, not to wake an idle/locked one -- see
    // BREAK_NOTIFICATION_PERSISTENT_ENABLED's doc comment.
    #[cfg(target_os = "android")]
    {
        let bridge = app.state::<crate::android_bridge::AndroidBridge<tauri::Wry>>();
        let persistent = crate::BREAK_NOTIFICATION_PERSISTENT_ENABLED.load(Ordering::SeqCst);
        let state_json = overlay_state_json_for_android(app);
        if let Err(e) = bridge.trigger_break_screen(persistent, state_json) {
            log::error!("trigger_break_screen failed: {e:?}");
        }
    }

    emit_state(app);
}

/// OverlayState serialized with an extra `dev_mode` field -- the native
/// overlay's plain WebView has no Tauri command access of its own to call
/// `is_dev_mode` the way the regular /overlay page does, so it needs this
/// riding along in every state push instead (see NativeOverlayManager.kt's
/// dev-close button).
#[cfg(target_os = "android")]
fn overlay_state_json_for_android(app: &AppHandle) -> serde_json::Value {
    let state = app.state::<AppState>();
    let snapshot = state.overlay.lock().unwrap().clone();
    let mut json = serde_json::to_value(&snapshot).unwrap_or_default();
    if let serde_json::Value::Object(map) = &mut json {
        map.insert("dev_mode".into(), serde_json::json!(state.dev_mode));
    }
    json
}

pub fn emit_state(app: &AppHandle) {
    let state = app.state::<AppState>();
    let snapshot = state.overlay.lock().unwrap().clone();

    // Keeps the native WindowManager overlay (if it's currently showing --
    // Kotlin decides that, not Rust) in sync with every state change: the
    // breakit counter, reflection_entered flipping, or a merged slot's new
    // challenge. Harmless no-op JNI round trip when it isn't showing.
    #[cfg(target_os = "android")]
    {
        let bridge = app.state::<crate::android_bridge::AndroidBridge<tauri::Wry>>();
        if let Err(e) = bridge.update_native_overlay(overlay_state_json_for_android(app)) {
            log::error!("update_native_overlay failed: {e:?}");
        }
    }

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

/// On mobile there's no separate window to hide -- `emit_state` below tells
/// `+layout.svelte`'s listener to route the single window back to `/`
/// instead (desktop doesn't need this: hiding the overlay window already
/// reveals "main" underneath without any frontend action).
pub fn close_overlay(app: &AppHandle) {
    let (slot_start, had_reflection) = {
        let state = app.state::<AppState>();
        let mut overlay = state.overlay.lock().unwrap();
        let slot_start = overlay.current_slot_start.clone();
        let had_reflection = overlay.reflection_entered;
        *overlay = OverlayState::closed();
        (slot_start, had_reflection)
    };

    if crate::MEDIA_PAUSE_ON_BREAK_ENABLED.load(Ordering::SeqCst) {
        crate::media::resume_playing_sessions(app);
    }

    #[cfg(desktop)]
    {
        // Hidden, not destroyed: keeps the webview warm so the *next* break
        // doesn't have to pay the creation cost (or risk the startup
        // blank-page race) all over again.
        if let Some(win) = app.get_webview_window(OVERLAY_LABEL) {
            let _ = win.hide();
        }
    }
    #[cfg(mobile)]
    emit_state(app);

    // Clears a break notification the user resolved some other way than
    // tapping it (e.g. they'd already switched back to the app on their
    // own) so it doesn't linger after the fact.
    #[cfg(target_os = "android")]
    {
        let bridge = app.state::<crate::android_bridge::AndroidBridge<tauri::Wry>>();
        if let Err(e) = bridge.cancel_break_notification() {
            log::error!("cancel_break_notification failed: {e:?}");
        }
    }

    crate::hook::uninstall();

    // Only prompt for the wellness check-in when a reflection was actually
    // recorded for this slot -- excludes force-closes (dev "Close (DEV)",
    // the Ctrl+Alt+Shift+F12 kill switch) where nothing was ever saved to
    // link a wellness_check row to.
    if had_reflection && !slot_start.is_empty() {
        open_checkin_for_slot(app, slot_start);
    }
}

/// Backs the `checkin` popup: decorated (so the native title bar
/// close/restore controls work), maximized, and always-on-top --
/// deliberately NOT the enforcement mechanism the live break overlay is (no
/// keyboard hook, no close-requested trap): by the time this window is
/// shown, there's no timer left to enforce, just an optional follow-up the
/// user can dismiss.
///
/// Desktop only -- see `precreate_windows`.
#[cfg(desktop)]
fn build_popup_window(app: &AppHandle, label: &str, page: &str, title: &str, visible: bool) -> WebviewWindow {
    let win = WebviewWindowBuilder::new(app, label, WebviewUrl::App(page.into()))
        .title(title)
        .visible(visible)
        .maximized(true)
        .always_on_top(true)
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

#[cfg(desktop)]
fn build_checkin_window(app: &AppHandle, visible: bool) -> WebviewWindow {
    build_popup_window(app, CHECKIN_LABEL, "checkin", "Wellness Check-in", visible)
}

/// Shows the (already pre-created) check-in window, or builds it fresh if it
/// was destroyed some other way. On mobile this is a no-op: the caller,
/// `open_checkin_for_slot`, already emits `checkin://slot` unconditionally,
/// which `+layout.svelte` listens for to navigate the single window to
/// `/checkin` itself.
#[cfg(desktop)]
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

#[cfg(mobile)]
pub fn spawn_checkin_window(_app: &AppHandle) {}

/// Triggers the wellness check-in, called from `close_overlay` when the live
/// break overlay finishes with a reflection saved. Stores the slot for the
/// window's own `get_checkin_slot` call on first mount, shows the window, and
/// emits an event for every reuse after that -- the window is hidden rather
/// than destroyed between uses, so its page mounts once per app run and needs
/// a signal to reset itself for each new occurrence.
pub fn open_checkin_for_slot(app: &AppHandle, slot_start_iso: String) {
    log::info!("open_checkin_for_slot: setting checkin_slot={slot_start_iso}");
    {
        let state = app.state::<AppState>();
        let mut slot = state.checkin_slot.lock().unwrap();
        *slot = Some(slot_start_iso.clone());
    }
    spawn_checkin_window(app);
    let _ = app.emit("checkin://slot", slot_start_iso);
}
