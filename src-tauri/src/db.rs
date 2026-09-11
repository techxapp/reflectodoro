use sqlx::sqlite::{SqlitePool, SqlitePoolOptions};
use tauri::{AppHandle, Manager};
use tauri_plugin_sql::{Migration, MigrationKind};

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
pub async fn open_direct_pool(app: &AppHandle) -> Result<SqlitePool, String> {
    let app_dir = app
        .path()
        .app_config_dir()
        .map_err(|e| format!("no app config dir: {e}"))?;
    std::fs::create_dir_all(&app_dir).map_err(|e| format!("couldn't create app config dir: {e}"))?;
    let db_path = app_dir.join(DB_URL.trim_start_matches("sqlite:"));
    let db_path_str = db_path.to_str().ok_or_else(|| "non-utf8 db path".to_string())?;
    SqlitePoolOptions::new()
        .connect(&format!("sqlite:{db_path_str}"))
        .await
        .map_err(|e| format!("failed to open database connection: {e}"))
}

pub fn migrations() -> Vec<Migration> {
    vec![
        Migration {
            version: 1,
            description: "create reflection, daily_task_list, app_setting tables",
            // NOTE: sqlx checksums this exact string against what's already recorded in
            // `_sqlx_migrations` on any db that already applied version 1 -- do not
            // reformat/re-indent it (even whitespace-only changes break the checksum
            // and silently abort every migration after it, including new ones).
            sql: r#"
            CREATE TABLE reflection (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                created_at TEXT NOT NULL,
                slot_start_at TEXT NOT NULL,
                text TEXT NOT NULL
            );

            CREATE INDEX idx_reflection_created_at ON reflection(created_at);
            CREATE INDEX idx_reflection_slot_start_at ON reflection(slot_start_at);

            CREATE TABLE daily_task_list (
                date TEXT PRIMARY KEY,
                content TEXT NOT NULL DEFAULT '',
                updated_at TEXT NOT NULL
            );

            CREATE TABLE app_setting (
                key TEXT PRIMARY KEY,
                value TEXT NOT NULL
            );

            INSERT INTO app_setting (key, value) VALUES ('breakit_length', '15');
            INSERT INTO app_setting (key, value) VALUES ('breakit_include_special', 'false');
        "#,
            kind: MigrationKind::Up,
        },
        Migration {
            version: 2,
            description: "create wellness_check table",
            sql: r#"
                CREATE TABLE wellness_check (
                    id INTEGER PRIMARY KEY AUTOINCREMENT,
                    reflection_id INTEGER NOT NULL REFERENCES reflection(id),
                    relaxed_eyes INTEGER NOT NULL DEFAULT 1,
                    exercise INTEGER NOT NULL DEFAULT 1,
                    drank_water INTEGER NOT NULL DEFAULT 1,
                    washroom INTEGER NOT NULL DEFAULT 0,
                    created_at TEXT NOT NULL
                );

                CREATE INDEX idx_wellness_check_reflection_id ON wellness_check(reflection_id);
            "#,
            kind: MigrationKind::Up,
        },
        Migration {
            version: 3,
            // Runs once on every db that hasn't seen it yet -- both brand new
            // installs (right after v1/v2) and existing installs upgrading
            // into this app version -- so the shortcut defaults to enabled in
            // both cases, matching the setting's intended default.
            description: "default force_close_shortcut_enabled setting to enabled",
            sql: r#"
                INSERT INTO app_setting (key, value) VALUES ('force_close_shortcut_enabled', 'true');
            "#,
            kind: MigrationKind::Up,
        },
        Migration {
            version: 4,
            description: "default overlay/checkin auto-close minutes settings",
            sql: r#"
                INSERT INTO app_setting (key, value) VALUES ('overlay_auto_close_minutes', '5');
                INSERT INTO app_setting (key, value) VALUES ('checkin_auto_close_minutes', '5');
            "#,
            kind: MigrationKind::Up,
        },
        Migration {
            version: 5,
            // OR IGNORE, unlike migrations 3/4: this key already existed as a
            // user-settable value (see getWellnessTextExclusions in db.ts)
            // before it had a default, so an upgrading db may already have a
            // row here -- a plain INSERT would hit the PRIMARY KEY and abort
            // the migration (and everything after it) on that db.
            description: "default wellness_text_exclusions to Washroom",
            sql: r#"
                INSERT OR IGNORE INTO app_setting (key, value) VALUES ('wellness_text_exclusions', 'Washroom');
            "#,
            kind: MigrationKind::Up,
        },
        Migration {
            version: 6,
            description: "default media_pause_on_break_enabled setting to enabled",
            sql: r#"
                INSERT INTO app_setting (key, value) VALUES ('media_pause_on_break_enabled', 'true');
            "#,
            kind: MigrationKind::Up,
        },
        Migration {
            version: 7,
            description: "bump default checkin_auto_close_minutes to 15",
            sql: r#"
                UPDATE app_setting SET value = '15'
                WHERE key = 'checkin_auto_close_minutes' AND value = '5';
            "#,
            kind: MigrationKind::Up,
        },
        Migration {
            version: 8,
            description: "bump default checkin_auto_close_minutes to 19",
            sql: r#"
                UPDATE app_setting SET value = '19'
                WHERE key = 'checkin_auto_close_minutes' AND value = '15';
            "#,
            kind: MigrationKind::Up,
        },
        Migration {
            version: 9,
            description: "default break_notification_persistent_enabled setting to enabled",
            sql: r#"
                INSERT INTO app_setting (key, value) VALUES ('break_notification_persistent_enabled', 'true');
            "#,
            kind: MigrationKind::Up,
        },
        Migration {
            version: 10,
            description: "create not_to_do_list table",
            sql: r#"
                -- date is TEXT (not a SQLite date type -- SQLite has none), storing
                -- the same local 'YYYY-MM-DD' string localDateStamp() produces, same
                -- as daily_task_list.date -- keeps it a drop-in string-equality join
                -- against the rest of the schema (see CLAUDE.md's Data model).
                CREATE TABLE not_to_do_list (
                    date TEXT PRIMARY KEY,
                    content TEXT NOT NULL DEFAULT '',
                    updated_at TEXT NOT NULL
                );
            "#,
            kind: MigrationKind::Up,
        },
        Migration {
            version: 11,
            // Backfills rows written before the fix that changed
            // reflection.slot_start_at's meaning from "start of the break
            // slot" (:25/:55) to "start of the work slot the break follows"
            // (:00/:30) -- see CLAUDE.md's Data model. Without this, old rows
            // never match the new coverage-check anchor findMissedSlots
            // uses, so every future submit walks the full missed-slot
            // lookback cap treating already-reflected history as uncovered.
            //
            // Classifies old-format rows by LOCAL minute (25/55), not the
            // raw UTC value: a half-hour-offset timezone (e.g. IST, +5:30)
            // can flip which UTC minute a given local :25/:55 lands on.
            // `localtime` re-derives that from whatever timezone this device
            // is in right now -- safe because reflection.slot_start_at is
            // only ever written and read on a single local device (this app
            // has no sync). The shift itself (-25 minutes) is a fixed
            // duration subtracted from the underlying instant, so it's
            // correct regardless of timezone once a row is classified.
            description: "backfill reflection.slot_start_at from break-slot-start to work-slot-start",
            sql: r#"
                UPDATE reflection
                SET slot_start_at = strftime('%Y-%m-%dT%H:%M:%f', datetime(slot_start_at, '-25 minutes')) || 'Z'
                WHERE CAST(strftime('%M', slot_start_at, 'localtime') AS INTEGER) IN (25, 55);
            "#,
            kind: MigrationKind::Up,
        },
        Migration {
            version: 12,
            // Off by default, unlike every other app_setting toggle here --
            // this is an opt-in feature (hides the macOS menu bar/Dock during
            // a break), materially more disruptive to the user's desktop than
            // anything else this app does. Only gates menu bar/Dock hiding --
            // Space-following and the Cmd+Tab block are always on. See
            // macos_overlay.rs.
            description: "default macos_hide_menu_bar_dock_enabled setting to disabled",
            sql: r#"
                INSERT INTO app_setting (key, value) VALUES ('macos_hide_menu_bar_dock_enabled', 'false');
            "#,
            kind: MigrationKind::Up,
        },
        Migration {
            version: 13,
            // Foreground-app focus tracking (see screen_time.rs). One row per
            // closed focus session, written in batches by the main window from
            // the "screentime://session-batch" event -- no per-focus-change
            // write, and nothing written at all during a stretch with no app
            // switches.
            //
            // `platform` is per-row because app_id's format is OS-specific
            // (exe basename / bundle id / WM_CLASS / package name), which only
            // matters once an export is opened on a different OS.
            // `device_name` disambiguates the same app on two machines (e.g.
            // two Windows laptops merged via Settings -> Data import) -- the
            // one path data crosses devices in this otherwise single-device
            // app. Seeded empty; the main window fills it from the OS hostname
            // on first boot (commands::get_hostname) and Settings lets the
            // user rename it.
            description: "create screen_time_session table",
            sql: r#"
                CREATE TABLE screen_time_session (
                    id INTEGER PRIMARY KEY AUTOINCREMENT,
                    app_id TEXT NOT NULL,
                    platform TEXT NOT NULL,
                    device_name TEXT NOT NULL DEFAULT '',
                    started_at TEXT NOT NULL,
                    ended_at TEXT NOT NULL
                );

                CREATE INDEX idx_screen_time_session_started_at ON screen_time_session(started_at);
                CREATE INDEX idx_screen_time_session_app_id ON screen_time_session(app_id);

                -- OR IGNORE, like migration 5 and unlike the other seeding
                -- migrations here: a db that already carries either key (an
                -- import from a newer export, or a dev db that ran the
                -- frontend's device-name seeding against a pre-migration
                -- binary) would otherwise hit the PRIMARY KEY, roll the whole
                -- migration back -- taking CREATE TABLE with it -- and abort
                -- every migration after it. Hit for real during development.
                INSERT OR IGNORE INTO app_setting (key, value) VALUES ('screen_time_tracking_enabled', 'true');
                INSERT OR IGNORE INTO app_setting (key, value) VALUES ('device_name', '');
            "#,
            kind: MigrationKind::Up,
        },
        Migration {
            version: 14,
            // Adds a best-effort friendly display name (e.g. "Google Chrome"
            // for app_id 'chrome.exe', read from the exe's FileDescription
            // version-resource string -- see screen_time.rs's Windows
            // platform_impl) alongside the stable app_id already captured.
            // Purely additive/display-only: app_id stays the grouping key
            // used everywhere (aggregation, self-exclusion, dedupe), so this
            // needs no backfill -- existing rows simply show '' until they
            // next age out, and getScreenTimeForDate (db.ts) already falls
            // back to app_id wherever display_name is empty.
            description: "add display_name to screen_time_session",
            sql: r#"
                ALTER TABLE screen_time_session ADD COLUMN display_name TEXT NOT NULL DEFAULT '';
            "#,
            kind: MigrationKind::Up,
        },
        Migration {
            version: 15,
            // Entries tab hides apps under this many minutes of focus time for
            // a given day (see getScreenTimeAppThresholdMinutes in db.ts) --
            // frontend-only filter, nothing in Rust reads this key. OR IGNORE
            // like migrations 5/13: db.ts's numberOr already falls back to the
            // same default (5) when the row is missing, so this is a
            // convenience seed, not load-bearing -- a pre-existing row from a
            // dev db or a newer export must not abort the migration.
            description: "default screen_time_app_threshold_minutes setting to 5",
            sql: r#"
                INSERT OR IGNORE INTO app_setting (key, value) VALUES ('screen_time_app_threshold_minutes', '5');
            "#,
            kind: MigrationKind::Up,
        },
        Migration {
            version: 16,
            // Backs import.rs's merge-mode dedupe: a Settings -> Data merge
            // import skips a screen_time_session row if one with the same
            // (app_id, platform, device_name, started_at, ended_at) already
            // exists (an INSERT ... WHERE NOT EXISTS per row), so re-importing
            // the same export twice no longer duplicates every session. This
            // index is what keeps that per-row NOT EXISTS check fast on a
            // table documented (see CLAUDE.md) as likely to become the
            // largest by row count.
            //
            // Deliberately NOT UNIQUE: a genuine duplicate could already
            // exist in an existing db from a double-import before this fix
            // shipped, and a UNIQUE index would abort this migration (and
            // every migration after it) on any such db. This index only
            // speeds up the lookup; import.rs's own INSERT ... WHERE NOT
            // EXISTS is what actually enforces the dedupe, and only during
            // import -- the normal screen_time.rs capture path is unaffected.
            description: "add non-unique dedupe index to screen_time_session",
            sql: r#"
                CREATE INDEX idx_screen_time_session_dedupe
                    ON screen_time_session(app_id, platform, device_name, started_at, ended_at);
            "#,
            kind: MigrationKind::Up,
        },
        Migration {
            version: 17,
            // Replaces wellness_check.reflection_id with slot_start_at, so
            // wellness_check keys off the same natural identity every other
            // table here uses (reflection/daily_task_list/not_to_do_list/
            // screen_time_session all key off a real-world identity, not a
            // surrogate FK) -- and so import.rs's merge-mode dedupe can key
            // on it directly instead of remapping reflection_id through
            // idMap on every import (see import.rs's import_wellness_checks).
            //
            // Not a plain `ALTER TABLE ... DROP COLUMN reflection_id`: SQLite
            // refuses to drop a column that's part of a FOREIGN KEY
            // constraint (reflection_id is declared
            // `REFERENCES reflection(id)`), so this uses SQLite's standard
            // rebuild-the-table procedure instead -- create the final shape,
            // copy data across (resolving each row's slot_start_at from its
            // current reflection_id), drop the old table, rename the new one
            // into place.
            //
            // COALESCE(..., '') guards a theoretical orphaned row (a
            // reflection_id that no longer resolves to any reflection row):
            // without it a NULL would violate the new column's NOT NULL and
            // abort this whole migration -- and every migration after it --
            // on that db. Explicit `id` values in the INSERT keep the
            // AUTOINCREMENT sequence continuous after the rename (SQLite
            // tracks the max rowid ever used regardless of whether it was
            // assigned explicitly or automatically).
            description: "replace wellness_check.reflection_id with slot_start_at",
            sql: r#"
                CREATE TABLE wellness_check_new (
                    id INTEGER PRIMARY KEY AUTOINCREMENT,
                    slot_start_at TEXT NOT NULL DEFAULT '',
                    relaxed_eyes INTEGER NOT NULL DEFAULT 1,
                    exercise INTEGER NOT NULL DEFAULT 1,
                    drank_water INTEGER NOT NULL DEFAULT 1,
                    washroom INTEGER NOT NULL DEFAULT 0,
                    created_at TEXT NOT NULL
                );

                INSERT INTO wellness_check_new (id, slot_start_at, relaxed_eyes, exercise, drank_water, washroom, created_at)
                SELECT wc.id,
                       COALESCE((SELECT r.slot_start_at FROM reflection r WHERE r.id = wc.reflection_id), ''),
                       wc.relaxed_eyes, wc.exercise, wc.drank_water, wc.washroom, wc.created_at
                FROM wellness_check wc;

                DROP TABLE wellness_check;
                ALTER TABLE wellness_check_new RENAME TO wellness_check;

                CREATE INDEX idx_wellness_check_slot_start_at ON wellness_check(slot_start_at);
            "#,
            kind: MigrationKind::Up,
        },
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use sqlx::Row;

    fn migration_sql(version: i64) -> String {
        migrations().into_iter().find(|m| m.version == version).unwrap().sql.to_string()
    }

    /// Runs migration 17's actual SQL (not a re-implementation of it) against
    /// an in-memory db seeded with the pre-migration schema and real rows --
    /// including a deliberately orphaned `reflection_id` -- to confirm the
    /// rebuild-the-table approach (required because SQLite refuses to
    /// `DROP COLUMN` a column that's part of a FOREIGN KEY constraint)
    /// actually backfills `slot_start_at` correctly, drops `reflection_id`,
    /// and doesn't abort on the orphaned row.
    #[tokio::test]
    async fn migration_17_backfills_slot_start_at_and_drops_reflection_id() {
        let pool = sqlx::SqlitePool::connect("sqlite::memory:").await.unwrap();
        // sqlx enables `foreign_keys` enforcement by default, which would
        // otherwise refuse the deliberately-orphaned insert below outright --
        // meaning a real orphan likely can't arise through this app's own
        // inserts today. Disabled here only to construct that scenario
        // anyway (an old pre-enforcement release, or manual db editing,
        // could still produce one) and confirm the migration's defensive
        // COALESCE actually holds up against it.
        sqlx::query("PRAGMA foreign_keys = OFF").execute(&pool).await.unwrap();

        // Pre-migration-17 schema (migrations 1 + 2).
        sqlx::query(
            "CREATE TABLE reflection (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                created_at TEXT NOT NULL,
                slot_start_at TEXT NOT NULL,
                text TEXT NOT NULL
            )",
        )
        .execute(&pool)
        .await
        .unwrap();
        sqlx::query(
            "CREATE TABLE wellness_check (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                reflection_id INTEGER NOT NULL REFERENCES reflection(id),
                relaxed_eyes INTEGER NOT NULL DEFAULT 1,
                exercise INTEGER NOT NULL DEFAULT 1,
                drank_water INTEGER NOT NULL DEFAULT 1,
                washroom INTEGER NOT NULL DEFAULT 0,
                created_at TEXT NOT NULL
            )",
        )
        .execute(&pool)
        .await
        .unwrap();
        sqlx::query("CREATE INDEX idx_wellness_check_reflection_id ON wellness_check(reflection_id)")
            .execute(&pool)
            .await
            .unwrap();

        sqlx::query(
            "INSERT INTO reflection (id, created_at, slot_start_at, text)
             VALUES (1, '2026-01-01T00:00:00.000Z', '2026-01-01T00:00:00.000Z', 'Did yoga')",
        )
        .execute(&pool)
        .await
        .unwrap();
        // Normal row -- resolves via reflection_id.
        sqlx::query(
            "INSERT INTO wellness_check (id, reflection_id, created_at)
             VALUES (10, 1, '2026-01-01T00:05:00.000Z')",
        )
        .execute(&pool)
        .await
        .unwrap();
        // Deliberately orphaned -- reflection_id 999 doesn't exist. Must not
        // abort the migration; must backfill to '' via the COALESCE.
        sqlx::query(
            "INSERT INTO wellness_check (id, reflection_id, created_at)
             VALUES (11, 999, '2026-01-01T00:06:00.000Z')",
        )
        .execute(&pool)
        .await
        .unwrap();

        sqlx::raw_sql(&migration_sql(17)).execute(&pool).await.unwrap();

        let columns = sqlx::query("PRAGMA table_info(wellness_check)").fetch_all(&pool).await.unwrap();
        let column_names: Vec<String> = columns.iter().map(|r| r.get("name")).collect();
        assert!(!column_names.contains(&"reflection_id".to_string()), "reflection_id must be gone");
        assert!(column_names.contains(&"slot_start_at".to_string()));

        let normal_slot: String = sqlx::query_scalar("SELECT slot_start_at FROM wellness_check WHERE id = 10")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(normal_slot, "2026-01-01T00:00:00.000Z");

        let orphan_slot: String = sqlx::query_scalar("SELECT slot_start_at FROM wellness_check WHERE id = 11")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(orphan_slot, "", "an orphaned reflection_id must backfill to '' rather than abort the migration");

        // A fresh insert continues the AUTOINCREMENT sequence past the max
        // explicit id (11) rather than colliding with or reusing it.
        let result = sqlx::query("INSERT INTO wellness_check (slot_start_at, created_at) VALUES ('x', 'y')")
            .execute(&pool)
            .await
            .unwrap();
        assert!(result.last_insert_rowid() > 11);
    }
}
