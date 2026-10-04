//! Whether anyone can currently see the screen: desktop "presence".
//!
//! The scheduler's and screen time's suspend-gap checks only catch a real
//! suspend/hibernate, because they work by noticing that a monotonic sleep
//! returned late. A PC that stays awake with its display off or its session
//! locked never produces a gap, so before this module both features carried
//! on as if the user were at the desk: screen time billed the whole stretch
//! to whatever app had focus, and auto-pause on wake never fired.
//!
//! The user counts as **away** while the session is locked *or* the display
//! is off (dimmed counts as on). Two consumers react to the edges:
//! - `screen_time` closes the in-progress session when the user leaves and
//!   ignores focus changes until they return (the lock screen,
//!   `LockApp.exe`, would otherwise be recorded as an app), then resyncs.
//! - `run_scheduler` treats the away duration like a suspend gap for auto-
//!   pause on wake (`take_pending_return`, woken via `RETURNED`), when
//!   `AUTO_PAUSE_ON_WAKE_INCLUDE_SCREEN_OFF` allows it.
//!
//! **When the user left is backdated** to their last keyboard/mouse input,
//! because a display that turns off on the power plan's idle timeout means
//! nobody touched the PC for that whole timeout before it. The backdate is
//! capped at that same timeout, read from the active power plan when the user
//! leaves. Without the cap a two-hour film watched with no input would lose
//! all two hours when the screen later timed out. A timeout of "never", or one
//! that can't be read, caps the backdate at zero (no backdating), which is
//! the conservative failure mode.
//!
//! **Windows only for now.** macOS (`NSWorkspaceScreensDidSleepNotification`,
//! `com.apple.screenIsLocked`) and Linux (logind's `LockedHint`) have
//! equivalents, but neither is built yet, so there `install` is a no-op and
//! nothing ever reports away.

// Only Windows calls into the state machine below; the tests exercise it on
// every host.
#![cfg_attr(not(windows), allow(dead_code))]

use std::sync::Mutex;
use std::time::Duration as StdDuration;

use chrono::{DateTime, Utc};

/// Woken whenever the user returns, so `run_scheduler` evaluates auto-pause
/// on wake right away rather than at its next boundary.
pub(crate) static RETURNED: tokio::sync::Notify = tokio::sync::Notify::const_new();

/// How long the user was away, recorded on return and consumed by the next
/// `run_scheduler` iteration. Keeps the longest of several returns that land
/// before the scheduler gets to it (lock then display-off then on again).
static PENDING_RETURN: Mutex<Option<StdDuration>> = Mutex::new(None);

static STATE: Mutex<Presence> = Mutex::new(Presence::new());

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Signal {
    Locked,
    Unlocked,
    DisplayOff,
    DisplayOn,
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Transition {
    Unchanged,
    /// The user went away, effective at `at` (already backdated).
    Left { at: DateTime<Utc> },
    /// The user is back after being away since `since`.
    Returned { since: DateTime<Utc> },
}

#[derive(Debug)]
pub(crate) struct Presence {
    locked: bool,
    display_off: bool,
    away_since: Option<DateTime<Utc>>,
}

impl Presence {
    pub(crate) const fn new() -> Self {
        Self { locked: false, display_off: false, away_since: None }
    }

    /// Applies one OS signal. `idle` is the time since the last user input;
    /// `backdate_cap` bounds how far that may move "left at" back from `now`
    /// (see the module doc). Both are only read on the present-to-away edge.
    pub(crate) fn apply(
        &mut self,
        signal: Signal,
        now: DateTime<Utc>,
        idle: StdDuration,
        backdate_cap: StdDuration,
    ) -> Transition {
        match signal {
            Signal::Locked => self.locked = true,
            Signal::Unlocked => self.locked = false,
            Signal::DisplayOff => self.display_off = true,
            Signal::DisplayOn => self.display_off = false,
        }
        let away = self.locked || self.display_off;
        match (away, self.away_since) {
            (true, None) => {
                let back = chrono::Duration::from_std(idle.min(backdate_cap))
                    .unwrap_or(chrono::Duration::zero());
                let at = now - back;
                self.away_since = Some(at);
                Transition::Left { at }
            }
            (false, Some(since)) => {
                self.away_since = None;
                Transition::Returned { since }
            }
            _ => Transition::Unchanged,
        }
    }
}

/// Takes the away duration recorded by the last return, if any. Always
/// clears it, so a value the scheduler chose not to act on (auto-pause off,
/// Pomodoro already paused) can't fire on some later iteration.
pub(crate) fn take_pending_return() -> Option<StdDuration> {
    PENDING_RETURN.lock().unwrap().take()
}

/// The shared reaction to an OS signal, independent of where it came from.
fn handle_signal(signal: Signal, idle: StdDuration, backdate_cap: impl FnOnce() -> StdDuration) {
    let now = Utc::now();
    let transition = {
        let mut state = STATE.lock().unwrap();
        // Only the present-to-away edge reads the cap, and reading it hits the
        // power plan, so don't pay for it on every signal.
        let cap = if state.away_since.is_none() && matches!(signal, Signal::Locked | Signal::DisplayOff) {
            backdate_cap()
        } else {
            StdDuration::ZERO
        };
        state.apply(signal, now, idle, cap)
    };
    match transition {
        Transition::Unchanged => {}
        Transition::Left { at } => {
            log::info!(
                "presence: away ({signal:?}), effective {}s ago",
                (now - at).num_seconds()
            );
            crate::screen_time::user_left(at);
        }
        Transition::Returned { since } => {
            let away = (now - since).to_std().unwrap_or(StdDuration::ZERO);
            log::info!("presence: back ({signal:?}) after {}s away", away.as_secs());
            crate::screen_time::user_returned();
            {
                let mut pending = PENDING_RETURN.lock().unwrap();
                *pending = Some(pending.map_or(away, |p| p.max(away)));
            }
            RETURNED.notify_one();
        }
    }
}

/// Starts watching for lock and display changes. Called once from
/// `.setup()`, after `screen_time::start_tracking`.
pub fn install() {
    platform_impl::install();
}

#[cfg(windows)]
mod platform_impl {
    use std::sync::Once;
    use std::time::Duration as StdDuration;

    use windows::core::{w, GUID};
    use windows::Win32::Foundation::{HANDLE, HLOCAL, HWND, LPARAM, LRESULT, WPARAM};
    use windows::Win32::Foundation::LocalFree;
    use windows::Win32::System::LibraryLoader::GetModuleHandleW;
    use windows::Win32::System::Power::{
        GetSystemPowerStatus, PowerGetActiveScheme, PowerReadACValueIndex, PowerReadDCValueIndex,
        RegisterPowerSettingNotification, POWERBROADCAST_SETTING, SYSTEM_POWER_STATUS,
    };
    use windows::Win32::System::RemoteDesktop::{
        WTSRegisterSessionNotification, NOTIFY_FOR_THIS_SESSION,
    };
    use windows::Win32::System::SystemInformation::GetTickCount;
    use windows::Win32::System::SystemServices::{
        GUID_SESSION_DISPLAY_STATUS, GUID_VIDEO_POWERDOWN_TIMEOUT, GUID_VIDEO_SUBGROUP,
    };
    use windows::Win32::UI::Input::KeyboardAndMouse::{GetLastInputInfo, LASTINPUTINFO};
    use windows::Win32::UI::WindowsAndMessaging::{
        CreateWindowExW, DefWindowProcW, DispatchMessageW, GetMessageW, RegisterClassW,
        TranslateMessage, DEVICE_NOTIFY_WINDOW_HANDLE, MSG, PBT_POWERSETTINGCHANGE,
        WINDOW_STYLE, WM_POWERBROADCAST, WM_WTSSESSION_CHANGE, WNDCLASSW, WS_EX_TOOLWINDOW,
        WTS_SESSION_LOCK, WTS_SESSION_UNLOCK,
    };

    use super::{handle_signal, Signal};

    static THREAD_STARTED: Once = Once::new();

    /// Time since the last keyboard/mouse input in this session. Both ends
    /// are `GetTickCount` values, so `wrapping_sub` stays right across the
    /// 49.7-day tick wrap.
    fn idle_duration() -> StdDuration {
        let mut info = LASTINPUTINFO {
            cbSize: std::mem::size_of::<LASTINPUTINFO>() as u32,
            dwTime: 0,
        };
        if !unsafe { GetLastInputInfo(&mut info) }.as_bool() {
            return StdDuration::ZERO;
        }
        StdDuration::from_millis(unsafe { GetTickCount() }.wrapping_sub(info.dwTime) as u64)
    }

    /// The active power plan's "turn off display after" for the current power
    /// source (battery or mains). Zero means "never", and is also what every
    /// failure returns, which disables backdating (see the module doc).
    fn display_off_timeout() -> StdDuration {
        unsafe {
            let mut scheme: *mut GUID = std::ptr::null_mut();
            if PowerGetActiveScheme(None, &mut scheme).is_err() || scheme.is_null() {
                return StdDuration::ZERO;
            }
            let mut status = SYSTEM_POWER_STATUS::default();
            // ACLineStatus: 0 = on battery, 1 = mains, 255 = unknown (read
            // as mains, since desktops without a battery report it).
            let on_battery =
                GetSystemPowerStatus(&mut status).is_ok() && status.ACLineStatus == 0;
            let mut seconds = 0u32;
            let ok = if on_battery {
                PowerReadDCValueIndex(
                    None,
                    Some(scheme),
                    Some(&GUID_VIDEO_SUBGROUP),
                    Some(&GUID_VIDEO_POWERDOWN_TIMEOUT),
                    &mut seconds,
                ) == 0
            } else {
                PowerReadACValueIndex(
                    None,
                    Some(scheme),
                    Some(&GUID_VIDEO_SUBGROUP),
                    Some(&GUID_VIDEO_POWERDOWN_TIMEOUT),
                    &mut seconds,
                )
                .is_ok()
            };
            // PowerGetActiveScheme allocates with LocalAlloc.
            let _ = LocalFree(Some(HLOCAL(scheme as *mut _)));
            if ok {
                StdDuration::from_secs(seconds as u64)
            } else {
                StdDuration::ZERO
            }
        }
    }

    fn signal(s: Signal) {
        handle_signal(s, idle_duration(), display_off_timeout);
    }

    unsafe extern "system" fn wnd_proc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
        match msg {
            WM_WTSSESSION_CHANGE => {
                match wparam.0 as u32 {
                    WTS_SESSION_LOCK => signal(Signal::Locked),
                    WTS_SESSION_UNLOCK => signal(Signal::Unlocked),
                    _ => {}
                }
                LRESULT(0)
            }
            WM_POWERBROADCAST => {
                if wparam.0 as u32 == PBT_POWERSETTINGCHANGE && lparam.0 != 0 {
                    let setting = unsafe { &*(lparam.0 as *const POWERBROADCAST_SETTING) };
                    if setting.PowerSetting == GUID_SESSION_DISPLAY_STATUS
                        && setting.DataLength as usize >= std::mem::size_of::<u32>()
                    {
                        // 0 = off, 1 = on, 2 = dimmed. `Data` is declared as
                        // a 1-byte array but holds a DWORD here, and isn't
                        // guaranteed to be aligned for one.
                        let value = unsafe {
                            std::ptr::read_unaligned(setting.Data.as_ptr() as *const u32)
                        };
                        signal(if value == 0 { Signal::DisplayOff } else { Signal::DisplayOn });
                    }
                }
                LRESULT(1)
            }
            _ => unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) },
        }
    }

    /// Own thread and message pump, like screen_time's foreground watcher.
    /// A hidden, never-shown top-level window rather than a message-only
    /// (`HWND_MESSAGE`) one: message-only windows don't receive broadcasts,
    /// and a hidden window is the documented recipient for both of these
    /// registrations, so it leaves no doubt about delivery.
    pub fn install() {
        THREAD_STARTED.call_once(|| {
            std::thread::spawn(|| unsafe {
                let instance = match GetModuleHandleW(None) {
                    Ok(m) => m,
                    Err(e) => {
                        log::error!("presence: GetModuleHandleW failed: {e:?}");
                        return;
                    }
                };
                let class_name = w!("ReflectodoroPresenceWatcher");
                let class = WNDCLASSW {
                    lpfnWndProc: Some(wnd_proc),
                    hInstance: instance.into(),
                    lpszClassName: class_name,
                    ..Default::default()
                };
                if RegisterClassW(&class) == 0 {
                    log::error!("presence: RegisterClassW failed");
                    return;
                }
                let hwnd = match CreateWindowExW(
                    WS_EX_TOOLWINDOW,
                    class_name,
                    w!(""),
                    WINDOW_STYLE(0),
                    0,
                    0,
                    0,
                    0,
                    None,
                    None,
                    Some(instance.into()),
                    None,
                ) {
                    Ok(h) => h,
                    Err(e) => {
                        log::error!("presence: CreateWindowExW failed: {e:?}");
                        return;
                    }
                };

                // Each registration is independent: a failure in one still
                // leaves the other signal working.
                match WTSRegisterSessionNotification(hwnd, NOTIFY_FOR_THIS_SESSION) {
                    Ok(()) => log::info!("presence: session lock watcher installed"),
                    Err(e) => log::error!("presence: WTSRegisterSessionNotification failed: {e:?}"),
                }
                match RegisterPowerSettingNotification(
                    HANDLE(hwnd.0),
                    &GUID_SESSION_DISPLAY_STATUS,
                    DEVICE_NOTIFY_WINDOW_HANDLE,
                ) {
                    Ok(_) => log::info!("presence: display state watcher installed"),
                    Err(e) => log::error!("presence: RegisterPowerSettingNotification failed: {e:?}"),
                }

                let mut msg = MSG::default();
                while GetMessageW(&mut msg, None, 0, 0).into() {
                    let _ = TranslateMessage(&msg);
                    DispatchMessageW(&msg);
                }
            });
        });
    }
}

#[cfg(not(windows))]
mod platform_impl {
    pub fn install() {}
}

#[cfg(test)]
mod tests {
    use super::*;

    fn t(secs: i64) -> DateTime<Utc> {
        DateTime::from_timestamp(1_800_000_000 + secs, 0).unwrap()
    }

    const MIN: StdDuration = StdDuration::from_secs(60);

    #[test]
    fn display_off_backdates_to_last_input_within_the_cap() {
        let mut p = Presence::new();
        let left = p.apply(Signal::DisplayOff, t(1000), 10 * MIN, 15 * MIN);
        assert_eq!(left, Transition::Left { at: t(1000 - 600) });
    }

    #[test]
    fn backdate_never_exceeds_the_display_timeout() {
        // Two hours without input (a film), then the screen times out after
        // 10 minutes: only those 10 minutes are taken back.
        let mut p = Presence::new();
        let left = p.apply(Signal::DisplayOff, t(10_000), 130 * MIN, 10 * MIN);
        assert_eq!(left, Transition::Left { at: t(10_000 - 600) });
    }

    #[test]
    fn zero_cap_means_no_backdating() {
        let mut p = Presence::new();
        let left = p.apply(Signal::Locked, t(500), 30 * MIN, StdDuration::ZERO);
        assert_eq!(left, Transition::Left { at: t(500) });
    }

    #[test]
    fn return_reports_when_the_absence_began() {
        let mut p = Presence::new();
        p.apply(Signal::DisplayOff, t(1000), StdDuration::ZERO, MIN);
        let back = p.apply(Signal::DisplayOn, t(4000), StdDuration::ZERO, MIN);
        assert_eq!(back, Transition::Returned { since: t(1000) });
    }

    #[test]
    fn away_lasts_until_both_lock_and_display_clear() {
        let mut p = Presence::new();
        assert!(matches!(p.apply(Signal::Locked, t(0), StdDuration::ZERO, MIN), Transition::Left { .. }));
        // The display turning off while locked is the same absence.
        assert_eq!(p.apply(Signal::DisplayOff, t(60), StdDuration::ZERO, MIN), Transition::Unchanged);
        // Display back on (a key press), still at the lock screen.
        assert_eq!(p.apply(Signal::DisplayOn, t(900), StdDuration::ZERO, MIN), Transition::Unchanged);
        assert_eq!(
            p.apply(Signal::Unlocked, t(930), StdDuration::ZERO, MIN),
            Transition::Returned { since: t(0) }
        );
    }

    #[test]
    fn repeated_or_present_signals_are_no_ops() {
        let mut p = Presence::new();
        // Registering for the display state reports "on" straight away.
        assert_eq!(p.apply(Signal::DisplayOn, t(0), StdDuration::ZERO, MIN), Transition::Unchanged);
        assert_eq!(p.apply(Signal::Unlocked, t(0), StdDuration::ZERO, MIN), Transition::Unchanged);
        assert!(matches!(p.apply(Signal::DisplayOff, t(10), StdDuration::ZERO, MIN), Transition::Left { .. }));
        assert_eq!(p.apply(Signal::DisplayOff, t(20), StdDuration::ZERO, MIN), Transition::Unchanged);
    }
}
