//! iOS break enforcement for while the app isn't running. Rust can't run in
//! the background on iOS (no foreground service, no alarms that relaunch the
//! app), so upcoming breaks are handed to the OS ahead of time as local
//! notifications, and a Live Activity shows the countdown on the Lock Screen /
//! Dynamic Island. Opening the app during a break then shows the ordinary
//! overlay. If the user never opens the app, nothing enforces the break -- a
//! platform ceiling (see CLAUDE.md's "iOS").
//!
//! All bridge calls happen here, from `run_scheduler`'s loop on the async
//! runtime, never from a command handler: synchronous commands run on the main
//! thread, and keeping every Swift call on one path makes that easy to hold.
//! State changes elsewhere (Pomodoro on/off, snooze, overlay open/close, app
//! resume) just call `wake()`.
#![cfg(target_os = "ios")]

use crate::grid::{self, Phase};
use crate::ios_bridge::{IosBridge, LiveActivityState};
use crate::state::AppState;
use crate::{POMODORO_ENABLED, POMODORO_SNOOZE_UNTIL_MS};
use chrono::{DateTime, Local, SecondsFormat, Utc};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;
use std::time::Duration;
use tauri::{AppHandle, Manager};
use tokio::sync::Notify;

/// iOS keeps at most 64 pending local notifications per app; 48 is one day of
/// breaks, leaving room for anything else the app schedules.
const NOTIFICATION_WINDOW: usize = 48;
const BREAK_LENGTH_MIN: i64 = 5;

static WAKE: Notify = Notify::const_new();
/// Set by `wake()`: the next `refresh` pushes even if nothing it tracks
/// changed (e.g. on resume, where a Live Activity may have ended on its own).
static FORCE: AtomicBool = AtomicBool::new(true);
static LAST_PUSHED: Mutex<Option<String>> = Mutex::new(None);

pub fn wake() {
    FORCE.store(true, Ordering::SeqCst);
    WAKE.notify_one();
}

/// `run_scheduler`'s sleep on iOS: returns early on `wake()`, so a resume or a
/// Settings change is acted on immediately rather than at the next poll.
pub async fn sleep_or_wake(dur: Duration) {
    tokio::select! {
        _ = tokio::time::sleep(dur) => {}
        _ = WAKE.notified() => {}
    }
}

fn iso(dt: DateTime<Local>) -> String {
    dt.with_timezone(&Utc).to_rfc3339_opts(SecondsFormat::Secs, true)
}

/// Brings the pending notifications and the Live Activity in line with the
/// current Pomodoro/overlay state. Cheap when nothing changed.
pub fn refresh(app: &AppHandle) {
    let force = FORCE.swap(false, Ordering::SeqCst);
    let now = Local::now();
    let enabled = POMODORO_ENABLED.load(Ordering::SeqCst);
    let snooze_until_ms = POMODORO_SNOOZE_UNTIL_MS.load(Ordering::SeqCst);
    let reflection_pending = {
        let state = app.state::<AppState>();
        let ov = state.overlay.lock().unwrap();
        ov.open && !ov.reflection_entered
    };
    let slot = grid::slot_for(now);

    let key = format!("{enabled}|{snooze_until_ms}|{reflection_pending}|{}", slot.start_iso());
    {
        let mut last = LAST_PUSHED.lock().unwrap();
        if !force && last.as_deref() == Some(key.as_str()) {
            return;
        }
        *last = Some(key);
    }

    let bridge = app.state::<IosBridge<tauri::Wry>>();
    let snoozed = !enabled && snooze_until_ms != 0;

    if !enabled && !snoozed {
        if let Err(e) = bridge.clear_break_notifications() {
            log::warn!("ios: clearing break notifications failed: {e:?}");
        }
    } else {
        // A snooze is persisted and restored on relaunch, so notifications
        // for breaks after it ends can be scheduled now: they still fire if
        // the process is gone by then.
        let resume_at = DateTime::<Utc>::from_timestamp_millis(snooze_until_ms).map(|d| d.with_timezone(&Local));
        let fire_dates: Vec<String> = grid::upcoming_break_starts(now, NOTIFICATION_WINDOW)
            .into_iter()
            .filter(|start| !snoozed || resume_at.map_or(true, |r| *start >= r))
            .map(iso)
            .collect();
        match bridge.schedule_break_notifications(
            &fire_dates,
            "Break time",
            "Open Reflectodoro and write down what you did.",
        ) {
            Ok(v) => log::info!("ios: break notifications synced: {v}"),
            Err(e) => log::warn!("ios: scheduling break notifications failed: {e:?}"),
        }
    }

    if !enabled {
        if let Err(e) = bridge.end_live_activity() {
            log::warn!("ios: ending Live Activity failed: {e:?}");
        }
        return;
    }
    let next_break_start = if slot.phase == Phase::Work {
        slot.end
    } else {
        match grid::upcoming_break_starts(now, 1).first() {
            Some(s) => *s,
            None => return,
        }
    };
    let state = LiveActivityState {
        phase: if slot.phase == Phase::Break { "break" } else { "work" },
        phase_end: iso(slot.end),
        next_break_start: iso(next_break_start),
        next_break_end: iso(next_break_start + chrono::Duration::minutes(BREAK_LENGTH_MIN)),
        reflection_pending,
    };
    match bridge.start_or_update_live_activity(&state) {
        Ok(v) => log::info!("ios: Live Activity: {v}"),
        Err(e) => log::warn!("ios: Live Activity update failed: {e:?}"),
    }
}
