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
//! was a deliberate, explicit tradeoff (see CLAUDE.md): shipping a
//! best-effort toggle now rather than waiting on a MediaRemote.framework-based
//! fix later. `macos_impl::should_skip_toggle` narrows (does not eliminate)
//! the risk by skipping a second toggle within the same break cycle -- see
//! its own doc comment. Note this guard uses `wellness_check.created_at` as
//! the "cycle completed" signal even though check-in is skippable (closing
//! or auto-closing it saves nothing); a user who routinely skips check-ins
//! will see the guard's reset stop firing after their first break. That's a
//! known, accepted tradeoff, not a bug -- `reflection.created_at` would have
//! been skip-proof instead, but `wellness_check.created_at` was the explicit
//! choice.
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
    use chrono::{SecondsFormat, Utc};
    use objc2_app_kit::{NSEvent, NSEventModifierFlags, NSEventType};
    use objc2_core_graphics::{CGEvent, CGEventTapLocation};
    use objc2_foundation::NSPoint;
    use tauri::{AppHandle, Emitter};

    use crate::{LAST_MEDIA_TOGGLE_AT, LAST_WELLNESS_CHECK_AT};

    // From the public IOKit header <IOKit/hidsystem/ev_keymap.h>, not exposed
    // as Rust constants by any crate here.
    const NX_KEYTYPE_PLAY: isize = 16;
    const NX_SUBTYPE_AUX_CONTROL_BUTTONS: i16 = 8;
    const KEY_STATE_DOWN: isize = 0xa;
    const KEY_STATE_UP: isize = 0xb;

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

    /// Skip iff we already toggled media paused this break cycle and no
    /// completed check-in has happened since -- toggling again in that
    /// window would resume the media we just paused. See this module's doc
    /// comment for what this guard does and does not protect against.
    fn should_skip_toggle() -> bool {
        let last_toggle = LAST_MEDIA_TOGGLE_AT.lock().unwrap().clone();
        let Some(last_toggle) = last_toggle else {
            return false;
        };
        match LAST_WELLNESS_CHECK_AT.lock().unwrap().clone() {
            Some(last_wellness) if last_wellness > last_toggle => false,
            _ => true,
        }
    }

    pub fn pause_playing_sessions(app: &AppHandle) {
        if should_skip_toggle() {
            log::info!("media toggle: skipped (already toggled since the last completed check-in)");
            return;
        }

        post_media_key_event(true);
        post_media_key_event(false);

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
pub use macos_impl::pause_playing_sessions;
#[cfg(target_os = "linux")]
pub use linux_impl::pause_playing_sessions;
#[cfg(target_os = "android")]
pub use android_impl::pause_playing_sessions;
#[cfg(not(any(windows, target_os = "macos", target_os = "linux", target_os = "android")))]
pub use noop_impl::pause_playing_sessions;

/// Symmetric release for `pause_playing_sessions`, called from
/// `close_overlay`. Only Android's audio-focus model has anything to
/// release (a granted `AudioFocusRequest`) -- Windows/macOS/Linux act on
/// media sessions directly with no analogous "hold" to give back, so they
/// stay a no-op here.
#[cfg(target_os = "android")]
pub use android_impl::resume_playing_sessions;
#[cfg(not(target_os = "android"))]
pub fn resume_playing_sessions(_app: &tauri::AppHandle) {}
