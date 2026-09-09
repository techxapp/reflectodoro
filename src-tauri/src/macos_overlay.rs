//! macOS-only enforcement layer for the break overlay.
//!
//! Two AppKit mechanisms, both permission-free (no Accessibility prompt, no
//! TCC entry at all -- unlike a `CGEventTap`-based approach), split into an
//! always-on tier and an opt-in tier:
//!
//! - **Always on, no Settings toggle** (same as the rest of the overlay's
//!   enforcement -- fullscreen/always-on-top aren't toggleable either):
//!   `NSWindowCollectionBehavior` (`CanJoinAllSpaces` + `FullScreenAuxiliary`)
//!   makes the overlay follow the user to any Space, including one occupied
//!   by another app's true full-screen window. That alone isn't enough,
//!   though: `FullScreenAuxiliary` only lifts the window into a full-screen
//!   Space if its `NSWindow.level` is also raised above the level Tauri's own
//!   `.always_on_top(true)` sets (meant only for "stay above other windows in
//!   *this* Space") -- so this also bumps it to `NSScreenSaverWindowLevel`.
//!   `NSApplicationPresentationOptions.DisableProcessSwitching` (blocks
//!   Cmd+Tab) is also always on, for the same reason.
//! - **Gated on the `macos_hide_menu_bar_dock_enabled` app_setting** (off by
//!   default -- opt-in): `HideMenuBar` + `HideDock`, a more disruptive change
//!   to the user's desktop than blocking one keyboard shortcut.
//!
//! Both tiers deliberately never include `DisableForceQuit`: Activity
//! Monitor/Force Quit is kill switch #1 (see CLAUDE.md) and must always work,
//! exactly like Task Manager on Windows.
//!
//! `presentationOptions` is process-global, not per-window, so
//! `exit_kiosk_mode` must be called on every overlay-close path -- see its
//! call site in `close_overlay` (overlay.rs), which is the one function every
//! close path (unlock, F12 kill switch, dev force-close, the save-failure
//! escape hatch, auto-close) already funnels through.
//!
//! **The overlay window must never use real (`toggleFullScreen:`-driven)
//! fullscreen on macOS** -- `overlay.rs`'s `build_overlay_window` skips
//! `.fullscreen(true)` on this platform for exactly that reason, and
//! `cover_current_monitor` below is the substitute. Confirmed the hard way:
//! entering native fullscreen gives a window its own dedicated Space, and
//! `CanJoinAllSpaces` fundamentally contradicts owning one dedicated Space --
//! setting both at once (as an earlier version of this file did, unaware
//! `.fullscreen(true)` meant real fullscreen rather than a borderless window
//! sized to the screen) left the WindowServer unable to keep the window
//! consistently assigned to any Space at all. Symptom on a real Mac: the
//! break screen didn't appear on its own at the scheduled boundary, then
//! flashed briefly visible and vanished again the moment anything (e.g.
//! bringing the main window forward) forced a re-composite -- consistent
//! with a window stuck in a broken Space-assignment state rather than a
//! timing/scheduling bug. A plain borderless window sized to the full screen
//! (no dedicated Space of its own) is the standard, conflict-free way to get
//! "covers the screen, follows every Space" at once; that's the whole reason
//! `cover_current_monitor` exists instead of just calling `.fullscreen(true)`.
#![cfg(target_os = "macos")]

use std::ffi::c_void;

use objc2::MainThreadMarker;
use objc2_app_kit::{
    NSApplication, NSApplicationPresentationOptions, NSScreenSaverWindowLevel, NSWindow,
    NSWindowCollectionBehavior,
};
use tauri::{AppHandle, Position, Size, WebviewWindow};

/// Substitute for real fullscreen (see module doc for why real fullscreen is
/// never used here): sizes and positions the overlay window to exactly cover
/// whichever screen it's currently on, so it visually fills the screen like a
/// fullscreen window would, without the dedicated-Space side effect. Falls
/// back to the primary monitor if the window isn't associated with one yet
/// (e.g. its first-ever show). Must be called before `.show()` -- calling it
/// after would flash the window at its previous (small default) frame first.
pub fn cover_current_monitor(win: &WebviewWindow) {
    let monitor = match win.current_monitor() {
        Ok(Some(m)) => Some(m),
        Ok(None) => {
            log::info!("macos_overlay::cover_current_monitor: current_monitor() returned None, falling back to primary_monitor()");
            win.primary_monitor().unwrap_or(None)
        }
        Err(e) => {
            log::warn!("macos_overlay::cover_current_monitor: current_monitor() failed: {e:?}, falling back to primary_monitor()");
            win.primary_monitor().unwrap_or(None)
        }
    };
    let Some(monitor) = monitor else {
        log::warn!(
            "macos_overlay::cover_current_monitor: no monitor found (current_monitor() and primary_monitor() both empty), leaving the overlay at its previous size/position"
        );
        return;
    };
    log::info!(
        "macos_overlay::cover_current_monitor: covering monitor name={:?} position={:?} size={:?}",
        monitor.name(),
        monitor.position(),
        monitor.size()
    );
    if let Err(e) = win.set_position(Position::Physical(*monitor.position())) {
        log::warn!("macos_overlay::cover_current_monitor: set_position failed: {e:?}");
    }
    if let Err(e) = win.set_size(Size::Physical(*monitor.size())) {
        log::warn!("macos_overlay::cover_current_monitor: set_size failed: {e:?}");
    }
}

/// Runs on the overlay's main-thread dispatch. `hide_menu_bar_and_dock` is
/// read by the caller from `MACOS_HIDE_MENU_BAR_DOCK_ENABLED` at call time
/// (see overlay.rs) -- everything else here (Space-following, Cmd+Tab block)
/// applies unconditionally, every break, regardless of that setting.
pub fn enter_kiosk_mode(win: &WebviewWindow, hide_menu_bar_and_dock: bool) {
    log::info!(
        "macos_overlay::enter_kiosk_mode: queuing onto main thread (hide_menu_bar_and_dock={hide_menu_bar_and_dock})"
    );
    // Cloned for the closure, not moved: `win` (the receiver below) stays
    // borrowed for the duration of the `run_on_main_thread` call itself, so
    // the `move` closure needs its own owned copy rather than trying to move
    // the same binding it's being called on.
    let win_for_closure = win.clone();
    if let Err(e) = win.run_on_main_thread(move || {
        // Logged from inside the queued closure, not just at the call site
        // above: `run_on_main_thread` only *queues* the work, so this is the
        // line that actually confirms the main thread's event loop got
        // around to running it (rather than the closure being dropped, or
        // the loop stalling) -- worth distinguishing in logs if this ever
        // needs debugging again without live access to the machine.
        log::info!("macos_overlay::enter_kiosk_mode: running on main thread now");
        match win_for_closure.ns_window() {
            Ok(ptr) => configure_window(ptr),
            Err(e) => log::warn!("macos_overlay::enter_kiosk_mode: ns_window() failed: {e:?}"),
        }
        enable_presentation_lockdown(hide_menu_bar_and_dock);
    }) {
        log::warn!("macos_overlay::enter_kiosk_mode: run_on_main_thread failed: {e:?}");
    }
}

/// SAFETY: `ptr` comes from `WebviewWindow::ns_window()`, which returns the
/// live `NSWindow*` backing this window for as long as the window exists --
/// borrowed, not owned, so no release here.
///
/// Always-on (see module doc): makes the overlay follow the user to any
/// Space, dev_mode or not, `macos_hide_menu_bar_dock_enabled` or not.
fn configure_window(ptr: *mut c_void) {
    let ns_window: &NSWindow = unsafe { &*(ptr as *mut NSWindow) };
    let behavior = NSWindowCollectionBehavior::CanJoinAllSpaces
        | NSWindowCollectionBehavior::FullScreenAuxiliary
        | NSWindowCollectionBehavior::Stationary
        | NSWindowCollectionBehavior::IgnoresCycle;
    ns_window.setCollectionBehavior(behavior);
    ns_window.setLevel(NSScreenSaverWindowLevel);
    log::info!(
        "macos_overlay::configure_window: set collectionBehavior={:?} level={:?} (readback: collectionBehavior={:?} level={:?})",
        behavior,
        NSScreenSaverWindowLevel,
        ns_window.collectionBehavior(),
        ns_window.level()
    );
}

/// `DisableProcessSwitching` (blocks Cmd+Tab) is always included; `HideMenuBar`
/// + `HideDock` are only added when `hide_menu_bar_and_dock` is true (the
/// user's `macos_hide_menu_bar_dock_enabled` Settings toggle, off by default).
fn enable_presentation_lockdown(hide_menu_bar_and_dock: bool) {
    let Some(mtm) = MainThreadMarker::new() else {
        log::error!("macos_overlay: enable_presentation_lockdown called off the main thread");
        return;
    };
    let mut options = NSApplicationPresentationOptions::DisableProcessSwitching;
    if hide_menu_bar_and_dock {
        options |= NSApplicationPresentationOptions::HideMenuBar
            | NSApplicationPresentationOptions::HideDock;
    }

    // `-setPresentationOptions:` raises an NSException on an invalid flag
    // combination (Apple's own doc comment on the method). Neither
    // combination set above conflicts with itself or anything else set here,
    // so this shouldn't be reachable -- but catching it here still matters
    // for dev builds (panic = "unwind"): it turns a would-be process abort
    // into a logged, skipped kiosk-mode-for-this-break instead. It does NOT
    // help in the shipped release build, which sets panic = "abort"
    // (Cargo.toml's [profile.release]) -- objc2::exception::catch's own doc
    // comment states it cannot catch anything in that configuration, so an
    // exception there would still abort the whole process. See
    // macos_overlay.rs's module doc for why the flag choice itself is the
    // real mitigation.
    let result = objc2::exception::catch(|| {
        let app = NSApplication::sharedApplication(mtm);
        app.setPresentationOptions(options);
        log::info!(
            "macos_overlay::enable_presentation_lockdown: set presentationOptions={:?} (readback: {:?})",
            options,
            app.presentationOptions()
        );
    });
    if let Err(e) = result {
        log::error!("macos_overlay: setPresentationOptions raised an exception: {e:?}");
    }
}

/// Restores default presentation options (visible menu bar/Dock, Cmd+Tab
/// re-enabled). Unconditional and idempotent -- safe to call even if
/// `hide_menu_bar_and_dock` was never true for this occurrence (the setting
/// could have been toggled off mid-break), same "harmless no-op" pattern as
/// `media::resume_playing_sessions` and `hook::uninstall()`.
pub fn exit_kiosk_mode(app: &AppHandle) {
    log::info!("macos_overlay::exit_kiosk_mode: queuing onto main thread");
    if let Err(e) = app.run_on_main_thread(|| {
        let Some(mtm) = MainThreadMarker::new() else {
            log::error!("macos_overlay: exit_kiosk_mode called off the main thread");
            return;
        };
        NSApplication::sharedApplication(mtm)
            .setPresentationOptions(NSApplicationPresentationOptions::Default);
        log::info!("macos_overlay::exit_kiosk_mode: presentationOptions restored to Default");
    }) {
        log::warn!("macos_overlay::exit_kiosk_mode: run_on_main_thread failed: {e:?}");
    }
}
