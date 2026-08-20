use serde::Serialize;
use std::sync::Mutex;

#[derive(Debug, Clone, Serialize)]
pub struct OverlayState {
    pub open: bool,
    pub reflection_entered: bool,
    /// Freshly generated per overlay -- see breakit.rs. Sent to the frontend
    /// so it can be displayed for the user to type.
    pub breakit_challenge: String,
    pub breakit_matched: bool,
    pub time_expired: bool,
    /// ISO local timestamp of the break slot this overlay currently represents.
    /// Rust only ever tracks the single *current* slot; whether a reflection
    /// should also cover the previous (unresolved) slot is decided entirely by
    /// the frontend against the DB when it mounts/updates — see overlay page.
    pub current_slot_start: String,
}

impl OverlayState {
    pub fn closed() -> Self {
        Self {
            open: false,
            reflection_entered: false,
            breakit_challenge: String::new(),
            breakit_matched: false,
            time_expired: false,
            current_slot_start: String::new(),
        }
    }

    pub fn opened_for(slot_start_iso: String, breakit_challenge: String) -> Self {
        Self {
            open: true,
            reflection_entered: false,
            breakit_challenge,
            breakit_matched: false,
            time_expired: false,
            current_slot_start: slot_start_iso,
        }
    }

    /// (time_expired AND reflection_entered) OR (breakit_matched AND reflection_entered)
    pub fn unlocked(&self) -> bool {
        self.reflection_entered && (self.time_expired || self.breakit_matched)
    }
}

#[derive(Debug, Clone)]
pub struct BreakitConfig {
    pub length: u32,
    pub include_special: bool,
}

impl Default for BreakitConfig {
    fn default() -> Self {
        Self {
            length: 15,
            include_special: false,
        }
    }
}

pub struct AppState {
    pub overlay: Mutex<OverlayState>,
    pub breakit_config: Mutex<BreakitConfig>,
    /// Slot start (RFC3339, local offset) handed off to the catch-up window --
    /// set by `open_catchup_window` right before spawning it, read back by the
    /// window's own `get_catchup_slot` call on mount. See overlay.rs.
    pub catchup_slot: Mutex<Option<String>>,
    /// When the app process started. A newly created WebviewWindow on this
    /// machine renders permanently blank if shown within ~a couple seconds
    /// of process start (some WebView2/wry initialization race) -- both the
    /// overlay and catch-up windows are pre-created hidden at startup and
    /// held back from their first `show()` until this much time has passed,
    /// which sidesteps it. See `overlay::wait_for_webview_warmup`.
    pub started_at: std::time::Instant,
    pub dev_mode: bool,
}

impl AppState {
    pub fn new(dev_mode: bool) -> Self {
        Self {
            overlay: Mutex::new(OverlayState::closed()),
            breakit_config: Mutex::new(BreakitConfig::default()),
            catchup_slot: Mutex::new(None),
            started_at: std::time::Instant::now(),
            dev_mode,
        }
    }
}
