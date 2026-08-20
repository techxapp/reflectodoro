use tauri_plugin_sql::{Migration, MigrationKind};

pub const DB_URL: &str = "sqlite:pomodoro.db";

pub fn migrations() -> Vec<Migration> {
    vec![
        Migration {
            version: 1,
            description: "create reflection, daily_task_list, app_setting tables",
            sql: r#"
                CREATE TABLE reflection (
                    id INTEGER PRIMARY KEY AUTOINCREMENT,
                    created_at TEXT NOT NULL,
                    covers_slot_start_ats TEXT NOT NULL,
                    text TEXT NOT NULL
                );

                CREATE INDEX idx_reflection_created_at ON reflection(created_at);

                CREATE TABLE daily_task_list (
                    date TEXT PRIMARY KEY,
                    content TEXT NOT NULL DEFAULT '',
                    updated_at TEXT NOT NULL
                );

                CREATE TABLE app_setting (
                    key TEXT PRIMARY KEY,
                    value TEXT NOT NULL
                );

                INSERT INTO app_setting (key, value) VALUES ('breakit_phrase', 'breakit');
                INSERT INTO app_setting (key, value) VALUES ('breakit_target', '10');
            "#,
            kind: MigrationKind::Up,
        },
        Migration {
            version: 2,
            description: "replace fixed breakit phrase/count with a random per-overlay challenge string",
            sql: r#"
                DELETE FROM app_setting WHERE key IN ('breakit_phrase', 'breakit_target');
                INSERT INTO app_setting (key, value) VALUES ('breakit_length', '15');
                INSERT INTO app_setting (key, value) VALUES ('breakit_include_special', 'false');
            "#,
            kind: MigrationKind::Up,
        },
        Migration {
            version: 3,
            description: "one reflection row per covered slot instead of a JSON array column, so each missed pomodoro is its own record",
            sql: r#"
                CREATE TABLE reflection_new (
                    id INTEGER PRIMARY KEY AUTOINCREMENT,
                    created_at TEXT NOT NULL,
                    slot_start_at TEXT NOT NULL,
                    text TEXT NOT NULL
                );

                INSERT INTO reflection_new (created_at, slot_start_at, text)
                SELECT reflection.created_at, je.value, reflection.text
                FROM reflection, json_each(reflection.covers_slot_start_ats) AS je;

                DROP TABLE reflection;
                ALTER TABLE reflection_new RENAME TO reflection;

                CREATE INDEX idx_reflection_created_at ON reflection(created_at);
                CREATE INDEX idx_reflection_slot_start_at ON reflection(slot_start_at);
            "#,
            kind: MigrationKind::Up,
        },
    ]
}
