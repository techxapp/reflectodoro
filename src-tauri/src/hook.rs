//! Low-level keyboard hook that suppresses Alt+Tab and the Windows key while
//! a break overlay is active ("strong deterrent" tier). Deliberately does NOT
//! touch Ctrl+Shift+Esc or Ctrl+Alt+Del (the latter can't be intercepted by an
//! unprivileged hook anyway) — Task Manager stays reachable as the kill switch.
//! Bare Tab (no Alt held) is left alone so it still works for field navigation
//! inside the overlay itself.

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

#[cfg(not(windows))]
mod noop_impl {
    pub fn install() {}
    pub fn uninstall() {}
}

#[cfg(windows)]
pub use windows_impl::{install, uninstall};
#[cfg(not(windows))]
pub use noop_impl::{install, uninstall};
