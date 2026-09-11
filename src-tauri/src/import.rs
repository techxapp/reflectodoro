//! Backs Settings' "Data" import (`invoke("import_data", ...)` from
//! `importData()` in `src/lib/db.ts`). Runs the whole operation -- replace-mode
//! wipes, the reflection line-merge algorithm below, the wellness_check and
//! screen_time_session dedupe/collapse logic, and the remaining table
//! upserts/inserts -- inside one real `sqlx` transaction on a dedicated
//! connection (`db::open_direct_pool`), fixing the non-atomicity `db.ts`'s
//! old `importData` doc comment used to flag: tauri-plugin-sql's pooled
//! `execute()` calls aren't guaranteed to land on the same SQLite connection,
//! so manual `BEGIN`/`COMMIT` sent through it isn't reliable.
//! `parseAndValidateExport` (db.ts) still runs first and fully validates the
//! payload client-side -- this module trusts its input.
use std::collections::HashMap;

use serde::{Deserialize, Serialize};
use sqlx::{Row, Sqlite, Transaction};
use tauri::AppHandle;

use crate::db;

// Serialize (as well as Deserialize) on the five row types below: p2p_sync.rs
// reuses these exact shapes to build its own delta payload (never including
// ImportSettingRow/app_setting -- P2P sync deliberately never touches
// settings, see CLAUDE.md's P2P LAN sync section), so both the file-based
// import and the LAN sync path share one wire format for these tables.

#[derive(Deserialize, Serialize, Clone)]
pub struct ImportReflectionRow {
    pub created_at: String,
    pub slot_start_at: String,
    pub text: String,
    /// Optional so older export files (written before this column existed)
    /// still deserialize -- `import_reflections` falls back to `created_at`
    /// per row when absent, same as migration 19's backfill for pre-existing
    /// rows. P2P sync (p2p_sync.rs) always sends a real value.
    #[serde(default)]
    pub updated_at: Option<String>,
}

#[derive(Deserialize, Serialize, Clone)]
pub struct ImportWellnessCheckRow {
    pub slot_start_at: String,
    pub relaxed_eyes: i64,
    pub exercise: i64,
    pub drank_water: i64,
    pub washroom: i64,
    pub created_at: String,
}

#[derive(Deserialize, Serialize, Clone)]
pub struct ImportTaskListRow {
    pub date: String,
    pub content: String,
    pub updated_at: String,
}

#[derive(Deserialize, Serialize, Clone)]
pub struct ImportNotToDoRow {
    pub date: String,
    pub content: String,
    pub updated_at: String,
}

#[derive(Deserialize)]
pub struct ImportSettingRow {
    pub key: String,
    pub value: String,
}

#[derive(Deserialize, Serialize, Clone)]
pub struct ImportScreenTimeSessionRow {
    pub app_id: String,
    pub display_name: String,
    pub platform: String,
    pub device_name: String,
    pub started_at: String,
    pub ended_at: String,
}

#[derive(Deserialize)]
pub struct ImportData {
    pub reflection: Vec<ImportReflectionRow>,
    pub daily_task_list: Vec<ImportTaskListRow>,
    pub not_to_do_list: Vec<ImportNotToDoRow>,
    pub app_setting: Vec<ImportSettingRow>,
    pub wellness_check: Vec<ImportWellnessCheckRow>,
    pub screen_time_session: Vec<ImportScreenTimeSessionRow>,
}

#[derive(Deserialize, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum ImportMode {
    Replace,
    Merge,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ImportResult {
    pub reflection_count: usize,
    pub task_list_count: usize,
    pub not_to_do_list_count: usize,
    pub setting_count: usize,
    pub wellness_check_count: usize,
    pub screen_time_session_count: usize,
    /// Number of distinct `slot_start_at` groups where more than one
    /// physical reflection row (some mix of pre-existing duplicates and/or
    /// multiple imported rows for that slot) collapsed into one surviving
    /// row. Zero on a clean import with no slot conflicts.
    pub merged_slot_count: usize,
    /// Number of imported `screen_time_session` rows skipped because a row
    /// with the same `(app_id, platform, device_name, started_at, ended_at)`
    /// already existed. Always zero in "replace" mode (the table was just
    /// wiped, so nothing to collide with).
    pub screen_time_duplicate_count: usize,
    /// Number of imported `wellness_check` rows skipped because they lost
    /// the "keep the earliest created_at per slot_start_at" collapse to
    /// either an existing row or another imported row -- see
    /// `import_wellness_checks`. Always zero in "replace" mode.
    pub wellness_check_duplicate_count: usize,
}

// --- Pure line-merge algorithm (no DB access -- see #[cfg(test)] below) ---

/// Splits `text` on newlines, trims each line, and discards blank lines --
/// blank lines carry no content and would make "already present" comparisons
/// and the rejoined text ambiguous.
fn line_split_trim(text: &str) -> Vec<String> {
    text.lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .map(str::to_string)
        .collect()
}

/// Case-insensitive first-occurrence dedupe, preserving original casing and
/// order of the kept lines.
fn dedupe_first_seen_ci(lines: Vec<String>) -> Vec<String> {
    let mut seen = std::collections::HashSet::new();
    lines
        .into_iter()
        .filter(|l| seen.insert(l.to_lowercase()))
        .collect()
}

/// A whole line whose trimmed value is "skip", case-insensitively -- the
/// literal placeholder text the overlay's textarea hints at
/// ("type 'Skip' to skip it"), not a token recognized anywhere else in the
/// app. `lines` passed into this module are already trimmed by
/// `line_split_trim`, so this only needs to lowercase-compare.
fn is_skip_line(line: &str) -> bool {
    line.to_lowercase() == "skip"
}

/// True for an imported row that carries no real content at all -- either
/// genuinely empty/blank, or made up entirely of "skip" placeholder lines.
/// Such a row is dropped before slot-grouping (see `import_reflections`),
/// as if it were never in the imported file: no existing row gets touched
/// or gets a "Skip" line appended, and a brand-new slot whose only imported
/// row is skip/empty never gets created at all. `.all()` is vacuously true
/// for an empty line list, which is exactly what a blank/whitespace-only
/// `text` reduces to after `line_split_trim` discards blank lines.
fn is_ignorable_entry(text: &str) -> bool {
    line_split_trim(text).iter().all(|l| is_skip_line(l))
}

/// Rule 1 of the line-merge, shared by every table that gets a git-style
/// merge (`reflection`, and -- without the skip-specific rule 2 below --
/// `daily_task_list`/`not_to_do_list`): any incoming line not already
/// present (case-insensitively) in `baseline` gets appended at the end;
/// lines already present are skipped rather than duplicated.
///
/// `baseline` is expected to already be deduped (see `dedupe_first_seen_ci`);
/// `incoming` does not need to be pre-deduped -- repeated incoming lines
/// naturally collapse here since the first occurrence is already in the
/// accumulator by the time a repeat is checked.
fn append_unique_lines_ci(baseline: &[String], incoming: &[String]) -> Vec<String> {
    let mut seen: std::collections::HashSet<String> = baseline.iter().map(|l| l.to_lowercase()).collect();
    let mut result = baseline.to_vec();
    for line in incoming {
        if seen.insert(line.to_lowercase()) {
            result.push(line.clone());
        }
    }
    result
}

/// The user's two reflection-specific merge rules: (1) `append_unique_lines_ci`
/// above; (2) if the incoming set carries any real (non-"skip") content,
/// existing "skip" placeholder lines are dropped first -- real content
/// having since arrived means the placeholder no longer describes the slot.
/// Rule 2 does NOT fire when the incoming set is itself skip-only, so an
/// existing "Skip" line is left untouched in that case, and the incoming
/// "skip" line is then just evaluated by ordinary rule 1 (appended only if
/// not already present).
///
/// `existing` is expected to already be a deduped baseline (see
/// `dedupe_first_seen_ci`); `incoming` does not need to be pre-deduped, same
/// as `append_unique_lines_ci`.
fn merge_reflection_lines(existing: &[String], incoming: &[String]) -> Vec<String> {
    let has_non_skip_incoming = incoming.iter().any(|l| !is_skip_line(l));

    let baseline: Vec<String> = if has_non_skip_incoming {
        existing.iter().filter(|l| !is_skip_line(l)).cloned().collect()
    } else {
        existing.to_vec()
    };

    append_unique_lines_ci(&baseline, incoming)
}

// --- DB-backed reflection merge/insert ------------------------------------

/// Groups `rows` by `slot_start_at` (skipping any row that's skip/empty-only
/// per `is_ignorable_entry` -- it never reaches the merge algorithm at all,
/// so it can't touch an existing row or seed a new one), `sqlx`-scoped merge
/// algorithm above determines final text per slot, merge mode collapses any
/// pre-existing duplicate rows for that slot plus every (non-ignored)
/// imported row targeting it into one surviving row (deleting the others),
/// replace mode always inserts fresh since the table was just wiped. Returns
/// how many slot-groups actually collapsed more than one physical row.
/// `wellness_check` no longer references `reflection.id` at all (it keys off
/// `slot_start_at` directly -- see `import_wellness_checks`), so unlike the
/// old version of this function there's no id map to build or FK to repoint
/// before deleting a collapsed duplicate.
pub(crate) async fn import_reflections(
    tx: &mut Transaction<'_, Sqlite>,
    rows: &[ImportReflectionRow],
    mode: ImportMode,
) -> Result<usize, String> {
    let mut order: Vec<String> = Vec::new();
    let mut groups: HashMap<String, Vec<&ImportReflectionRow>> = HashMap::new();
    for row in rows {
        if is_ignorable_entry(&row.text) {
            continue;
        }
        groups
            .entry(row.slot_start_at.clone())
            .or_insert_with(|| {
                order.push(row.slot_start_at.clone());
                Vec::new()
            })
            .push(row);
    }

    let mut merged_slot_count = 0usize;

    for slot in order {
        let group = &groups[&slot];

        let existing_rows: Vec<(i64, String)> = if mode == ImportMode::Merge {
            let db_rows = sqlx::query("SELECT id, text FROM reflection WHERE slot_start_at = ? ORDER BY id ASC")
                .bind(&slot)
                .fetch_all(&mut **tx)
                .await
                .map_err(|e| e.to_string())?;
            let mut out = Vec::with_capacity(db_rows.len());
            for row in &db_rows {
                let id: i64 = row.try_get("id").map_err(|e| e.to_string())?;
                let text: String = row.try_get("text").map_err(|e| e.to_string())?;
                out.push((id, text));
            }
            out
        } else {
            Vec::new()
        };

        let mut existing_lines_raw: Vec<String> = Vec::new();
        for (_, text) in &existing_rows {
            existing_lines_raw.extend(line_split_trim(text));
        }
        let existing_baseline = dedupe_first_seen_ci(existing_lines_raw);

        let mut incoming_lines: Vec<String> = Vec::new();
        for row in group.iter() {
            incoming_lines.extend(line_split_trim(&row.text));
        }

        let final_text = merge_reflection_lines(&existing_baseline, &incoming_lines).join("\n");

        // The delta-sync cursor (p2p_sync.rs): the latest of every incoming
        // row's updated_at (falling back to its created_at when a legacy
        // export omits it) for this slot -- a plain string max is safe, same
        // ISO/UTC format as everywhere else in this app.
        let final_updated_at = group
            .iter()
            .map(|r| r.updated_at.as_deref().unwrap_or(r.created_at.as_str()))
            .max()
            .unwrap_or_default()
            .to_string();

        if existing_rows.len() + group.len() > 1 {
            merged_slot_count += 1;
        }

        if let Some((first_id, _)) = existing_rows.first() {
            let survivor_id = *first_id;
            sqlx::query("UPDATE reflection SET text = ?, updated_at = ? WHERE id = ?")
                .bind(&final_text)
                .bind(&final_updated_at)
                .bind(survivor_id)
                .execute(&mut **tx)
                .await
                .map_err(|e| e.to_string())?;

            let other_ids: Vec<i64> = existing_rows[1..].iter().map(|(id, _)| *id).collect();
            if !other_ids.is_empty() {
                let placeholders = other_ids.iter().map(|_| "?").collect::<Vec<_>>().join(", ");

                // Query string is dynamic (variable-length IN-list), so it
                // needs its own named binding -- `sqlx::query(&format!(...))`
                // inline would borrow a temporary `String` that gets dropped
                // before the later `.bind()`/`.execute()` calls that need it.
                let delete_sql = format!("DELETE FROM reflection WHERE id IN ({placeholders})");
                let mut delete_dupes = sqlx::query(&delete_sql);
                for id in &other_ids {
                    delete_dupes = delete_dupes.bind(id);
                }
                delete_dupes.execute(&mut **tx).await.map_err(|e| e.to_string())?;
            }
        } else {
            // No existing row: brand-new slot. `created_at` is the earliest
            // among the imported rows collapsing into it (plain string min
            // is safe -- these are all `toISOString()` UTC strings).
            let created_at = group
                .iter()
                .map(|r| r.created_at.as_str())
                .min()
                .unwrap_or_default()
                .to_string();
            sqlx::query("INSERT INTO reflection (created_at, slot_start_at, text, updated_at) VALUES (?, ?, ?, ?)")
                .bind(&created_at)
                .bind(&slot)
                .bind(&final_text)
                .bind(&final_updated_at)
                .execute(&mut **tx)
                .await
                .map_err(|e| e.to_string())?;
        }
    }

    Ok(merged_slot_count)
}

/// Appends every row fresh (own autoincrement id, the file's is never
/// reused, nothing references their ids so no id remapping is needed --
/// same as db.ts's old TS loop), except this dedupes: a row whose
/// `(app_id, platform, device_name, started_at, ended_at)` already exists
/// is skipped rather than duplicated (`display_name` deliberately excluded
/// from the key -- it's a display-only label, not part of a session's
/// identity, per CLAUDE.md's Screen time tracking section). Plain
/// `INSERT ... WHERE NOT EXISTS` rather than a `UNIQUE` constraint + `INSERT
/// OR IGNORE`: a `UNIQUE` index would need a backfill migration to dedupe
/// any rows a pre-fix double-import already wrote, and would also start
/// constraining `screen_time.rs`'s normal capture-write path, which this
/// dedupe has no reason to touch. `idx_screen_time_session_dedupe` (db.rs
/// migration 16) is what keeps this `NOT EXISTS` check fast on a table
/// documented as likely to become the largest by row count. One statement
/// per row rather than db.ts's chunked multi-row INSERT: that chunking
/// amortized tauri-plugin-sql's per-call IPC overhead, which doesn't exist
/// here (this runs entirely inside one Rust process/connection). Returns how
/// many rows were skipped as duplicates.
pub(crate) async fn import_screen_time_sessions(
    tx: &mut Transaction<'_, Sqlite>,
    rows: &[ImportScreenTimeSessionRow],
) -> Result<usize, String> {
    let mut duplicate_count = 0usize;
    for row in rows {
        let result = sqlx::query(
            "INSERT INTO screen_time_session (app_id, display_name, platform, device_name, started_at, ended_at)
             SELECT ?, ?, ?, ?, ?, ?
             WHERE NOT EXISTS (
                 SELECT 1 FROM screen_time_session
                 WHERE app_id = ? AND platform = ? AND device_name = ? AND started_at = ? AND ended_at = ?
             )",
        )
        .bind(&row.app_id)
        .bind(&row.display_name)
        .bind(&row.platform)
        .bind(&row.device_name)
        .bind(&row.started_at)
        .bind(&row.ended_at)
        .bind(&row.app_id)
        .bind(&row.platform)
        .bind(&row.device_name)
        .bind(&row.started_at)
        .bind(&row.ended_at)
        .execute(&mut **tx)
        .await
        .map_err(|e| e.to_string())?;
        if result.rows_affected() == 0 {
            duplicate_count += 1;
        }
    }
    Ok(duplicate_count)
}

/// Groups `rows` by `slot_start_at` (merge mode only -- replace mode skips
/// the existing-row query since the table was just wiped) and, per slot,
/// keeps exactly one row: whichever of {existing rows} ∪ {incoming rows} has
/// the earliest `created_at` (plain string comparison -- these are all
/// `toISOString()` UTC strings). Ties favor the existing row, avoiding a
/// pointless delete+insert when it's genuinely the same event. Any other
/// existing row for that slot is deleted (cleans up duplicates a pre-fix
/// double-import already wrote, same spirit as `import_reflections`'s
/// collapse), and every other incoming row for that slot is skipped.
///
/// Like `import_reflections`, this only ever looks at slots that actually
/// appear in `rows` -- a slot with pre-existing duplicate rows but nothing
/// incoming for it is never visited, so its duplicates aren't cleaned up by
/// this pass. That only matters for the rare case a previous double-import
/// left duplicates on a slot the current import file doesn't touch at all.
///
/// Unlike `import_reflections`, there's no FK to repoint here at all --
/// `wellness_check.slot_start_at` (migration 17) replaced
/// `wellness_check.reflection_id` specifically so this dedupe wouldn't need
/// to remap anything through an id map. Returns how many incoming rows were
/// skipped (lost the collapse to something else for their slot).
pub(crate) async fn import_wellness_checks(
    tx: &mut Transaction<'_, Sqlite>,
    rows: &[ImportWellnessCheckRow],
    mode: ImportMode,
) -> Result<usize, String> {
    let mut order: Vec<String> = Vec::new();
    let mut groups: HashMap<String, Vec<&ImportWellnessCheckRow>> = HashMap::new();
    for row in rows {
        groups
            .entry(row.slot_start_at.clone())
            .or_insert_with(|| {
                order.push(row.slot_start_at.clone());
                Vec::new()
            })
            .push(row);
    }

    let mut duplicate_count = 0usize;

    for slot in order {
        let group = &groups[&slot];

        let existing_rows: Vec<(i64, String)> = if mode == ImportMode::Merge {
            let db_rows = sqlx::query(
                "SELECT id, created_at FROM wellness_check WHERE slot_start_at = ? ORDER BY created_at ASC, id ASC",
            )
            .bind(&slot)
            .fetch_all(&mut **tx)
            .await
            .map_err(|e| e.to_string())?;
            let mut out = Vec::with_capacity(db_rows.len());
            for row in &db_rows {
                let id: i64 = row.try_get("id").map_err(|e| e.to_string())?;
                let created_at: String = row.try_get("created_at").map_err(|e| e.to_string())?;
                out.push((id, created_at));
            }
            out
        } else {
            Vec::new()
        };

        // The earliest incoming row for this slot -- the only one that can
        // possibly win, since anything else in `group` is by definition not
        // the minimum among incoming rows and would lose to it regardless of
        // what the existing side looks like.
        let incoming_survivor = group.iter().min_by(|a, b| a.created_at.cmp(&b.created_at)).unwrap();

        let existing_survivor = existing_rows.first();

        // Ties favor the existing row (`<`, not `<=`): equal timestamps are
        // treated as the same event, so there's no reason to delete+reinsert.
        let incoming_wins = match existing_survivor {
            Some((_, existing_created_at)) => incoming_survivor.created_at < *existing_created_at,
            None => true,
        };

        if incoming_wins {
            sqlx::query(
                "INSERT INTO wellness_check (slot_start_at, relaxed_eyes, exercise, drank_water, washroom, created_at)
                 VALUES (?, ?, ?, ?, ?, ?)",
            )
            .bind(&slot)
            .bind(incoming_survivor.relaxed_eyes)
            .bind(incoming_survivor.exercise)
            .bind(incoming_survivor.drank_water)
            .bind(incoming_survivor.washroom)
            .bind(&incoming_survivor.created_at)
            .execute(&mut **tx)
            .await
            .map_err(|e| e.to_string())?;

            // The incoming survivor won outright, so every existing row for
            // this slot (if any) is superseded.
            let existing_ids: Vec<i64> = existing_rows.iter().map(|(id, _)| *id).collect();
            if !existing_ids.is_empty() {
                let placeholders = existing_ids.iter().map(|_| "?").collect::<Vec<_>>().join(", ");
                let delete_sql = format!("DELETE FROM wellness_check WHERE id IN ({placeholders})");
                let mut delete_losers = sqlx::query(&delete_sql);
                for id in &existing_ids {
                    delete_losers = delete_losers.bind(id);
                }
                delete_losers.execute(&mut **tx).await.map_err(|e| e.to_string())?;
            }

            duplicate_count += group.len() - 1;
        } else {
            // An existing row already covers this slot and is at least as
            // old as anything incoming -- delete any *other* pre-existing
            // duplicates, but insert nothing.
            let other_existing_ids: Vec<i64> = existing_rows[1..].iter().map(|(id, _)| *id).collect();
            if !other_existing_ids.is_empty() {
                let placeholders = other_existing_ids.iter().map(|_| "?").collect::<Vec<_>>().join(", ");
                let delete_sql = format!("DELETE FROM wellness_check WHERE id IN ({placeholders})");
                let mut delete_dupes = sqlx::query(&delete_sql);
                for id in &other_existing_ids {
                    delete_dupes = delete_dupes.bind(id);
                }
                delete_dupes.execute(&mut **tx).await.map_err(|e| e.to_string())?;
            }

            duplicate_count += group.len();
        }
    }

    Ok(duplicate_count)
}

/// Shared by `daily_task_list` and `not_to_do_list` -- both are keyed by a
/// real `PRIMARY KEY` (`date`), so unlike `reflection`/`wellness_check`
/// there's no duplicate-row collapsing to do here, just a per-day upsert.
///
/// In merge mode, a day that already has content gets it git-style
/// line-merged with the imported day's content via `append_unique_lines_ci`
/// (existing lines kept, new incoming lines appended -- no "skip" handling,
/// that convention is specific to the break overlay's reflection textarea)
/// rather than the import blindly overwriting it. A day with no existing
/// row is inserted fresh either way -- same as a brand-new reflection slot.
///
/// `table` is always one of the two literal table names this module's own
/// call sites pass (never external/user input), so interpolating it
/// directly into the SQL string is safe here -- sqlx has no way to bind an
/// identifier as a query parameter.
pub(crate) async fn merge_import_day_rows(
    tx: &mut Transaction<'_, Sqlite>,
    table: &str,
    rows: &[(&str, &str, &str)],
    mode: ImportMode,
) -> Result<(), String> {
    for &(date, content, updated_at) in rows {
        let merged_content = if mode == ImportMode::Merge {
            let existing: Option<String> =
                sqlx::query_scalar(&format!("SELECT content FROM {table} WHERE date = ?"))
                    .bind(date)
                    .fetch_optional(&mut **tx)
                    .await
                    .map_err(|e| e.to_string())?;
            match existing {
                Some(existing_content) => {
                    let baseline = dedupe_first_seen_ci(line_split_trim(&existing_content));
                    let incoming_lines = line_split_trim(content);
                    append_unique_lines_ci(&baseline, &incoming_lines).join("\n")
                }
                None => content.to_string(),
            }
        } else {
            content.to_string()
        };

        let sql = if mode == ImportMode::Merge {
            format!(
                "INSERT INTO {table} (date, content, updated_at) VALUES (?, ?, ?)
                 ON CONFLICT(date) DO UPDATE SET content = excluded.content, updated_at = excluded.updated_at"
            )
        } else {
            format!("INSERT INTO {table} (date, content, updated_at) VALUES (?, ?, ?)")
        };
        sqlx::query(&sql)
            .bind(date)
            .bind(&merged_content)
            .bind(updated_at)
            .execute(&mut **tx)
            .await
            .map_err(|e| e.to_string())?;
    }
    Ok(())
}

#[tauri::command]
pub async fn import_data(app: AppHandle, data: ImportData, mode: ImportMode, include_settings: bool) -> Result<ImportResult, String> {
    let pool = db::open_direct_pool(&app).await?;
    let mut tx = pool.begin().await.map_err(|e| format!("failed to start import transaction: {e}"))?;

    if mode == ImportMode::Replace {
        // Order no longer matters for FK reasons (wellness_check.slot_start_at,
        // migration 17, replaced the reflection_id FK) -- kept in the same
        // order as before purely because there's no reason to change it.
        sqlx::query("DELETE FROM wellness_check").execute(&mut *tx).await.map_err(|e| e.to_string())?;
        sqlx::query("DELETE FROM reflection").execute(&mut *tx).await.map_err(|e| e.to_string())?;
        sqlx::query("DELETE FROM daily_task_list").execute(&mut *tx).await.map_err(|e| e.to_string())?;
        sqlx::query("DELETE FROM not_to_do_list").execute(&mut *tx).await.map_err(|e| e.to_string())?;
        sqlx::query("DELETE FROM screen_time_session").execute(&mut *tx).await.map_err(|e| e.to_string())?;
        if include_settings {
            sqlx::query("DELETE FROM app_setting").execute(&mut *tx).await.map_err(|e| e.to_string())?;
        }
    }

    let merged_slot_count = import_reflections(&mut tx, &data.reflection, mode).await?;
    let wellness_check_duplicate_count = import_wellness_checks(&mut tx, &data.wellness_check, mode).await?;

    let daily_task_rows: Vec<(&str, &str, &str)> = data
        .daily_task_list
        .iter()
        .map(|r| (r.date.as_str(), r.content.as_str(), r.updated_at.as_str()))
        .collect();
    merge_import_day_rows(&mut tx, "daily_task_list", &daily_task_rows, mode).await?;

    let not_to_do_rows: Vec<(&str, &str, &str)> = data
        .not_to_do_list
        .iter()
        .map(|r| (r.date.as_str(), r.content.as_str(), r.updated_at.as_str()))
        .collect();
    merge_import_day_rows(&mut tx, "not_to_do_list", &not_to_do_rows, mode).await?;

    let screen_time_duplicate_count = import_screen_time_sessions(&mut tx, &data.screen_time_session).await?;

    if include_settings {
        for row in &data.app_setting {
            let sql = if mode == ImportMode::Merge {
                "INSERT INTO app_setting (key, value) VALUES (?, ?)
                 ON CONFLICT(key) DO UPDATE SET value = excluded.value"
            } else {
                "INSERT INTO app_setting (key, value) VALUES (?, ?)"
            };
            sqlx::query(sql)
                .bind(&row.key)
                .bind(&row.value)
                .execute(&mut *tx)
                .await
                .map_err(|e| e.to_string())?;
        }
    }

    tx.commit().await.map_err(|e| format!("failed to commit import transaction: {e}"))?;

    Ok(ImportResult {
        reflection_count: data.reflection.len(),
        task_list_count: data.daily_task_list.len(),
        not_to_do_list_count: data.not_to_do_list.len(),
        setting_count: if include_settings { data.app_setting.len() } else { 0 },
        wellness_check_count: data.wellness_check.len(),
        screen_time_session_count: data.screen_time_session.len(),
        merged_slot_count,
        screen_time_duplicate_count,
        wellness_check_duplicate_count,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lines(strs: &[&str]) -> Vec<String> {
        strs.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn fresh_lines_with_no_existing_row() {
        let result = merge_reflection_lines(&[], &lines(&["A", "B"]));
        assert_eq!(result, lines(&["A", "B"]));
    }

    #[test]
    fn case_insensitive_dedupe_against_existing() {
        let existing = lines(&["A", "B"]);
        let result = merge_reflection_lines(&existing, &lines(&["a", "B"]));
        assert_eq!(result, lines(&["A", "B"]));
    }

    #[test]
    fn skip_line_removed_when_real_content_arrives() {
        let existing = lines(&["Skip"]);
        let result = merge_reflection_lines(&existing, &lines(&["Went for a walk"]));
        assert_eq!(result, lines(&["Went for a walk"]));
    }

    // Characterizes `merge_reflection_lines` in isolation -- an all-skip
    // `incoming` is no longer reachable through the real `import_reflections`
    // path, since `is_ignorable_entry` now drops a skip/empty-only row
    // before it's ever handed to this function (see
    // `empty_or_skip_only_incoming_leaves_existing_row_untouched` below for
    // the actual end-to-end behavior: the existing row is left untouched,
    // not appended to).
    #[test]
    fn existing_skip_untouched_and_incoming_skip_appended_when_not_present() {
        let existing = lines(&["Did yoga"]);
        let result = merge_reflection_lines(&existing, &lines(&["Skip"]));
        assert_eq!(result, lines(&["Did yoga", "Skip"]));
    }

    #[test]
    fn existing_skip_left_alone_when_incoming_is_skip_only_and_already_present() {
        let existing = lines(&["Skip"]);
        let result = merge_reflection_lines(&existing, &lines(&["skip"]));
        assert_eq!(result, lines(&["Skip"]));
    }

    #[test]
    fn dedupe_across_duplicate_existing_rows() {
        let raw: Vec<String> = ["A", "A", "B"].iter().map(|s| s.to_string()).collect();
        assert_eq!(dedupe_first_seen_ci(raw), lines(&["A", "B"]));
    }

    #[test]
    fn multiple_imported_rows_collapse_into_one_new_slot() {
        let incoming: Vec<String> = ["A", "A", "C"].iter().map(|s| s.to_string()).collect();
        let result = merge_reflection_lines(&[], &incoming);
        assert_eq!(result, lines(&["A", "C"]));
    }

    #[test]
    fn blank_lines_are_discarded() {
        assert_eq!(line_split_trim("A\n\n  \nB\n"), lines(&["A", "B"]));
    }

    #[test]
    fn entries_that_are_only_skip_or_empty_are_ignorable() {
        assert!(is_ignorable_entry(""));
        assert!(is_ignorable_entry("   \n  \n"));
        assert!(is_ignorable_entry("Skip"));
        assert!(is_ignorable_entry("skip\n\nSKIP"));
        assert!(!is_ignorable_entry("Skip\nWent for a walk"));
        assert!(!is_ignorable_entry("Went for a walk"));
    }

    #[test]
    fn skip_check_is_case_insensitive() {
        assert!(is_skip_line("Skip"));
        assert!(is_skip_line("  SKIP  ".trim()));
        assert!(!is_skip_line("Skipped lunch"));
    }

    // --- DB-backed integration test -------------------------------------
    //
    // The unit tests above only exercise the pure `merge_reflection_lines`
    // algorithm. This one drives `import_reflections` against a real
    // in-memory SQLite DB to confirm the surrounding DB logic -- duplicate
    // pre-existing rows collapsing into the earliest-id survivor -- actually
    // holds together, not just the line merge.

    async fn test_pool() -> sqlx::SqlitePool {
        let pool = sqlx::SqlitePool::connect("sqlite::memory:").await.unwrap();
        sqlx::query(
            "CREATE TABLE reflection (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                created_at TEXT NOT NULL,
                slot_start_at TEXT NOT NULL,
                text TEXT NOT NULL,
                updated_at TEXT
            )",
        )
        .execute(&pool)
        .await
        .unwrap();
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
        sqlx::query(
            "CREATE TABLE daily_task_list (
                date TEXT PRIMARY KEY,
                content TEXT NOT NULL DEFAULT '',
                updated_at TEXT NOT NULL
            )",
        )
        .execute(&pool)
        .await
        .unwrap();
        sqlx::query(
            "CREATE TABLE not_to_do_list (
                date TEXT PRIMARY KEY,
                content TEXT NOT NULL DEFAULT '',
                updated_at TEXT NOT NULL
            )",
        )
        .execute(&pool)
        .await
        .unwrap();
        pool
    }

    #[tokio::test]
    async fn collapses_duplicate_existing_rows_into_earliest_survivor() {
        let pool = test_pool().await;

        // Two pre-existing duplicate rows for the same slot -- id 1 (older,
        // "Skip") and id 2 (newer, real content).
        sqlx::query(
            "INSERT INTO reflection (id, created_at, slot_start_at, text)
             VALUES (1, '2026-01-01T00:00:00.000Z', '2026-01-01T00:00:00.000Z', 'Skip')",
        )
        .execute(&pool)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO reflection (id, created_at, slot_start_at, text)
             VALUES (2, '2026-01-01T00:05:00.000Z', '2026-01-01T00:00:00.000Z', 'Did laundry')",
        )
        .execute(&pool)
        .await
        .unwrap();

        let imported = vec![ImportReflectionRow {
            created_at: "2026-01-01T00:10:00.000Z".to_string(),
            slot_start_at: "2026-01-01T00:00:00.000Z".to_string(),
            text: "Went for a walk".to_string(),
            updated_at: None,
        }];

        let mut tx = pool.begin().await.unwrap();
        let merged_slot_count = import_reflections(&mut tx, &imported, ImportMode::Merge).await.unwrap();
        tx.commit().await.unwrap();

        assert_eq!(merged_slot_count, 1);

        let remaining = sqlx::query("SELECT id, text FROM reflection ORDER BY id")
            .fetch_all(&pool)
            .await
            .unwrap();
        assert_eq!(remaining.len(), 1, "duplicate row (id 2) should have been deleted");
        let id: i64 = remaining[0].try_get("id").unwrap();
        let text: String = remaining[0].try_get("text").unwrap();
        assert_eq!(id, 1, "the earliest existing id survives");
        // Rule 2 fired ("Went for a walk" is non-skip) so existing "Skip" was
        // dropped; "Did laundry" survived from the other duplicate row; the
        // incoming line was appended last.
        assert_eq!(text, "Did laundry\nWent for a walk");
    }

    #[tokio::test]
    async fn no_existing_row_is_a_plain_insert_with_no_merge() {
        let pool = test_pool().await;

        let imported = vec![ImportReflectionRow {
            created_at: "2026-02-01T00:00:00.000Z".to_string(),
            slot_start_at: "2026-02-01T00:00:00.000Z".to_string(),
            text: "Fresh slot".to_string(),
            updated_at: None,
        }];

        let mut tx = pool.begin().await.unwrap();
        let merged_slot_count = import_reflections(&mut tx, &imported, ImportMode::Merge).await.unwrap();
        tx.commit().await.unwrap();

        assert_eq!(merged_slot_count, 0, "a single fresh row with nothing to merge against shouldn't count as merged");

        let row = sqlx::query("SELECT created_at, text FROM reflection WHERE slot_start_at = ?")
            .bind("2026-02-01T00:00:00.000Z")
            .fetch_one(&pool)
            .await
            .unwrap();
        let created_at: String = row.try_get("created_at").unwrap();
        let text: String = row.try_get("text").unwrap();
        assert_eq!(created_at, "2026-02-01T00:00:00.000Z");
        assert_eq!(text, "Fresh slot");
    }

    #[tokio::test]
    async fn skip_only_row_for_a_new_slot_creates_nothing() {
        let pool = test_pool().await;

        let imported = vec![ImportReflectionRow {
            created_at: "2026-03-01T00:00:00.000Z".to_string(),
            slot_start_at: "2026-03-01T00:00:00.000Z".to_string(),
            text: "Skip".to_string(),
            updated_at: None,
        }];

        let mut tx = pool.begin().await.unwrap();
        let merged_slot_count = import_reflections(&mut tx, &imported, ImportMode::Merge).await.unwrap();
        tx.commit().await.unwrap();

        assert_eq!(merged_slot_count, 0);

        let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM reflection")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(count, 0, "a skip-only row for a brand-new slot must not create a row at all");
    }

    #[tokio::test]
    async fn empty_or_skip_only_incoming_leaves_existing_row_untouched() {
        let pool = test_pool().await;

        sqlx::query(
            "INSERT INTO reflection (id, created_at, slot_start_at, text)
             VALUES (1, '2026-01-01T00:00:00.000Z', '2026-01-01T00:00:00.000Z', 'Did yoga')",
        )
        .execute(&pool)
        .await
        .unwrap();

        let imported = vec![
            ImportReflectionRow {
                created_at: "2026-01-01T00:10:00.000Z".to_string(),
                slot_start_at: "2026-01-01T00:00:00.000Z".to_string(),
                text: "Skip".to_string(),
                updated_at: None,
            },
            ImportReflectionRow {
                created_at: "2026-01-01T00:10:00.000Z".to_string(),
                slot_start_at: "2026-01-01T00:00:00.000Z".to_string(),
                text: "   \n  ".to_string(),
                updated_at: None,
            },
        ];

        let mut tx = pool.begin().await.unwrap();
        let merged_slot_count = import_reflections(&mut tx, &imported, ImportMode::Merge).await.unwrap();
        tx.commit().await.unwrap();

        assert_eq!(merged_slot_count, 0);

        let row = sqlx::query("SELECT text FROM reflection WHERE id = 1").fetch_one(&pool).await.unwrap();
        let text: String = row.try_get("text").unwrap();
        assert_eq!(text, "Did yoga", "existing row must be left exactly as it was");
    }

    fn screen_time_row(
        app_id: &str,
        display_name: &str,
        platform: &str,
        device_name: &str,
        started_at: &str,
        ended_at: &str,
    ) -> ImportScreenTimeSessionRow {
        ImportScreenTimeSessionRow {
            app_id: app_id.to_string(),
            display_name: display_name.to_string(),
            platform: platform.to_string(),
            device_name: device_name.to_string(),
            started_at: started_at.to_string(),
            ended_at: ended_at.to_string(),
        }
    }

    #[tokio::test]
    async fn screen_time_duplicate_is_skipped_not_duplicated() {
        let pool = test_pool().await;

        sqlx::query(
            "INSERT INTO screen_time_session (app_id, display_name, platform, device_name, started_at, ended_at)
             VALUES ('chrome.exe', 'Google Chrome', 'windows', 'laptop-a', '2026-01-01T10:00:00.000Z', '2026-01-01T10:05:00.000Z')",
        )
        .execute(&pool)
        .await
        .unwrap();

        let imported = vec![screen_time_row(
            "chrome.exe",
            "Google Chrome",
            "windows",
            "laptop-a",
            "2026-01-01T10:00:00.000Z",
            "2026-01-01T10:05:00.000Z",
        )];

        let mut tx = pool.begin().await.unwrap();
        let duplicate_count = import_screen_time_sessions(&mut tx, &imported).await.unwrap();
        tx.commit().await.unwrap();

        assert_eq!(duplicate_count, 1);
        let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM screen_time_session")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(count, 1, "the duplicate must not have been inserted a second time");
    }

    #[tokio::test]
    async fn screen_time_display_name_difference_is_still_a_duplicate() {
        let pool = test_pool().await;

        sqlx::query(
            "INSERT INTO screen_time_session (app_id, display_name, platform, device_name, started_at, ended_at)
             VALUES ('chrome.exe', '', 'windows', 'laptop-a', '2026-01-01T10:00:00.000Z', '2026-01-01T10:05:00.000Z')",
        )
        .execute(&pool)
        .await
        .unwrap();

        // display_name is excluded from the dedupe key -- a resolved friendly
        // name arriving later for the same session must not be treated as a
        // new, distinct session.
        let imported = vec![screen_time_row(
            "chrome.exe",
            "Google Chrome",
            "windows",
            "laptop-a",
            "2026-01-01T10:00:00.000Z",
            "2026-01-01T10:05:00.000Z",
        )];

        let mut tx = pool.begin().await.unwrap();
        let duplicate_count = import_screen_time_sessions(&mut tx, &imported).await.unwrap();
        tx.commit().await.unwrap();

        assert_eq!(duplicate_count, 1);
        let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM screen_time_session")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(count, 1);
    }

    #[tokio::test]
    async fn screen_time_distinct_sessions_are_all_inserted() {
        let pool = test_pool().await;

        let imported = vec![
            screen_time_row("chrome.exe", "Google Chrome", "windows", "laptop-a", "2026-01-01T10:00:00.000Z", "2026-01-01T10:05:00.000Z"),
            // Different started_at -- a genuinely different session for the
            // same app/platform/device.
            screen_time_row("chrome.exe", "Google Chrome", "windows", "laptop-a", "2026-01-01T11:00:00.000Z", "2026-01-01T11:05:00.000Z"),
            // Different device_name -- a genuinely different machine.
            screen_time_row("chrome.exe", "Google Chrome", "windows", "laptop-b", "2026-01-01T10:00:00.000Z", "2026-01-01T10:05:00.000Z"),
        ];

        let mut tx = pool.begin().await.unwrap();
        let duplicate_count = import_screen_time_sessions(&mut tx, &imported).await.unwrap();
        tx.commit().await.unwrap();

        assert_eq!(duplicate_count, 0);
        let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM screen_time_session")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(count, 3);
    }

    fn wellness_row(slot_start_at: &str, created_at: &str) -> ImportWellnessCheckRow {
        ImportWellnessCheckRow {
            slot_start_at: slot_start_at.to_string(),
            relaxed_eyes: 1,
            exercise: 1,
            drank_water: 1,
            washroom: 0,
            created_at: created_at.to_string(),
        }
    }

    #[tokio::test]
    async fn wellness_existing_row_wins_over_a_newer_incoming_duplicate() {
        let pool = test_pool().await;

        sqlx::query(
            "INSERT INTO wellness_check (id, slot_start_at, created_at)
             VALUES (1, '2026-01-01T00:00:00.000Z', '2026-01-01T00:05:00.000Z')",
        )
        .execute(&pool)
        .await
        .unwrap();

        // Incoming is newer than the existing row -- existing should win.
        let imported = vec![wellness_row("2026-01-01T00:00:00.000Z", "2026-01-01T00:10:00.000Z")];

        let mut tx = pool.begin().await.unwrap();
        let duplicate_count = import_wellness_checks(&mut tx, &imported, ImportMode::Merge).await.unwrap();
        tx.commit().await.unwrap();

        assert_eq!(duplicate_count, 1);
        let rows = sqlx::query("SELECT id, created_at FROM wellness_check").fetch_all(&pool).await.unwrap();
        assert_eq!(rows.len(), 1, "the incoming duplicate must not have been inserted");
        let id: i64 = rows[0].try_get("id").unwrap();
        let created_at: String = rows[0].try_get("created_at").unwrap();
        assert_eq!(id, 1, "the existing row is untouched");
        assert_eq!(created_at, "2026-01-01T00:05:00.000Z");
    }

    #[tokio::test]
    async fn wellness_older_incoming_row_replaces_a_newer_existing_row() {
        let pool = test_pool().await;

        sqlx::query(
            "INSERT INTO wellness_check (id, slot_start_at, created_at)
             VALUES (1, '2026-01-01T00:00:00.000Z', '2026-01-01T00:10:00.000Z')",
        )
        .execute(&pool)
        .await
        .unwrap();

        // Incoming is older than the existing row -- incoming should win and
        // the existing (newer) row should be deleted.
        let imported = vec![wellness_row("2026-01-01T00:00:00.000Z", "2026-01-01T00:05:00.000Z")];

        let mut tx = pool.begin().await.unwrap();
        let duplicate_count = import_wellness_checks(&mut tx, &imported, ImportMode::Merge).await.unwrap();
        tx.commit().await.unwrap();

        assert_eq!(duplicate_count, 0);
        let rows = sqlx::query("SELECT id, created_at FROM wellness_check").fetch_all(&pool).await.unwrap();
        assert_eq!(rows.len(), 1, "the superseded existing row must have been deleted");
        let id: i64 = rows[0].try_get("id").unwrap();
        let created_at: String = rows[0].try_get("created_at").unwrap();
        assert_ne!(id, 1, "a fresh row was inserted, not the old id reused");
        assert_eq!(created_at, "2026-01-01T00:05:00.000Z");
    }

    #[tokio::test]
    async fn wellness_multiple_existing_duplicates_collapse_to_the_earliest_with_no_incoming_conflict() {
        let pool = test_pool().await;

        sqlx::query(
            "INSERT INTO wellness_check (id, slot_start_at, created_at)
             VALUES (1, '2026-01-01T00:00:00.000Z', '2026-01-01T00:05:00.000Z')",
        )
        .execute(&pool)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO wellness_check (id, slot_start_at, created_at)
             VALUES (2, '2026-01-01T00:00:00.000Z', '2026-01-01T00:07:00.000Z')",
        )
        .execute(&pool)
        .await
        .unwrap();

        // Like import_reflections, cleanup only runs for slots the import
        // actually touches -- an incoming row for this slot is what triggers
        // the pre-existing duplicate cleanup. This one is newer than both
        // existing rows, so it loses outright and is itself skipped, but its
        // mere presence still collapses the existing duplicates to the
        // earliest.
        let imported = vec![wellness_row("2026-01-01T00:00:00.000Z", "2026-01-01T00:10:00.000Z")];

        let mut tx = pool.begin().await.unwrap();
        let duplicate_count = import_wellness_checks(&mut tx, &imported, ImportMode::Merge).await.unwrap();
        tx.commit().await.unwrap();

        assert_eq!(duplicate_count, 1, "the one incoming row lost and was skipped");
        let rows = sqlx::query("SELECT id FROM wellness_check").fetch_all(&pool).await.unwrap();
        assert_eq!(rows.len(), 1);
        let id: i64 = rows[0].try_get("id").unwrap();
        assert_eq!(id, 1, "the earliest existing row survives");
    }

    #[tokio::test]
    async fn wellness_multiple_incoming_rows_for_a_new_slot_collapse_to_the_earliest() {
        let pool = test_pool().await;

        let imported = vec![
            wellness_row("2026-02-01T00:00:00.000Z", "2026-02-01T00:10:00.000Z"),
            wellness_row("2026-02-01T00:00:00.000Z", "2026-02-01T00:05:00.000Z"),
            wellness_row("2026-02-01T00:00:00.000Z", "2026-02-01T00:15:00.000Z"),
        ];

        let mut tx = pool.begin().await.unwrap();
        let duplicate_count = import_wellness_checks(&mut tx, &imported, ImportMode::Merge).await.unwrap();
        tx.commit().await.unwrap();

        assert_eq!(duplicate_count, 2);
        let rows = sqlx::query("SELECT created_at FROM wellness_check").fetch_all(&pool).await.unwrap();
        assert_eq!(rows.len(), 1);
        let created_at: String = rows[0].try_get("created_at").unwrap();
        assert_eq!(created_at, "2026-02-01T00:05:00.000Z", "the earliest of the three incoming rows wins");
    }

    #[tokio::test]
    async fn wellness_exact_timestamp_tie_keeps_the_existing_row() {
        let pool = test_pool().await;

        sqlx::query(
            "INSERT INTO wellness_check (id, slot_start_at, created_at)
             VALUES (1, '2026-01-01T00:00:00.000Z', '2026-01-01T00:05:00.000Z')",
        )
        .execute(&pool)
        .await
        .unwrap();

        let imported = vec![wellness_row("2026-01-01T00:00:00.000Z", "2026-01-01T00:05:00.000Z")];

        let mut tx = pool.begin().await.unwrap();
        let duplicate_count = import_wellness_checks(&mut tx, &imported, ImportMode::Merge).await.unwrap();
        tx.commit().await.unwrap();

        assert_eq!(duplicate_count, 1);
        let rows = sqlx::query("SELECT id FROM wellness_check").fetch_all(&pool).await.unwrap();
        assert_eq!(rows.len(), 1);
        let id: i64 = rows[0].try_get("id").unwrap();
        assert_eq!(id, 1, "a tie favors the existing row rather than delete+reinsert");
    }

    #[tokio::test]
    async fn task_list_merge_preserves_both_existing_and_incoming_lines() {
        let pool = test_pool().await;

        sqlx::query(
            "INSERT INTO daily_task_list (date, content, updated_at)
             VALUES ('2026-01-01', 'Write report\nCall dentist', '2026-01-01T08:00:00.000Z')",
        )
        .execute(&pool)
        .await
        .unwrap();

        let rows: Vec<(&str, &str, &str)> = vec![("2026-01-01", "Call dentist\nBuy groceries", "2026-01-01T09:00:00.000Z")];

        let mut tx = pool.begin().await.unwrap();
        merge_import_day_rows(&mut tx, "daily_task_list", &rows, ImportMode::Merge).await.unwrap();
        tx.commit().await.unwrap();

        let row = sqlx::query("SELECT content, updated_at FROM daily_task_list WHERE date = '2026-01-01'")
            .fetch_one(&pool)
            .await
            .unwrap();
        let content: String = row.try_get("content").unwrap();
        let updated_at: String = row.try_get("updated_at").unwrap();
        // Existing lines kept in place; "Call dentist" (case-sensitively
        // identical, already present) isn't duplicated; the genuinely new
        // "Buy groceries" is appended.
        assert_eq!(content, "Write report\nCall dentist\nBuy groceries");
        assert_eq!(updated_at, "2026-01-01T09:00:00.000Z");
    }

    #[tokio::test]
    async fn task_list_merge_is_case_insensitive_and_no_op_when_fully_covered() {
        let pool = test_pool().await;

        sqlx::query(
            "INSERT INTO not_to_do_list (date, content, updated_at)
             VALUES ('2026-01-01', 'Check email before 10am', '2026-01-01T08:00:00.000Z')",
        )
        .execute(&pool)
        .await
        .unwrap();

        let rows: Vec<(&str, &str, &str)> = vec![("2026-01-01", "check email before 10am", "2026-01-01T09:00:00.000Z")];

        let mut tx = pool.begin().await.unwrap();
        merge_import_day_rows(&mut tx, "not_to_do_list", &rows, ImportMode::Merge).await.unwrap();
        tx.commit().await.unwrap();

        let content: String = sqlx::query_scalar("SELECT content FROM not_to_do_list WHERE date = '2026-01-01'")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(content, "Check email before 10am", "the case-insensitively-identical incoming line adds nothing");
    }

    #[tokio::test]
    async fn task_list_no_existing_day_is_a_plain_insert() {
        let pool = test_pool().await;

        let rows: Vec<(&str, &str, &str)> = vec![("2026-03-01", "Plan trip", "2026-03-01T08:00:00.000Z")];

        let mut tx = pool.begin().await.unwrap();
        merge_import_day_rows(&mut tx, "daily_task_list", &rows, ImportMode::Merge).await.unwrap();
        tx.commit().await.unwrap();

        let content: String = sqlx::query_scalar("SELECT content FROM daily_task_list WHERE date = '2026-03-01'")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(content, "Plan trip");
    }

    #[tokio::test]
    async fn task_list_replace_mode_overwrites_instead_of_merging() {
        let pool = test_pool().await;

        sqlx::query(
            "INSERT INTO daily_task_list (date, content, updated_at)
             VALUES ('2026-01-01', 'Old task', '2026-01-01T08:00:00.000Z')",
        )
        .execute(&pool)
        .await
        .unwrap();
        // Replace mode wipes the table before this runs in the real
        // import_data flow -- simulated here directly since this test only
        // exercises merge_import_day_rows in isolation.
        sqlx::query("DELETE FROM daily_task_list").execute(&pool).await.unwrap();

        let rows: Vec<(&str, &str, &str)> = vec![("2026-01-01", "New task", "2026-01-01T09:00:00.000Z")];

        let mut tx = pool.begin().await.unwrap();
        merge_import_day_rows(&mut tx, "daily_task_list", &rows, ImportMode::Replace).await.unwrap();
        tx.commit().await.unwrap();

        let content: String = sqlx::query_scalar("SELECT content FROM daily_task_list WHERE date = '2026-01-01'")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(content, "New task");
    }
}
