//! Low-level keyboard hook that suppresses Alt+Tab and the Windows key while
//! a break overlay is active ("strong deterrent" tier). Deliberately does NOT
//! touch Ctrl+Shift+Esc or Ctrl+Alt+Del (the latter can't be intercepted by an
//! unprivileged hook anyway) — Task Manager stays reachable as the kill switch.
//! Bare Tab (no Alt held) is left alone so it still works for field navigation
//! inside the overlay itself.
//!
//! Linux: suppression is only possible on X11 sessions (via `XGrabKey`, see
//! `linux_impl` below), never on Wayland. That's not a scope cut -- no
//! client, sandboxed or not, can suppress another client's/the compositor's
//! own global shortcuts under Wayland; it's a deliberate security property of
//! the protocol with no portable workaround. Wayland sessions fall back to
//! the same no-op macOS already ships with.

#[cfg(windows)]
mod windows_impl {
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Once;
    use windows::Win32::Foundation::{LPARAM, LRESULT, WPARAM};
    use windows::Win32::UI::Input::KeyboardAndMouse::{VK_LWIN, VK_MENU, VK_RWIN, VK_TAB};
    use windows::Win32::UI::WindowsAndMessaging::{
        CallNextHookEx, DispatchMessageW, GetMessageW, SetWindowsHookExW, TranslateMessage,
        MSG, WH_KEYBOARD_LL, WM_KEYDOWN, WM_KEYUP, WM_SYSKEYDOWN, WM_SYSKEYUP,
        KBDLLHOOKSTRUCT,
    };

    static ACTIVE: AtomicBool = AtomicBool::new(false);
    static ALT_DOWN: AtomicBool = AtomicBool::new(false);
    static THREAD_STARTED: Once = Once::new();

    unsafe extern "system" fn hook_proc(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
        if code >= 0 && ACTIVE.load(Ordering::SeqCst) {
            let kb = &*(lparam.0 as *const KBDLLHOOKSTRUCT);
            let vk = kb.vkCode;
            let msg = wparam.0 as u32;
            let is_down = msg == WM_KEYDOWN || msg == WM_SYSKEYDOWN;
            let is_up = msg == WM_KEYUP || msg == WM_SYSKEYUP;

            if vk == VK_MENU.0 as u32 {
                if is_down {
                    ALT_DOWN.store(true, Ordering::SeqCst);
                } else if is_up {
                    ALT_DOWN.store(false, Ordering::SeqCst);
                }
            }

            if is_down {
                let alt_tab = vk == VK_TAB.0 as u32 && ALT_DOWN.load(Ordering::SeqCst);
                let win_key = vk == VK_LWIN.0 as u32 || vk == VK_RWIN.0 as u32;
                if alt_tab || win_key {
                    return LRESULT(1);
                }
            }
        }
        CallNextHookEx(None, code, wparam, lparam)
    }

    pub fn install() {
        if ACTIVE.swap(true, Ordering::SeqCst) {
            return; // already active, nothing to do
        }
        THREAD_STARTED.call_once(|| {
            std::thread::spawn(|| unsafe {
                let hook = match SetWindowsHookExW(WH_KEYBOARD_LL, Some(hook_proc), None, 0) {
                    Ok(h) => h,
                    Err(e) => {
                        eprintln!("failed to install low-level keyboard hook: {e:?}");
                        return;
                    }
                };
                let mut msg = MSG::default();
                // Low-level hooks require the installing thread to pump messages.
                while GetMessageW(&mut msg, None, 0, 0).into() {
                    let _ = TranslateMessage(&msg);
                    DispatchMessageW(&msg);
                }
                let _ = hook;
            });
        });
    }

    /// Deactivates enforcement (hook_proc becomes a no-op passthrough). We don't
    /// unhook/tear down the thread since Alt-Tab/Win suppression may be needed
    /// again for the next break; toggling ACTIVE is cheap and safe from any thread.
    pub fn uninstall() {
        ACTIVE.store(false, Ordering::SeqCst);
    }
}

#[cfg(target_os = "linux")]
mod linux_impl {
    //! `XGrabKey`-based suppression for X11 sessions only. Grabbing a key
    //! combo on the root window makes the X server deliver those key events
    //! to us instead of the window manager -- that redirection *is* the
    //! suppression, mirroring how the Windows low-level hook works, just
    //! implemented at the X protocol level instead of via a message hook.
    //! Wayland sessions (detected via `XDG_SESSION_TYPE`/`WAYLAND_DISPLAY`)
    //! get no grabs at all and behave exactly like the cross-platform no-op.
    //!
    //! Crate-API note: `x11rb`'s generated enum casing (e.g. `GrabMode::Async`)
    //! and bitflag constant names (e.g. `ModMask::M1`) should be re-verified
    //! against the pinned `x11rb` version the first time this is actually
    //! compiled on a Linux toolchain -- this can't be compile-checked from a
    //! Windows dev machine.

    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::mpsc::{self, Sender};
    use std::sync::OnceLock;
    use x11rb::connection::Connection;
    use x11rb::protocol::xproto::{ConnectionExt as _, GrabMode, ModMask};
    use x11rb::protocol::Event;

    // Core X keysyms, from <X11/keysymdef.h>.
    const XK_TAB: u32 = 0xff09;
    const XK_SUPER_L: u32 = 0xffeb;
    const XK_SUPER_R: u32 = 0xffec;

    enum Command {
        Install,
        Uninstall,
    }

    static ACTIVE: AtomicBool = AtomicBool::new(false);
    static SENDER: OnceLock<Sender<Command>> = OnceLock::new();

    fn is_x11_session() -> bool {
        match std::env::var("XDG_SESSION_TYPE") {
            Ok(v) => v.eq_ignore_ascii_case("x11"),
            Err(_) => std::env::var("WAYLAND_DISPLAY").is_err(),
        }
    }

    fn keysym_to_keycode(
        conn: &impl Connection,
        min_keycode: u8,
        max_keycode: u8,
        keysym: u32,
    ) -> Option<u8> {
        let count = max_keycode - min_keycode + 1;
        let reply = conn
            .get_keyboard_mapping(min_keycode, count)
            .ok()?
            .reply()
            .ok()?;
        let per = reply.keysyms_per_keycode as usize;
        if per == 0 {
            return None;
        }
        reply
            .keysyms
            .chunks(per)
            .position(|chunk| chunk.contains(&keysym))
            .map(|i| min_keycode + i as u8)
    }

    /// Owns the X11 connection for the process lifetime and grabs/ungrabs on
    /// command. Kept alive on a dedicated thread rather than reconnecting per
    /// break, since breaks recur every ~30 minutes for the life of the app.
    fn run(rx: mpsc::Receiver<Command>) {
        let (conn, screen_num) = match x11rb::connect(None) {
            Ok(c) => c,
            Err(e) => {
                log::warn!("hook: failed to connect to X server: {e:?}");
                return;
            }
        };
        let root = conn.setup().roots[screen_num].root;
        let (min_kc, max_kc) = (conn.setup().min_keycode, conn.setup().max_keycode);

        let tab_kc = keysym_to_keycode(&conn, min_kc, max_kc, XK_TAB);
        let super_keycodes: Vec<u8> = [XK_SUPER_L, XK_SUPER_R]
            .into_iter()
            .filter_map(|ks| keysym_to_keycode(&conn, min_kc, max_kc, ks))
            .collect();

        if tab_kc.is_none() && super_keycodes.is_empty() {
            log::warn!("hook: could not resolve Tab/Super keycodes, X11 suppression disabled");
            return;
        }

        // X11 key grabs don't ignore "uninteresting" modifiers (NumLock,
        // CapsLock) automatically, so grab every combination that includes
        // Alt (Mod1) plus each lock-state combination.
        let lock_masks = [
            ModMask::from(0u16),
            ModMask::LOCK,
            ModMask::M2,
            ModMask::LOCK | ModMask::M2,
        ];
        let mut grabbed = false;

        loop {
            match rx.recv() {
                Ok(Command::Install) if !grabbed => {
                    for &extra in &lock_masks {
                        if let Some(kc) = tab_kc {
                            let _ = conn.grab_key(
                                true,
                                root,
                                ModMask::M1 | extra,
                                kc,
                                GrabMode::ASYNC,
                                GrabMode::ASYNC,
                            );
                        }
                        for &kc in &super_keycodes {
                            let _ =
                                conn.grab_key(true, root, extra, kc, GrabMode::ASYNC, GrabMode::ASYNC);
                        }
                    }
                    let _ = conn.flush();
                    grabbed = true;
                }
                Ok(Command::Uninstall) if grabbed => {
                    for &extra in &lock_masks {
                        if let Some(kc) = tab_kc {
                            let _ = conn.ungrab_key(kc, root, ModMask::M1 | extra);
                        }
                        for &kc in &super_keycodes {
                            let _ = conn.ungrab_key(kc, root, extra);
                        }
                    }
                    let _ = conn.flush();
                    grabbed = false;
                }
                Ok(_) => {}
                Err(_) => return, // sender dropped: process exiting
            }

            // Drain queued grabbed-key events without blocking, so receiving
            // them instead of the window manager (which is what achieves the
            // suppression) doesn't build up an unbounded backlog between
            // commands. The events themselves carry no useful signal here.
            while let Ok(Some(event)) = conn.poll_for_event() {
                if let Event::KeyPress(_) | Event::KeyRelease(_) = event {}
            }
        }
    }

    pub fn install() {
        if !is_x11_session() {
            return; // Wayland (or undetectable): behave like the no-op fallback
        }
        if ACTIVE.swap(true, Ordering::SeqCst) {
            return; // already active
        }
        let sender = SENDER.get_or_init(|| {
            let (tx, rx) = mpsc::channel();
            std::thread::spawn(move || run(rx));
            tx
        });
        let _ = sender.send(Command::Install);
    }

    /// Mirrors `windows_impl::uninstall`: ungrabs rather than tearing down
    /// the connection/thread, since suppression is needed again for the next
    /// break.
    pub fn uninstall() {
        ACTIVE.store(false, Ordering::SeqCst);
        if let Some(sender) = SENDER.get() {
            let _ = sender.send(Command::Uninstall);
        }
    }
}

#[cfg(target_os = "macos")]
mod macos_impl {
    //! `CGEventTap`-based suppression of Cmd+Tab, gated on the user having
    //! granted Accessibility permission (`AXIsProcessTrusted`) -- macOS gives
    //! no unprivileged app a way to swallow a system-level key combo without
    //! it. Structurally this mirrors `windows_impl`: the tap is created once
    //! for the process lifetime and left permanently enabled; `install`/
    //! `uninstall` just flip the `ACTIVE` flag the callback checks, rather
    //! than tearing the tap down and rebuilding it every break.
    //!
    //! Crate-API note: the exact `objc2-core-graphics`/`objc2-core-foundation`
    //! surface used below (free-function names, `CFRetained` plumbing) was
    //! confirmed against the pinned 0.3.2 docs but not yet compiled on this
    //! toolchain the first time this landed -- re-verify with `cargo check`
    //! if it doesn't build cleanly, same caveat already given to `linux_impl`
    //! for `x11rb`.

    use std::ffi::c_void;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::OnceLock;

    use objc2_application_services::AXIsProcessTrusted;
    use objc2_core_foundation::{kCFRunLoopCommonModes, CFMachPort, CFRunLoop};
    use objc2_core_graphics::{
        CGEvent, CGEventField, CGEventFlags, CGEventMask, CGEventTapLocation, CGEventTapOptions,
        CGEventTapPlacement, CGEventTapProxy, CGEventType,
    };

    // From Carbon's <HIToolbox/Events.h>, not exposed as a Rust constant by
    // any crate here (same situation as NX_KEYTYPE_PLAY in media.rs).
    const KVK_TAB: i64 = 0x30;

    static ACTIVE: AtomicBool = AtomicBool::new(false);
    // Guards against spawning more than one tap thread at once, *without*
    // permanently latching on a failed attempt (a plain `Once` would): right
    // after the user grants Accessibility permission, `AXIsProcessTrusted`
    // can flip true slightly before `CGEventTapCreate` actually starts
    // succeeding (a known macOS TCC-propagation race, more likely on an
    // ad-hoc-signed dev build). `run` resets this to false on every failure
    // path so the next break's `install()` call retries instead of the
    // feature silently staying off until the app is restarted.
    static TAP_THREAD_ACTIVE: AtomicBool = AtomicBool::new(false);
    // Raw pointer to the CFMachPort backing the tap, set once the tap thread
    // creates it, so the callback can re-enable a tap the OS disabled for
    // being slow (see the TapDisabledBy* arms below). Never null once the
    // tap thread reaches CFRunLoopRun; read-only from the callback's POV.
    static TAP_PORT: OnceLock<usize> = OnceLock::new();

    /// Whether the user has granted this app Accessibility permission. No
    /// prompt -- just a status check, safe to call as often as needed.
    pub fn is_trusted() -> bool {
        unsafe { AXIsProcessTrusted() }
    }

    unsafe extern "C-unwind" fn tap_callback(
        _proxy: CGEventTapProxy,
        event_type: CGEventType,
        event: std::ptr::NonNull<CGEvent>,
        _user_info: *mut c_void,
    ) -> *mut CGEvent {
        if event_type == CGEventType::TapDisabledByTimeout
            || event_type == CGEventType::TapDisabledByUserInput
        {
            if let Some(&port_addr) = TAP_PORT.get() {
                let port = &*(port_addr as *const CFMachPort);
                CGEvent::tap_enable(port, true);
            }
            return event.as_ptr();
        }

        if ACTIVE.load(Ordering::SeqCst) && event_type == CGEventType::KeyDown {
            let event_ref = event.as_ref();
            let flags = CGEvent::flags(Some(event_ref));
            let keycode = CGEvent::integer_value_field(Some(event_ref), CGEventField::KeyboardEventKeycode);
            if flags.contains(CGEventFlags::MaskCommand) && keycode == KVK_TAB {
                return std::ptr::null_mut();
            }
        }

        event.as_ptr()
    }

    /// Runs on a dedicated thread for the process lifetime once it succeeds,
    /// same shape as `linux_impl::run` owning the X11 connection thread.
    /// Spawned whenever Accessibility permission looks granted; every early
    /// return resets `TAP_THREAD_ACTIVE` so a failed attempt can be retried
    /// by a later `install()` call instead of giving up for the process's
    /// life -- see `TAP_THREAD_ACTIVE`'s doc comment for why that matters.
    fn run() {
        let mask: CGEventMask = 1u64 << (CGEventType::KeyDown.0 as u64);
        let Some(tap) = (unsafe {
            CGEvent::tap_create(
                CGEventTapLocation::SessionEventTap,
                CGEventTapPlacement::HeadInsertEventTap,
                CGEventTapOptions::Default,
                mask,
                Some(tap_callback),
                std::ptr::null_mut(),
            )
        }) else {
            log::warn!("hook: CGEventTapCreate failed (Accessibility permission not actually granted yet?); will retry next break");
            TAP_THREAD_ACTIVE.store(false, Ordering::SeqCst);
            return;
        };

        let _ = TAP_PORT.set(&*tap as *const CFMachPort as usize);

        let Some(source) = CFMachPort::new_run_loop_source(None, Some(&tap), 0) else {
            log::warn!("hook: failed to create run loop source for the CGEventTap; will retry next break");
            TAP_THREAD_ACTIVE.store(false, Ordering::SeqCst);
            return;
        };
        let Some(run_loop) = CFRunLoop::current() else {
            log::warn!("hook: failed to get current CFRunLoop; will retry next break");
            TAP_THREAD_ACTIVE.store(false, Ordering::SeqCst);
            return;
        };
        run_loop.add_source(Some(&source), unsafe { kCFRunLoopCommonModes });
        CGEvent::tap_enable(&tap, true);
        log::info!("hook: CGEventTap created and enabled, Cmd+Tab suppression armed");
        CFRunLoop::run();
        // CFRunLoopRun() only returns if something calls CFRunLoopStop on
        // this run loop, which nothing here does -- reachable in principle
        // (e.g. an unexpected external stop), so reset the guard rather than
        // leave a dead tap latched as "active" forever.
        TAP_THREAD_ACTIVE.store(false, Ordering::SeqCst);
    }

    pub fn install() {
        if ACTIVE.swap(true, Ordering::SeqCst) {
            return; // already active, nothing to do
        }
        if !is_trusted() {
            ACTIVE.store(false, Ordering::SeqCst);
            log::info!("hook: Accessibility permission not granted, Cmd+Tab suppression unavailable this break");
            return;
        }
        if TAP_THREAD_ACTIVE
            .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
            .is_ok()
        {
            std::thread::spawn(run);
        }
    }

    /// Mirrors `windows_impl::uninstall`/`linux_impl::uninstall`: leaves the
    /// tap installed and enabled, just gates suppression back off via
    /// `ACTIVE` -- cheap and safe from any thread, and avoids re-creating the
    /// tap (and re-prompting the OS) on every single break.
    pub fn uninstall() {
        ACTIVE.store(false, Ordering::SeqCst);
    }
}

#[cfg(not(any(windows, target_os = "linux", target_os = "macos")))]
mod noop_impl {
    pub fn install() {}
    pub fn uninstall() {}
}

#[cfg(windows)]
pub use windows_impl::{install, uninstall};
#[cfg(target_os = "linux")]
pub use linux_impl::{install, uninstall};
#[cfg(target_os = "macos")]
pub use macos_impl::{install, is_trusted, uninstall};
#[cfg(not(any(windows, target_os = "linux", target_os = "macos")))]
pub use noop_impl::{install, uninstall};
