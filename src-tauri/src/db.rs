use tauri_plugin_sql::{Migration, MigrationKind};

pub const DB_URL: &str = "sqlite:pomodoro.db";

pub fn migrations() -> Vec<Migration> {
    vec![Migration {
        version: 1,
        description: "create reflection, daily_task_list, app_setting tables",
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
    }]
}
