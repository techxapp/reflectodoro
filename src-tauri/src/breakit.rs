use chrono::Local;
use rand::Rng;
use tauri::AppHandle;

use crate::db;

// Excludes 0/O, 1/l/I -- those pairs render near-identically in several
// monospace fonts (including the Android overlay's plain "monospace"
// fallback), so a generated challenge could show an "O" indistinguishable
// from "0" and never be typeable correctly.
const ALPHANUMERIC: &[u8] = b"ABCDEFGHJKLMNPQRSTUVWXYZabcdefghijkmnopqrstuvwxyz23456789";
const SPECIAL: &[u8] = b"!@#$%^&*()-_=+[]{}";

/// Fresh random string generated per overlay -- typing it out (no paste
/// allowed) is the early-exit alternative to waiting out the break timer.
/// Being random rather than a fixed word means it can't become muscle memory.
pub fn generate_challenge(length: u32, include_special: bool) -> String {
    let mut charset: Vec<u8> = ALPHANUMERIC.to_vec();
    if include_special {
        charset.extend_from_slice(SPECIAL);
    }

    let mut rng = rand::thread_rng();
    (0..length.clamp(4, 64))
        .map(|_| charset[rng.gen_range(0..charset.len())] as char)
        .collect()
}

/// Today's already-used count of successful breakit early-exits, read from
/// `breakit_daily_use` (a single-row table -- see `increment_daily_use`'s
/// doc comment for why there's never more than one row to look up).
pub async fn uses_today(app: &AppHandle) -> Result<u32, String> {
    let pool = db::open_direct_pool(app).await?;
    let today = Local::now().format("%Y-%m-%d").to_string();
    let count = sqlx::query_scalar::<_, i64>("SELECT count FROM breakit_daily_use WHERE date = ?1")
        .bind(&today)
        .fetch_optional(&pool)
        .await
        .map_err(|e| format!("failed to read breakit_daily_use: {e}"))?;
    Ok(count.unwrap_or(0) as u32)
}

/// Records one more successful breakit early-exit for today and returns the
/// new count. Deletes any other date's row first rather than just upserting
/// today's -- nothing ever reads a date other than "today", so this keeps
/// the table at exactly one row forever instead of accumulating one row per
/// day indefinitely; simpler than a time-based retention sweep since there's
/// no history to retain at all.
pub async fn increment_daily_use(app: &AppHandle) -> Result<u32, String> {
    let pool = db::open_direct_pool(app).await?;
    let today = Local::now().format("%Y-%m-%d").to_string();
    sqlx::query("DELETE FROM breakit_daily_use WHERE date != ?1")
        .bind(&today)
        .execute(&pool)
        .await
        .map_err(|e| format!("failed to prune breakit_daily_use: {e}"))?;
    sqlx::query(
        "INSERT INTO breakit_daily_use (date, count) VALUES (?1, 1)
         ON CONFLICT(date) DO UPDATE SET count = count + 1",
    )
    .bind(&today)
    .execute(&pool)
    .await
    .map_err(|e| format!("failed to increment breakit_daily_use: {e}"))?;
    uses_today(app).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn respects_length() {
        assert_eq!(generate_challenge(15, false).chars().count(), 15);
        assert_eq!(generate_challenge(2, false).chars().count(), 4); // clamped up
        assert_eq!(generate_challenge(200, false).chars().count(), 64); // clamped down
    }

    #[test]
    fn excludes_special_by_default() {
        let s = generate_challenge(200_u32.min(64), false);
        assert!(s.chars().all(|c| c.is_ascii_alphanumeric()));
    }

    #[test]
    fn can_include_special() {
        // Generate many long samples; at least one special char should show up.
        let found = (0..20).any(|_| {
            generate_challenge(64, true)
                .chars()
                .any(|c| !c.is_ascii_alphanumeric())
        });
        assert!(found);
    }
}
