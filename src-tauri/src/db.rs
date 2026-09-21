use sqlx::sqlite::{SqliteConnectOptions, SqlitePool, SqlitePoolOptions};
use tauri::{AppHandle, Manager};

pub const DB_URL: &str = "sqlite:pomodoro.db";

/// Opens a fresh, direct `sqlx` connection to the same `pomodoro.db`
/// tauri-plugin-sql manages -- that plugin's own connection pool isn't part
/// of its public API (confirmed against its source: `execute` calls
/// `pool.execute(query)` per invocation with no connection pinned across
/// calls, so manual `BEGIN`/`COMMIT` sent as separate plugin `execute()`
/// calls is not reliably atomic). `native_overlay.rs` (Android-only)
/// establishes this same "second direct connection" pattern already, but
/// caches its pool for the process lifetime via a `OnceCell` since it's
/// called on every reflection/breakit submit from the native overlay. This
/// helper deliberately does NOT cache -- it backs `import::import_data`,
/// a rare, user-triggered action (Settings -> Data import), so a fresh
/// pool opened and dropped per call is simpler and avoids holding a second
/// long-lived connection open for the whole app lifetime on every platform.
///
/// Deliberately does NOT create the file when it's missing (sqlx's default
/// for a bare `sqlite:` URL): every caller runs long after `ensure_schema`
/// created and verified the database, so a missing file here means something
/// is wrong and should surface as an error rather than as a silently empty
/// second database with no schema in it.
pub async fn open_direct_pool(app: &AppHandle) -> Result<SqlitePool, String> {
    let db_path_str = db_path_string(app)?;
    SqlitePoolOptions::new()
        .connect(&format!("sqlite:{db_path_str}"))
        .await
        .map_err(|e| format!("failed to open database connection: {e}"))
}

/// Resolves the same on-disk path tauri-plugin-sql's own path mapper uses for
/// `DB_URL` (its `app_config_dir()` plus the file name), creating the config
/// directory if needed.
fn db_path_string(app: &AppHandle) -> Result<String, String> {
    let app_dir = app
        .path()
        .app_config_dir()
        .map_err(|e| format!("no app config dir: {e}"))?;
    std::fs::create_dir_all(&app_dir).map_err(|e| format!("couldn't create app config dir: {e}"))?;
    let db_path = app_dir.join(DB_URL.trim_start_matches("sqlite:"));
    db_path
        .to_str()
        .map(|s| s.to_string())
        .ok_or_else(|| "non-utf8 db path".to_string())
}

/// Why `ensure_schema` failed, split by what the user can actually do about it.
#[derive(Debug)]
pub enum SchemaError {
    /// The database couldn't be opened or inspected at all (disk full, file
    /// locked, permissions). Nothing to do with the schema's shape, and
    /// nothing the user fixes by installing a different build.
    Unavailable(String),
    /// The database opened fine, but its shape predates this build -- it was
    /// last written by a release older than the one that squashed the
    /// migration list, and no incremental migrations remain to bring it
    /// forward. See `FULL_SCHEMA_SQL`'s comment.
    Incompatible(String),
}

impl std::fmt::Display for SchemaError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SchemaError::Unavailable(m) => write!(f, "database unavailable: {m}"),
            SchemaError::Incompatible(m) => write!(f, "database shape is not current: {m}"),
        }
    }
}

/// The complete schema as one script -- the squashed equivalent of the 28
/// incremental `tauri-plugin-sql` migrations this app shipped through
/// v0.13.10 (frozen in `db_legacy_fixture.rs`, for the test that proves the
/// two still agree).
///
/// This app no longer registers any migrations with tauri-plugin-sql, which
/// is exactly what makes replacing them safe: that plugin only constructs a
/// `sqlx::migrate::Migrator` -- the thing that hard-errors with
/// `VersionMissing` when a recorded `_sqlx_migrations` row has no matching
/// entry in the list -- if a migration list was registered for that database
/// URL (see `commands::load`, tauri-plugin-sql 2.4.0). Register nothing and
/// `_sqlx_migrations` is never read or validated at all. An existing database
/// is therefore left exactly as it is, rows and all; `ensure_schema` only
/// confirms it already has the shape below. The now-stale `_sqlx_migrations`
/// table is deliberately left in place too -- nothing reads it, and keeping
/// it means downgrading to a pre-squash build still works.
///
/// Column ORDER here matches the order the old migration chain produced (each
/// `ALTER TABLE ADD COLUMN` appended to the end), so a database created fresh
/// by this script is structurally identical to one upgraded through all 28.
/// Three indexes that chain created and later dropped are deliberately
/// absent: `idx_reflection_created_at` (created_at is ciphertext now),
/// `idx_wellness_check_reflection_id` (that column is gone), and
/// `idx_screen_time_session_app_id` (app_id is ciphertext now).
///
/// **Editing this is a schema change for new installs only.** An existing
/// database never re-runs it, so anything added here must also be added to
/// the `EXPECTED_*` lists below AND reach existing databases some other way
/// -- which, with no migration runner left, means a new one-shot pass.
/// Adding a column here alone would make every existing install fail
/// `verify_final_shape` and refuse to start.
const FULL_SCHEMA_SQL: &str = r#"
    CREATE TABLE reflection (
        id INTEGER PRIMARY KEY AUTOINCREMENT,
        created_at TEXT NOT NULL,
        slot_start_at TEXT NOT NULL,
        text TEXT NOT NULL,
        updated_at TEXT,
        rev INTEGER NOT NULL DEFAULT 0
    );
    CREATE INDEX idx_reflection_slot_start_at ON reflection(slot_start_at);
    CREATE INDEX idx_reflection_rev ON reflection(rev);

    CREATE TABLE daily_task_list (
        date TEXT PRIMARY KEY,
        content TEXT NOT NULL DEFAULT '',
        updated_at TEXT NOT NULL,
        rev INTEGER NOT NULL DEFAULT 0
    );
    CREATE INDEX idx_daily_task_list_rev ON daily_task_list(rev);

    CREATE TABLE not_to_do_list (
        date TEXT PRIMARY KEY,
        content TEXT NOT NULL DEFAULT '',
        updated_at TEXT NOT NULL,
        rev INTEGER NOT NULL DEFAULT 0
    );
    CREATE INDEX idx_not_to_do_list_rev ON not_to_do_list(rev);

    CREATE TABLE app_setting (
        key TEXT PRIMARY KEY,
        value TEXT NOT NULL
    );

    CREATE TABLE wellness_check (
        id INTEGER PRIMARY KEY AUTOINCREMENT,
        slot_start_at TEXT NOT NULL DEFAULT '',
        relaxed_eyes TEXT NOT NULL DEFAULT '1',
        exercise TEXT NOT NULL DEFAULT '1',
        drank_water TEXT NOT NULL DEFAULT '1',
        washroom TEXT NOT NULL DEFAULT '0',
        created_at TEXT NOT NULL,
        rev INTEGER NOT NULL DEFAULT 0
    );
    CREATE INDEX idx_wellness_check_slot_start_at ON wellness_check(slot_start_at);
    CREATE INDEX idx_wellness_check_rev ON wellness_check(rev);

    CREATE TABLE screen_time_session (
        id INTEGER PRIMARY KEY AUTOINCREMENT,
        app_id TEXT NOT NULL,
        platform TEXT NOT NULL,
        device_name TEXT NOT NULL DEFAULT '',
        started_at TEXT NOT NULL,
        ended_at TEXT NOT NULL,
        display_name TEXT NOT NULL DEFAULT '',
        app_id_hash TEXT NOT NULL DEFAULT '',
        rev INTEGER NOT NULL DEFAULT 0
    );
    CREATE INDEX idx_screen_time_session_started_at ON screen_time_session(started_at);
    CREATE INDEX idx_screen_time_session_dedupe
                    ON screen_time_session(app_id_hash, platform, device_name, started_at, ended_at);
    CREATE INDEX idx_screen_time_session_rev ON screen_time_session(rev);

    CREATE TABLE bulk_edit_preset (
        id TEXT PRIMARY KEY,
        name TEXT NOT NULL,
        start_time TEXT NOT NULL,
        end_time TEXT NOT NULL,
        text TEXT NOT NULL,
        created_at TEXT NOT NULL,
        updated_at TEXT NOT NULL,
        rev INTEGER NOT NULL DEFAULT 0
    );
    CREATE INDEX idx_bulk_edit_preset_rev ON bulk_edit_preset(rev);

    CREATE TABLE paired_device (
        device_id TEXT PRIMARY KEY,
        name TEXT NOT NULL,
        platform TEXT NOT NULL,
        shared_key TEXT NOT NULL,
        paired_at TEXT NOT NULL,
        last_sync_at TEXT,
        auto_sync_enabled INTEGER NOT NULL DEFAULT 0
    );

    CREATE TABLE sync_cursor (
        device_id TEXT NOT NULL,
        table_name TEXT NOT NULL,
        last_rev INTEGER NOT NULL,
        PRIMARY KEY (device_id, table_name)
    );

    CREATE TABLE breakit_daily_use (
        date TEXT PRIMARY KEY,
        count INTEGER NOT NULL DEFAULT 0
    );

    CREATE TRIGGER trg_reflection_rev_insert AFTER INSERT ON reflection
                BEGIN
                    UPDATE reflection SET rev = (SELECT COALESCE(MAX(rev), 0) + 1 FROM reflection)
                    WHERE rowid = NEW.rowid;
                END;
    CREATE TRIGGER trg_reflection_rev_update AFTER UPDATE ON reflection
                FOR EACH ROW WHEN NEW.rev = OLD.rev
                BEGIN
                    UPDATE reflection SET rev = (SELECT COALESCE(MAX(rev), 0) + 1 FROM reflection)
                    WHERE rowid = NEW.rowid;
                END;

    CREATE TRIGGER trg_daily_task_list_rev_insert AFTER INSERT ON daily_task_list
                BEGIN
                    UPDATE daily_task_list SET rev = (SELECT COALESCE(MAX(rev), 0) + 1 FROM daily_task_list)
                    WHERE rowid = NEW.rowid;
                END;
    CREATE TRIGGER trg_daily_task_list_rev_update AFTER UPDATE ON daily_task_list
                FOR EACH ROW WHEN NEW.rev = OLD.rev
                BEGIN
                    UPDATE daily_task_list SET rev = (SELECT COALESCE(MAX(rev), 0) + 1 FROM daily_task_list)
                    WHERE rowid = NEW.rowid;
                END;

    CREATE TRIGGER trg_not_to_do_list_rev_insert AFTER INSERT ON not_to_do_list
                BEGIN
                    UPDATE not_to_do_list SET rev = (SELECT COALESCE(MAX(rev), 0) + 1 FROM not_to_do_list)
                    WHERE rowid = NEW.rowid;
                END;
    CREATE TRIGGER trg_not_to_do_list_rev_update AFTER UPDATE ON not_to_do_list
                FOR EACH ROW WHEN NEW.rev = OLD.rev
                BEGIN
                    UPDATE not_to_do_list SET rev = (SELECT COALESCE(MAX(rev), 0) + 1 FROM not_to_do_list)
                    WHERE rowid = NEW.rowid;
                END;

    CREATE TRIGGER trg_wellness_check_rev_insert AFTER INSERT ON wellness_check
                BEGIN
                    UPDATE wellness_check SET rev = (SELECT COALESCE(MAX(rev), 0) + 1 FROM wellness_check)
                    WHERE rowid = NEW.rowid;
                END;
    CREATE TRIGGER trg_wellness_check_rev_update AFTER UPDATE ON wellness_check
                FOR EACH ROW WHEN NEW.rev = OLD.rev
                BEGIN
                    UPDATE wellness_check SET rev = (SELECT COALESCE(MAX(rev), 0) + 1 FROM wellness_check)
                    WHERE rowid = NEW.rowid;
                END;

    CREATE TRIGGER trg_screen_time_session_rev_insert AFTER INSERT ON screen_time_session
                BEGIN
                    UPDATE screen_time_session SET rev = (SELECT COALESCE(MAX(rev), 0) + 1 FROM screen_time_session)
                    WHERE rowid = NEW.rowid;
                END;
    CREATE TRIGGER trg_screen_time_session_rev_update AFTER UPDATE ON screen_time_session
                FOR EACH ROW WHEN NEW.rev = OLD.rev
                BEGIN
                    UPDATE screen_time_session SET rev = (SELECT COALESCE(MAX(rev), 0) + 1 FROM screen_time_session)
                    WHERE rowid = NEW.rowid;
                END;

    CREATE TRIGGER trg_bulk_edit_preset_rev_insert AFTER INSERT ON bulk_edit_preset
                BEGIN
                    UPDATE bulk_edit_preset SET rev = (SELECT COALESCE(MAX(rev), 0) + 1 FROM bulk_edit_preset)
                    WHERE rowid = NEW.rowid;
                END;
    CREATE TRIGGER trg_bulk_edit_preset_rev_update AFTER UPDATE ON bulk_edit_preset
                FOR EACH ROW WHEN NEW.rev = OLD.rev
                BEGIN
                    UPDATE bulk_edit_preset SET rev = (SELECT COALESCE(MAX(rev), 0) + 1 FROM bulk_edit_preset)
                    WHERE rowid = NEW.rowid;
                END;
"#;

/// Settings rows a brand-new database starts with -- the *final* values the
/// old migration chain arrived at, not the historical sequence it took to get
/// there (`checkin_auto_close_minutes`, for one, was seeded 5, then bumped to
/// 15, then to 19, by three separate migrations).
///
/// The four `*_encryption_migrated` flags are seeded `true` because a
/// brand-new database has no pre-encryption rows to back-fill -- which is
/// also what lets `verify_final_shape` apply one uniform check to fresh and
/// existing databases alike. Nothing in this build reads them anymore (the
/// one-time passes they gated are gone, see `crypto.rs`); they're kept so
/// that downgrading to a build which *does* read them still behaves
/// correctly, and so an existing install's record of having completed those
/// passes stays intact.
const SEED_SETTINGS: &[(&str, &str)] = &[
    ("breakit_length", "12"),
    ("breakit_include_special", "false"),
    ("breakit_max_per_day", "5"),
    ("force_close_shortcut_enabled", "true"),
    ("overlay_auto_close_minutes", "5"),
    ("checkin_auto_close_minutes", "19"),
    ("wellness_text_exclusions", "Washroom"),
    ("media_pause_on_break_enabled", "true"),
    ("break_notification_persistent_enabled", "true"),
    ("hide_overlay_on_call_enabled", "true"),
    ("macos_hide_menu_bar_dock_enabled", "false"),
    ("macos_media_key_fallback_enabled", "false"),
    ("screen_time_tracking_enabled", "true"),
    ("device_name", ""),
    ("screen_time_app_threshold_minutes", "5"),
    ("quote_api_url", "https://zenquotes.io/api/random"),
    ("data_encryption_migrated", "true"),
    ("screen_time_encryption_migrated", "true"),
    ("reflection_timestamp_encryption_migrated", "true"),
    ("paired_device_encryption_migrated", "true"),
];

/// Every table, with the columns it must have. Drives `verify_final_shape`.
const EXPECTED_TABLES: &[(&str, &[&str])] = &[
    (
        "reflection",
        &["id", "created_at", "slot_start_at", "text", "updated_at", "rev"],
    ),
    ("daily_task_list", &["date", "content", "updated_at", "rev"]),
    ("not_to_do_list", &["date", "content", "updated_at", "rev"]),
    ("app_setting", &["key", "value"]),
    (
        "wellness_check",
        &[
            "id",
            "slot_start_at",
            "relaxed_eyes",
            "exercise",
            "drank_water",
            "washroom",
            "created_at",
            "rev",
        ],
    ),
    (
        "screen_time_session",
        &[
            "id",
            "app_id",
            "platform",
            "device_name",
            "started_at",
            "ended_at",
            "display_name",
            "app_id_hash",
            "rev",
        ],
    ),
    (
        "bulk_edit_preset",
        &["id", "name", "start_time", "end_time", "text", "created_at", "updated_at", "rev"],
    ),
    (
        "paired_device",
        &[
            "device_id",
            "name",
            "platform",
            "shared_key",
            "paired_at",
            "last_sync_at",
            "auto_sync_enabled",
        ],
    ),
    ("sync_cursor", &["device_id", "table_name", "last_rev"]),
    ("breakit_daily_use", &["date", "count"]),
];

/// Columns whose declared TYPE matters, not just their presence.
///
/// `wellness_check`'s four booleans are the only such case, and they're what
/// catches a database that stopped partway through the old chain: by that
/// chain's migration 22 the table already has every column *name* expected
/// here, and only the declared type (still INTEGER, before the rebuild that
/// made room for ciphertext) reveals it's stale.
const EXPECTED_COLUMN_TYPES: &[(&str, &str, &str)] = &[
    ("wellness_check", "relaxed_eyes", "TEXT"),
    ("wellness_check", "exercise", "TEXT"),
    ("wellness_check", "drank_water", "TEXT"),
    ("wellness_check", "washroom", "TEXT"),
];

/// Columns that must be GONE. `wellness_check.reflection_id` was replaced by
/// `slot_start_at`; a database still carrying it predates that rebuild, and
/// `import.rs`'s dedupe and P2P sync both key off the replacement.
const FORBIDDEN_COLUMNS: &[(&str, &str)] = &[("wellness_check", "reflection_id")];

const EXPECTED_INDEXES: &[&str] = &[
    "idx_reflection_slot_start_at",
    "idx_reflection_rev",
    "idx_daily_task_list_rev",
    "idx_not_to_do_list_rev",
    "idx_wellness_check_slot_start_at",
    "idx_wellness_check_rev",
    "idx_screen_time_session_started_at",
    "idx_screen_time_session_dedupe",
    "idx_screen_time_session_rev",
    "idx_bulk_edit_preset_rev",
];

/// The `rev` maintenance triggers. A missing one would silently stop that
/// table syncing rather than failing loudly (`p2p_sync::build_delta_payload`
/// reads `rev` and nothing else), so they're verified by name.
const EXPECTED_TRIGGERS: &[&str] = &[
    "trg_reflection_rev_insert",
    "trg_reflection_rev_update",
    "trg_daily_task_list_rev_insert",
    "trg_daily_task_list_rev_update",
    "trg_not_to_do_list_rev_insert",
    "trg_not_to_do_list_rev_update",
    "trg_wellness_check_rev_insert",
    "trg_wellness_check_rev_update",
    "trg_screen_time_session_rev_insert",
    "trg_screen_time_session_rev_update",
    "trg_bulk_edit_preset_rev_insert",
    "trg_bulk_edit_preset_rev_update",
];

/// The one-time encryption back-fill flags `crypto.rs` used to own. All four
/// must read `true`: this build has no back-fill pass left, so a database
/// that never finished one would carry plaintext or un-hashed rows with
/// nothing able to fix them. See `verify_final_shape`.
const REQUIRED_TRUE_SETTINGS: &[&str] = &[
    "data_encryption_migrated",
    "screen_time_encryption_migrated",
    "reflection_timestamp_encryption_migrated",
    "paired_device_encryption_migrated",
];

/// Creates the database and its schema on a fresh install, or confirms an
/// existing one is already current.
///
/// Runs synchronously from `lib.rs`'s `.setup()`, before any webview boots.
/// That ordering is deliberate and load-bearing: it makes "pomodoro.db exists
/// and has its final schema" an invariant for everything after it, rather
/// than something the rest of the app has to race the frontend's
/// `Database.load()` for.
pub async fn ensure_schema(app: &AppHandle) -> Result<(), SchemaError> {
    let db_path_str = db_path_string(app).map_err(SchemaError::Unavailable)?;

    // create_if_missing, unlike open_direct_pool: on a fresh install this is
    // the call that brings the file into existence at all.
    let options = SqliteConnectOptions::new()
        .filename(&db_path_str)
        .create_if_missing(true);
    let pool = SqlitePoolOptions::new()
        .connect_with(options)
        .await
        .map_err(|e| SchemaError::Unavailable(format!("couldn't open {db_path_str}: {e}")))?;

    let result = ensure_schema_on_pool(&pool).await;
    pool.close().await;
    result
}

/// The part of `ensure_schema` that works on an already-open pool, so tests
/// can drive it against `sqlite::memory:`.
async fn ensure_schema_on_pool(pool: &SqlitePool) -> Result<(), SchemaError> {
    let existing: Option<String> = sqlx::query_scalar(
        "SELECT name FROM sqlite_master WHERE type = 'table' AND name = 'reflection'",
    )
    .fetch_optional(pool)
    .await
    .map_err(|e| SchemaError::Unavailable(format!("couldn't inspect schema: {e}")))?;

    if existing.is_none() {
        log::info!("no existing schema found -- creating a fresh database");
        create_schema(pool).await?;
        log::info!("fresh database schema created");
        return Ok(());
    }

    verify_final_shape(pool).await?;
    log::info!("existing database schema verified as current");
    Ok(())
}

/// Applies `FULL_SCHEMA_SQL` and the seed settings in one transaction, so a
/// failure partway through leaves no half-built database behind for the next
/// launch to mistake for an existing (and therefore verifiable) one.
async fn create_schema(pool: &SqlitePool) -> Result<(), SchemaError> {
    let mut tx = pool
        .begin()
        .await
        .map_err(|e| SchemaError::Unavailable(format!("couldn't begin schema transaction: {e}")))?;

    sqlx::raw_sql(FULL_SCHEMA_SQL)
        .execute(&mut *tx)
        .await
        .map_err(|e| SchemaError::Unavailable(format!("couldn't create schema: {e}")))?;

    for (key, value) in SEED_SETTINGS {
        sqlx::query("INSERT INTO app_setting (key, value) VALUES (?, ?)")
            .bind(key)
            .bind(value)
            .execute(&mut *tx)
            .await
            .map_err(|e| SchemaError::Unavailable(format!("couldn't seed setting {key}: {e}")))?;
    }

    tx.commit()
        .await
        .map_err(|e| SchemaError::Unavailable(format!("couldn't commit schema transaction: {e}")))
}

/// Confirms an existing database already has the shape `FULL_SCHEMA_SQL`
/// describes, and that its one-time encryption back-fills all completed.
///
/// Checks are STRUCTURAL, never textual: a database upgraded through the old
/// migration chain stores each table's original `CREATE TABLE` text, which
/// will never match this file's wording however faithful the squash is.
/// `PRAGMA table_info` and `sqlite_master`'s `name` column are what both
/// kinds of database do agree on.
///
/// Returns `Incompatible` naming the first failing check, so the log line
/// says exactly which part of the database is stale.
async fn verify_final_shape(pool: &SqlitePool) -> Result<(), SchemaError> {
    for (table, expected_columns) in EXPECTED_TABLES {
        let columns = table_columns(pool, table).await?;
        if columns.is_empty() {
            return Err(SchemaError::Incompatible(format!("missing table `{table}`")));
        }
        for expected in *expected_columns {
            if !columns.iter().any(|(name, _)| name == expected) {
                return Err(SchemaError::Incompatible(format!(
                    "table `{table}` is missing column `{expected}`"
                )));
            }
        }
        for (forbidden_table, forbidden_column) in FORBIDDEN_COLUMNS {
            if forbidden_table == table && columns.iter().any(|(name, _)| name == forbidden_column) {
                return Err(SchemaError::Incompatible(format!(
                    "table `{table}` still has the removed column `{forbidden_column}`"
                )));
            }
        }
        for (typed_table, typed_column, expected_type) in EXPECTED_COLUMN_TYPES {
            if typed_table != table {
                continue;
            }
            if let Some((_, actual_type)) = columns.iter().find(|(name, _)| name == typed_column) {
                if !actual_type.eq_ignore_ascii_case(expected_type) {
                    return Err(SchemaError::Incompatible(format!(
                        "column `{table}.{typed_column}` is `{actual_type}`, expected `{expected_type}`"
                    )));
                }
            }
        }
    }

    let indexes = object_names(pool, "index").await?;
    for expected in EXPECTED_INDEXES {
        if !indexes.iter().any(|name| name == expected) {
            return Err(SchemaError::Incompatible(format!("missing index `{expected}`")));
        }
    }

    let triggers = object_names(pool, "trigger").await?;
    for expected in EXPECTED_TRIGGERS {
        if !triggers.iter().any(|name| name == expected) {
            return Err(SchemaError::Incompatible(format!("missing trigger `{expected}`")));
        }
    }

    for key in REQUIRED_TRUE_SETTINGS {
        let value: Option<String> = sqlx::query_scalar("SELECT value FROM app_setting WHERE key = ?")
            .bind(key)
            .fetch_optional(pool)
            .await
            .map_err(|e| SchemaError::Unavailable(format!("couldn't read setting {key}: {e}")))?;
        match value.as_deref() {
            Some("true") => {}
            other => {
                return Err(SchemaError::Incompatible(format!(
                    "setting `{key}` is {}, expected `true` -- a one-time data pass never finished",
                    other
                        .map(|v| format!("`{v}`"))
                        .unwrap_or_else(|| "missing".to_string())
                )))
            }
        }
    }

    Ok(())
}

/// `(name, declared type)` for every column of `table`, in declaration order.
/// Empty when the table doesn't exist.
async fn table_columns(pool: &SqlitePool, table: &str) -> Result<Vec<(String, String)>, SchemaError> {
    // PRAGMA takes no bind parameters, hence the format!. `table` is only ever
    // one of this file's own consts, never user input.
    let rows: Vec<(i64, String, String)> = sqlx::query_as(&format!("PRAGMA table_info({table})"))
        .fetch_all(pool)
        .await
        .map_err(|e| SchemaError::Unavailable(format!("couldn't read columns of {table}: {e}")))?;
    Ok(rows.into_iter().map(|(_, name, decl_type)| (name, decl_type)).collect())
}

async fn object_names(pool: &SqlitePool, kind: &str) -> Result<Vec<String>, SchemaError> {
    sqlx::query_scalar("SELECT name FROM sqlite_master WHERE type = ?")
        .bind(kind)
        .fetch_all(pool)
        .await
        .map_err(|e| SchemaError::Unavailable(format!("couldn't list {kind}s: {e}")))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db_legacy_fixture::legacy_migrations;
    use sqlx::Row;
    use std::collections::BTreeMap;

    async fn fresh_pool() -> SqlitePool {
        let pool = SqlitePool::connect("sqlite::memory:").await.unwrap();
        create_schema(&pool).await.unwrap();
        pool
    }

    /// Applies the frozen pre-squash migration chain, up to and including
    /// `through`, exactly as a real install of that era would have.
    async fn legacy_pool(through: i64) -> SqlitePool {
        let pool = SqlitePool::connect("sqlite::memory:").await.unwrap();
        for migration in legacy_migrations().into_iter().filter(|m| m.version <= through) {
            sqlx::raw_sql(migration.sql)
                .execute(&pool)
                .await
                .unwrap_or_else(|e| panic!("legacy migration {} failed: {e}", migration.version));
        }
        pool
    }

    /// Marks a legacy pool the way a real v0.13.10 install is marked once its
    /// one-time encryption back-fills have run.
    async fn mark_backfills_complete(pool: &SqlitePool) {
        for key in REQUIRED_TRUE_SETTINGS {
            sqlx::query("INSERT OR REPLACE INTO app_setting (key, value) VALUES (?, 'true')")
                .bind(key)
                .execute(pool)
                .await
                .unwrap();
        }
    }

    /// Every column of every table, as `table -> [(name, type, notnull,
    /// default, pk)]` -- the structure a fresh and an upgraded database must
    /// agree on, ignoring the `CREATE TABLE` text itself (which never
    /// matches, since an upgraded database keeps its original wording).
    async fn column_shape(
        pool: &SqlitePool,
    ) -> BTreeMap<String, Vec<(String, String, i64, Option<String>, i64)>> {
        let tables: Vec<String> = sqlx::query_scalar(
            "SELECT name FROM sqlite_master WHERE type = 'table'
             AND name NOT LIKE 'sqlite_%' AND name != '_sqlx_migrations'
             ORDER BY name",
        )
        .fetch_all(pool)
        .await
        .unwrap();

        let mut shape = BTreeMap::new();
        for table in tables {
            let rows: Vec<(i64, String, String, i64, Option<String>, i64)> =
                sqlx::query_as(&format!("PRAGMA table_info({table})"))
                    .fetch_all(pool)
                    .await
                    .unwrap();
            shape.insert(
                table,
                rows.into_iter()
                    .map(|(_, name, ty, notnull, dflt, pk)| (name, ty, notnull, dflt, pk))
                    .collect(),
            );
        }
        shape
    }

    /// `name -> whitespace-normalized SQL` for every index or trigger.
    /// Normalizing whitespace is what makes the two sides comparable at all:
    /// the legacy chain's definitions carry their original indentation.
    async fn object_shape(pool: &SqlitePool, kind: &str) -> BTreeMap<String, String> {
        let rows: Vec<(String, Option<String>)> =
            sqlx::query_as("SELECT name, sql FROM sqlite_master WHERE type = ? ORDER BY name")
                .bind(kind)
                .fetch_all(pool)
                .await
                .unwrap();
        rows.into_iter()
            .map(|(name, sql)| {
                let normalized =
                    sql.unwrap_or_default().split_whitespace().collect::<Vec<_>>().join(" ");
                (name, normalized)
            })
            .collect()
    }

    async fn autoincrement_tables(pool: &SqlitePool) -> Vec<String> {
        let mut names: Vec<String> = sqlx::query_scalar(
            "SELECT name FROM sqlite_master WHERE type = 'table' AND sql LIKE '%AUTOINCREMENT%'",
        )
        .fetch_all(pool)
        .await
        .unwrap();
        names.sort();
        names
    }

    /// The load-bearing test for the whole squash: a database built by the 28
    /// migrations real installs actually ran must be structurally identical
    /// to one built by `FULL_SCHEMA_SQL`. If this fails, existing users' data
    /// no longer matches what this build expects.
    #[tokio::test]
    async fn full_schema_matches_the_legacy_migration_chain() {
        let legacy = legacy_pool(i64::MAX).await;
        let fresh = fresh_pool().await;

        assert_eq!(
            column_shape(&legacy).await,
            column_shape(&fresh).await,
            "tables/columns must match what the 28-migration chain produced"
        );
        assert_eq!(
            object_shape(&legacy, "index").await,
            object_shape(&fresh, "index").await,
            "indexes must match"
        );
        assert_eq!(
            object_shape(&legacy, "trigger").await,
            object_shape(&fresh, "trigger").await,
            "rev triggers must match -- P2P sync's delta cursor depends on them"
        );
        // PRAGMA table_info can't see AUTOINCREMENT, so this is checked
        // separately: losing it would start reusing deleted rows' ids.
        assert_eq!(
            autoincrement_tables(&legacy).await,
            autoincrement_tables(&fresh).await,
            "the same tables must be AUTOINCREMENT"
        );
    }

    /// The "existing installs keep working" case: a real v0.13.10 database
    /// must pass the guard untouched.
    #[tokio::test]
    async fn verify_accepts_a_fully_migrated_legacy_database() {
        let pool = legacy_pool(i64::MAX).await;
        mark_backfills_complete(&pool).await;

        verify_final_shape(&pool).await.expect("a fully migrated v0.13.10 database must verify");
    }

    #[tokio::test]
    async fn verify_accepts_a_freshly_created_database() {
        let pool = fresh_pool().await;
        verify_final_shape(&pool).await.expect("a database this build just created must verify");
    }

    /// A database left partway through the old chain must be refused, not
    /// silently used. Migration 22 is the interesting stopping point: by then
    /// every column NAME the guard looks for already exists on
    /// `wellness_check`, and only the declared type gives it away.
    #[tokio::test]
    async fn verify_rejects_a_pre_squash_database() {
        let pool = legacy_pool(22).await;
        mark_backfills_complete(&pool).await;

        match verify_final_shape(&pool).await {
            Err(SchemaError::Incompatible(_)) => {}
            other => panic!("a pre-squash database must be refused, got {other:?}"),
        }
    }

    /// An unfinished encryption back-fill must be refused too: this build has
    /// no pass left that could ever encrypt or hash those rows.
    #[tokio::test]
    async fn verify_rejects_an_unfinished_encryption_backfill() {
        let pool = fresh_pool().await;
        sqlx::query("UPDATE app_setting SET value = 'false' WHERE key = 'screen_time_encryption_migrated'")
            .execute(&pool)
            .await
            .unwrap();

        match verify_final_shape(&pool).await {
            Err(SchemaError::Incompatible(msg)) => assert!(
                msg.contains("screen_time_encryption_migrated"),
                "the message should name the flag: {msg}"
            ),
            other => panic!("an unfinished back-fill must be refused, got {other:?}"),
        }
    }

    /// `ensure_schema_on_pool` must be safe to run on every launch: the first
    /// creates, every one after that verifies.
    #[tokio::test]
    async fn ensure_schema_is_idempotent() {
        let pool = SqlitePool::connect("sqlite::memory:").await.unwrap();
        ensure_schema_on_pool(&pool).await.expect("first run creates");
        ensure_schema_on_pool(&pool).await.expect("second run verifies");
    }

    /// The seeded settings must be the values the old chain ended on, not the
    /// ones it started with -- several were re-set by later migrations.
    #[tokio::test]
    async fn seeded_settings_match_the_legacy_chain_final_values() {
        async fn settings(pool: &SqlitePool) -> BTreeMap<String, String> {
            sqlx::query_as::<_, (String, String)>("SELECT key, value FROM app_setting ORDER BY key")
                .fetch_all(pool)
                .await
                .unwrap()
                .into_iter()
                .collect()
        }

        let legacy_settings = settings(&legacy_pool(i64::MAX).await).await;
        let fresh_settings = settings(&fresh_pool().await).await;

        for (key, legacy_value) in &legacy_settings {
            // The one deliberate difference: the chain seeded this 'false' so
            // its back-fill pass would pick the database up. A fresh database
            // has nothing to back-fill, so it starts 'true'.
            if key == "data_encryption_migrated" {
                assert_eq!(legacy_value, "false");
                assert_eq!(fresh_settings.get(key).map(String::as_str), Some("true"));
                continue;
            }
            assert_eq!(
                fresh_settings.get(key),
                Some(legacy_value),
                "setting `{key}` should be seeded with the value the migration chain ended on"
            );
        }
    }

    /// The triggers are what make `rev` usable as a delta cursor at all: every
    /// write has to advance it without any write path having to remember to.
    #[tokio::test]
    async fn rev_triggers_bump_on_insert_and_update() {
        let pool = fresh_pool().await;

        sqlx::query("INSERT INTO reflection (created_at, slot_start_at, text) VALUES ('a', 'a', 'first')")
            .execute(&pool)
            .await
            .unwrap();
        let rev: i64 =
            sqlx::query_scalar("SELECT rev FROM reflection WHERE id = 1").fetch_one(&pool).await.unwrap();
        assert_eq!(rev, 1, "an insert into an empty table should land at rev 1, not the DEFAULT 0");

        sqlx::query("UPDATE reflection SET text = 'edited' WHERE id = 1").execute(&pool).await.unwrap();
        let bumped: i64 =
            sqlx::query_scalar("SELECT rev FROM reflection WHERE id = 1").fetch_one(&pool).await.unwrap();
        assert_eq!(bumped, 2, "an update must advance rev or the edit never syncs");

        // Multi-row INSERT: the trigger is FOR EACH ROW, so each row must get
        // its own rev rather than all sharing one. db.ts's screen-time batch
        // writes up to 100 rows in one statement this way.
        sqlx::query(
            "INSERT INTO reflection (created_at, slot_start_at, text) VALUES ('b','b','2'), ('c','c','3'), ('d','d','4')",
        )
        .execute(&pool)
        .await
        .unwrap();
        let revs: Vec<i64> = sqlx::query_scalar("SELECT rev FROM reflection WHERE id > 1 ORDER BY id")
            .fetch_all(&pool)
            .await
            .unwrap();
        assert_eq!(revs, vec![3, 4, 5], "each row of a multi-row insert needs its own rev");
    }

    /// The `WHEN NEW.rev = OLD.rev` guard: a writer that sets rev explicitly
    /// is honored rather than clobbered. This is also what stops the insert
    /// trigger's own UPDATE from re-entering the update trigger if
    /// `recursive_triggers` is ever switched on.
    #[tokio::test]
    async fn explicit_rev_write_is_not_overwritten() {
        let pool = fresh_pool().await;

        sqlx::query("INSERT INTO reflection (created_at, slot_start_at, text) VALUES ('a', 'a', 'x')")
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query("UPDATE reflection SET text = 'y', rev = 99 WHERE id = 1").execute(&pool).await.unwrap();

        let rev: i64 =
            sqlx::query_scalar("SELECT rev FROM reflection WHERE id = 1").fetch_one(&pool).await.unwrap();
        assert_eq!(rev, 99, "an explicit rev write should win over the trigger");
    }

    /// Every synced table needs the same treatment -- one that silently
    /// lacked its trigger would stop syncing edits entirely.
    #[tokio::test]
    async fn every_synced_table_has_a_rev_column_and_triggers() {
        let pool = fresh_pool().await;

        for table in [
            "reflection",
            "daily_task_list",
            "not_to_do_list",
            "wellness_check",
            "screen_time_session",
            "bulk_edit_preset",
        ] {
            let columns = sqlx::query(&format!("PRAGMA table_info({table})")).fetch_all(&pool).await.unwrap();
            assert!(
                columns.iter().any(|r| r.get::<String, _>("name") == "rev"),
                "{table} should have a rev column"
            );

            let triggers: Vec<String> = sqlx::query_scalar(&format!(
                "SELECT name FROM sqlite_master WHERE type = 'trigger' AND tbl_name = '{table}'"
            ))
            .fetch_all(&pool)
            .await
            .unwrap();
            assert!(triggers.contains(&format!("trg_{table}_rev_insert")), "{table} missing its insert trigger");
            assert!(triggers.contains(&format!("trg_{table}_rev_update")), "{table} missing its update trigger");
        }
    }

    /// Both tables keyed by a TEXT primary key go through the same upsert
    /// path (`ON CONFLICT(date) DO UPDATE`), where the conflicting write
    /// lands as an UPDATE -- so the update trigger, not the insert one, is
    /// what has to advance rev.
    #[tokio::test]
    async fn upsert_on_text_pk_bumps_rev() {
        let pool = fresh_pool().await;

        for content in ["first", "second"] {
            sqlx::query(
                "INSERT INTO daily_task_list (date, content, updated_at) VALUES ('2026-01-01', ?, 'ts')
                 ON CONFLICT(date) DO UPDATE SET content = excluded.content, updated_at = excluded.updated_at",
            )
            .bind(content)
            .execute(&pool)
            .await
            .unwrap();
        }

        let rev: i64 = sqlx::query_scalar("SELECT rev FROM daily_task_list WHERE date = '2026-01-01'")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(rev, 2, "the conflicting upsert lands as an UPDATE and must still advance rev");
    }

    /// The delta query p2p_sync.rs runs (`WHERE rev > ? AND rev <= ?`) against
    /// a realistic sequence: sync, then edit an *old* row. Under the previous
    /// timestamp cursor this was a real bug -- an edit that carried a
    /// timestamp below the peer's cursor (a row restored from an old export,
    /// or one received from another peer with its original stamp) sat below
    /// the cursor forever and never synced. rev advances on the write itself,
    /// so it can't happen.
    #[tokio::test]
    async fn rev_cursor_catches_an_edit_to_an_old_row() {
        let pool = fresh_pool().await;
        for i in 1..=3 {
            sqlx::query("INSERT INTO reflection (created_at, slot_start_at, text) VALUES (?, ?, 'x')")
                .bind(format!("2020-01-0{i}T00:00:00.000Z"))
                .bind(format!("2020-01-0{i}T00:00:00.000Z"))
                .execute(&pool)
                .await
                .unwrap();
        }

        // First sync ships everything and parks the cursor at the high mark.
        let cursor: i64 =
            sqlx::query_scalar("SELECT COALESCE(MAX(rev), 0) FROM reflection").fetch_one(&pool).await.unwrap();
        let sent: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM reflection WHERE rev > 0 AND rev <= ?")
            .bind(cursor)
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(sent, 3);

        // Nothing changed: the next delta is empty.
        let next: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM reflection WHERE rev > ?")
            .bind(cursor)
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(next, 0, "an unchanged table must produce an empty delta");

        // Edit the *oldest* row -- the one whose timestamps are furthest
        // behind the cursor.
        sqlx::query("UPDATE reflection SET text = 'edited' WHERE id = 1").execute(&pool).await.unwrap();

        let after: Vec<i64> = sqlx::query_scalar("SELECT id FROM reflection WHERE rev > ?")
            .bind(cursor)
            .fetch_all(&pool)
            .await
            .unwrap();
        assert_eq!(after, vec![1], "editing an old row must put it back in the delta");
    }

    /// `import.rs` reads `rows_affected() == 0` as a meaningful signal in two
    /// places -- the screen_time_session duplicate count and the
    /// bulk_edit_preset stale count, both surfaced to the user in
    /// `ImportResult`. An AFTER trigger performs an extra UPDATE behind each
    /// of those statements, so this pins down that the count still reflects
    /// the outer statement and not the trigger's write.
    #[tokio::test]
    async fn rev_triggers_do_not_perturb_rows_affected() {
        let pool = fresh_pool().await;

        // import_screen_time_sessions' dedupe shape: inserts once, then the
        // identical row must report 0.
        let insert_if_absent = "INSERT INTO screen_time_session (app_id, display_name, app_id_hash, platform, device_name, started_at, ended_at)
             SELECT 'enc', '', 'hash', 'windows', 'pc', 's', 'e'
             WHERE NOT EXISTS (
                 SELECT 1 FROM screen_time_session
                 WHERE app_id_hash = 'hash' AND platform = 'windows' AND device_name = 'pc' AND started_at = 's' AND ended_at = 'e'
             )";
        let first = sqlx::query(insert_if_absent).execute(&pool).await.unwrap();
        assert_eq!(first.rows_affected(), 1, "a genuinely new session must report 1, not the trigger's update");
        let second = sqlx::query(insert_if_absent).execute(&pool).await.unwrap();
        assert_eq!(second.rows_affected(), 0, "a duplicate session must still report 0 so it's counted as a duplicate");

        // import_bulk_edit_presets' last-write-wins shape: the guarded upsert
        // must report 0 when the incoming row is staler than what's stored.
        let upsert = "INSERT INTO bulk_edit_preset (id, name, start_time, end_time, text, created_at, updated_at)
             VALUES ('p1', 'n', 's', 'e', 't', 'c', ?)
             ON CONFLICT(id) DO UPDATE SET
                 name = excluded.name, start_time = excluded.start_time, end_time = excluded.end_time,
                 text = excluded.text, updated_at = excluded.updated_at
             WHERE excluded.updated_at > bulk_edit_preset.updated_at";
        let inserted = sqlx::query(upsert).bind("2026-01-02T00:00:00.000Z").execute(&pool).await.unwrap();
        assert_eq!(inserted.rows_affected(), 1);
        let stale = sqlx::query(upsert).bind("2026-01-01T00:00:00.000Z").execute(&pool).await.unwrap();
        assert_eq!(stale.rows_affected(), 0, "a stale preset must still report 0 so it's counted as stale");
        let newer = sqlx::query(upsert).bind("2026-01-03T00:00:00.000Z").execute(&pool).await.unwrap();
        assert_eq!(newer.rows_affected(), 1, "a newer preset must still report 1");
    }

    /// The AUTOINCREMENT tables must keep handing out fresh ids rather than
    /// reusing a deleted row's -- `reflection.id` and `screen_time_session.id`
    /// both travel in export files that get re-imported elsewhere.
    #[tokio::test]
    async fn autoincrement_ids_are_not_reused() {
        let pool = fresh_pool().await;

        sqlx::query("INSERT INTO reflection (id, created_at, slot_start_at, text) VALUES (7, 'a', 'a', 'x')")
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query("DELETE FROM reflection WHERE id = 7").execute(&pool).await.unwrap();

        let result = sqlx::query("INSERT INTO reflection (created_at, slot_start_at, text) VALUES ('b', 'b', 'y')")
            .execute(&pool)
            .await
            .unwrap();
        assert!(result.last_insert_rowid() > 7, "AUTOINCREMENT must not reuse a deleted row's id");
    }
}
