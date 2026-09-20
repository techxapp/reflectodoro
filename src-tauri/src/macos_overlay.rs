//! macOS-only enforcement layer for the break overlay.
//!
//! AppKit mechanisms, all permission-free (no Accessibility prompt, no TCC
//! entry at all -- unlike a `CGEventTap`-based approach), split into an
//! always-on tier and an opt-in tier:
//!
//! - **Always on, no Settings toggle** (same as the rest of the overlay's
//!   enforcement -- fullscreen/always-on-top aren't toggleable either):
//!   - The overlay window is **created** while the process is temporarily an
//!     *accessory* app (`with_accessory_policy`, wrapped around the build in
//!     `overlay::precreate_windows`). Since macOS 10.14 a regular (Dock-icon)
//!     app's windows can't float over *another app's* full-screen Space -- and
//!     the measurements in `with_accessory_policy`'s doc comment show that
//!     eligibility is fixed at window-*creation* time, so switching the policy
//!     later cannot rescue a window that was born under `Regular`. This is the
//!     load-bearing step; everything below is necessary but not sufficient.
//!   - The process *also* switches to the accessory policy for the duration of
//!     a break (`enter_accessory_policy`, restored to regular in
//!     `exit_kiosk_mode`). That is no longer what makes the overlay cover a
//!     full-screen Space (see above) -- it is kept because
//!     `presentationOptions` (the Cmd+Tab block) only applies while this app is
//!     active, and because an app with a Dock icon mid-break would otherwise
//!     offer the user a Cmd+Tab entry back out. Side effect: no Dock icon or
//!     Cmd+Tab entry while a break is open.
//!   - `NSWindowCollectionBehavior` (`CanJoinAllSpaces` + `FullScreenAuxiliary`)
//!     makes the overlay follow the user to any Space. `FullScreenAuxiliary`
//!     only lifts the window into a full-screen Space if its `NSWindow.level`
//!     is also raised above the level Tauri's own `.always_on_top(true)` sets
//!     (meant only for "stay above other windows in *this* Space") -- so this
//!     also bumps it to `NSScreenSaverWindowLevel`.
//!   - `NSApplicationPresentationOptions.DisableProcessSwitching` (blocks
//!     Cmd+Tab). AppKit refuses that flag unless a Dock flag comes with it
//!     (see `enable_presentation_lockdown` for the exact rules), so
//!     `AutoHideDock` rides along with it by default. Presentation options only
//!     apply while this app is *active*, which is why the overlay also
//!     re-requests activation (`reassert_front_after_delay`).
//! - **Gated on the `macos_hide_menu_bar_dock_enabled` app_setting** (off by
//!   default -- opt-in): upgrades that to `HideDock` + `HideMenuBar`, a more
//!   disruptive change to the user's desktop than blocking one keyboard
//!   shortcut. The toggle therefore picks *which* Dock flag accompanies the
//!   Cmd+Tab block, not whether one is present -- one always is.
//!
//! Both tiers deliberately never include `DisableForceQuit`: Activity
//! Monitor/Force Quit is kill switch #1 (see CLAUDE.md) and must always work,
//! exactly like Task Manager on Windows.
//!
//! `presentationOptions` and the activation policy are process-global, not
//! per-window, so `exit_kiosk_mode` must be called on every overlay-close
//! path -- see its call site in `close_overlay` (overlay.rs), which is the one
//! function every close path (unlock, F12 kill switch, dev force-close, the
//! save-failure escape hatch, auto-close) already funnels through.
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
use std::time::Duration;

use objc2::MainThreadMarker;
use objc2_app_kit::{
    NSApplication, NSApplicationPresentationOptions, NSScreenSaverWindowLevel, NSWindow,
    NSWindowCollectionBehavior,
};
use tauri::{ActivationPolicy, AppHandle, Manager, Position, Size, WebviewWindow};

use crate::state::AppState;

/// Activation-policy changes reach the WindowServer asynchronously with no
/// completion signal, so the overlay is re-ordered front once more after this.
const REASSERT_DELAY: Duration = Duration::from_secs(1);

/// Must run before the overlay's `.show()` -- see the module doc. Tauri queues
/// this onto the main thread in order with the show/focus calls after it.
///
/// **Not** what lets the overlay cover another app's full-screen Space: that is
/// decided when the window is *created* (see `with_accessory_policy`). This is
/// kept for `presentationOptions`/Cmd+Tab, and is hoisted above the window
/// lookup in `spawn_or_update_overlay` so the rare rebuild path there also
/// creates its window under the accessory policy.
pub fn enter_accessory_policy(app: &AppHandle) {
    log::info!("macos_overlay::enter_accessory_policy: switching to Accessory activation policy");
    if let Err(e) = app.set_activation_policy(ActivationPolicy::Accessory) {
        log::error!("macos_overlay::enter_accessory_policy: set_activation_policy failed: {e:?}");
    }
}

/// `NSApplicationActivationPolicy::Accessory`'s raw value, for comparing
/// against `app_policy_and_active()`'s readback.
const POLICY_ACCESSORY: isize = 1;

/// Builds a window while the process is temporarily an *accessory* app, then
/// restores the policy that was in effect before.
///
/// **This, not the collection behavior, is what decides whether the overlay can
/// ever cover another app's full-screen Space.** Measured directly on this Mac
/// with a standalone AppKit probe (a second app put *itself* into real
/// fullscreen, so no permissions were involved), each run verified externally
/// via `CGWindowListCopyWindowInfo(.optionOnScreenOnly)` -- which lists only
/// windows on the *active* Space -- rather than trusting AppKit's own
/// readbacks:
///
/// | window created while process was | policy at show time | on the full-screen Space? |
/// |----------------------------------|---------------------|---------------------------|
/// | `Regular`                        | `Accessory`         | **no**                    |
/// | `Regular`                        | `Accessory` (+ `TransformProcessType`, `setCanHide:NO`, re-ordering, retries) | **no** |
/// | `Accessory`                      | `Accessory`         | yes                       |
/// | `Accessory`                      | `Regular`           | yes                       |
///
/// So the WindowServer fixes a window's eligibility to join a full-screen Space
/// when the `NSWindow` is **created**, from the process's activation policy at
/// that instant -- not when the window is ordered in, and not from anything
/// re-applied afterwards. A window born under `Regular` is permanently
/// ineligible; a window born under `Accessory` stays eligible even after the
/// process goes back to `Regular`.
///
/// That is exactly why the previous fix (switch to `Accessory`, apply
/// `CanJoinAllSpaces` before `.show()`, re-order the window in on failure) could
/// not work: `precreate_windows` builds the overlay at app start, while the
/// process is still `Regular`, and nothing after that point can undo it.
/// A real user's log showed every readback correct -- policy 1, behavior 337,
/// level 1000, `isVisible=true` -- and `isOnActiveSpace=false` anyway, with
/// `force_space_replacement` firing and changing nothing.
///
/// The wrap is deliberately narrow (create-time only) so the app keeps its Dock
/// icon and Cmd+Tab entry outside of breaks: per the table above, the policy at
/// *show* time is irrelevant to Space membership.
///
/// Must be called on the main thread to be correct. `AppHandle::set_activation_policy`
/// goes through `tauri-runtime-wry`'s `send_user_message`, which runs the message
/// **inline** when the caller is the main thread and only *queues* it otherwise
/// (`tauri-runtime-wry-2.11.4/src/lib.rs:239`) -- so off the main thread the
/// switch may not have landed by the time `build` creates the window, which is
/// the one thing this function exists to guarantee. Logged rather than enforced,
/// since the ordering still usually holds (the queued policy message is
/// processed before the queued window-creation message).
pub fn with_accessory_policy<R>(app: &AppHandle, build: impl FnOnce() -> R) -> R {
    let previous = app_policy_and_active().map(|(policy, _)| policy);
    if previous.is_none() {
        log::warn!(
            "macos_overlay::with_accessory_policy: not on the main thread -- the activation-policy switch is queued rather than applied inline, so the window may be created under the old policy"
        );
    }
    if let Err(e) = app.set_activation_policy(ActivationPolicy::Accessory) {
        log::error!(
            "macos_overlay::with_accessory_policy: set_activation_policy(Accessory) failed: {e:?} -- the window built below will not be able to cover another app's full-screen Space"
        );
    }

    let built = build();

    // Restored only if the process wasn't already Accessory. It normally is
    // not (this runs at app start, from `.setup()`), but the overlay can also
    // be rebuilt mid-break by `spawn_or_update_overlay`'s fallback path, and
    // dropping back to Regular there would undo `enter_accessory_policy` in
    // the middle of a live break.
    if previous != Some(POLICY_ACCESSORY) {
        match app.set_activation_policy(ActivationPolicy::Regular) {
            Ok(()) => log::info!(
                "macos_overlay::with_accessory_policy: window built under Accessory, policy restored to Regular (readback: {:?})",
                app_policy_and_active()
            ),
            Err(e) => log::error!(
                "macos_overlay::with_accessory_policy: set_activation_policy(Regular) failed: {e:?} -- the app may be left with no Dock icon"
            ),
        }
    } else {
        log::info!(
            "macos_overlay::with_accessory_policy: window built under Accessory, policy left as-is (was already Accessory)"
        );
    }

    built
}

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
    // Size *before* position, and the order is load-bearing on macOS. AppKit
    // frames are bottom-left origin, so tao converts the top-left position
    // handed to `set_position` using the window's height *at that moment*, and
    // a later `set_size` then grows the window upwards from its fixed
    // bottom-left corner. Positioning first therefore placed the overlay using
    // the 800x600 default it still had, and the resize to full height pushed
    // its top edge off-screen by exactly (screen height - 600): measured on a
    // 1920x1080 screen as `y=-480 h=1080` in CGWindowList, i.e. the overlay
    // covered only the top 600px and left the bottom 480px of the desktop --
    // and whatever app was under it -- fully visible and clickable during a
    // break. Sizing first makes the conversion use the final height, landing
    // the window at y=0.
    if let Err(e) = win.set_size(Size::Physical(*monitor.size())) {
        log::warn!("macos_overlay::cover_current_monitor: set_size failed: {e:?}");
    }
    if let Err(e) = win.set_position(Position::Physical(*monitor.position())) {
        log::warn!("macos_overlay::cover_current_monitor: set_position failed: {e:?}");
    }
    // Logged as an outcome, not just an attempt (see CLAUDE.md's note on
    // silently-discarded Results): this is the pair that says whether the
    // overlay actually ended up covering the screen.
    log::info!(
        "macos_overlay::cover_current_monitor: after resize/move, outer_position={:?} outer_size={:?}",
        win.outer_position(),
        win.outer_size()
    );
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
    reassert_front_after_delay(win);
}

/// `(activationPolicy, isActive)` for log lines; policy 0 = Regular, 1 = Accessory.
fn app_policy_and_active() -> Option<(isize, bool)> {
    let mtm = MainThreadMarker::new()?;
    let app = NSApplication::sharedApplication(mtm);
    Some((app.activationPolicy().0, app.isActive()))
}

/// The Space/level half of `configure_window`, split out so it can also run on
/// the still-hidden window before `.show()` (see `prepare_window_before_show`).
/// Idempotent -- both call sites apply exactly the same flags.
fn apply_space_behavior(ns_window: &NSWindow) -> NSWindowCollectionBehavior {
    let behavior = NSWindowCollectionBehavior::CanJoinAllSpaces
        | NSWindowCollectionBehavior::FullScreenAuxiliary
        | NSWindowCollectionBehavior::Stationary
        | NSWindowCollectionBehavior::IgnoresCycle;
    ns_window.setCollectionBehavior(behavior);
    ns_window.setLevel(NSScreenSaverWindowLevel);
    behavior
}

/// Applies the collection behavior and window level while the overlay is still
/// hidden, so they are already in effect at the moment `.show()` orders the
/// window in.
///
/// This is load-bearing, not a tidy-up. The WindowServer decides which Space a
/// window belongs to when it is *ordered in*, and it makes that decision from
/// the state in effect at that instant. `enter_kiosk_mode`'s `configure_window`
/// runs after `.show()`, so on the very first break of a process the window was
/// ordered in with Tauri's default collection behavior -- which cannot join
/// another app's full-screen Space -- and got parked on a desktop Space instead.
/// Re-applying `CanJoinAllSpaces` a moment later does not move an
/// already-ordered-in window (`orderFrontRegardless` raises a window within its
/// current Space; it does not re-assign Spaces), which is exactly the
/// `isVisible=true isOnActiveSpace=false` pair seen in a real user's log with
/// every other readback correct. `force_space_replacement` is the recovery for
/// when this still loses the race against the async activation-policy switch.
///
/// Queued onto the main thread, like `enter_kiosk_mode` -- Tauri runs queued
/// main-thread work in order, so this lands before the `.show()` that follows it
/// at the call site.
pub fn prepare_window_before_show(win: &WebviewWindow) {
    let win_for_closure = win.clone();
    if let Err(e) = win.run_on_main_thread(move || match win_for_closure.ns_window() {
        Ok(ptr) => {
            // SAFETY: same as configure_window.
            let ns_window: &NSWindow = unsafe { &*(ptr as *mut NSWindow) };
            let behavior = apply_space_behavior(ns_window);
            log::info!(
                "macos_overlay::prepare_window_before_show: set collectionBehavior={:?} level={:?} on the hidden window (readback: collectionBehavior={:?} level={:?} app(policy,active)={:?})",
                behavior,
                NSScreenSaverWindowLevel,
                ns_window.collectionBehavior(),
                ns_window.level(),
                app_policy_and_active()
            );
        }
        Err(e) => log::warn!("macos_overlay::prepare_window_before_show: ns_window() failed: {e:?}"),
    }) {
        log::warn!("macos_overlay::prepare_window_before_show: run_on_main_thread failed: {e:?}");
    }
}

/// Ordering a visible window out and straight back in makes the WindowServer
/// re-decide which Space the window belongs to, for the case where it landed on
/// the wrong *desktop* Space.
///
/// Kept as a cheap last-ditch recovery, but deliberately no longer treated as
/// the fix for the full-screen case: measured on a real Mac, this does **not**
/// rescue a window that was created while the process was `Regular` -- such a
/// window is permanently ineligible for a full-screen Space and re-ordering it
/// in changes nothing (a real user's log shows exactly that, this firing and
/// `isOnActiveSpace` still reading false straight afterwards). See
/// `with_accessory_policy`. Costs a single frame of flicker in the case where
/// the overlay is currently invisible to the user anyway.
fn force_space_replacement(ns_window: &NSWindow) {
    ns_window.orderOut(None);
    ns_window.orderFrontRegardless();
}

/// SAFETY: `ptr` comes from `WebviewWindow::ns_window()`, which returns the
/// live `NSWindow*` backing this window for as long as the window exists --
/// borrowed, not owned, so no release here.
///
/// Always-on (see module doc): makes the overlay follow the user to any
/// Space, dev_mode or not, `macos_hide_menu_bar_dock_enabled` or not.
fn configure_window(ptr: *mut c_void) {
    let ns_window: &NSWindow = unsafe { &*(ptr as *mut NSWindow) };
    let behavior = apply_space_behavior(ns_window);
    // Unlike tao's makeKeyAndOrderFront, this orders front even while another
    // app (e.g. a full-screen video player) is still the active one.
    ns_window.orderFrontRegardless();
    log::info!(
        "macos_overlay::configure_window: set collectionBehavior={:?} level={:?} (readback: collectionBehavior={:?} level={:?} isOnActiveSpace={} app(policy,active)={:?})",
        behavior,
        NSScreenSaverWindowLevel,
        ns_window.collectionBehavior(),
        ns_window.level(),
        ns_window.isOnActiveSpace(),
        app_policy_and_active()
    );
}

/// Re-orders the overlay front and re-requests activation once the accessory
/// policy switch has had time to land. `isOnActiveSpace=false` in this log line
/// means the overlay is still not on the Space the user is looking at.
fn reassert_front_after_delay(win: &WebviewWindow) {
    let win = win.clone();
    tauri::async_runtime::spawn(async move {
        tokio::time::sleep(REASSERT_DELAY).await;
        let still_open = {
            let state = win.app_handle().state::<AppState>();
            let open = state.overlay.lock().unwrap().open;
            open
        };
        if !still_open {
            return;
        }
        let win_for_closure = win.clone();
        if let Err(e) = win.run_on_main_thread(move || {
            let ptr = match win_for_closure.ns_window() {
                Ok(ptr) => ptr,
                Err(e) => {
                    log::warn!("macos_overlay::reassert_front_after_delay: ns_window() failed: {e:?}");
                    return;
                }
            };
            // SAFETY: same as configure_window.
            let ns_window: &NSWindow = unsafe { &*(ptr as *mut NSWindow) };
            ns_window.orderFrontRegardless();
            if let Some(mtm) = MainThreadMarker::new() {
                // Presentation options (the Cmd+Tab block) only apply while active.
                #[allow(deprecated)]
                NSApplication::sharedApplication(mtm).activateIgnoringOtherApps(true);
            }
            // The actual repair, not just a diagnostic. By now the accessory
            // policy has had REASSERT_DELAY to land, so if the window is still
            // off the active Space it is because it was *ordered in* before
            // that policy (or the collection behavior) took effect, and no
            // amount of re-ordering front will move it -- only re-ordering it
            // in will. Checked rather than done unconditionally so the normal
            // path costs nothing and never flickers.
            let on_active_space = ns_window.isOnActiveSpace();
            if !on_active_space {
                log::warn!(
                    "macos_overlay::reassert_front_after_delay: overlay is off the active Space -- ordering it out and back in to force the WindowServer to re-place it"
                );
                force_space_replacement(ns_window);
            }
            log::info!(
                "macos_overlay::reassert_front_after_delay: isVisible={} isOnActiveSpace={} (before re-place: {}) app(policy,active)={:?}",
                ns_window.isVisible(),
                ns_window.isOnActiveSpace(),
                on_active_space,
                app_policy_and_active()
            );
        }) {
            log::warn!("macos_overlay::reassert_front_after_delay: run_on_main_thread failed: {e:?}");
        }
    });
}

/// `DisableProcessSwitching` (blocks Cmd+Tab) is always included, and AppKit
/// requires it to be accompanied by one of the two Dock flags -- so
/// `hide_menu_bar_and_dock` (the user's `macos_hide_menu_bar_dock_enabled`
/// Settings toggle, off by default) selects *which* Dock flag comes with it,
/// rather than whether one is present at all. See the combination rules below.
fn enable_presentation_lockdown(hide_menu_bar_and_dock: bool) {
    let Some(mtm) = MainThreadMarker::new() else {
        log::error!("macos_overlay: enable_presentation_lockdown called off the main thread");
        return;
    };

    // Apple's documented restrictions on presentation-option combinations
    // (the "Valid Combinations of Settings" section of the Kiosk Mode
    // technote; AppKit's own NSApplication.h points at the same rules):
    //
    //   - DisableProcessSwitching "must be accompanied by either
    //     NSApplicationPresentationHideDock or
    //     NSApplicationPresentationAutoHideDock".
    //   - HideMenuBar "must be accompanied by
    //     NSApplicationPresentationHideDock".
    //   - HideDock and AutoHideDock are mutually exclusive.
    //
    // Getting this wrong is not survivable here: -setPresentationOptions:
    // raises NSInvalidArgumentException on an invalid combination, and the
    // shipped release build cannot catch it (see the catch below). This
    // shipped setting DisableProcessSwitching *on its own* whenever the
    // menu-bar/Dock toggle was off -- i.e. in the default configuration --
    // which violates the first rule above and aborted the process at the
    // start of every single break on a real Mac, in a restart loop.
    //
    // With the toggle off, AutoHideDock ("Dock appears when moused to") is
    // the gentlest flag that satisfies the rule, and it costs the user
    // nothing visible during a break anyway: the overlay sits at
    // NSScreenSaverWindowLevel, above the Dock either way.
    let options = if hide_menu_bar_and_dock {
        NSApplicationPresentationOptions::DisableProcessSwitching
            | NSApplicationPresentationOptions::HideDock
            | NSApplicationPresentationOptions::HideMenuBar
    } else {
        NSApplicationPresentationOptions::DisableProcessSwitching
            | NSApplicationPresentationOptions::AutoHideDock
    };

    // Deliberately never touches NSApplicationPresentationFullScreen. That
    // bit is AppKit's own "a window of this app is in fullscreen" state, and
    // passing it when no fullscreen window is visible draws a complaint from
    // AppKit; nothing here needs to preserve it, since the overlay never
    // uses real fullscreen on macOS (see this module's doc comment).

    // Kept as defense-in-depth, but it does nothing in the build that
    // actually ships: objc2::exception::catch's own docs say "if your Rust
    // code is compiled with panic=abort ... this cannot catch the
    // exception", and [profile.release] sets panic = "abort" (Cargo.toml).
    // An invalid combination therefore aborts the whole process in release
    // -- the flag rules above are the real safety mechanism, not this.
    //
    // NSApplication is built inside the closure rather than hoisted out of
    // it: Retained<NSApplication> isn't RefUnwindSafe, so capturing one by
    // reference fails catch's UnwindSafe bound.
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
/// re-enabled) and the regular activation policy (Dock icon back). Unconditional
/// and idempotent -- safe to call even if `hide_menu_bar_and_dock` was never
/// true for this occurrence (the setting could have been toggled off mid-break),
/// same "harmless no-op" pattern as `media::resume_playing_sessions` and
/// `hook::uninstall()`.
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
    match app.set_activation_policy(ActivationPolicy::Regular) {
        Ok(()) => log::info!("macos_overlay::exit_kiosk_mode: activation policy restored to Regular"),
        Err(e) => log::error!("macos_overlay::exit_kiosk_mode: set_activation_policy(Regular) failed: {e:?}"),
    }
}
