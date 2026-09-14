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
//! On macOS: there is no public API to query playback state the way SMTC
//! does (only the private, undocumented MediaRemote.framework can), so this
//! posts a synthetic hardware Play/Pause media-key event instead -- a blind
//! toggle, with exactly the failure mode described above for Windows. This
//! was a deliberate, explicit tradeoff (see CLAUDE.md). Three things bound it:
//! `macos_impl::already_toggled_this_break` skips a second toggle for the
//! same break occurrence (a relaunch or suspend-resume mid-break re-shows the
//! overlay); `macos_impl::any_output_device_running` skips when no audio
//! output device is running anywhere, since then nothing can be playing and a
//! toggle could only resume something; and nothing is posted at all unless
//! the app holds macOS's post-event access (listed under Privacy & Security >
//! Accessibility) -- without it the WindowServer silently drops synthetic
//! events. An earlier
//! guard reset only on a *submitted* wellness check-in, which in practice
//! blocked the pause on every break after one that auto-closed unanswered
//! (confirmed from a real user's log).
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
    use std::ffi::c_void;
    use std::mem::size_of;
    use std::ptr;

    use chrono::{DateTime, SecondsFormat, Utc};
    use objc2_app_kit::{NSEvent, NSEventModifierFlags, NSEventType};
    use objc2_core_graphics::{
        CGEvent, CGEventTapLocation, CGPreflightPostEventAccess, CGRequestPostEventAccess,
    };
    use objc2_foundation::NSPoint;
    use tauri::{AppHandle, Emitter, Manager};

    use crate::state::AppState;
    use crate::LAST_MEDIA_TOGGLE_AT;

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
    /// start -- i.e. this same break already toggled once.
    fn already_toggled_this_break(slot_start: &str) -> bool {
        let Some(last_toggle) = LAST_MEDIA_TOGGLE_AT.lock().unwrap().clone() else {
            return false;
        };
        match (
            DateTime::parse_from_rfc3339(&last_toggle),
            DateTime::parse_from_rfc3339(slot_start),
        ) {
            (Ok(toggled_at), Ok(slot_start)) => toggled_at >= slot_start,
            _ => false,
        }
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

    pub fn pause_playing_sessions(app: &AppHandle) {
        let slot_start = app
            .state::<AppState>()
            .overlay
            .lock()
            .unwrap()
            .current_slot_start
            .clone();

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

#[cfg(not(any(windows, target_os = "macos", target_os = "linux", target_os = "android")))]
mod noop_impl {
    use tauri::AppHandle;

    pub fn pause_playing_sessions(_app: &AppHandle) {}
}

#[cfg(windows)]
pub use windows_impl::pause_playing_sessions;
#[cfg(target_os = "macos")]
pub use macos_impl::{media_key_permission_granted, pause_playing_sessions, request_media_key_permission};
#[cfg(target_os = "linux")]
pub use linux_impl::pause_playing_sessions;
#[cfg(target_os = "android")]
pub use android_impl::pause_playing_sessions;
#[cfg(not(any(windows, target_os = "macos", target_os = "linux", target_os = "android")))]
pub use noop_impl::pause_playing_sessions;

/// Only macOS gates media pause on a permission (see macos_impl).
#[cfg(not(target_os = "macos"))]
pub fn media_key_permission_granted() -> bool {
    true
}

#[cfg(not(target_os = "macos"))]
pub fn request_media_key_permission() {}

/// Symmetric release for `pause_playing_sessions`, called from
/// `close_overlay`. Only Android's audio-focus model has anything to
/// release (a granted `AudioFocusRequest`) -- Windows/macOS/Linux act on
/// media sessions directly with no analogous "hold" to give back, so they
/// stay a no-op here.
#[cfg(target_os = "android")]
pub use android_impl::resume_playing_sessions;
#[cfg(not(target_os = "android"))]
pub fn resume_playing_sessions(_app: &tauri::AppHandle) {}
