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

#[cfg(not(any(windows, target_os = "linux")))]
mod noop_impl {
    pub fn install() {}
    pub fn uninstall() {}
}

#[cfg(windows)]
pub use windows_impl::{install, uninstall};
#[cfg(target_os = "linux")]
pub use linux_impl::{install, uninstall};
#[cfg(not(any(windows, target_os = "linux")))]
pub use noop_impl::{install, uninstall};
