//! Best-effort media pause fired when the break overlay opens. Queries
//! Windows' System Media Transport Controls (SMTC) for every registered
//! session (browser tabs, Spotify desktop, etc.) and pauses only the ones
//! actually playing. Deliberately NOT the simpler VK_MEDIA_PLAY_PAUSE
//! key-simulation approach: that's a toggle, so on a break that starts
//! while media is already paused (e.g. the previous break paused it and it
//! was never resumed) it would resume playback instead of leaving it
//! alone -- the opposite of what this feature is for. This is a
//! "strong deterrent, not an absolute lock" like the rest of the overlay
//! enforcement (see hook.rs): nothing here guarantees a session was found
//! or actually paused, and any failure here must never block the overlay
//! from showing.

#[cfg(windows)]
mod windows_impl {
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

    pub fn pause_playing_sessions() {
        if let Err(e) = pause_playing_sessions_inner() {
            log::warn!("pause_playing_sessions: failed to query/pause media sessions: {e:?}");
        }
    }
}

#[cfg(not(windows))]
mod noop_impl {
    pub fn pause_playing_sessions() {}
}

#[cfg(windows)]
pub use windows_impl::pause_playing_sessions;
#[cfg(not(windows))]
pub use noop_impl::pause_playing_sessions;
