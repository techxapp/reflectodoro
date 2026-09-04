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
    /// How many times `report_reflection_save_failure` (commands.rs) has been
    /// called for *this* overlay occurrence -- i.e. how many times the
    /// frontend's own `saveReflection`/`mark_reflection_entered` call has
    /// failed in a row while trying to submit. Tracked here (server-side,
    /// reset whenever the overlay opens/closes) rather than as pure frontend
    /// component state so the desktop escape hatch it gates
    /// (`close_after_save_failure`) can't be reached by a single direct
    /// `invoke` call -- it requires having actually reported failures first.
    /// Not a real security boundary against a devtools-capable user (nothing
    /// here is), just a deliberately higher bar than "one line of JS" for
    /// what's meant to be a last-resort escape from a DB that won't accept
    /// writes, not a casual way to skip reflecting -- see overlay page.
    pub save_failure_count: u32,
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
            save_failure_count: 0,
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
            save_failure_count: 0,
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
    /// Slot start (RFC3339, local offset) handed off to the wellness check-in
    /// window -- set by `open_checkin_for_slot` right before spawning it, read
    /// back by the window's own `get_checkin_slot` call on mount. See overlay.rs.
    pub checkin_slot: Mutex<Option<String>>,
    /// When the app process started. A newly created WebviewWindow on this
    /// machine renders permanently blank if shown within ~a couple seconds
    /// of process start (some WebView2/wry initialization race) -- both the
    /// overlay and catch-up windows are pre-created hidden at startup and
    /// held back from their first `show()` until this much time has passed,
    /// which sidesteps it. See `overlay::wait_for_webview_warmup`.
    pub started_at: std::time::Instant,
    pub dev_mode: bool,
    /// Cache of today's `daily_task_list.content`, Android only -- the native
    /// WindowManager overlay's plain WebView has no Tauri command/DB access of
    /// its own (see native_overlay.rs), so it can't fetch this itself the way
    /// the regular /overlay page calls `getTaskList` directly. Refreshed from
    /// SQLite right before the overlay is triggered (`native_overlay::
    /// refresh_task_list_cache`) and kept current by the overlay's own saves;
    /// safe to read synchronously from `overlay_state_json_for_android`
    /// because nothing else can write to today's row while the overlay --
    /// which captures all touch input -- is the only thing on screen.
    pub task_list: Mutex<String>,
    /// Cache of today's `not_to_do_list.content`, Android only -- mirrors
    /// `task_list` above for the same reason (see its doc comment).
    pub not_to_do_list: Mutex<String>,
    /// The local `YYYY-MM-DD` date stamp `task_list`/`not_to_do_list` above
    /// were captured under, Android only -- set once when the overlay opens
    /// (`native_overlay::refresh_task_list_cache`) and reused by
    /// `handle_save_task_list`/`handle_save_not_to_do_list` instead of each
    /// recomputing "today" fresh from `Local::now()` at save time. Without
    /// this, the `:55`-`:00` break slot -- which always straddles midnight --
    /// could load day N's content when the overlay opens at 23:55 but upsert
    /// a submission at 00:01 into day N+1's row instead, silently splitting
    /// one day's task list across two rows.
    pub task_list_date: Mutex<String>,
    /// Count of break slots the *next* submitted reflection will cover
    /// (`native_overlay::find_missed_slots(...).len()`), Android only --
    /// mirrors `task_list` above: the native WindowManager overlay has no DB
    /// access of its own, so this is computed once in Rust right before the
    /// overlay is shown (`native_overlay::refresh_missed_slot_count`) and read
    /// synchronously from `overlay_state_json_for_android` so the heading can
    /// say "last N pomodoros" the same way the regular /overlay page's
    /// `findMissedSlots`-derived `promptLabel` does. Safe to compute once per
    /// slot: nothing else writes to `reflection` while this overlay -- which
    /// captures all touch input -- is the only thing on screen.
    pub missed_slot_count: Mutex<usize>,
}

impl AppState {
    pub fn new(dev_mode: bool) -> Self {
        Self {
            overlay: Mutex::new(OverlayState::closed()),
            breakit_config: Mutex::new(BreakitConfig::default()),
            checkin_slot: Mutex::new(None),
            started_at: std::time::Instant::now(),
            dev_mode,
            task_list: Mutex::new(String::new()),
            not_to_do_list: Mutex::new(String::new()),
            task_list_date: Mutex::new(String::new()),
            missed_slot_count: Mutex::new(1),
        }
    }
}
