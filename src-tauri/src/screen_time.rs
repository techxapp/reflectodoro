//! Foreground-app focus tracking ("screen time"): which app had focus, for
//! how long. Same shape as media.rs -- one feature, a different OS mechanism
//! per platform, one common interface, no OS branching at call sites.
//!
//! On Windows: a dedicated thread running `SetWinEventHook`
//! (`EVENT_SYSTEM_FOREGROUND`) plus its own `GetMessageW` pump. Push-based,
//! no polling -- the OS calls us on every foreground change. Deliberately a
//! separate thread from hook.rs's keyboard hook: that one only needs to exist
//! during a break, this one runs for the whole process lifetime. The stable
//! `app_id` is the exe basename, resolved via `GetWindowThreadProcessId` ->
//! `OpenProcess` -> `QueryFullProcessImageNameW`. A separate `display_name`
//! (e.g. "Google Chrome" for `chrome.exe`) is read from the exe's version
//! resource (`FileDescription`, via `version.dll`'s `GetFileVersionInfoW`/
//! `VerQueryValueW`) off that same resolved path -- the same field most of
//! the Windows shell surfaces as an app's friendly name. Purely a display
//! aid: `app_id` stays the stable grouping/storage key (unaffected by a
//! missing or renamed `FileDescription`), `display_name` is what the Entries
//! breakdown shows, falling back to `app_id` wherever it's empty (no version
//! resource at all -- common for console tools/scripts-turned-exe -- or one
//! present without this specific string).
//!
//! macOS / Linux / Android: not implemented yet -- `install_watcher` is a
//! documented no-op on those, so everything below (buffering, flushing, the
//! Settings toggle, the Entries breakdown) is already wired and simply has
//! nothing feeding it until those land. Planned mechanisms, per the plan doc:
//! macOS `NSWorkspace.didActivateApplicationNotification`, Linux X11
//! `_NET_ACTIVE_WINDOW` property watching (no Wayland equivalent, same
//! protocol-level wall hook.rs already documents), and Android
//! `UsageStatsManager.queryEvents` polling (the one non-push platform -- a
//! third-party app can't get focus pushes there without an Accessibility
//! Service, which this app deliberately doesn't use).
//!
//! **Self-exclusion is mandatory on every platform**: Reflectodoro's own
//! windows (main, overlay, checkin) take focus every ~25 minutes by design,
//! and would otherwise dominate the user's own breakdown. On Windows that's
//! the `pid == std::process::id()` check in `app_id_for_window`, which
//! records "no app in focus" rather than a session -- so time spent in the
//! break overlay is attributed to nothing rather than leaking into whatever
//! app happened to be focused before it.
//!
//! ## Session buffering, flushing, and why the two intervals differ
//!
//! A session is only closed (and therefore only ever written) when a *real*
//! focus change happens. `run_flush_loop` doesn't create rows -- it just
//! ships whatever real switches accumulated since its last tick as one
//! batched event, so a quiet `FLUSH_INTERVAL` with no app switch costs
//! nothing: no emit, no IPC, no DB write. That's the actual "efficient"
//! lever here -- write *frequency* is capped, write *volume* tracks real
//! behavior.
//!
//! `CHECKPOINT_INTERVAL` is deliberately much coarser and exists for one
//! narrow reason: bounding how much of a single very-long session a hard
//! crash can lose. Running the checkpoint on every flush tick instead would
//! write a fresh row every minute for an app the user never switched away
//! from -- turning a 3-hour reading session into ~180 rows instead of 1.
//!
//! Nothing here writes to SQLite directly: sessions leave Rust as a
//! `"screentime://session-batch"` event and the main window persists them
//! (see `listenForScreenTimeSessionBatches` in db.ts), the same
//! Rust-emits/frontend-writes split `"media-toggle://recorded"` already uses.

use std::sync::atomic::Ordering;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use chrono::{DateTime, SecondsFormat, Utc};
use serde::Serialize;
use tauri::{AppHandle, Emitter};

use crate::SCREEN_TIME_TRACKING_ENABLED;

/// How often closed sessions are batched up and shipped to the frontend.
/// Caps write/IPC *frequency* only -- see the module doc.
const FLUSH_INTERVAL: Duration = Duration::from_secs(60);

/// How often an *in-progress* session is split at "now" so a hard kill can't
/// lose more than this much of it. Deliberately ~15x coarser than
/// `FLUSH_INTERVAL` -- see the module doc for why the two are decoupled.
const CHECKPOINT_INTERVAL: Duration = Duration::from_secs(15 * 60);

/// Sessions shorter than this are dropped rather than recorded: transient
/// windows (an alt-tab passed through, a splash screen, a UAC prompt) would
/// otherwise fill the breakdown with sub-second noise. A starting guess, not
/// empirically tuned yet.
const MIN_SESSION_DURATION: Duration = Duration::from_secs(5);

/// How far past its intended wake instant a flush tick has to land before
/// it's treated as "the machine was suspended through this" rather than
/// ordinary scheduling jitter -- same idea (and same value) as lib.rs's
/// `SUSPEND_GAP_THRESHOLD`, for a different consequence. Without this, a
/// laptop closed for 10 hours with a browser focused wakes up and eventually
/// records a single 10-hour "session" of screen time for an app nobody was
/// looking at. On a gap this large the current session is truncated at the
/// last tick's expected wake instant (roughly when the machine actually went
/// away) and a fresh one opened at "now".
const SUSPEND_GAP_THRESHOLD: Duration = Duration::from_secs(120);

pub const SESSION_BATCH_EVENT: &str = "screentime://session-batch";

/// One closed focus session, as handed to the frontend. Field names are the
/// `screen_time_session` column names (minus `device_name`, which the
/// frontend fills in from `app_setting` at write time -- Rust has no reason
/// to carry it through the buffer).
#[derive(Clone, Debug, Serialize)]
pub struct ScreenTimeSession {
    pub app_id: String,
    pub display_name: String,
    pub platform: &'static str,
    pub started_at: String,
    pub ended_at: String,
}

/// What a platform callback resolves the focused window to: the stable
/// grouping key plus a best-effort friendly label for it. `display_name`
/// empty means "couldn't resolve one" -- every consumer falls back to
/// `app_id` at that point rather than needing its own empty check.
pub struct FocusedApp {
    pub app_id: String,
    pub display_name: String,
}

/// The in-progress session: whatever currently has focus, since when.
struct PendingSession {
    app_id: String,
    display_name: String,
    started_at: DateTime<Utc>,
}

/// Module-private statics rather than `AppState` fields, for the same reason
/// hook.rs keeps its own `ACTIVE`/`SENDER`: the platform callbacks that write
/// these are raw OS callbacks (a `WINEVENTPROC` on Windows) with no path to a
/// `State<AppState>`.
///
/// **Lock ordering**: `CURRENT_SESSION` may be taken while `PENDING_FLUSH` is
/// free, never the other way around. Every function below releases the
/// `CURRENT_SESSION` guard before pushing into `PENDING_FLUSH`; keep it that
/// way or two threads doing this concurrently deadlock.
static CURRENT_SESSION: Mutex<Option<PendingSession>> = Mutex::new(None);
static PENDING_FLUSH: Mutex<Vec<ScreenTimeSession>> = Mutex::new(Vec::new());

fn iso(at: DateTime<Utc>) -> String {
    at.to_rfc3339_opts(SecondsFormat::Millis, true)
}

/// Takes the in-progress session out and returns it as a closed session --
/// or `None` if there wasn't one, or it was too short to be worth recording.
/// Caller holds the `CURRENT_SESSION` guard and must push the result into
/// `PENDING_FLUSH` only after dropping it (see the lock-ordering note above).
fn take_closed(cur: &mut Option<PendingSession>, ended_at: DateTime<Utc>) -> Option<ScreenTimeSession> {
    let pending = cur.take()?;
    let elapsed = (ended_at - pending.started_at).to_std().unwrap_or(Duration::ZERO);
    if elapsed < MIN_SESSION_DURATION {
        return None;
    }
    Some(ScreenTimeSession {
        app_id: pending.app_id,
        display_name: pending.display_name,
        platform: std::env::consts::OS,
        started_at: iso(pending.started_at),
        ended_at: iso(ended_at),
    })
}

fn queue(session: Option<ScreenTimeSession>) {
    if let Some(session) = session {
        PENDING_FLUSH.lock().unwrap().push(session);
    }
}

/// Called by every platform's focus callback. `None` means "focus went
/// somewhere we don't attribute" -- Reflectodoro itself, or a window whose
/// owning process couldn't be resolved -- which closes the current session
/// without opening a new one.
///
/// Gated on `SCREEN_TIME_TRACKING_ENABLED` here rather than by
/// installing/uninstalling the OS watcher, mirroring hook.rs's `ACTIVE`
/// flag: toggling this feature isn't latency-sensitive, and re-registering a
/// hook at runtime is a whole class of failure this doesn't need.
pub fn record_focus_change(new_app: Option<FocusedApp>) {
    if !SCREEN_TIME_TRACKING_ENABLED.load(Ordering::SeqCst) {
        return;
    }
    let now = Utc::now();
    let closed = {
        let mut cur = CURRENT_SESSION.lock().unwrap();
        // Re-focusing the app that's already focused (a second window of the
        // same exe, a click back into it) isn't a switch -- splitting the
        // session there would just produce two rows saying what one already
        // says.
        if let (Some(pending), Some(new_app)) = (cur.as_ref(), new_app.as_ref()) {
            if pending.app_id == new_app.app_id {
                return;
            }
        }
        let closed = take_closed(&mut cur, now);
        *cur = new_app.map(|app| PendingSession {
            app_id: app.app_id,
            display_name: app.display_name,
            started_at: now,
        });
        closed
    };
    queue(closed);
}

/// Splits the in-progress session at `at` and immediately reopens the same
/// app from there. Purely a crash-durability bound (see `CHECKPOINT_INTERVAL`);
/// the running total the UI shows does not depend on this -- that comes from
/// `current_session_elapsed_ms`.
fn split_current_session(at: DateTime<Utc>) {
    let closed = {
        let mut cur = CURRENT_SESSION.lock().unwrap();
        let Some((app_id, display_name)) =
            cur.as_ref().map(|p| (p.app_id.clone(), p.display_name.clone()))
        else {
            return;
        };
        let closed = take_closed(&mut cur, at);
        *cur = Some(PendingSession { app_id, display_name, started_at: at });
        closed
    };
    queue(closed);
}

/// Closes the in-progress session without opening a new one -- used when
/// tracking is switched off, so whatever was in flight lands in the DB right
/// away instead of sitting stranded in memory until it's switched back on.
fn close_current_session() {
    let closed = {
        let mut cur = CURRENT_SESSION.lock().unwrap();
        take_closed(&mut cur, Utc::now())
    };
    queue(closed);
}

/// App id/display name + how long it's been focused, straight from memory --
/// no DB write, no row created. This is what keeps *today's* breakdown
/// accurate for the app currently in focus (persisted rows only ever cover
/// sessions already closed by a real switch); the checkpoint interval exists
/// for crash durability, not for this.
pub fn current_session_elapsed_ms() -> Option<(String, String, i64)> {
    let cur = CURRENT_SESSION.lock().unwrap();
    let pending = cur.as_ref()?;
    let elapsed = (Utc::now() - pending.started_at).num_milliseconds().max(0);
    Some((pending.app_id.clone(), pending.display_name.clone(), elapsed))
}

/// Ships everything buffered as one event. No-op (no emit at all) when
/// nothing accumulated, which is the common case for a quiet minute.
fn drain_and_emit(app: &AppHandle) {
    let batch = {
        let mut pending = PENDING_FLUSH.lock().unwrap();
        if pending.is_empty() {
            return;
        }
        std::mem::take(&mut *pending)
    };
    let count = batch.len();
    if let Err(e) = app.emit(SESSION_BATCH_EVENT, &batch) {
        // Put them back rather than dropping them on the floor: an emit
        // failure here is exactly the silently-discarded-Result class of bug
        // CLAUDE.md calls out, and the next tick can retry.
        log::error!("screen_time: failed to emit {count} buffered session(s), re-queueing: {e:?}");
        let mut pending = PENDING_FLUSH.lock().unwrap();
        pending.splice(0..0, batch);
        return;
    }
    log::info!("screen_time: flushed {count} session(s)");
}

/// Closes the in-progress session and flushes immediately -- called when the
/// Settings toggle is switched off (see `set_screen_time_tracking_enabled`).
pub fn flush_now(app: &AppHandle) {
    close_current_session();
    drain_and_emit(app);
}

/// Opens a session for whatever is focused right now, without waiting for the
/// next switch -- called when tracking is switched back on, so re-enabling it
/// doesn't silently record nothing until the user next changes windows.
pub fn resync_current_focus() {
    platform_impl::resync_current_focus();
}

/// Installs the platform's focus watcher. Called once from `.setup()`,
/// unconditionally: the enable flag is checked inside `record_focus_change`,
/// not here.
pub fn start_tracking(_app: &AppHandle) {
    platform_impl::install_watcher();
}

/// Batches buffered sessions to the frontend every `FLUSH_INTERVAL`, and
/// splits an in-progress session every `CHECKPOINT_INTERVAL`. Spawned once
/// from `.setup()`, alongside `run_scheduler`.
pub async fn run_flush_loop(app: AppHandle) {
    let mut last_checkpoint = Instant::now();
    let mut expected_wake = Utc::now() + chrono::Duration::from_std(FLUSH_INTERVAL).unwrap();

    loop {
        tokio::time::sleep(FLUSH_INTERVAL).await;
        let now = Utc::now();

        // Suspend detection: `Instant`-based sleeps don't advance across a
        // real suspend, so a wake this far past what this tick was aiming for
        // means the machine was away, not that the app was busy. Truncate the
        // in-flight session at roughly when it went away instead of billing
        // the whole nap to whatever had focus. See SUSPEND_GAP_THRESHOLD.
        let overslept = (now - expected_wake).to_std().unwrap_or(Duration::ZERO);
        if overslept > SUSPEND_GAP_THRESHOLD {
            log::info!(
                "screen_time: flush tick woke {}s late -- treating as a suspend gap, truncating the in-progress session",
                overslept.as_secs()
            );
            split_current_session(expected_wake);
            // `split_current_session` reopens from the same instant it closed
            // at, which would re-accrue the whole suspend -- restart the
            // reopened half at "now" instead. Nothing was on screen in between.
            if let Some(pending) = CURRENT_SESSION.lock().unwrap().as_mut() {
                pending.started_at = now;
            }
            last_checkpoint = Instant::now();
        } else if last_checkpoint.elapsed() >= CHECKPOINT_INTERVAL {
            split_current_session(now);
            last_checkpoint = Instant::now();
        }

        drain_and_emit(&app);
        expected_wake = Utc::now() + chrono::Duration::from_std(FLUSH_INTERVAL).unwrap();
    }
}

#[cfg(windows)]
mod platform_impl {
    use std::collections::HashMap;
    use std::sync::{Mutex, Once};

    use windows::Win32::Foundation::{CloseHandle, HWND, MAX_PATH};
    use windows::Win32::Storage::FileSystem::{
        GetFileVersionInfoSizeW, GetFileVersionInfoW, VerQueryValueW,
    };
    use windows::Win32::System::Threading::{
        OpenProcess, QueryFullProcessImageNameW, PROCESS_NAME_WIN32,
        PROCESS_QUERY_LIMITED_INFORMATION,
    };
    use windows::Win32::UI::Accessibility::{SetWinEventHook, HWINEVENTHOOK};
    use windows::Win32::UI::WindowsAndMessaging::{
        DispatchMessageW, GetForegroundWindow, GetMessageW, GetWindowThreadProcessId,
        TranslateMessage, EVENT_SYSTEM_FOREGROUND, MSG, WINEVENT_OUTOFCONTEXT,
    };
    use windows::core::{PCWSTR, PWSTR};

    use super::FocusedApp;

    static THREAD_STARTED: Once = Once::new();

    /// Caches resolved friendly names by full exe path (not just basename --
    /// two different apps could in principle share a basename, and the path
    /// is what `friendly_name_for_exe` actually reads from disk anyway).
    /// `GetFileVersionInfoW` does real file I/O, and the same handful of
    /// apps get focused over and over in a normal day -- this keeps that
    /// cost to once per distinct exe per process lifetime rather than once
    /// per focus switch. Runs on this module's own dedicated message-pump
    /// thread (see `install_watcher`), never the WRY UI thread, so the I/O
    /// here was never a UI-blocking concern even before caching.
    static NAME_CACHE: Mutex<Option<HashMap<String, String>>> = Mutex::new(None);

    fn to_wide_null(s: &str) -> Vec<u16> {
        s.encode_utf16().chain(std::iter::once(0)).collect()
    }

    /// Reads the exe's `FileDescription` version-resource string (e.g.
    /// "Google Chrome" for `chrome.exe`, "Visual Studio Code" for
    /// `Code.exe`) -- the same field most of the Windows shell surfaces as a
    /// friendly app name. Returns `""` for every failure mode uniformly (no
    /// version resource at all -- common for console tools/scripts-turned-
    /// exe; a resource present but missing this specific string; any Win32
    /// call failing) since every caller's fallback is the exe basename
    /// either way, not a `None` they'd need to branch on separately.
    fn friendly_name_for_exe(path: &str) -> String {
        resolve_friendly_name(path).unwrap_or_default()
    }

    fn resolve_friendly_name(path: &str) -> Option<String> {
        unsafe {
            let wide_path = to_wide_null(path);
            let pcwstr_path = PCWSTR(wide_path.as_ptr());

            let size = GetFileVersionInfoSizeW(pcwstr_path, None);
            if size == 0 {
                return None;
            }

            let mut buffer = vec![0u8; size as usize];
            GetFileVersionInfoW(pcwstr_path, None, size, buffer.as_mut_ptr() as *mut _).ok()?;

            // \VarFileInfo\Translation gives the (language, codepage) pairs
            // this resource actually has strings for -- FileDescription
            // lives under a language-specific subblock, and guessing the
            // common "040904b0" (US English, Unicode) block directly would
            // miss anything localized differently.
            let translation_key = to_wide_null(r"\VarFileInfo\Translation");
            let mut translation_ptr: *mut core::ffi::c_void = std::ptr::null_mut();
            let mut translation_len: u32 = 0;
            let found: bool = VerQueryValueW(
                buffer.as_ptr() as *const _,
                PCWSTR(translation_key.as_ptr()),
                &mut translation_ptr,
                &mut translation_len,
            )
            .into();
            if !found || translation_ptr.is_null() || translation_len < 4 {
                return None;
            }
            let pair = translation_ptr as *const u16;
            let (lang, codepage) = (*pair, *pair.add(1));

            let desc_key =
                to_wide_null(&format!(r"\StringFileInfo\{lang:04x}{codepage:04x}\FileDescription"));
            let mut desc_ptr: *mut core::ffi::c_void = std::ptr::null_mut();
            let mut desc_len: u32 = 0;
            let found: bool = VerQueryValueW(
                buffer.as_ptr() as *const _,
                PCWSTR(desc_key.as_ptr()),
                &mut desc_ptr,
                &mut desc_len,
            )
            .into();
            if !found || desc_ptr.is_null() || desc_len == 0 {
                return None;
            }

            // desc_len is a WCHAR count including the trailing NUL for this
            // string-block form; trim that and any incidental whitespace.
            let desc_slice = std::slice::from_raw_parts(desc_ptr as *const u16, desc_len as usize);
            let trimmed = String::from_utf16_lossy(desc_slice)
                .trim_end_matches('\0')
                .trim()
                .to_string();
            if trimmed.is_empty() {
                None
            } else {
                Some(trimmed)
            }
        }
    }

    fn cached_friendly_name(path: &str) -> String {
        let mut cache = NAME_CACHE.lock().unwrap();
        let map = cache.get_or_insert_with(HashMap::new);
        if let Some(name) = map.get(path) {
            return name.clone();
        }
        let name = friendly_name_for_exe(path);
        map.insert(path.to_string(), name.clone());
        name
    }

    /// The exe basename of the process owning `hwnd` (e.g. `chrome.exe`)
    /// plus its best-effort friendly display name (e.g. "Google Chrome").
    /// `None` means "don't attribute this": our own process (see the module
    /// doc's self-exclusion note), or a window we couldn't resolve a process
    /// for at all (`OpenProcess` can legitimately fail on a
    /// higher-integrity/protected process, which is not an error worth
    /// logging on every focus change).
    ///
    /// Known gap, not fixed here: a UWP/packaged app's foreground window can
    /// belong to `ApplicationFrameHost.exe` rather than the real app, so it
    /// shows up (basename and friendly name both) under that host process's
    /// identity instead of the app the user actually sees.
    fn app_id_for_window(hwnd: HWND) -> Option<FocusedApp> {
        unsafe {
            let mut pid = 0u32;
            GetWindowThreadProcessId(hwnd, Some(&mut pid));
            if pid == 0 || pid == std::process::id() {
                return None;
            }

            let handle = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid).ok()?;
            let mut buf = [0u16; MAX_PATH as usize];
            let mut len = buf.len() as u32;
            let result = QueryFullProcessImageNameW(
                handle,
                PROCESS_NAME_WIN32,
                PWSTR(buf.as_mut_ptr()),
                &mut len,
            );
            let _ = CloseHandle(handle);
            result.ok()?;

            let path = String::from_utf16_lossy(&buf[..len as usize]);
            let name = path.rsplit(['\\', '/']).next().unwrap_or(&path);
            if name.is_empty() {
                None
            } else {
                Some(FocusedApp {
                    app_id: name.to_string(),
                    display_name: cached_friendly_name(&path),
                })
            }
        }
    }

    fn handle_foreground(hwnd: HWND) {
        if hwnd.is_invalid() {
            // No foreground window at all (the desktop, a switch in flight) --
            // still a switch away from whatever was focused.
            super::record_focus_change(None);
            return;
        }
        super::record_focus_change(app_id_for_window(hwnd));
    }

    unsafe extern "system" fn win_event_proc(
        _hook: HWINEVENTHOOK,
        event: u32,
        hwnd: HWND,
        _id_object: i32,
        _id_child: i32,
        _thread: u32,
        _time: u32,
    ) {
        if event == EVENT_SYSTEM_FOREGROUND {
            handle_foreground(hwnd);
        }
    }

    /// Own thread with its own message pump: `WINEVENT_OUTOFCONTEXT` hooks
    /// are delivered as messages to the installing thread, so one has to
    /// exist to pump them. Deliberately not hook.rs's keyboard-hook thread --
    /// that one's lifecycle is per-break, this one is per-process.
    ///
    /// Deliberately NOT `WINEVENT_SKIPOWNPROCESS`: we want the event when
    /// *our* window takes focus too, so the previous app's session gets
    /// closed rather than left running while the user sits in the break
    /// overlay. `app_id_for_window`'s pid check is what excludes us from the
    /// breakdown itself.
    /// Cheap enough to call on demand (one `GetForegroundWindow` plus one
    /// process-name lookup) -- no need to cache anything for it.
    pub fn resync_current_focus() {
        unsafe { handle_foreground(GetForegroundWindow()) };
    }

    pub fn install_watcher() {
        THREAD_STARTED.call_once(|| {
            std::thread::spawn(|| unsafe {
                // Seed with whatever is already focused, so the app doesn't
                // have to wait for the first switch to start attributing time.
                handle_foreground(GetForegroundWindow());

                let hook = SetWinEventHook(
                    EVENT_SYSTEM_FOREGROUND,
                    EVENT_SYSTEM_FOREGROUND,
                    None,
                    Some(win_event_proc),
                    0,
                    0,
                    WINEVENT_OUTOFCONTEXT,
                );
                if hook.is_invalid() {
                    log::error!("screen_time: SetWinEventHook(EVENT_SYSTEM_FOREGROUND) failed");
                    return;
                }
                log::info!("screen_time: foreground watcher installed");

                let mut msg = MSG::default();
                while GetMessageW(&mut msg, None, 0, 0).into() {
                    let _ = TranslateMessage(&msg);
                    DispatchMessageW(&msg);
                }
            });
        });
    }
}

/// macOS/Linux/Android capture isn't built yet -- see the module doc for the
/// planned mechanism on each. Everything else (buffering, flushing, the
/// Settings toggle, the Entries breakdown) is already platform-agnostic and
/// simply has nothing feeding it here.
#[cfg(not(windows))]
mod platform_impl {
    pub fn install_watcher() {
        log::info!(
            "screen_time: no foreground watcher on this platform yet -- tracking will record nothing"
        );
    }

    pub fn resync_current_focus() {}
}
