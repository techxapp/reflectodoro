use tauri_plugin_sql::{Migration, MigrationKind};

pub const DB_URL: &str = "sqlite:pomodoro.db";

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
    ]
}
