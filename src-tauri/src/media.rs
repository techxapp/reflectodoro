//! Best-effort media pause fired when the break overlay opens.
//!
//! On Windows: queries System Media Transport Controls (SMTC) for every
//! registered session (browser tabs, Spotify desktop, etc.) and pauses only
//! the ones actually playing. Deliberately NOT the simpler
//! VK_MEDIA_PLAY_PAUSE key-simulation approach: that's a toggle, so on a
//! break that starts while media is already paused (e.g. the previous break
//! paused it and it was never resumed) it would resume playback instead of
//! leaving it alone -- the opposite of what this feature is for.
//!
//! On macOS: two mechanisms, selected by `select_macos_pause_strategy`.
//!
//! The default is the private MediaRemote framework's
//! `MRMediaRemoteSendCommand(kMRPause)`, which needs **no permission at all**
//! -- confirmed on macOS 15.7.3 pausing a playing browser video with
//! `CGPreflightPostEventAccess()` reading false. It is an explicit,
//! idempotent pause rather than a toggle, so it cannot resume media the user
//! had already paused, and it needs none of the guards the media-key path
//! below does. It is still NOT state-aware: MediaRemote's now-playing
//! *getters* have been entitlement-walled since macOS 15.4 (they report "not
//! playing" against live playback), so true SMTC/MPRIS-style query-then-pause
//! is impossible here, not merely unbuilt -- `kMRPause` just makes it
//! unnecessary. Like the media key it also targets macOS's single "Now
//! Playing" app, so with several players open it can act on the wrong one.
//!
//! The fallback is the original synthetic hardware Play/Pause media-key event
//! -- a blind toggle, with exactly the failure mode described above for
//! Windows, and it needs the app to hold macOS's post-event access (listed
//! under Privacy & Security > Accessibility) or the WindowServer silently
//! drops the event. It is used only when the user forces it
//! (`app_setting.macos_media_key_fallback_enabled`) or when
//! `MRMediaRemoteSendCommand` cannot be resolved, since that is a private
//! symbol Apple can remove without notice. Because it IS a toggle it keeps
//! every guard it has always had: `macos_impl::already_toggled_this_break`
//! skips a second toggle for the same break occurrence (a relaunch or
//! suspend-resume mid-break re-shows the overlay), and
//! `macos_impl::any_output_device_running` skips when no audio output device
//! is running anywhere, since then nothing can be playing and a toggle could
//! only resume something. An earlier guard reset only on a *submitted*
//! wellness check-in, which in practice blocked the pause on every break
//! after one that auto-closed unanswered (confirmed from a real user's log).
//!
//! On Linux: uses MPRIS (the Media Player Remote Interfacing Specification,
//! exposed over the session D-Bus) via the `mpris` crate. Unlike macOS's
//! private-API wall, MPRIS genuinely exposes per-player `PlaybackStatus`, so
//! Linux gets the same query-then-pause approach as Windows -- pausing only
//! players actually playing -- rather than the macOS blind toggle.
//!
//! On Android: there is no cross-app API to list playback sessions and
//! their state the way SMTC (Windows) or MPRIS (Linux) do, so this requests
//! transient audio focus (`AUDIOFOCUS_GAIN_TRANSIENT`) via `AudioManager`
//! instead -- a request, not a query. Any well-behaved playing app receives
//! `AUDIOFOCUS_LOSS_TRANSIENT` and pauses itself as a matter of the
//! platform's audio-focus contract; an app with nothing playing simply has
//! nothing to duck. This is NOT macOS's blind key-toggle: abandoning the
//! focus request on break-end (`resume_playing_sessions`, called from
//! `close_overlay`) only signals apps that actually ducked for *this*
//! request, so it can't resume media that was already paused before the
//! break started. No special permission needed -- a plain public API.
//!
//! All platforms: this is a "strong deterrent, not an absolute lock" like
//! the rest of the overlay enforcement (see hook.rs) -- nothing here
//! guarantees media was found or actually paused, and any failure here must
//! never block the overlay from showing.

use chrono::DateTime;

/// Which mechanism macOS uses to pause media. Module scope and deliberately
/// not `#[cfg(target_os = "macos")]`-gated so the selection rule below is unit
/// tested on every host, not only on a Mac.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(dead_code)]
pub(crate) enum MacosPauseStrategy {
    /// `MRMediaRemoteSendCommand(kMRPause)` -- no permission, explicit pause.
    MediaRemote,
    /// The synthetic NX_KEYTYPE_PLAY media key -- needs Accessibility
    /// (post-event) access, and is a blind toggle.
    MediaKey,
}

/// MediaRemote wins by default: it needs no TCC grant, and `kMRPause` is
/// explicit so it cannot resume media the user had already paused. The media
/// key is used only when the user forces it via
/// `app_setting.macos_media_key_fallback_enabled`, or when
/// `MRMediaRemoteSendCommand` could not be resolved -- it is a private symbol
/// Apple can withdraw without notice (it already walled MediaRemote's
/// *getters* in macOS 15.4), so "unavailable" has to degrade to the old path
/// rather than silently stop pausing media altogether.
#[allow(dead_code)]
pub(crate) fn select_macos_pause_strategy(
    force_media_key: bool,
    media_remote_available: bool,
) -> MacosPauseStrategy {
    if force_media_key || !media_remote_available {
        MacosPauseStrategy::MediaKey
    } else {
        MacosPauseStrategy::MediaRemote
    }
}

/// True iff `last` is a timestamp at or after `slot_start` -- i.e. it happened
/// during this same break occurrence. Both are RFC3339 and carry different
/// UTC offsets, so this parses them rather than comparing strings. A missing
/// or unparseable timestamp reads as "not during this break", which fails
/// open: the caller does its action rather than skipping it.
#[allow(dead_code)]
pub(crate) fn timestamp_is_within_break(last: Option<&str>, slot_start: &str) -> bool {
    let Some(last) = last else {
        return false;
    };
    match (
        DateTime::parse_from_rfc3339(last),
        DateTime::parse_from_rfc3339(slot_start),
    ) {
        (Ok(last), Ok(slot_start)) => last >= slot_start,
        _ => false,
    }
}

#[cfg(windows)]
mod windows_impl {
    use tauri::AppHandle;
    use windows::Media::Control::{
        GlobalSystemMediaTransportControlsSessionManager,
        GlobalSystemMediaTransportControlsSessionPlaybackStatus,
    };

    fn pause_playing_sessions_inner() -> windows::core::Result<()> {
        let manager = GlobalSystemMediaTransportControlsSessionManager::RequestAsync()?.get()?;
        for session in manager.GetSessions()? {
            let status = session.GetPlaybackInfo()?.PlaybackStatus()?;
            if status == GlobalSystemMediaTransportControlsSessionPlaybackStatus::Playing {
                let _ = session.TryPauseAsync()?.get();
            }
        }
        Ok(())
    }

    pub fn pause_playing_sessions(_app: &AppHandle) {
        if let Err(e) = pause_playing_sessions_inner() {
            log::warn!("pause_playing_sessions: failed to query/pause media sessions: {e:?}");
        }
    }
}

#[cfg(target_os = "macos")]
mod macos_impl {
    use std::ffi::{c_char, c_int, c_void, CStr, CString};
    use std::mem::size_of;
    use std::ptr;
    use std::sync::atomic::Ordering;
    use std::sync::OnceLock;

    use chrono::{SecondsFormat, Utc};
    use objc2_app_kit::{NSEvent, NSEventModifierFlags, NSEventType};
    use objc2_core_graphics::{
        CGEvent, CGEventTapLocation, CGPreflightPostEventAccess, CGRequestPostEventAccess,
    };
    use objc2_foundation::NSPoint;
    use tauri::{AppHandle, Emitter, Manager};

    use crate::state::AppState;
    use crate::{LAST_MEDIA_TOGGLE_AT, MACOS_MEDIA_KEY_FALLBACK_ENABLED, MEDIA_USER_RESUMED_AT};

    // From the public IOKit header <IOKit/hidsystem/ev_keymap.h>, not exposed
    // as Rust constants by any crate here.
    const NX_KEYTYPE_PLAY: isize = 16;
    const NX_SUBTYPE_AUX_CONTROL_BUTTONS: i16 = 8;
    const KEY_STATE_DOWN: isize = 0xa;
    const KEY_STATE_UP: isize = 0xb;

    const ACCESSIBILITY_SETTINGS_URL: &str =
        "x-apple.systempreferences:com.apple.preference.security?Privacy_Accessibility";

    // Core Audio HAL, from <CoreAudio/AudioHardware.h> and AudioHardwareBase.h.
    // C multi-char constants like 'dev#' are big-endian four-char codes.
    const fn fourcc(code: &[u8; 4]) -> u32 {
        u32::from_be_bytes(*code)
    }
    const K_AUDIO_OBJECT_SYSTEM_OBJECT: u32 = 1;
    const K_AUDIO_HARDWARE_PROPERTY_DEVICES: u32 = fourcc(b"dev#");
    const K_AUDIO_DEVICE_PROPERTY_STREAMS: u32 = fourcc(b"stm#");
    const K_AUDIO_DEVICE_PROPERTY_DEVICE_IS_RUNNING_SOMEWHERE: u32 = fourcc(b"gone");
    const K_AUDIO_OBJECT_PROPERTY_SCOPE_GLOBAL: u32 = fourcc(b"glob");
    const K_AUDIO_OBJECT_PROPERTY_SCOPE_OUTPUT: u32 = fourcc(b"outp");
    const K_AUDIO_OBJECT_PROPERTY_ELEMENT_MAIN: u32 = 0;

    #[repr(C)]
    struct AudioObjectPropertyAddress {
        selector: u32,
        scope: u32,
        element: u32,
    }

    #[link(name = "CoreAudio", kind = "framework")]
    extern "C" {
        fn AudioObjectGetPropertyDataSize(
            object_id: u32,
            address: *const AudioObjectPropertyAddress,
            qualifier_data_size: u32,
            qualifier_data: *const c_void,
            out_data_size: *mut u32,
        ) -> i32;
        fn AudioObjectGetPropertyData(
            object_id: u32,
            address: *const AudioObjectPropertyAddress,
            qualifier_data_size: u32,
            qualifier_data: *const c_void,
            io_data_size: *mut u32,
            out_data: *mut c_void,
        ) -> i32;
    }

    fn property_address(selector: u32, scope: u32) -> AudioObjectPropertyAddress {
        AudioObjectPropertyAddress {
            selector,
            scope,
            element: K_AUDIO_OBJECT_PROPERTY_ELEMENT_MAIN,
        }
    }

    // ---- MediaRemote (private framework), the default pause mechanism ----
    //
    // MediaRemote's binary exists only inside the dyld shared cache and ships
    // no .tbd stub, so it cannot be linked the way CoreAudio above is -- it
    // has to be resolved at runtime. dlopen/dlsym/dlerror live in libSystem,
    // which std already links on macOS, so this needs no new crate.
    // Deliberately not the `libloading` crate: its main benefit over this is
    // an RAII `dlclose`, which is exactly what must NOT happen here (see
    // MR_SEND_COMMAND below).
    const RTLD_LAZY: c_int = 0x1;
    const RTLD_LOCAL: c_int = 0x4;
    const MEDIA_REMOTE_PATH: &str =
        "/System/Library/PrivateFrameworks/MediaRemote.framework/MediaRemote";
    const MR_SEND_COMMAND_SYMBOL: &[u8] = b"MRMediaRemoteSendCommand\0";

    /// From MediaRemote's private command enum. `kMRTogglePlayPause` (2) is
    /// deliberately absent: it was never confirmed to actually work during
    /// testing (unlike these two, both visually confirmed), and being a
    /// *toggle* it would reintroduce the exact failure mode this path exists
    /// to avoid.
    const K_MR_PLAY: c_int = 0;
    const K_MR_PAUSE: c_int = 1;

    extern "C" {
        fn dlopen(path: *const c_char, mode: c_int) -> *mut c_void;
        fn dlsym(handle: *mut c_void, symbol: *const c_char) -> *mut c_void;
        fn dlerror() -> *const c_char;
    }

    /// `MRMediaRemoteSendCommand(MRMediaRemoteCommand, CFDictionaryRef) -> Boolean`.
    /// CoreFoundation's `Boolean` is an `unsigned char`, NOT a Rust `bool` --
    /// declaring it as `bool` would be undefined behavior the moment the
    /// callee returned any nonzero byte other than 1. The userInfo dictionary
    /// is always null for the commands used here.
    type MrSendCommandFn = unsafe extern "C" fn(c_int, *const c_void) -> u8;

    /// Resolved once per process. Only the *function pointer* is cached, never
    /// the `dlopen` handle: `unsafe extern "C" fn` pointers are already
    /// `Send + Sync`, so this needs no newtype wrapper and no `unsafe impl`.
    /// The handle is deliberately leaked (never `dlclose`d) -- the framework
    /// should stay mapped for the life of the process anyway, and unloading it
    /// is precisely what would leave this cached pointer dangling.
    static MR_SEND_COMMAND: OnceLock<Option<MrSendCommandFn>> = OnceLock::new();

    fn last_dl_error() -> String {
        // SAFETY: dlerror() returns either null or a valid C string owned by
        // libSystem -- read only, never freed here.
        let ptr = unsafe { dlerror() };
        if ptr.is_null() {
            return "(no dlerror)".to_string();
        }
        unsafe { CStr::from_ptr(ptr) }.to_string_lossy().into_owned()
    }

    /// `None` means the symbol could not be resolved, which is a supported
    /// state, not an error: the caller falls back to the media-key path.
    /// Never panics -- a private symbol Apple withdrew must degrade, not crash.
    fn mr_send_command() -> Option<MrSendCommandFn> {
        *MR_SEND_COMMAND.get_or_init(|| {
            let Ok(path) = CString::new(MEDIA_REMOTE_PATH) else {
                return None;
            };
            // SAFETY: `path` is a valid NUL-terminated C string that outlives
            // this call; dlopen returns either a valid handle or null.
            let handle = unsafe { dlopen(path.as_ptr(), RTLD_LAZY | RTLD_LOCAL) };
            if handle.is_null() {
                log::warn!(
                    "media pause: dlopen(MediaRemote) failed ({}) -- falling back to the media-key path",
                    last_dl_error()
                );
                return None;
            }
            // SAFETY: `handle` is non-null from dlopen above, and the symbol
            // name is a NUL-terminated byte string literal.
            let sym = unsafe { dlsym(handle, MR_SEND_COMMAND_SYMBOL.as_ptr().cast::<c_char>()) };
            if sym.is_null() {
                log::warn!(
                    "media pause: MRMediaRemoteSendCommand not found ({}) -- falling back to the media-key path",
                    last_dl_error()
                );
                return None;
            }
            log::info!(
                "media pause: resolved MRMediaRemoteSendCommand -- the permission-free path is available"
            );
            // SAFETY: the symbol is non-null, and MediaRemote's
            // MRMediaRemoteSendCommand has exactly this C signature.
            Some(unsafe { std::mem::transmute::<*mut c_void, MrSendCommandFn>(sym) })
        })
    }

    pub fn media_remote_available() -> bool {
        mr_send_command().is_some()
    }

    /// Dispatches one MediaRemote command. The returned byte means the command
    /// was *accepted for dispatch* -- MediaRemote hands it to `mediaremoted`
    /// over XPC asynchronously -- and NOT that anything was actually paused.
    /// There is no way to learn that: MediaRemote's now-playing getters are
    /// entitlement-walled since macOS 15.4. Never surface this as "Paused".
    fn send_media_remote_command(command: c_int) -> bool {
        let Some(send) = mr_send_command() else {
            return false;
        };
        // SAFETY: `send` is MRMediaRemoteSendCommand as resolved above; a null
        // userInfo dictionary is what these commands take.
        let dispatched = unsafe { send(command, ptr::null()) };
        log::info!("media pause: MRMediaRemoteSendCommand({command}) dispatched={dispatched}");
        dispatched != 0
    }

    /// Resumes playback. Stamps `MEDIA_USER_RESUMED_AT` so a mid-break overlay
    /// re-show can't immediately undo an explicit user resume -- see
    /// `pause_via_media_remote`.
    pub fn media_remote_play() -> bool {
        let dispatched = send_media_remote_command(K_MR_PLAY);
        if dispatched {
            *MEDIA_USER_RESUMED_AT.lock().unwrap() =
                Some(Utc::now().to_rfc3339_opts(SecondsFormat::Millis, true));
        }
        dispatched
    }

    pub fn media_remote_pause() -> bool {
        send_media_remote_command(K_MR_PAUSE)
    }

    /// Which mechanism a break would actually use right now. Computed live --
    /// the Settings toggle changes it at runtime.
    pub fn pause_backend() -> &'static str {
        match super::select_macos_pause_strategy(
            MACOS_MEDIA_KEY_FALLBACK_ENABLED.load(Ordering::SeqCst),
            media_remote_available(),
        ) {
            super::MacosPauseStrategy::MediaRemote => "mediaremote",
            super::MacosPauseStrategy::MediaKey => "media_key",
        }
    }

    /// Whether any audio output device is currently running in any process.
    /// `None` if Core Audio couldn't be queried, so the caller can fail open.
    /// "Running" is not "audible": apps can hold a device open while paused or
    /// silent, so `Some(true)` is only a hint. `Some(false)` is the useful
    /// answer: nothing is sending audio anywhere, so a blind toggle could only
    /// resume something.
    fn any_output_device_running() -> Option<bool> {
        let devices_address =
            property_address(K_AUDIO_HARDWARE_PROPERTY_DEVICES, K_AUDIO_OBJECT_PROPERTY_SCOPE_GLOBAL);
        let mut size: u32 = 0;
        // SAFETY: plain C calls with valid pointers to live locals/buffers; the
        // HAL writes at most `size` bytes into the device-ID buffer.
        let status = unsafe {
            AudioObjectGetPropertyDataSize(
                K_AUDIO_OBJECT_SYSTEM_OBJECT,
                &devices_address,
                0,
                ptr::null(),
                &mut size,
            )
        };
        if status != 0 {
            log::warn!("media toggle: listing audio devices (size) failed, OSStatus={status}");
            return None;
        }
        let mut device_ids = vec![0u32; size as usize / size_of::<u32>()];
        let mut size = (device_ids.len() * size_of::<u32>()) as u32;
        let status = unsafe {
            AudioObjectGetPropertyData(
                K_AUDIO_OBJECT_SYSTEM_OBJECT,
                &devices_address,
                0,
                ptr::null(),
                &mut size,
                device_ids.as_mut_ptr().cast(),
            )
        };
        if status != 0 {
            log::warn!("media toggle: listing audio devices failed, OSStatus={status}");
            return None;
        }
        device_ids.truncate(size as usize / size_of::<u32>());

        let streams_address =
            property_address(K_AUDIO_DEVICE_PROPERTY_STREAMS, K_AUDIO_OBJECT_PROPERTY_SCOPE_OUTPUT);
        let running_address = property_address(
            K_AUDIO_DEVICE_PROPERTY_DEVICE_IS_RUNNING_SOMEWHERE,
            K_AUDIO_OBJECT_PROPERTY_SCOPE_GLOBAL,
        );
        let mut queried_output_devices = 0;
        for device_id in device_ids {
            let mut streams_size: u32 = 0;
            let status = unsafe {
                AudioObjectGetPropertyDataSize(
                    device_id,
                    &streams_address,
                    0,
                    ptr::null(),
                    &mut streams_size,
                )
            };
            if status != 0 || streams_size == 0 {
                continue;
            }
            let mut running: u32 = 0;
            let mut running_size = size_of::<u32>() as u32;
            let status = unsafe {
                AudioObjectGetPropertyData(
                    device_id,
                    &running_address,
                    0,
                    ptr::null(),
                    &mut running_size,
                    (&mut running as *mut u32).cast(),
                )
            };
            if status != 0 {
                continue;
            }
            queried_output_devices += 1;
            if running != 0 {
                log::info!("media toggle: audio output device {device_id} is running");
                return Some(true);
            }
        }
        if queried_output_devices == 0 {
            log::warn!("media toggle: no audio output device could be queried");
            return None;
        }
        Some(false)
    }

    /// Posts one half (key-down or key-up) of a synthetic hardware Play/Pause
    /// media-key press. NSEventTypeSystemDefined media-key events can only be
    /// constructed via the AppKit NSEvent factory method -- there's no plain
    /// CGEventCreate* equivalent for this event subtype -- so this goes
    /// through NSEvent first and reads the CGEvent back off it to post.
    fn post_media_key_event(down: bool) {
        let state = if down { KEY_STATE_DOWN } else { KEY_STATE_UP };
        let data1 = (NX_KEYTYPE_PLAY << 16) | (state << 8);
        let modifier_flags = if down { 0xa00 } else { 0xb00 };

        let Some(event) = NSEvent::otherEventWithType_location_modifierFlags_timestamp_windowNumber_context_subtype_data1_data2(
            NSEventType::SystemDefined,
            NSPoint::new(0.0, 0.0),
            NSEventModifierFlags(modifier_flags),
            0.0,
            0,
            None,
            NX_SUBTYPE_AUX_CONTROL_BUTTONS,
            data1,
            -1,
        ) else {
            log::warn!("media toggle: failed to construct synthetic media-key NSEvent");
            return;
        };

        let Some(cg_event) = event.CGEvent() else {
            log::warn!("media toggle: synthetic NSEvent had no backing CGEvent");
            return;
        };

        CGEvent::post(CGEventTapLocation::SessionEventTap, Some(&cg_event));
    }

    /// True iff the last posted toggle landed at or after this break's slot
    /// start -- i.e. this same break already toggled once. Media-key path
    /// only: the MediaRemote path sends an explicit, idempotent pause and so
    /// needs no such guard.
    fn already_toggled_this_break(slot_start: &str) -> bool {
        let last_toggle = LAST_MEDIA_TOGGLE_AT.lock().unwrap().clone();
        super::timestamp_is_within_break(last_toggle.as_deref(), slot_start)
    }

    pub fn media_key_permission_granted() -> bool {
        CGPreflightPostEventAccess()
    }

    /// TCC only ever shows its own prompt once per app identity, so this also
    /// opens the Accessibility pane directly -- otherwise a second click after
    /// a dismissed prompt would appear to do nothing.
    pub fn request_media_key_permission() {
        if CGPreflightPostEventAccess() {
            return;
        }
        let granted = CGRequestPostEventAccess();
        log::info!("media toggle: requested post-event access, granted={granted}");
        if let Err(e) = std::process::Command::new("open")
            .arg(ACCESSIBILITY_SETTINGS_URL)
            .status()
        {
            log::warn!("media toggle: failed to open Accessibility settings: {e}");
        }
    }

    /// The current break's slot start, as recorded on `OverlayState`.
    fn current_slot_start(app: &AppHandle) -> String {
        app.state::<AppState>()
            .overlay
            .lock()
            .unwrap()
            .current_slot_start
            .clone()
    }

    pub fn pause_playing_sessions(app: &AppHandle) {
        let force_media_key = MACOS_MEDIA_KEY_FALLBACK_ENABLED.load(Ordering::SeqCst);
        match super::select_macos_pause_strategy(force_media_key, media_remote_available()) {
            super::MacosPauseStrategy::MediaRemote => pause_via_media_remote(app),
            super::MacosPauseStrategy::MediaKey => pause_via_media_key(app),
        }
    }

    /// The permission-free default. Deliberately carries none of the guards
    /// `pause_via_media_key` does: `already_toggled_this_break` and
    /// `any_output_device_running` both exist only because the media key is a
    /// blind *toggle*, and `kMRPause` is explicit and idempotent -- re-pausing
    /// an already-paused player is a no-op, so a mid-break relaunch or
    /// suspend-resume costs nothing here. Also deliberately does NOT record
    /// `LAST_MEDIA_TOGGLE_AT` or emit "media-toggle://recorded": that guard
    /// belongs to the fallback path alone, and writing it here would make a
    /// later switch to that path skip a legitimate pause.
    ///
    /// Note the one thing it does NOT skip on: post-event/Accessibility access.
    /// `CGPreflightPostEventAccess` must stay inside `pause_via_media_key` --
    /// checking it here would silently disable the pause for every user
    /// without a grant, which is now essentially all of them.
    fn pause_via_media_remote(app: &AppHandle) {
        let slot_start = current_slot_start(app);
        // A "Play" pressed on the break screen is explicit user intent. A
        // mid-break overlay re-show (relaunch, suspend-resume) would otherwise
        // immediately undo it -- the same case already_toggled_this_break
        // covers for the fallback path.
        let resumed_at = MEDIA_USER_RESUMED_AT.lock().unwrap().clone();
        if super::timestamp_is_within_break(resumed_at.as_deref(), &slot_start) {
            log::info!(
                "media pause: skipped (user resumed media from the break screen during the break starting {slot_start})"
            );
            return;
        }
        send_media_remote_command(K_MR_PAUSE);
    }

    /// The original media-key path, unchanged -- still a blind toggle, so it
    /// still needs every guard it has always had. Used only when the user
    /// forces it or MediaRemote could not be resolved.
    fn pause_via_media_key(app: &AppHandle) {
        let slot_start = current_slot_start(app);

        if already_toggled_this_break(&slot_start) {
            log::info!("media toggle: skipped (already toggled for the break starting {slot_start})");
            return;
        }

        match any_output_device_running() {
            Some(false) => {
                log::info!(
                    "media toggle: skipped (no audio output device is running, so nothing is playing -- toggling could only resume paused media)"
                );
                return;
            }
            Some(true) => {}
            None => log::warn!("media toggle: couldn't determine whether audio is playing, toggling anyway"),
        }

        if !CGPreflightPostEventAccess() {
            log::warn!(
                "media toggle: skipped -- post-event (Accessibility) access not granted, macOS would silently drop the media key"
            );
            return;
        }

        post_media_key_event(true);
        post_media_key_event(false);
        log::info!("media toggle: posted Play/Pause media key for the break starting {slot_start}");

        let now = Utc::now().to_rfc3339_opts(SecondsFormat::Millis, true);
        *LAST_MEDIA_TOGGLE_AT.lock().unwrap() = Some(now.clone());
        // The frontend persists this to app_setting.last_toggle_time so the
        // guard survives a crash/relaunch mid-break -- see media-toggle://recorded
        // in src/routes/+page.svelte.
        let _ = app.emit("media-toggle://recorded", now);
    }
}

#[cfg(target_os = "linux")]
mod linux_impl {
    //! Crate-API note: verify `mpris` crate's exact surface (`PlayerFinder`,
    //! `find_all`, error types) against its currently pinned version the
    //! first time this is compiled on a Linux toolchain -- this can't be
    //! compile-checked from a Windows dev machine.
    use mpris::{PlaybackStatus, PlayerFinder};
    use tauri::AppHandle;

    fn pause_playing_sessions_inner() -> Result<(), mpris::FindingError> {
        let finder = PlayerFinder::new()?;
        for player in finder.find_all()? {
            if let Ok(PlaybackStatus::Playing) = player.get_playback_status() {
                let _ = player.pause();
            }
        }
        Ok(())
    }

    pub fn pause_playing_sessions(_app: &AppHandle) {
        if let Err(e) = pause_playing_sessions_inner() {
            log::warn!("pause_playing_sessions: failed to query/pause MPRIS players: {e:?}");
        }
    }
}

#[cfg(target_os = "android")]
mod android_impl {
    use tauri::{AppHandle, Manager, Wry};

    use crate::android_bridge::AndroidBridge;

    pub fn pause_playing_sessions(app: &AppHandle) {
        let bridge = app.state::<AndroidBridge<Wry>>();
        if let Err(e) = bridge.pause_audio_focus() {
            log::warn!("pause_playing_sessions: pauseAudioFocus failed: {e:?}");
        }
    }

    /// Abandons the focus request from `pause_playing_sessions`, if one is
    /// outstanding -- releases the transient hold so whatever ducked for it
    /// is free to resume. Called from `close_overlay`; harmless no-op if
    /// nothing was ever granted (e.g. the request was denied, or this fires
    /// twice).
    pub fn resume_playing_sessions(app: &AppHandle) {
        let bridge = app.state::<AndroidBridge<Wry>>();
        if let Err(e) = bridge.resume_audio_focus() {
            log::warn!("resume_playing_sessions: resumeAudioFocus failed: {e:?}");
        }
    }
}

/// Same shape as `android_impl`: a non-mixable AVAudioSession interrupts
/// other apps' playback, and deactivating it lets them resume. iOS refuses
/// activation from the background, so `IOS_MEDIA_PAUSED_SLOT` records which
/// break was actually paused and `retry_pause_on_resume` (called from
/// `RunEvent::Resumed`) fills in a break whose pause attempt failed --
/// never one that succeeded, so it can't re-pause what the user restarted.
#[cfg(target_os = "ios")]
mod ios_impl {
    use std::sync::atomic::Ordering;
    use std::sync::Mutex;

    use tauri::{AppHandle, Manager, Wry};

    use crate::ios_bridge::IosBridge;
    use crate::state::AppState;

    static IOS_MEDIA_PAUSED_SLOT: Mutex<Option<String>> = Mutex::new(None);

    fn current_open_slot(app: &AppHandle) -> Option<String> {
        let state = app.state::<AppState>();
        let overlay = state.overlay.lock().unwrap();
        overlay.open.then(|| overlay.current_slot_start.clone())
    }

    pub fn pause_playing_sessions(app: &AppHandle) {
        let bridge = app.state::<IosBridge<Wry>>();
        match bridge.pause_other_audio() {
            Ok((true, _)) => {
                *IOS_MEDIA_PAUSED_SLOT.lock().unwrap() = current_open_slot(app);
            }
            Ok((false, err)) => {
                log::warn!("pause_playing_sessions: audio session not activated: {err:?}");
            }
            Err(e) => log::warn!("pause_playing_sessions: pauseOtherAudio failed: {e:?}"),
        }
    }

    pub fn resume_playing_sessions(app: &AppHandle) {
        *IOS_MEDIA_PAUSED_SLOT.lock().unwrap() = None;
        let bridge = app.state::<IosBridge<Wry>>();
        if let Err(e) = bridge.resume_other_audio() {
            log::warn!("resume_playing_sessions: resumeOtherAudio failed: {e:?}");
        }
    }

    pub fn retry_pause_on_resume(app: &AppHandle) {
        if !crate::MEDIA_PAUSE_ON_BREAK_ENABLED.load(Ordering::SeqCst) {
            return;
        }
        let Some(slot) = current_open_slot(app) else {
            return;
        };
        if IOS_MEDIA_PAUSED_SLOT.lock().unwrap().as_deref() == Some(slot.as_str()) {
            return;
        }
        log::info!("media: retrying break pause after resume");
        pause_playing_sessions(app);
    }
}

#[cfg(not(any(
    windows,
    target_os = "macos",
    target_os = "linux",
    target_os = "android",
    target_os = "ios"
)))]
mod noop_impl {
    use tauri::AppHandle;

    pub fn pause_playing_sessions(_app: &AppHandle) {}
}

#[cfg(windows)]
pub use windows_impl::pause_playing_sessions;
#[cfg(target_os = "macos")]
pub use macos_impl::{
    media_key_permission_granted, media_remote_available, media_remote_pause, media_remote_play,
    pause_backend, pause_playing_sessions, request_media_key_permission,
};
#[cfg(target_os = "linux")]
pub use linux_impl::pause_playing_sessions;
#[cfg(target_os = "android")]
pub use android_impl::pause_playing_sessions;
#[cfg(target_os = "ios")]
pub use ios_impl::{pause_playing_sessions, retry_pause_on_resume};
#[cfg(not(any(
    windows,
    target_os = "macos",
    target_os = "linux",
    target_os = "android",
    target_os = "ios"
)))]
pub use noop_impl::pause_playing_sessions;

/// Only macOS gates media pause on a permission (see macos_impl).
#[cfg(not(target_os = "macos"))]
pub fn media_key_permission_granted() -> bool {
    true
}

#[cfg(not(target_os = "macos"))]
pub fn request_media_key_permission() {}

/// MediaRemote is a macOS-private framework -- everywhere else these are
/// stubs, so the frontend can call the commands wrapping them without a
/// per-platform invoke gate (same reasoning as the macOS kiosk-mode setting).
#[cfg(not(target_os = "macos"))]
pub fn media_remote_available() -> bool {
    false
}

#[cfg(not(target_os = "macos"))]
pub fn media_remote_play() -> bool {
    false
}

#[cfg(not(target_os = "macos"))]
pub fn media_remote_pause() -> bool {
    false
}

/// Only macOS picks between two pause mechanisms; every other platform has
/// exactly one, so there is nothing to report.
#[cfg(not(target_os = "macos"))]
pub fn pause_backend() -> &'static str {
    "other"
}

/// Symmetric release for `pause_playing_sessions`, called from
/// `close_overlay`. Only Android's audio-focus model has anything to
/// release (a granted `AudioFocusRequest`) and iOS's (an active
/// AVAudioSession) -- Windows/macOS/Linux act on media sessions directly
/// with no analogous "hold" to give back, so they stay a no-op here.
#[cfg(target_os = "android")]
pub use android_impl::resume_playing_sessions;
#[cfg(target_os = "ios")]
pub use ios_impl::resume_playing_sessions;
#[cfg(not(any(target_os = "android", target_os = "ios")))]
pub fn resume_playing_sessions(_app: &tauri::AppHandle) {}


#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn media_remote_is_the_default_when_available() {
        assert_eq!(
            select_macos_pause_strategy(false, true),
            MacosPauseStrategy::MediaRemote
        );
    }

    #[test]
    fn falls_back_to_the_media_key_when_mediaremote_is_unavailable() {
        // Apple withdrawing the private symbol must degrade to the old path,
        // not silently stop pausing media.
        assert_eq!(
            select_macos_pause_strategy(false, false),
            MacosPauseStrategy::MediaKey
        );
    }

    #[test]
    fn the_user_setting_forces_the_media_key_even_when_mediaremote_works() {
        assert_eq!(
            select_macos_pause_strategy(true, true),
            MacosPauseStrategy::MediaKey
        );
    }

    #[test]
    fn forced_and_unavailable_still_selects_the_media_key() {
        assert_eq!(
            select_macos_pause_strategy(true, false),
            MacosPauseStrategy::MediaKey
        );
    }

    #[test]
    fn a_timestamp_after_the_slot_start_is_within_the_break() {
        assert!(timestamp_is_within_break(
            Some("2026-09-20T10:26:00.000Z"),
            "2026-09-20T10:25:00+00:00"
        ));
    }

    #[test]
    fn a_timestamp_before_the_slot_start_is_not_within_the_break() {
        assert!(!timestamp_is_within_break(
            Some("2026-09-20T10:24:59.000Z"),
            "2026-09-20T10:25:00+00:00"
        ));
    }

    #[test]
    fn compares_instants_not_strings_across_differing_offsets() {
        // 12:26+02:00 is 10:26Z, i.e. after a 10:25Z slot start -- but sorts
        // BEFORE it as a plain string. This is why these are parsed.
        assert!(timestamp_is_within_break(
            Some("2026-09-20T12:26:00+02:00"),
            "2026-09-20T10:25:00+00:00"
        ));
    }

    #[test]
    fn a_missing_or_unparseable_timestamp_fails_open() {
        // Fail open: the caller pauses rather than skipping.
        assert!(!timestamp_is_within_break(None, "2026-09-20T10:25:00+00:00"));
        assert!(!timestamp_is_within_break(
            Some("not a timestamp"),
            "2026-09-20T10:25:00+00:00"
        ));
    }
}
