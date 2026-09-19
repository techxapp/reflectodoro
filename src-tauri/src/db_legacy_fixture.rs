//! Frozen copy of the 28 incremental `tauri-plugin-sql` migrations this app
//! shipped before `db::FULL_SCHEMA_SQL` replaced them, kept ONLY as a test
//! fixture.
//!
//! `db::full_schema_shape_matches_legacy_migration_chain` applies these in
//! order to an in-memory database and diffs the resulting structure against
//! what `FULL_SCHEMA_SQL` produces. That diff is the actual proof that the
//! squash is faithful -- i.e. that a real user's database, built by running
//! all 28 of these, still satisfies `db::verify_final_shape` and so keeps
//! working untouched. Nothing here ships: the whole module is `cfg(test)`.
//!
//! Do not edit these strings. They are a historical record of what existing
//! databases were actually built from, not live code -- editing one would
//! only make the fixture describe a database that never existed. Safe to
//! delete this file (and its test) once no supported upgrade path starts
//! from a pre-squash database anymore.

#![cfg(test)]

use tauri_plugin_sql::{Migration, MigrationKind};

/// The exact migration list `db::migrations()` returned at v0.13.10.
pub fn legacy_migrations() -> Vec<Migration> {
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
