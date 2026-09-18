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
        Migration {
            version: 18,
            // Backs P2P LAN device pairing/sync (p2p_sync.rs). device_id is
            // the peer's stable self-identity (see
            // p2p_sync::get_or_create_device_id) -- not a local autoincrement
            // id, since it must stay the same across reconnects/IP changes
            // and be exchanged during pairing. shared_key is the hex-encoded
            // symmetric key the SPAKE2 pairing handshake derived -- never the
            // PIN itself (see p2p_sync.rs's module doc comment for why).
            // last_sync_at is the delta-sync cursor for this specific peer;
            // NULL means "never synced, send everything".
            description: "create paired_device table",
            sql: r#"
                CREATE TABLE paired_device (
                    device_id TEXT PRIMARY KEY,
                    name TEXT NOT NULL,
                    platform TEXT NOT NULL,
                    shared_key TEXT NOT NULL,
                    paired_at TEXT NOT NULL,
                    last_sync_at TEXT
                );
            "#,
            kind: MigrationKind::Up,
        },
        Migration {
            version: 19,
            // Delta-sync cursor for reflection (p2p_sync.rs): unlike
            // daily_task_list/not_to_do_list (already have updated_at) or
            // screen_time_session/wellness_check (append-only, created_at/
            // started_at already work as a cursor), reflection.text can be
            // edited in place after insert (updateReflectionText,
            // bulkUpsertReflections in db.ts) with nothing recording *when*.
            // Without this, a delta sync keyed on created_at would miss a
            // post-insert edit entirely. Backfilled from created_at so every
            // existing row already has a usable value.
            description: "add updated_at to reflection",
            sql: r#"
                ALTER TABLE reflection ADD COLUMN updated_at TEXT;
                UPDATE reflection SET updated_at = created_at WHERE updated_at IS NULL;
            "#,
            kind: MigrationKind::Up,
        },
        Migration {
            version: 20,
            // Caps how many times/day the breakit captcha can be used to
            // exit a break early (commands::breakit_attempt) -- local
            // device usage state, not user content, so deliberately never
            // included in file export/import or P2P sync (unlike
            // breakit_max_per_day itself, an app_setting seeded below).
            // breakit_attempt keeps this table at exactly one row (today's)
            // at all times -- see breakit::increment_daily_use -- so there's
            // no history to retain and nothing to prune later.
            description: "add breakit_daily_use table and breakit_max_per_day setting",
            sql: r#"
                CREATE TABLE breakit_daily_use (
                    date TEXT PRIMARY KEY,
                    count INTEGER NOT NULL DEFAULT 0
                );

                INSERT INTO app_setting (key, value) VALUES ('breakit_max_per_day', '5');
            "#,
            kind: MigrationKind::Up,
        },
        Migration {
            version: 21,
            // Seeds a real, editable default rather than hardcoding one as a
            // read-side fallback in db.ts -- Settings' quoteApiUrl field can
            // then delete this row's value down to "" to genuinely disable
            // the panel, the same as any other app_setting toggle here,
            // without a code-level default fighting that choice on the next
            // read. zenquotes.io's response shape ([{"q": ..., "a": ...}])
            // is one of the field conventions fetch_quote/extract_quote_text
            // already parse (commands.rs).
            description: "seed default quote_api_url setting",
            sql: r#"
                INSERT INTO app_setting (key, value) VALUES ('quote_api_url', 'https://zenquotes.io/api/random');
            "#,
            kind: MigrationKind::Up,
        },
        Migration {
            version: 22,
            // Marks whether crypto.rs's one-time pass over pre-existing
            // plaintext rows (reflection.text, daily_task_list.content,
            // not_to_do_list.content) has run -- see
            // crypto::run_encryption_migration_after_db_ready. Seeded 'false'
            // for every db, new or upgrading: a brand-new db has no rows to
            // encrypt, so its migration pass is a no-op that just flips this
            // to 'true'.
            //
            // This is only a "don't rescan every launch" shortcut, NOT the
            // actual safety mechanism -- that's the per-row 'enc1:' marker
            // check, which is what makes the pass idempotent even if this
            // value is lost, wrong, or wiped by a settings import.
            //
            // OR IGNORE like migrations 5/13/15: an import from a newer
            // export could already carry this key, and a plain INSERT would
            // hit the PRIMARY KEY and abort every later migration on that db.
            description: "seed data_encryption_migrated flag",
            sql: r#"
                INSERT OR IGNORE INTO app_setting (key, value) VALUES ('data_encryption_migrated', 'false');
            "#,
            kind: MigrationKind::Up,
        },
        Migration {
            version: 23,
            // Changes wellness_check's four boolean columns from INTEGER to
            // TEXT so they can hold crypto.rs's 'enc1:'-prefixed ciphertext --
            // see CLAUDE.md's "Encryption at rest". This migration only
            // changes storage type; it does not itself encrypt anything.
            // Existing values are cast to their plain-text '0'/'1' string
            // form here, and crypto::run_encryption_migration_after_db_ready
            // (via the four new wellness_check entries in crypto.rs's
            // ENCRYPTED_COLUMNS) picks them up on next launch, same as any
            // other pre-existing plaintext row.
            //
            // Same rebuild-the-table technique migration 17 already used on
            // this table (SQLite has no ALTER COLUMN TYPE): create the final
            // shape, copy data across with an explicit CAST, drop the old
            // table, rename the new one into place. Explicit `id` values in
            // the INSERT keep the AUTOINCREMENT sequence continuous.
            description: "change wellness_check boolean columns from INTEGER to TEXT for encryption at rest",
            sql: r#"
                CREATE TABLE wellness_check_new (
                    id INTEGER PRIMARY KEY AUTOINCREMENT,
                    slot_start_at TEXT NOT NULL DEFAULT '',
                    relaxed_eyes TEXT NOT NULL DEFAULT '1',
                    exercise TEXT NOT NULL DEFAULT '1',
                    drank_water TEXT NOT NULL DEFAULT '1',
                    washroom TEXT NOT NULL DEFAULT '0',
                    created_at TEXT NOT NULL
                );

                INSERT INTO wellness_check_new (id, slot_start_at, relaxed_eyes, exercise, drank_water, washroom, created_at)
                SELECT id, slot_start_at,
                       CAST(relaxed_eyes AS TEXT), CAST(exercise AS TEXT), CAST(drank_water AS TEXT), CAST(washroom AS TEXT),
                       created_at
                FROM wellness_check;

                DROP TABLE wellness_check;
                ALTER TABLE wellness_check_new RENAME TO wellness_check;

                CREATE INDEX idx_wellness_check_slot_start_at ON wellness_check(slot_start_at);
            "#,
            kind: MigrationKind::Up,
        },
        Migration {
            version: 24,
            // screen_time_session's app_id/display_name become crypto.rs's
            // usual 'enc1:'-prefixed ciphertext (see CLAUDE.md's "Encryption
            // at rest") -- a fresh random nonce every time, so the same
            // plaintext never encrypts to the same value twice. That breaks
            // both getScreenTimeForDate's GROUP BY and import.rs's
            // duplicate check, which used to compare app_id directly.
            // app_id_hash is a deterministic HMAC-SHA256 "blind index" of
            // app_id (crypto::FieldCipher::blind_index_many) that both of
            // those switch to instead. Defaults to '' for every existing
            // row -- crypto::run_encryption_migration_after_db_ready's
            // screen-time backfill picks those up afterward, the same way
            // migration 23 left encryption itself to the four-table
            // migration that already existed.
            //
            // idx_screen_time_session_dedupe (migration 16) is rebuilt to
            // match: it existed to keep import.rs's per-row duplicate check
            // fast, and that check now filters on app_id_hash instead of
            // ciphertext app_id. idx_screen_time_session_app_id (migration
            // 13) is dropped outright -- nothing looks up by app_id's value
            // anymore now that it's ciphertext, so an index on it only adds
            // write overhead on this app's fastest-growing table.
            description: "add app_id_hash to screen_time_session for encrypted-column dedupe/aggregation",
            sql: r#"
                ALTER TABLE screen_time_session ADD COLUMN app_id_hash TEXT NOT NULL DEFAULT '';

                DROP INDEX idx_screen_time_session_dedupe;
                CREATE INDEX idx_screen_time_session_dedupe
                    ON screen_time_session(app_id_hash, platform, device_name, started_at, ended_at);

                DROP INDEX idx_screen_time_session_app_id;
            "#,
            kind: MigrationKind::Up,
        },
        Migration {
            version: 25,
            // Backs the Entries page's "Bulk edit reflections" -> Prefill
            // feature (see CLAUDE.md's "Bulk-editing reflections by time
            // range"): named presets a user can save once (a time range +
            // text) and reapply to the bulk-edit fields instead of retyping
            // them every time for a recurring routine (sleep, lunch, ...).
            //
            // `id` is a client-generated crypto.randomUUID() (db.ts), not an
            // autoincrement integer -- it has to stay stable and collision-free
            // across two independent devices for P2P sync/export-import to
            // upsert by it, the same reasoning paired_device.device_id and
            // p2p_device_id already follow.
            //
            // start_time/end_time/text are ENCRYPTED AT REST (crypto.rs's
            // usual 'enc1:'-prefixed ciphertext, written via db.ts's
            // encryptField/encryptFields -- never via raw migration SQL,
            // which has no access to FieldCipher) -- `name` stays plaintext
            // since getBulkEditPresets sorts by it in SQL, which ciphertext
            // can't support. No ENCRYPTED_COLUMNS backfill entry needed in
            // crypto.rs: this table is brand new, so there are no
            // pre-existing plaintext rows to migrate -- every row, seeded or
            // user-created, is written through the cipher from the moment it
            // can exist.
            //
            // updated_at is this table's P2P sync delta cursor, same role as
            // reflection.updated_at.
            description: "create bulk_edit_preset table for saved bulk-edit prefill presets",
            sql: r#"
                CREATE TABLE bulk_edit_preset (
                    id TEXT PRIMARY KEY,
                    name TEXT NOT NULL,
                    start_time TEXT NOT NULL,
                    end_time TEXT NOT NULL,
                    text TEXT NOT NULL,
                    created_at TEXT NOT NULL,
                    updated_at TEXT NOT NULL
                );
            "#,
            kind: MigrationKind::Up,
        },
        Migration {
            version: 26,
            // Lowers the default breakit challenge length from 15 to 12.
            // Guarded like migrations 7/8 (checkin_auto_close_minutes): only
            // flips a db that still holds the old default value, so a user
            // who already customized breakit_length via Settings keeps their
            // own value rather than having it silently overwritten.
            description: "lower default breakit_length to 12",
            sql: r#"
                UPDATE app_setting SET value = '12'
                WHERE key = 'breakit_length' AND value = '15';
            "#,
            kind: MigrationKind::Up,
        },
        Migration {
            version: 27,
            // Per-device opt-in for automatic P2P sync (see p2p_sync.rs's
            // maybe_auto_sync and CLAUDE.md's "P2P LAN sync"). Local to each
            // side's own copy of the pairing record, same as last_sync_at --
            // never included in the sync payload, so device A's opt-in
            // choice never dictates device B's. Defaults off, matching
            // p2p_sync.rs's existing "manual, opt-in" framing for the whole
            // feature.
            description: "add auto_sync_enabled to paired_device (per-device opt-in, default off)",
            sql: r#"
                ALTER TABLE paired_device ADD COLUMN auto_sync_enabled INTEGER NOT NULL DEFAULT 0;
            "#,
            kind: MigrationKind::Up,
        },
        Migration {
            version: 28,
            // Replaces P2P sync's timestamp delta cursor with a device-local
            // monotonic `rev` counter on every synced table (see p2p_sync.rs's
            // build_delta_payload and CLAUDE.md's "P2P LAN sync").
            //
            // Why: reflection.created_at/updated_at are encrypted at rest as
            // of this release (crypto.rs), and random-nonce ciphertext can't
            // support the `COALESCE(updated_at, created_at) > ?` inequality the
            // cursor used to run. A counter also carries no wall-clock
            // information at all, which is the point -- a plaintext timestamp
            // column leaks edit times (and so sleep/activity patterns) to
            // anyone who can read the db file without the key.
            //
            // Two bugs the timestamp cursor had, which this also fixes:
            //   * A backward clock jump (NTP correction, manual change) wrote
            //     rows with timestamps below last_sync_at that were then
            //     silently never synced.
            //   * import.rs stamps a merged row with the *incoming* peer's
            //     updated_at, so a row received from device A could already sit
            //     below this device's cursor with device C and never forward to
            //     it. A local bump on every local write fixes that fan-out.
            //
            // Statement order per table is load-bearing: the `rev = rowid`
            // backfill must run BEFORE the triggers exist, or it would fire the
            // update trigger once per row. rowid is used rather than each
            // table's primary key because three of these six are keyed by TEXT
            // (date / uuid); all six are ordinary rowid tables (none is
            // WITHOUT ROWID), and rowid is monotonic in insert order.
            //
            // The `WHEN NEW.rev = OLD.rev` guard on the update triggers means an
            // explicit rev write is honored rather than clobbered, and blocks
            // re-entry if `recursive_triggers` is ever switched on (it is
            // currently off -- open_direct_pool sets no pragmas, and
            // tauri-plugin-sql opens its pool with a bare Pool::connect).
            //
            // idx_reflection_created_at (migration 1) is dropped: created_at is
            // ciphertext now, so the index can only ever index random bytes.
            description: "add monotonic rev counter + triggers to synced tables, add sync_cursor",
            sql: r#"
                ALTER TABLE reflection ADD COLUMN rev INTEGER NOT NULL DEFAULT 0;
                UPDATE reflection SET rev = rowid;
                CREATE INDEX idx_reflection_rev ON reflection(rev);
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

                ALTER TABLE daily_task_list ADD COLUMN rev INTEGER NOT NULL DEFAULT 0;
                UPDATE daily_task_list SET rev = rowid;
                CREATE INDEX idx_daily_task_list_rev ON daily_task_list(rev);
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

                ALTER TABLE not_to_do_list ADD COLUMN rev INTEGER NOT NULL DEFAULT 0;
                UPDATE not_to_do_list SET rev = rowid;
                CREATE INDEX idx_not_to_do_list_rev ON not_to_do_list(rev);
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

                ALTER TABLE wellness_check ADD COLUMN rev INTEGER NOT NULL DEFAULT 0;
                UPDATE wellness_check SET rev = rowid;
                CREATE INDEX idx_wellness_check_rev ON wellness_check(rev);
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

                ALTER TABLE screen_time_session ADD COLUMN rev INTEGER NOT NULL DEFAULT 0;
                UPDATE screen_time_session SET rev = rowid;
                CREATE INDEX idx_screen_time_session_rev ON screen_time_session(rev);
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

                ALTER TABLE bulk_edit_preset ADD COLUMN rev INTEGER NOT NULL DEFAULT 0;
                UPDATE bulk_edit_preset SET rev = rowid;
                CREATE INDEX idx_bulk_edit_preset_rev ON bulk_edit_preset(rev);
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

                CREATE TABLE sync_cursor (
                    device_id TEXT NOT NULL,
                    table_name TEXT NOT NULL,
                    last_rev INTEGER NOT NULL,
                    PRIMARY KEY (device_id, table_name)
                );

                DROP INDEX IF EXISTS idx_reflection_created_at;
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

    /// Runs migration 23's actual SQL against a post-migration-17
    /// `wellness_check` (INTEGER booleans) seeded with real rows, to confirm
    /// the rebuild casts existing 1/0 integers to the '1'/'0' text form
    /// `crypto.rs`'s encryption migration expects, without losing any other
    /// column or the AUTOINCREMENT sequence.
    #[tokio::test]
    async fn migration_23_changes_wellness_check_booleans_to_text() {
        let pool = sqlx::SqlitePool::connect("sqlite::memory:").await.unwrap();

        // Post-migration-17 shape (INTEGER booleans, slot_start_at already in
        // place).
        sqlx::query(
            "CREATE TABLE wellness_check (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                slot_start_at TEXT NOT NULL DEFAULT '',
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
        sqlx::query(
            "INSERT INTO wellness_check (id, slot_start_at, relaxed_eyes, exercise, drank_water, washroom, created_at)
             VALUES (5, '2026-01-01T00:00:00.000Z', 0, 1, 0, 1, '2026-01-01T00:05:00.000Z')",
        )
        .execute(&pool)
        .await
        .unwrap();

        sqlx::raw_sql(&migration_sql(23)).execute(&pool).await.unwrap();

        let columns = sqlx::query("PRAGMA table_info(wellness_check)").fetch_all(&pool).await.unwrap();
        for column in ["relaxed_eyes", "exercise", "drank_water", "washroom"] {
            let decltype: String =
                columns.iter().find(|r| r.get::<String, _>("name") == column).unwrap().get("type");
            assert_eq!(decltype, "TEXT", "{column} should now be TEXT-typed");
        }

        let row = sqlx::query(
            "SELECT id, slot_start_at, relaxed_eyes, exercise, drank_water, washroom, created_at FROM wellness_check WHERE id = 5",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(row.get::<String, _>("slot_start_at"), "2026-01-01T00:00:00.000Z");
        assert_eq!(row.get::<String, _>("relaxed_eyes"), "0");
        assert_eq!(row.get::<String, _>("exercise"), "1");
        assert_eq!(row.get::<String, _>("drank_water"), "0");
        assert_eq!(row.get::<String, _>("washroom"), "1");
        assert_eq!(row.get::<String, _>("created_at"), "2026-01-01T00:05:00.000Z");

        // A fresh insert continues the AUTOINCREMENT sequence past the max
        // explicit id (5) rather than colliding with or reusing it.
        let result =
            sqlx::query("INSERT INTO wellness_check (slot_start_at, created_at) VALUES ('x', 'y')")
                .execute(&pool)
                .await
                .unwrap();
        assert!(result.last_insert_rowid() > 5);
    }

    /// Runs migration 24's actual SQL against a post-migration-16
    /// `screen_time_session` (both original indexes in place, plaintext
    /// app_id) to confirm: app_id_hash is added and defaults to '' for
    /// existing rows (what crypto.rs's screen-time backfill selects on),
    /// the dedupe index is rebuilt to key on app_id_hash instead of app_id,
    /// and the now-useless plain app_id index is gone.
    #[tokio::test]
    async fn migration_24_adds_app_id_hash_and_rebuilds_indexes() {
        let pool = sqlx::SqlitePool::connect("sqlite::memory:").await.unwrap();

        // Post-migration-16 shape.
        sqlx::query(
            "CREATE TABLE screen_time_session (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                app_id TEXT NOT NULL,
                display_name TEXT NOT NULL DEFAULT '',
                platform TEXT NOT NULL,
                device_name TEXT NOT NULL DEFAULT '',
                started_at TEXT NOT NULL,
                ended_at TEXT NOT NULL
            )",
        )
        .execute(&pool)
        .await
        .unwrap();
        sqlx::query("CREATE INDEX idx_screen_time_session_started_at ON screen_time_session(started_at)")
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query("CREATE INDEX idx_screen_time_session_app_id ON screen_time_session(app_id)")
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query(
            "CREATE INDEX idx_screen_time_session_dedupe
                 ON screen_time_session(app_id, platform, device_name, started_at, ended_at)",
        )
        .execute(&pool)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO screen_time_session (id, app_id, display_name, platform, device_name, started_at, ended_at)
             VALUES (1, 'chrome.exe', 'Google Chrome', 'windows', 'desktop', '2026-01-01T00:00:00.000Z', '2026-01-01T00:05:00.000Z')",
        )
        .execute(&pool)
        .await
        .unwrap();

        sqlx::raw_sql(&migration_sql(24)).execute(&pool).await.unwrap();

        let columns = sqlx::query("PRAGMA table_info(screen_time_session)").fetch_all(&pool).await.unwrap();
        assert!(columns.iter().any(|r| r.get::<String, _>("name") == "app_id_hash"), "app_id_hash column should exist");

        let hash: String =
            sqlx::query_scalar("SELECT app_id_hash FROM screen_time_session WHERE id = 1").fetch_one(&pool).await.unwrap();
        assert_eq!(hash, "", "pre-existing rows should default to an empty hash for the backfill to pick up");

        let indexes: Vec<String> = sqlx::query_scalar("SELECT name FROM sqlite_master WHERE type = 'index'")
            .fetch_all(&pool)
            .await
            .unwrap();
        assert!(!indexes.contains(&"idx_screen_time_session_app_id".to_string()), "plain app_id index should be dropped");
        assert!(indexes.contains(&"idx_screen_time_session_dedupe".to_string()), "dedupe index should still exist");

        let dedupe_sql: String =
            sqlx::query_scalar("SELECT sql FROM sqlite_master WHERE name = 'idx_screen_time_session_dedupe'")
                .fetch_one(&pool)
                .await
                .unwrap();
        assert!(dedupe_sql.contains("app_id_hash"), "dedupe index should key on app_id_hash now: {dedupe_sql}");
        assert!(!dedupe_sql.contains("(app_id,"), "dedupe index should no longer key on plaintext app_id: {dedupe_sql}");
    }

    /// Minimal pre-migration-28 shapes for the six synced tables. Only the
    /// columns migration 28 or its triggers actually touch are modelled --
    /// this is a trigger/rev test, not a full schema reproduction.
    async fn pre_migration_28_pool() -> sqlx::SqlitePool {
        let pool = sqlx::SqlitePool::connect("sqlite::memory:").await.unwrap();
        for ddl in [
            "CREATE TABLE reflection (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                created_at TEXT NOT NULL,
                slot_start_at TEXT NOT NULL,
                text TEXT NOT NULL,
                updated_at TEXT
            )",
            "CREATE INDEX idx_reflection_created_at ON reflection(created_at)",
            "CREATE TABLE daily_task_list (date TEXT PRIMARY KEY, content TEXT NOT NULL DEFAULT '', updated_at TEXT NOT NULL)",
            "CREATE TABLE not_to_do_list (date TEXT PRIMARY KEY, content TEXT NOT NULL DEFAULT '', updated_at TEXT NOT NULL)",
            "CREATE TABLE wellness_check (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                slot_start_at TEXT NOT NULL DEFAULT '',
                relaxed_eyes TEXT NOT NULL DEFAULT '1',
                exercise TEXT NOT NULL DEFAULT '1',
                drank_water TEXT NOT NULL DEFAULT '1',
                washroom TEXT NOT NULL DEFAULT '0',
                created_at TEXT NOT NULL
            )",
            "CREATE TABLE screen_time_session (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                app_id TEXT NOT NULL,
                display_name TEXT NOT NULL DEFAULT '',
                app_id_hash TEXT NOT NULL DEFAULT '',
                platform TEXT NOT NULL,
                device_name TEXT NOT NULL DEFAULT '',
                started_at TEXT NOT NULL,
                ended_at TEXT NOT NULL
            )",
            "CREATE TABLE bulk_edit_preset (
                id TEXT PRIMARY KEY,
                name TEXT NOT NULL,
                start_time TEXT NOT NULL,
                end_time TEXT NOT NULL,
                text TEXT NOT NULL,
                created_at TEXT NOT NULL,
                updated_at TEXT NOT NULL
            )",
        ] {
            sqlx::query(ddl).execute(&pool).await.unwrap();
        }
        pool
    }

    /// Migration 28 seeds `rev` from rowid *before* creating the triggers, so
    /// the backfill must not fire them -- and every row must still come out
    /// with a distinct, non-zero rev for the P2P cursor to be able to order
    /// them.
    #[tokio::test]
    async fn migration_28_backfills_distinct_revs_without_firing_triggers() {
        let pool = pre_migration_28_pool().await;
        for slot in ["2026-01-01T09:00:00.000Z", "2026-01-01T09:30:00.000Z", "2026-01-01T10:00:00.000Z"] {
            sqlx::query("INSERT INTO reflection (created_at, slot_start_at, text) VALUES (?, ?, 'x')")
                .bind(slot)
                .bind(slot)
                .execute(&pool)
                .await
                .unwrap();
        }

        sqlx::raw_sql(&migration_sql(28)).execute(&pool).await.unwrap();

        let revs: Vec<i64> = sqlx::query_scalar("SELECT rev FROM reflection ORDER BY id").fetch_all(&pool).await.unwrap();
        assert_eq!(revs, vec![1, 2, 3], "backfill should seed rev from rowid, untouched by the triggers");

        let indexes: Vec<String> = sqlx::query_scalar("SELECT name FROM sqlite_master WHERE type = 'index'")
            .fetch_all(&pool)
            .await
            .unwrap();
        assert!(
            !indexes.contains(&"idx_reflection_created_at".to_string()),
            "created_at is ciphertext now, so its index should be dropped"
        );

        let tables: Vec<String> = sqlx::query_scalar("SELECT name FROM sqlite_master WHERE type = 'table'")
            .fetch_all(&pool)
            .await
            .unwrap();
        assert!(tables.contains(&"sync_cursor".to_string()), "sync_cursor should exist");
    }

    /// The triggers are what make `rev` usable as a delta cursor at all: every
    /// write has to advance it without any write path having to remember to.
    #[tokio::test]
    async fn migration_28_triggers_bump_rev_on_insert_and_update() {
        let pool = pre_migration_28_pool().await;
        sqlx::raw_sql(&migration_sql(28)).execute(&pool).await.unwrap();

        sqlx::query("INSERT INTO reflection (created_at, slot_start_at, text) VALUES ('a', 'a', 'first')")
            .execute(&pool)
            .await
            .unwrap();
        let rev: i64 = sqlx::query_scalar("SELECT rev FROM reflection WHERE id = 1").fetch_one(&pool).await.unwrap();
        assert_eq!(rev, 1, "an insert into an empty table should land at rev 1, not the DEFAULT 0");

        sqlx::query("UPDATE reflection SET text = 'edited' WHERE id = 1").execute(&pool).await.unwrap();
        let bumped: i64 = sqlx::query_scalar("SELECT rev FROM reflection WHERE id = 1").fetch_one(&pool).await.unwrap();
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
    async fn migration_28_explicit_rev_write_is_not_overwritten() {
        let pool = pre_migration_28_pool().await;
        sqlx::raw_sql(&migration_sql(28)).execute(&pool).await.unwrap();

        sqlx::query("INSERT INTO reflection (created_at, slot_start_at, text) VALUES ('a', 'a', 'x')")
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query("UPDATE reflection SET text = 'y', rev = 99 WHERE id = 1").execute(&pool).await.unwrap();

        let rev: i64 = sqlx::query_scalar("SELECT rev FROM reflection WHERE id = 1").fetch_one(&pool).await.unwrap();
        assert_eq!(rev, 99, "an explicit rev write should win over the trigger");
    }

    /// Every synced table needs the same treatment -- a table that silently
    /// lacked the trigger would stop syncing edits entirely.
    #[tokio::test]
    async fn migration_28_covers_every_synced_table() {
        let pool = pre_migration_28_pool().await;
        sqlx::raw_sql(&migration_sql(28)).execute(&pool).await.unwrap();

        for table in
            ["reflection", "daily_task_list", "not_to_do_list", "wellness_check", "screen_time_session", "bulk_edit_preset"]
        {
            let columns = sqlx::query(&format!("PRAGMA table_info({table})")).fetch_all(&pool).await.unwrap();
            assert!(columns.iter().any(|r| r.get::<String, _>("name") == "rev"), "{table} should have a rev column");

            let triggers: Vec<String> =
                sqlx::query_scalar(&format!("SELECT name FROM sqlite_master WHERE type = 'trigger' AND tbl_name = '{table}'"))
                    .fetch_all(&pool)
                    .await
                    .unwrap();
            assert!(triggers.contains(&format!("trg_{table}_rev_insert")), "{table} missing its insert trigger");
            assert!(triggers.contains(&format!("trg_{table}_rev_update")), "{table} missing its update trigger");
        }
    }

    /// Both tables keyed by a TEXT primary key go through the same upsert
    /// path (`ON CONFLICT(date) DO UPDATE`), where the conflicting write lands
    /// as an UPDATE -- so the update trigger, not the insert one, is what has
    /// to advance rev.
    #[tokio::test]
    async fn migration_28_upsert_on_text_pk_bumps_rev() {
        let pool = pre_migration_28_pool().await;
        sqlx::raw_sql(&migration_sql(28)).execute(&pool).await.unwrap();

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
    async fn migration_28_rev_cursor_catches_an_edit_to_an_old_row() {
        let pool = pre_migration_28_pool().await;
        for i in 1..=3 {
            sqlx::query("INSERT INTO reflection (created_at, slot_start_at, text) VALUES (?, ?, 'x')")
                .bind(format!("2020-01-0{i}T00:00:00.000Z"))
                .bind(format!("2020-01-0{i}T00:00:00.000Z"))
                .execute(&pool)
                .await
                .unwrap();
        }
        sqlx::raw_sql(&migration_sql(28)).execute(&pool).await.unwrap();

        // First sync ships everything and parks the cursor at the high mark.
        let cursor: i64 = sqlx::query_scalar("SELECT COALESCE(MAX(rev), 0) FROM reflection").fetch_one(&pool).await.unwrap();
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
    async fn migration_28_triggers_do_not_perturb_rows_affected() {
        let pool = pre_migration_28_pool().await;
        sqlx::raw_sql(&migration_sql(28)).execute(&pool).await.unwrap();

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
        let fresh = sqlx::query(upsert).bind("2026-01-02T00:00:00.000Z").execute(&pool).await.unwrap();
        assert_eq!(fresh.rows_affected(), 1);
        let stale = sqlx::query(upsert).bind("2026-01-01T00:00:00.000Z").execute(&pool).await.unwrap();
        assert_eq!(stale.rows_affected(), 0, "a stale preset must still report 0 so it's counted as stale");
        let newer = sqlx::query(upsert).bind("2026-01-03T00:00:00.000Z").execute(&pool).await.unwrap();
        assert_eq!(newer.rows_affected(), 1, "a newer preset must still report 1");
    }
}
