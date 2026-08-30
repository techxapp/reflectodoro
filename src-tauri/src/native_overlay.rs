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

async fn is_slot_covered(pool: &SqlitePool, slot_start_iso: &str) -> bool {
    sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM reflection WHERE slot_start_at = ?")
        .bind(slot_start_iso)
        .fetch_one(pool)
        .await
        .unwrap_or(0)
        > 0
}

/// Mirrors db.ts's `findMissedSlots` exactly (including its defensive lookback cap).
const MAX_MISSED_SLOT_LOOKBACK: u32 = 96;

async fn find_missed_slots(pool: &SqlitePool, current_slot_iso: &str) -> Vec<String> {
    let Some(current) = canonical_iso(current_slot_iso) else {
        return vec![current_slot_iso.to_string()];
    };
    let first_run_at = ensure_first_run_marker(pool).await;
    let first_run_ms = DateTime::parse_from_rfc3339(&first_run_at)
        .map(|d| d.timestamp_millis())
        .unwrap_or(i64::MIN);

    let mut slots = vec![current.clone()];
    let mut cursor = current;
    for _ in 0..MAX_MISSED_SLOT_LOOKBACK {
        let Some(prev) = previous_slot_iso(&cursor) else {
            break;
        };
        let prev_ms = DateTime::parse_from_rfc3339(&prev)
            .map(|d| d.timestamp_millis())
            .unwrap_or(0);
        if prev_ms < first_run_ms {
            break;
        }
        if is_slot_covered(pool, &prev).await {
            break;
        }
        slots.insert(0, prev.clone());
        cursor = prev;
    }
    slots
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
pub async fn refresh_task_list_cache(app: &AppHandle) {
    let date = local_date_stamp();
    let content = load_task_list(pool(app).await, &date).await;
    *app.state::<AppState>().task_list.lock().unwrap() = content;
}

async fn handle_save_task_list(app: AppHandle, content: String) {
    let date = local_date_stamp();
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

#[derive(Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum NativeOverlayEvent {
    SubmitReflection { text: String },
    BreakitAttempt { input: String },
    SaveTaskList { content: String },
    DevForceClose,
}

async fn handle_submit_reflection(app: AppHandle, text: String) {
    let current_slot_start = {
        let state = app.state::<AppState>();
        let overlay = state.overlay.lock().unwrap();
        overlay.current_slot_start.clone()
    };
    if current_slot_start.is_empty() {
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
            let app = app.clone();
            tauri::async_runtime::spawn(async move {
                handle_submit_reflection(app, text).await;
            });
        }
        NativeOverlayEvent::SaveTaskList { content } => {
            let app = app.clone();
            tauri::async_runtime::spawn(async move {
                handle_save_task_list(app, content).await;
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
