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
#![cfg(target_os = "macos")]

use std::ffi::c_void;

use objc2::MainThreadMarker;
use objc2_app_kit::{
    NSApplication, NSApplicationPresentationOptions, NSScreenSaverWindowLevel, NSWindow,
    NSWindowCollectionBehavior,
};
use tauri::{AppHandle, WebviewWindow};

/// Runs on the overlay's main-thread dispatch. `hide_menu_bar_and_dock` is
/// read by the caller from `MACOS_HIDE_MENU_BAR_DOCK_ENABLED` at call time
/// (see overlay.rs) -- everything else here (Space-following, Cmd+Tab block)
/// applies unconditionally, every break, regardless of that setting.
pub fn enter_kiosk_mode(win: &WebviewWindow, hide_menu_bar_and_dock: bool) {
    // Cloned for the closure, not moved: `win` (the receiver below) stays
    // borrowed for the duration of the `run_on_main_thread` call itself, so
    // the `move` closure needs its own owned copy rather than trying to move
    // the same binding it's being called on.
    let win_for_closure = win.clone();
    if let Err(e) = win.run_on_main_thread(move || {
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
    ns_window.setCollectionBehavior(
        NSWindowCollectionBehavior::CanJoinAllSpaces
            | NSWindowCollectionBehavior::FullScreenAuxiliary
            | NSWindowCollectionBehavior::Stationary
            | NSWindowCollectionBehavior::IgnoresCycle,
    );
    ns_window.setLevel(NSScreenSaverWindowLevel);
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
        NSApplication::sharedApplication(mtm).setPresentationOptions(options);
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
    if let Err(e) = app.run_on_main_thread(|| {
        let Some(mtm) = MainThreadMarker::new() else {
            log::error!("macos_overlay: exit_kiosk_mode called off the main thread");
            return;
        };
        NSApplication::sharedApplication(mtm)
            .setPresentationOptions(NSApplicationPresentationOptions::Default);
    }) {
        log::warn!("macos_overlay::exit_kiosk_mode: run_on_main_thread failed: {e:?}");
    }
}
