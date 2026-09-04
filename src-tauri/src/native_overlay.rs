//! Rust-side counterpart to the native `WindowManager` break overlay
//! (`NativeOverlayManager.kt`) -- Android only. Unlike the regular /overlay
//! route (which runs inside Tauri's own webview and can use $lib/db.ts and
//! Tauri commands directly), this overlay is a *plain* `android.webkit.WebView`
//! added straight to `WindowManager` with no Tauri runtime of its own -- it
//! exists specifically to survive the user backgrounding the app (Home,
//! switching apps), which the single-Activity Tauri webview cannot do. Its
//! reflection-submit and breakit-check actions are driven from here instead:
//! a Tauri `Channel` set up once at startup (see `install_channel`, called
//! from lib.rs's `.setup()`) lets Kotlin call back into Rust whenever the
//! user interacts with the overlay, and this module does the SQLite writes
//! directly -- a second connection to the same pomodoro.db tauri-plugin-sql
//! already manages, since that plugin's own pool isn't part of its public API
//! -- then reuses `commands::mark_reflection_entered`/`breakit_attempt` so the
//! actual unlock-formula logic (state.rs) stays in exactly one place.
#![cfg(target_os = "android")]

use chrono::{DateTime, Local, SecondsFormat, Utc};
use serde::Deserialize;
use serde_json::Value;
use sqlx::sqlite::SqlitePoolOptions;
use sqlx::SqlitePool;
use tauri::ipc::{Channel, InvokeResponseBody};
use tauri::{AppHandle, Emitter, Manager};
use tokio::sync::OnceCell;

use crate::android_bridge::AndroidBridge;
use crate::commands;
use crate::state::AppState;

static POOL: OnceCell<SqlitePool> = OnceCell::const_new();

async fn pool(app: &AppHandle) -> &SqlitePool {
    POOL.get_or_init(|| async {
        let app_dir = app
            .path()
            .app_config_dir()
            .expect("no app config dir");
        std::fs::create_dir_all(&app_dir).expect("couldn't create app config dir");
        let db_path = app_dir.join(crate::db::DB_URL.trim_start_matches("sqlite:"));
        SqlitePoolOptions::new()
            .connect(&format!("sqlite:{}", db_path.to_str().expect("non-utf8 db path")))
            .await
            .expect("native_overlay: failed to open pomodoro.db connection")
    })
    .await
}

/// Reparses any RFC3339 string (Rust's `to_rfc3339()` local-offset form, or
/// an already-UTC one) and re-emits it in exactly the format JS's
/// `Date.prototype.toISOString()` produces -- millisecond precision, `Z`
/// suffix. `reflection.slot_start_at` values written by the regular
/// $lib/db.ts path are always in this exact form (`canonicalIso`/
/// `previousSlotIso` both go through `new Date(...).toISOString()`), so this
/// module has to match it byte-for-byte or the plain string-equality checks
/// (`isSlotCovered`) silently stop recognizing slots as covered.
fn canonical_iso(iso: &str) -> Option<String> {
    DateTime::parse_from_rfc3339(iso)
        .ok()
        .map(|dt| dt.with_timezone(&Utc).to_rfc3339_opts(SecondsFormat::Millis, true))
}

fn previous_slot_iso(slot_iso: &str) -> Option<String> {
    let dt = DateTime::parse_from_rfc3339(slot_iso).ok()?;
    let prev = dt.with_timezone(&Utc) - chrono::Duration::minutes(30);
    Some(prev.to_rfc3339_opts(SecondsFormat::Millis, true))
}

/// Mirrors db.ts's `ensureFirstRunMarker` -- written once, read forever,
/// bounds how far back `find_missed_slots` will cascade.
async fn ensure_first_run_marker(pool: &SqlitePool) -> String {
    if let Ok(Some(value)) = sqlx::query_scalar::<_, String>(
        "SELECT value FROM app_setting WHERE key = 'first_run_at'",
    )
    .fetch_optional(pool)
    .await
    {
        return value;
    }
    let now = Utc::now().to_rfc3339_opts(SecondsFormat::Millis, true);
    let _ = sqlx::query(
        "INSERT INTO app_setting (key, value) VALUES ('first_run_at', ?) ON CONFLICT(key) DO NOTHING",
    )
    .bind(&now)
    .execute(pool)
    .await;
    now
}

/// `Err` means the query itself failed (lock contention, most concretely) --
/// deliberately *not* folded into `Ok(false)` the way this used to via
/// `unwrap_or(0)`. Conflating "couldn't tell" with "definitely not covered"
/// is what let a transient DB error make `find_missed_slots` walk the full
/// 96-slot cap and write 96 duplicate rows (48 hours of fabricated history)
/// under nothing worse than lock contention.
async fn is_slot_covered(pool: &SqlitePool, slot_start_iso: &str) -> Result<bool, sqlx::Error> {
    let count = sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM reflection WHERE slot_start_at = ?")
        .bind(slot_start_iso)
        .fetch_one(pool)
        .await?;
    Ok(count > 0)
}

/// Mirrors db.ts's `findMissedSlots` exactly (including its defensive lookback cap).
const MAX_MISSED_SLOT_LOOKBACK: u32 = 96;

async fn find_missed_slots(pool: &SqlitePool, current_slot_iso: &str) -> Vec<String> {
    let Some(current) = canonical_iso(current_slot_iso) else {
        return vec![current_slot_iso.to_string()];
    };
    let first_run_at = ensure_first_run_marker(pool).await;
    // A malformed `first_run_at` and a malformed `prev` used to default in
    // opposite directions (i64::MIN -- effectively no lower bound at all --
    // vs. 0, i.e. treat it as before the epoch and stop immediately). Both
    // now fail the same conservative way: stop extending the cascade rather
    // than guess a bound in either direction, since guessing wrong here means
    // either fabricating history (unbounded) or silently dropping a slot that
    // should have been covered (stopping too early).
    let Ok(first_run_ms) = DateTime::parse_from_rfc3339(&first_run_at).map(|d| d.timestamp_millis())
    else {
        log::error!("find_missed_slots: unparseable first_run_at {first_run_at:?}, stopping cascade at current slot only");
        return vec![current];
    };

    let mut slots = vec![current.clone()];
    let mut cursor = current;
    for _ in 0..MAX_MISSED_SLOT_LOOKBACK {
        let Some(prev) = previous_slot_iso(&cursor) else {
            break;
        };
        let Ok(prev_ms) = DateTime::parse_from_rfc3339(&prev).map(|d| d.timestamp_millis()) else {
            log::error!("find_missed_slots: unparseable slot {prev:?}, stopping cascade");
            break;
        };
        if prev_ms < first_run_ms {
            break;
        }
        match is_slot_covered(pool, &prev).await {
            Ok(true) => break,
            Ok(false) => {}
            Err(e) => {
                log::error!("find_missed_slots: coverage check for {prev:?} failed: {e:?}, stopping cascade");
                break;
            }
        }
        slots.insert(0, prev.clone());
        cursor = prev;
    }
    slots
}

/// Refreshes `AppState.missed_slot_count` from the same `find_missed_slots`
/// walk `handle_submit_reflection` will run at actual submit time -- called
/// right before the overlay is triggered (`overlay::spawn_or_update_overlay`'s
/// Android arm), mirroring `refresh_task_list_cache` above. Computing it here
/// rather than at submit time is what lets the heading show "last N
/// pomodoros" before the user has typed anything.
pub async fn refresh_missed_slot_count(app: &AppHandle) {
    let current_slot_start = {
        let state = app.state::<AppState>();
        let overlay = state.overlay.lock().unwrap();
        overlay.current_slot_start.clone()
    };
    if current_slot_start.is_empty() {
        return;
    }
    let count = find_missed_slots(pool(app).await, &current_slot_start).await.len();
    *app.state::<AppState>().missed_slot_count.lock().unwrap() = count;
}

async fn save_reflection(pool: &SqlitePool, covered_slots: &[String], text: &str) {
    let created_at = Utc::now().to_rfc3339_opts(SecondsFormat::Millis, true);
    for slot in covered_slots {
        let _ = sqlx::query(
            "INSERT INTO reflection (created_at, slot_start_at, text) VALUES (?, ?, ?)",
        )
        .bind(&created_at)
        .bind(slot)
        .bind(text)
        .execute(pool)
        .await;
    }
}

/// Mirrors db.ts's `localDateStamp()` exactly (local calendar date, zero
/// padded) -- `daily_task_list.date` is keyed on this string, so this module
/// has to match it byte-for-byte or it'll read/write a different row than
/// the frontend's own `getTaskList`/`saveTaskList` calls do for "today".
///
/// Callers that read/write the task list or not-to-do list for the
/// *currently open overlay* should use `AppState.task_list_date` (see its
/// doc comment) instead of calling this directly -- this raw "right now"
/// stamp is only safe to call at the one moment the overlay's date gets
/// captured (`refresh_task_list_cache`, right before the overlay opens). The
/// `:55`-`:00` break slot always straddles midnight, so a save that
/// recomputed this fresh instead of reusing the captured date could silently
/// upsert into tomorrow's row for a slot that opened, and was shown to the
/// user as, today's.
fn local_date_stamp() -> String {
    Local::now().format("%Y-%m-%d").to_string()
}

async fn load_task_list(pool: &SqlitePool, date: &str) -> String {
    sqlx::query_scalar::<_, String>("SELECT content FROM daily_task_list WHERE date = ?")
        .bind(date)
        .fetch_optional(pool)
        .await
        .ok()
        .flatten()
        .unwrap_or_default()
}

/// Mirrors db.ts's `saveTaskList` upsert exactly (see that function's own
/// comment for why the `sourceLabel` broadcast below matters).
async fn persist_task_list(pool: &SqlitePool, date: &str, content: &str) {
    let updated_at = Utc::now().to_rfc3339_opts(SecondsFormat::Millis, true);
    let _ = sqlx::query(
        "INSERT INTO daily_task_list (date, content, updated_at) VALUES (?, ?, ?)
         ON CONFLICT(date) DO UPDATE SET content = excluded.content, updated_at = excluded.updated_at",
    )
    .bind(date)
    .bind(content)
    .bind(&updated_at)
    .execute(pool)
    .await;
}

/// Refreshes `AppState.task_list` from SQLite -- called right before the
/// overlay is triggered (`overlay::spawn_or_update_overlay`'s Android arm) so
/// it shows whatever the user last saved in the main app, not a stale cache
/// from process start. See the field's doc comment in state.rs for why a
/// one-shot refresh here is enough (nothing else can write to today's row
/// while the overlay is up).
///
/// Also captures `AppState.task_list_date` -- the one and only place this
/// process computes "today" for the overlay's task list / not-to-do list,
/// reused by `refresh_not_to_do_list_cache` and both save handlers below so
/// a slot that straddles midnight can't have its open and its save disagree
/// about which day's row they mean. Called first in
/// `spawn_or_update_overlay`'s Android arm, before
/// `refresh_not_to_do_list_cache`.
pub async fn refresh_task_list_cache(app: &AppHandle) {
    let date = local_date_stamp();
    let content = load_task_list(pool(app).await, &date).await;
    *app.state::<AppState>().task_list.lock().unwrap() = content;
    *app.state::<AppState>().task_list_date.lock().unwrap() = date;
}

/// The date captured by `refresh_task_list_cache` for the currently open
/// overlay -- falls back to a fresh `local_date_stamp()` only if nothing has
/// captured one yet (defensive; not expected in practice since
/// `refresh_task_list_cache` always runs before the overlay that could
/// trigger a save even exists).
fn active_task_list_date(app: &AppHandle) -> String {
    let captured = app.state::<AppState>().task_list_date.lock().unwrap().clone();
    if captured.is_empty() {
        local_date_stamp()
    } else {
        captured
    }
}

async fn load_not_to_do_list(pool: &SqlitePool, date: &str) -> String {
    sqlx::query_scalar::<_, String>("SELECT content FROM not_to_do_list WHERE date = ?")
        .bind(date)
        .fetch_optional(pool)
        .await
        .ok()
        .flatten()
        .unwrap_or_default()
}

/// Mirrors db.ts's `saveNotToDoList` upsert exactly.
async fn persist_not_to_do_list(pool: &SqlitePool, date: &str, content: &str) {
    let updated_at = Utc::now().to_rfc3339_opts(SecondsFormat::Millis, true);
    let _ = sqlx::query(
        "INSERT INTO not_to_do_list (date, content, updated_at) VALUES (?, ?, ?)
         ON CONFLICT(date) DO UPDATE SET content = excluded.content, updated_at = excluded.updated_at",
    )
    .bind(date)
    .bind(content)
    .bind(&updated_at)
    .execute(pool)
    .await;
}

/// Mirrors `refresh_task_list_cache` above for `AppState.not_to_do_list` --
/// reuses the same captured date rather than calling `local_date_stamp()`
/// independently, so the two lists can't disagree about which day they're
/// for. Must run after `refresh_task_list_cache` in the same overlay-open
/// sequence (see that function's doc comment); `spawn_or_update_overlay`'s
/// Android arm already calls them in that order.
pub async fn refresh_not_to_do_list_cache(app: &AppHandle) {
    let date = active_task_list_date(app);
    let content = load_not_to_do_list(pool(app).await, &date).await;
    *app.state::<AppState>().not_to_do_list.lock().unwrap() = content;
}

async fn handle_save_task_list(app: AppHandle, content: String) {
    let date = active_task_list_date(&app);
    persist_task_list(pool(&app).await, &date, &content).await;
    *app.state::<AppState>().task_list.lock().unwrap() = content.clone();
    // Keeps the main Activity's own webview in sync in the (rare but
    // possible) case it's still mounted on a task-list-showing route behind
    // this overlay -- see saveTaskList/listenForTaskListUpdates in db.ts.
    // sourceLabel isn't a real window label, so the frontend's own
    // self-clobber guard never filters it out.
    let _ = app.emit(
        "tasklist://updated",
        serde_json::json!({
            "date": date,
            "content": content,
            "sourceLabel": "android-native-overlay",
        }),
    );
}

async fn handle_save_not_to_do_list(app: AppHandle, content: String) {
    let date = active_task_list_date(&app);
    persist_not_to_do_list(pool(&app).await, &date, &content).await;
    *app.state::<AppState>().not_to_do_list.lock().unwrap() = content.clone();
    let _ = app.emit(
        "nottodolist://updated",
        serde_json::json!({
            "date": date,
            "content": content,
            "sourceLabel": "android-native-overlay",
        }),
    );
}

#[derive(Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum NativeOverlayEvent {
    SubmitReflection { text: String },
    BreakitAttempt { input: String },
    SaveTaskList { content: String },
    SaveNotToDoList { content: String },
    DevForceClose,
}

/// Guards `SubmitReflection` against two channel events being handled
/// concurrently -- unlike the regular `/overlay` page's `isSubmitting` Svelte
/// state, this handler is dispatched via `tauri::async_runtime::spawn` (see
/// `handle_channel_event` below), so two submits arriving close together
/// (a double-tap racing the WebView's own submit-disables-itself-on-click
/// behavior) would otherwise both run `find_missed_slots` and insert before
/// either had flipped `reflection_entered` -- there's no `UNIQUE` constraint
/// on `reflection.slot_start_at` to catch that at the DB level, so it just
/// writes the current slot's row twice.
static SUBMIT_IN_FLIGHT: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

async fn handle_submit_reflection(app: AppHandle, text: String) {
    let (current_slot_start, already_entered) = {
        let state = app.state::<AppState>();
        let overlay = state.overlay.lock().unwrap();
        (overlay.current_slot_start.clone(), overlay.reflection_entered)
    };
    // Empty slot: no overlay is actually open for this to mean anything (a
    // stale/duplicate channel event). Already entered: a second submit for a
    // slot this process already recorded -- most likely a delayed duplicate
    // that arrived after the first one succeeded but before the WebView
    // learned to disable itself. Either way, writing again would just
    // duplicate the row.
    if current_slot_start.is_empty() || already_entered {
        return;
    }
    let db = pool(&app).await;
    let covered = find_missed_slots(db, &current_slot_start).await;
    save_reflection(db, &covered, &text).await;

    let state = app.state::<AppState>();
    commands::mark_reflection_entered(app.clone(), state);
}

fn handle_channel_event(app: &AppHandle, value: Value) {
    let event: NativeOverlayEvent = match serde_json::from_value(value) {
        Ok(e) => e,
        Err(e) => {
            log::warn!("native_overlay: unrecognized channel event: {e:?}");
            return;
        }
    };
    match event {
        NativeOverlayEvent::SubmitReflection { text } => {
            if SUBMIT_IN_FLIGHT.swap(true, std::sync::atomic::Ordering::SeqCst) {
                log::info!("native_overlay: dropping submit_reflection, one is already in flight");
                return;
            }
            let app = app.clone();
            tauri::async_runtime::spawn(async move {
                handle_submit_reflection(app, text).await;
                SUBMIT_IN_FLIGHT.store(false, std::sync::atomic::Ordering::SeqCst);
            });
        }
        NativeOverlayEvent::SaveTaskList { content } => {
            let app = app.clone();
            tauri::async_runtime::spawn(async move {
                handle_save_task_list(app, content).await;
            });
        }
        NativeOverlayEvent::SaveNotToDoList { content } => {
            let app = app.clone();
            tauri::async_runtime::spawn(async move {
                handle_save_not_to_do_list(app, content).await;
            });
        }
        NativeOverlayEvent::BreakitAttempt { input } => {
            let state = app.state::<AppState>();
            commands::breakit_attempt(app.clone(), state, input);
        }
        NativeOverlayEvent::DevForceClose => {
            let state = app.state::<AppState>();
            let _ = commands::dev_force_close(app.clone(), state);
        }
    }
}

/// Sets up the one long-lived `Channel` Kotlin uses to call back into Rust
/// for the lifetime of the app -- called once from lib.rs's `.setup()`.
/// Kotlin stores the channel it receives here (`NativeBridgePlugin`'s
/// `overlayChannel` field) and reuses it across every subsequent break, so
/// this only needs to run once per process, not once per overlay.
pub fn install_channel(app: &AppHandle) {
    let app_for_channel = app.clone();
    let channel = Channel::new(move |body: InvokeResponseBody| {
        if let InvokeResponseBody::Json(json) = body {
            if let Ok(value) = serde_json::from_str::<Value>(&json) {
                handle_channel_event(&app_for_channel, value);
            }
        }
        Ok(())
    });

    let bridge = app.state::<AndroidBridge<tauri::Wry>>();
    if let Err(e) = bridge.init_native_overlay_channel(channel) {
        log::error!("init_native_overlay_channel failed: {e:?}");
    }
}
