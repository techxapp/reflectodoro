//! App lock: a numeric PIN (at least `MIN_PIN_DIGITS` digits) the user has to
//! enter every time they come back to Reflectodoro from another app. See
//! CLAUDE.md's "App lock (PIN)" section for the app-level summary.
//!
//! **What it is**: a privacy screen over the app's own UI, so someone handed
//! an unlocked computer/phone can't read reflections, task lists, habits or
//! screen time by switching to the app. **What it isn't**: a security boundary
//! against someone who can run code as the user (devtools, reading
//! `pomodoro.db` directly) -- the data itself is protected at rest by
//! crypto.rs, not by this. That's also why the PIN hash lives in an ordinary
//! `app_setting` row.
//!
//! **When the app locks** (`left_app`): whenever focus moves from one of this
//! app's windows to another app, not when it moves between this app's own
//! windows (main -> break overlay -> check-in) or to one of its own native
//! dialogs. How "another app" is detected differs per platform:
//! - Windows: screen_time.rs's `EVENT_SYSTEM_FOREGROUND` hook, when the new
//!   foreground window belongs to another process, plus presence.rs reporting
//!   the session locked or the display off.
//! - macOS: screen_time.rs's `NSWorkspaceDidActivateApplicationNotification`
//!   observer, when the activated app is another process.
//! - Linux: no global foreground hook exists here, so `run()` watches its own
//!   windows' `Focused(false)` and locks once none of them has focus after
//!   `LINUX_FOCUS_SETTLE` (long enough for focus to land on a sibling window).
//!   Accepted side effect: a native file/confirm dialog also counts as leaving.
//! - Android/iOS: the webview reporting `visibilitychange` -> hidden, i.e. the
//!   app went to the background (`app_lock_engage`).
//!
//! The app also starts locked whenever a PIN is set.
//!
//! **Failed attempts**: at most `MAX_FAILED_ATTEMPTS` wrong PINs per
//! `ATTEMPT_WINDOW`, tracked in two `app_setting` rows rather than a table:
//! the first failure records its timestamp and sets the counter to
//! `MAX_FAILED_ATTEMPTS - 1`; each further failure inside the window
//! decrements it; at 0, PIN entry is refused until the window (counted from
//! that first failure) runs out. A success clears both. Kept in the database,
//! not memory, so restarting the app doesn't hand out fresh attempts.
//!
//! **Recovery code**: setting or changing the PIN also issues a random
//! `RECOVERY_CODE_LEN`-character code, shown once and stored only as a hash.
//! Entering it on the lock screen (`app_lock_recover`) sets a new PIN without
//! erasing anything, consumes the code and issues a fresh one. It has its own
//! attempt window (`RECOVERY_LIMITER`), so a user who burned the PIN's
//! attempts can still use it.

use base64::{engine::general_purpose::STANDARD as B64, Engine};
use hmac::{Hmac, Mac};
use serde::Serialize;
use sha2::Sha256;
use sqlx::SqlitePool;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::OnceLock;
use tauri::{AppHandle, Emitter};

use crate::db;

pub const MIN_PIN_DIGITS: usize = 4;
pub const MAX_PIN_DIGITS: usize = 12;
pub const MAX_FAILED_ATTEMPTS: u32 = 5;
/// 15 minutes.
pub const ATTEMPT_WINDOW_MS: i64 = 15 * 60 * 1000;
/// PBKDF2-HMAC-SHA256 rounds. A 4-digit PIN can't survive an offline brute
/// force at any cost here, so this only keeps the hash from being trivially
/// reversible; the attempt limit above is the real protection.
const PBKDF2_ITERATIONS: u32 = 100_000;
const HASH_SCHEME: &str = "pbkdf2-sha256";

/// 16 symbols from a 32-symbol alphabet = 80 bits. No 0/O/1/I, which are easy
/// to misread when copying a code off a screen or paper.
const RECOVERY_ALPHABET: &[u8] = b"ABCDEFGHJKLMNPQRSTUVWXYZ23456789";
pub const RECOVERY_CODE_LEN: usize = 16;
/// Characters per dash-separated group when a code is displayed.
const RECOVERY_GROUP: usize = 4;

pub const PIN_HASH_SETTING: &str = "app_lock_pin_hash";
pub const ATTEMPTS_LEFT_SETTING: &str = "app_lock_attempts_left";
pub const WINDOW_START_SETTING: &str = "app_lock_window_start_ms";
pub const RECOVERY_HASH_SETTING: &str = "app_lock_recovery_hash";
pub const RECOVERY_ATTEMPTS_LEFT_SETTING: &str = "app_lock_recovery_attempts_left";
pub const RECOVERY_WINDOW_START_SETTING: &str = "app_lock_recovery_window_start_ms";
/// Device-local: never exported, never imported, never wiped by a
/// replace-mode import (a PIN set on another device, or a stale attempt
/// counter, has no business arriving here). Keep `exportAllData`'s
/// `APP_LOCK_DEVICE_LOCAL_KEYS` (db.ts) in step with this list.
pub const DEVICE_LOCAL_SETTINGS: [&str; 6] = [
    PIN_HASH_SETTING,
    ATTEMPTS_LEFT_SETTING,
    WINDOW_START_SETTING,
    RECOVERY_HASH_SETTING,
    RECOVERY_ATTEMPTS_LEFT_SETTING,
    RECOVERY_WINDOW_START_SETTING,
];

/// Which secret a failed-attempt window guards: where its hash lives, where
/// its counter lives, and what to call it in messages.
struct Limiter {
    hash_key: &'static str,
    left_key: &'static str,
    start_key: &'static str,
    what: &'static str,
}

const PIN_LIMITER: Limiter = Limiter {
    hash_key: PIN_HASH_SETTING,
    left_key: ATTEMPTS_LEFT_SETTING,
    start_key: WINDOW_START_SETTING,
    what: "PIN",
};

const RECOVERY_LIMITER: Limiter = Limiter {
    hash_key: RECOVERY_HASH_SETTING,
    left_key: RECOVERY_ATTEMPTS_LEFT_SETTING,
    start_key: RECOVERY_WINDOW_START_SETTING,
    what: "recovery code",
};

pub const STATE_EVENT: &str = "applock://state";
/// Emitted after "Forgot PIN" erased everything, so every window reloads
/// instead of showing data that's no longer there.
pub const ERASED_EVENT: &str = "applock://erased";

#[cfg(target_os = "linux")]
pub const LINUX_FOCUS_SETTLE: std::time::Duration = std::time::Duration::from_millis(300);

/// Whether a PIN is set. Cached so the focus hooks never touch the database.
static ENABLED: AtomicBool = AtomicBool::new(false);
static LOCKED: AtomicBool = AtomicBool::new(false);
/// One PIN check at a time, so two windows submitting at once can't both read
/// the same attempt counter and each write back only their own decrement.
static VERIFYING: AtomicBool = AtomicBool::new(false);
static APP: OnceLock<AppHandle> = OnceLock::new();

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AppLockStatus {
    pub enabled: bool,
    pub locked: bool,
    pub attempts_left: u32,
    /// Unix ms until which PIN entry is refused, if attempts are used up.
    pub locked_out_until_ms: Option<i64>,
    pub min_pin_digits: usize,
    pub max_pin_digits: usize,
    pub has_recovery_code: bool,
}

/// Returned by every command that issues a recovery code. The code is shown
/// to the user once and exists nowhere else in plaintext.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AppLockCodeResult {
    pub status: AppLockStatus,
    pub recovery_code: String,
}

// --- Failed-attempt window (pure, unit tested) ------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Attempts {
    window_start_ms: Option<i64>,
    left: u32,
}

impl Attempts {
    const FRESH: Attempts = Attempts { window_start_ms: None, left: MAX_FAILED_ATTEMPTS };

    /// A window that started "in the future" (the clock was set back) counts
    /// as expired rather than extending the lockout indefinitely.
    fn active_start(&self, now_ms: i64) -> Option<i64> {
        self.window_start_ms.filter(|&s| now_ms >= s && now_ms < s + ATTEMPT_WINDOW_MS)
    }

    fn left(&self, now_ms: i64) -> u32 {
        if self.active_start(now_ms).is_some() {
            self.left
        } else {
            MAX_FAILED_ATTEMPTS
        }
    }

    fn locked_out_until(&self, now_ms: i64) -> Option<i64> {
        match self.active_start(now_ms) {
            Some(start) if self.left == 0 => Some(start + ATTEMPT_WINDOW_MS),
            _ => None,
        }
    }

    fn after_failure(&self, now_ms: i64) -> Attempts {
        match self.active_start(now_ms) {
            Some(start) => Attempts { window_start_ms: Some(start), left: self.left.saturating_sub(1) },
            None => Attempts { window_start_ms: Some(now_ms), left: MAX_FAILED_ATTEMPTS - 1 },
        }
    }
}

// --- PIN hashing ------------------------------------------------------------

fn pbkdf2_sha256(password: &[u8], salt: &[u8], iterations: u32) -> [u8; 32] {
    type H = Hmac<Sha256>;
    let prf = <H as Mac>::new_from_slice(password).expect("HMAC accepts any key length");
    let mut mac = prf.clone();
    mac.update(salt);
    mac.update(&1u32.to_be_bytes());
    let mut u: [u8; 32] = mac.finalize().into_bytes().into();
    let mut out = u;
    for _ in 1..iterations {
        let mut mac = prf.clone();
        mac.update(&u);
        u = mac.finalize().into_bytes().into();
        for (o, b) in out.iter_mut().zip(u.iter()) {
            *o ^= b;
        }
    }
    out
}

fn hash_secret_with(secret: &str, salt: &[u8], iterations: u32) -> String {
    let hash = pbkdf2_sha256(secret.as_bytes(), salt, iterations);
    format!("{HASH_SCHEME}${iterations}${}${}", B64.encode(salt), B64.encode(hash))
}

fn hash_secret(secret: &str) -> String {
    use rand::RngCore;
    let mut salt = [0u8; 16];
    rand::thread_rng().fill_bytes(&mut salt);
    hash_secret_with(secret, &salt, PBKDF2_ITERATIONS)
}

/// `false` for a wrong secret *and* for a malformed stored hash -- an
/// unreadable hash must never read as "matches".
fn secret_matches(pin: &str, stored: &str) -> bool {
    let parts: Vec<&str> = stored.split('$').collect();
    let [scheme, iterations, salt, hash] = parts[..] else { return false };
    if scheme != HASH_SCHEME {
        return false;
    }
    let (Ok(iterations), Ok(salt), Ok(expected)) = (iterations.parse::<u32>(), B64.decode(salt), B64.decode(hash))
    else {
        return false;
    };
    if iterations == 0 || expected.len() != 32 {
        return false;
    }
    let actual = pbkdf2_sha256(pin.as_bytes(), &salt, iterations);
    actual.iter().zip(expected.iter()).fold(0u8, |acc, (a, b)| acc | (a ^ b)) == 0
}

fn validate_new_pin(pin: &str) -> Result<(), String> {
    if !pin.chars().all(|c| c.is_ascii_digit()) {
        return Err("The PIN can only contain digits.".into());
    }
    if pin.len() < MIN_PIN_DIGITS || pin.len() > MAX_PIN_DIGITS {
        return Err(format!("Use {MIN_PIN_DIGITS} to {MAX_PIN_DIGITS} digits."));
    }
    Ok(())
}

// --- Recovery code ------------------------------------------------------------

/// A fresh code, unformatted (`RECOVERY_CODE_LEN` characters of
/// `RECOVERY_ALPHABET`).
fn generate_recovery_code() -> String {
    use rand::Rng;
    let mut rng = rand::thread_rng();
    (0..RECOVERY_CODE_LEN)
        .map(|_| RECOVERY_ALPHABET[rng.gen_range(0..RECOVERY_ALPHABET.len())] as char)
        .collect()
}

/// What the hash is taken over, and what a typed code is reduced to first:
/// upper case, letters and digits only, so dashes, spaces and case don't
/// matter.
fn normalize_recovery_code(input: &str) -> String {
    input
        .chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .map(|c| c.to_ascii_uppercase())
        .collect()
}

/// `ABCD-EFGH-...`, for display.
fn format_recovery_code(raw: &str) -> String {
    raw.as_bytes()
        .chunks(RECOVERY_GROUP)
        .map(|c| std::str::from_utf8(c).unwrap_or(""))
        .collect::<Vec<_>>()
        .join("-")
}

// --- Storage ----------------------------------------------------------------

async fn read_setting(pool: &SqlitePool, key: &str) -> Result<Option<String>, String> {
    sqlx::query_scalar("SELECT value FROM app_setting WHERE key = ?")
        .bind(key)
        .fetch_optional(pool)
        .await
        .map_err(|e| format!("couldn't read {key}: {e}"))
}

async fn write_setting(pool: &SqlitePool, key: &str, value: &str) -> Result<(), String> {
    sqlx::query("INSERT INTO app_setting (key, value) VALUES (?, ?) ON CONFLICT(key) DO UPDATE SET value = excluded.value")
        .bind(key)
        .bind(value)
        .execute(pool)
        .await
        .map(|_| ())
        .map_err(|e| format!("couldn't save {key}: {e}"))
}

async fn delete_settings(pool: &SqlitePool, keys: &[&str]) -> Result<(), String> {
    for key in keys {
        sqlx::query("DELETE FROM app_setting WHERE key = ?")
            .bind(key)
            .execute(pool)
            .await
            .map_err(|e| format!("couldn't clear {key}: {e}"))?;
    }
    Ok(())
}

async fn read_pin_hash(pool: &SqlitePool) -> Result<Option<String>, String> {
    Ok(read_setting(pool, PIN_HASH_SETTING).await?.filter(|h| !h.is_empty()))
}

async fn has_recovery_code(pool: &SqlitePool) -> Result<bool, String> {
    Ok(read_setting(pool, RECOVERY_HASH_SETTING).await?.is_some_and(|h| !h.is_empty()))
}

async fn read_attempts(pool: &SqlitePool, limiter: &Limiter) -> Result<Attempts, String> {
    let start = read_setting(pool, limiter.start_key).await?.and_then(|v| v.parse::<i64>().ok());
    let left = read_setting(pool, limiter.left_key).await?.and_then(|v| v.parse::<u32>().ok());
    Ok(match (start, left) {
        (Some(start), Some(left)) => Attempts { window_start_ms: Some(start), left: left.min(MAX_FAILED_ATTEMPTS) },
        _ => Attempts::FRESH,
    })
}

async fn write_attempts(pool: &SqlitePool, limiter: &Limiter, a: Attempts) -> Result<(), String> {
    match a.window_start_ms {
        Some(start) => {
            write_setting(pool, limiter.start_key, &start.to_string()).await?;
            write_setting(pool, limiter.left_key, &a.left.to_string()).await
        }
        None => delete_settings(pool, &[limiter.start_key, limiter.left_key]).await,
    }
}

fn now_ms() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

enum Verify {
    Match,
    Wrong { left: u32 },
    LockedOut { until_ms: i64 },
    NoPin,
}

/// Checks `secret` against the hash the limiter guards, honoring and updating
/// that limiter's failed-attempt window. `NoPin` means no hash is stored.
async fn verify_limited(pool: &SqlitePool, limiter: &Limiter, secret: &str) -> Result<Verify, String> {
    if VERIFYING.swap(true, Ordering::SeqCst) {
        return Err(format!("A {} check is already in progress. Try again.", limiter.what));
    }
    struct Release;
    impl Drop for Release {
        fn drop(&mut self) {
            VERIFYING.store(false, Ordering::SeqCst);
        }
    }
    let _release = Release;

    let Some(stored) = read_setting(pool, limiter.hash_key).await?.filter(|h| !h.is_empty()) else {
        return Ok(Verify::NoPin);
    };
    let attempts = read_attempts(pool, limiter).await?;
    if let Some(until_ms) = attempts.locked_out_until(now_ms()) {
        return Ok(Verify::LockedOut { until_ms });
    }
    let secret_owned = secret.to_string();
    let matched = tauri::async_runtime::spawn_blocking(move || secret_matches(&secret_owned, &stored))
        .await
        .map_err(|e| format!("{} check failed: {e}", limiter.what))?;
    if matched {
        write_attempts(pool, limiter, Attempts::FRESH).await?;
        return Ok(Verify::Match);
    }
    let next = attempts.after_failure(now_ms());
    write_attempts(pool, limiter, next).await?;
    log::info!("app lock: wrong {}, {} attempt(s) left in this window", limiter.what, next.left);
    Ok(match next.locked_out_until(now_ms()) {
        Some(until_ms) => Verify::LockedOut { until_ms },
        None => Verify::Wrong { left: next.left },
    })
}

fn lockout_message(limiter: &Limiter, until_ms: i64) -> String {
    let minutes = ((until_ms - now_ms()).max(0) + 59_999) / 60_000;
    format!("Too many wrong {}s. Try again in {minutes} min.", limiter.what)
}

/// Replaces the stored recovery code with a fresh one and returns it in its
/// display form. The previous code stops working the moment the new hash is
/// written; the recovery attempt window starts over.
async fn issue_recovery_code(pool: &SqlitePool) -> Result<String, String> {
    let raw = generate_recovery_code();
    let to_hash = raw.clone();
    let hash = tauri::async_runtime::spawn_blocking(move || hash_secret(&to_hash))
        .await
        .map_err(|e| format!("couldn't hash the recovery code: {e}"))?;
    write_setting(pool, RECOVERY_HASH_SETTING, &hash).await?;
    write_attempts(pool, &RECOVERY_LIMITER, Attempts::FRESH).await?;
    Ok(format_recovery_code(&raw))
}

/// Verifies `current_pin` against the PIN limiter, mapping every non-match to
/// the error the UI shows. Shared by the commands that need the current PIN.
async fn require_current_pin(pool: &SqlitePool, current_pin: &str) -> Result<(), String> {
    match verify_limited(pool, &PIN_LIMITER, current_pin).await? {
        Verify::Match | Verify::NoPin => Ok(()),
        Verify::Wrong { left } => Err(format!("Current PIN is incorrect. {left} attempt(s) left.")),
        Verify::LockedOut { until_ms } => Err(lockout_message(&PIN_LIMITER, until_ms)),
    }
}

/// The database half of `app_lock_recover` (no lock state, so it can be
/// tested against an in-memory pool): checks the code through the recovery
/// limiter, then replaces the PIN and issues the next code. A bad `new_pin`
/// is rejected *before* the code is checked, so a typo there can't cost an
/// attempt or consume the code.
async fn recover_with_pool(pool: &SqlitePool, code: &str, new_pin: &str) -> Result<String, String> {
    validate_new_pin(new_pin)?;
    let normalized = normalize_recovery_code(code);
    if normalized.len() != RECOVERY_CODE_LEN {
        return Err(format!("A recovery code has {RECOVERY_CODE_LEN} letters and digits."));
    }
    match verify_limited(pool, &RECOVERY_LIMITER, &normalized).await? {
        Verify::Match => {}
        Verify::NoPin => return Err("This device has no recovery code.".into()),
        Verify::Wrong { left } => return Err(format!("Recovery code is incorrect. {left} attempt(s) left.")),
        Verify::LockedOut { until_ms } => return Err(lockout_message(&RECOVERY_LIMITER, until_ms)),
    }
    let new_pin = new_pin.to_string();
    let pin_hash = tauri::async_runtime::spawn_blocking(move || hash_secret(&new_pin))
        .await
        .map_err(|e| format!("couldn't hash the PIN: {e}"))?;
    write_setting(pool, PIN_HASH_SETTING, &pin_hash).await?;
    write_attempts(pool, &PIN_LIMITER, Attempts::FRESH).await?;
    issue_recovery_code(pool).await
}

// --- State ------------------------------------------------------------------

async fn status_from(pool: &SqlitePool) -> Result<AppLockStatus, String> {
    let attempts = read_attempts(pool, &PIN_LIMITER).await?;
    let now = now_ms();
    Ok(AppLockStatus {
        enabled: ENABLED.load(Ordering::SeqCst),
        locked: ENABLED.load(Ordering::SeqCst) && LOCKED.load(Ordering::SeqCst),
        attempts_left: attempts.left(now),
        locked_out_until_ms: attempts.locked_out_until(now),
        min_pin_digits: MIN_PIN_DIGITS,
        max_pin_digits: MAX_PIN_DIGITS,
        has_recovery_code: has_recovery_code(pool).await?,
    })
}

fn emit_state(app: &AppHandle) {
    if let Err(e) = app.emit(STATE_EVENT, ()) {
        log::warn!("app lock: failed to emit state: {e}");
    }
}

/// Called once from `.setup()`, after `db::ensure_schema`: a set PIN means
/// the app starts locked.
pub async fn load(app: &AppHandle) {
    let _ = APP.set(app.clone());
    let enabled = match db::open_direct_pool(app).await {
        Ok(pool) => {
            let result = read_pin_hash(&pool).await;
            pool.close().await;
            match result {
                Ok(hash) => hash.is_some(),
                Err(e) => {
                    // Fail closed: a PIN we can't read is still a PIN.
                    log::error!("app lock: {e}; starting locked");
                    true
                }
            }
        }
        Err(e) => {
            log::error!("app lock: {e}; starting locked");
            true
        }
    };
    ENABLED.store(enabled, Ordering::SeqCst);
    LOCKED.store(enabled, Ordering::SeqCst);
    log::info!("app lock: enabled={enabled}");
}

/// Whether a PIN is set. Android's native break overlay (a plain WebView
/// with no way to ask for the PIN) hides today's task lists and "Coming
/// next" while this is true, and its task-list saves are refused.
#[cfg_attr(not(target_os = "android"), allow(dead_code))]
pub fn is_enabled() -> bool {
    ENABLED.load(Ordering::SeqCst)
}

/// Locks the app if a PIN is set. Cheap and idempotent (atomics only, an
/// event only on the unlocked -> locked edge), so the focus hooks can call it
/// on every foreground change.
pub fn left_app() {
    if !ENABLED.load(Ordering::SeqCst) || LOCKED.swap(true, Ordering::SeqCst) {
        return;
    }
    log::info!("app lock: locked (left the app)");
    if let Some(app) = APP.get() {
        emit_state(app);
    }
}

/// Linux only (see the module doc): locks once focus has settled outside
/// every one of this app's windows.
#[cfg(target_os = "linux")]
pub fn window_focus_lost(app: &AppHandle) {
    use tauri::Manager;
    if !ENABLED.load(Ordering::SeqCst) || LOCKED.load(Ordering::SeqCst) {
        return;
    }
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        tokio::time::sleep(LINUX_FOCUS_SETTLE).await;
        let any_focused = app
            .webview_windows()
            .values()
            .any(|w| w.is_focused().unwrap_or(false));
        if !any_focused {
            left_app();
        }
    });
}

async fn with_pool<T, F, Fut>(app: &AppHandle, f: F) -> Result<T, String>
where
    F: FnOnce(SqlitePool) -> Fut,
    Fut: std::future::Future<Output = Result<T, String>>,
{
    let pool = db::open_direct_pool(app).await?;
    let result = f(pool.clone()).await;
    pool.close().await;
    result
}

// --- Commands ---------------------------------------------------------------

#[tauri::command]
pub async fn app_lock_status(app: AppHandle) -> Result<AppLockStatus, String> {
    with_pool(&app, |pool| async move { status_from(&pool).await }).await
}

/// Lock now. Used by the mobile `visibilitychange` hook and Settings' "Lock
/// now". No-op without a PIN.
#[tauri::command]
pub fn app_lock_engage() {
    left_app();
}

/// A wrong PIN is not an `Err`: the returned status is still `locked`, with
/// the attempts left (or the lockout) for the UI to show.
#[tauri::command]
pub async fn app_lock_unlock(app: AppHandle, pin: String) -> Result<AppLockStatus, String> {
    let handle = app.clone();
    with_pool(&app, |pool| async move {
        match verify_limited(&pool, &PIN_LIMITER, &pin).await? {
            Verify::Match | Verify::NoPin => {
                if !LOCKED.swap(false, Ordering::SeqCst) {
                    // Already unlocked (another window got there first).
                } else {
                    log::info!("app lock: unlocked");
                }
                emit_state(&handle);
            }
            Verify::Wrong { .. } | Verify::LockedOut { .. } => {}
        }
        status_from(&pool).await
    })
    .await
}

/// Sets a new PIN and issues a fresh recovery code for it (returned once, in
/// the result). When a PIN is already set, `current_pin` must match it (and a
/// wrong one counts as a failed attempt, so this isn't a way around the
/// limit).
#[tauri::command]
pub async fn app_lock_set_pin(
    app: AppHandle,
    current_pin: Option<String>,
    new_pin: String,
) -> Result<AppLockCodeResult, String> {
    validate_new_pin(&new_pin)?;
    let handle = app.clone();
    with_pool(&app, |pool| async move {
        if read_pin_hash(&pool).await?.is_some() {
            require_current_pin(&pool, current_pin.as_deref().unwrap_or("")).await?;
        }
        let hash = tauri::async_runtime::spawn_blocking(move || hash_secret(&new_pin))
            .await
            .map_err(|e| format!("couldn't hash the PIN: {e}"))?;
        write_setting(&pool, PIN_HASH_SETTING, &hash).await?;
        write_attempts(&pool, &PIN_LIMITER, Attempts::FRESH).await?;
        let recovery_code = issue_recovery_code(&pool).await?;
        ENABLED.store(true, Ordering::SeqCst);
        // The user is here, setting it: stay unlocked until they next leave.
        LOCKED.store(false, Ordering::SeqCst);
        log::info!("app lock: PIN set, recovery code issued");
        emit_state(&handle);
        Ok(AppLockCodeResult { status: status_from(&pool).await?, recovery_code })
    })
    .await
}

/// Replaces the recovery code (the way to get one for a PIN that predates
/// recovery codes, or after losing the copy). Needs the current PIN.
#[tauri::command]
pub async fn app_lock_regenerate_recovery_code(
    app: AppHandle,
    current_pin: String,
) -> Result<AppLockCodeResult, String> {
    let handle = app.clone();
    with_pool(&app, |pool| async move {
        if read_pin_hash(&pool).await?.is_none() {
            return Err("Turn on app lock first.".into());
        }
        require_current_pin(&pool, &current_pin).await?;
        let recovery_code = issue_recovery_code(&pool).await?;
        log::info!("app lock: recovery code regenerated");
        emit_state(&handle);
        Ok(AppLockCodeResult { status: status_from(&pool).await?, recovery_code })
    })
    .await
}

/// "Forgot PIN" with a recovery code: sets `new_pin`, unlocks, and returns the
/// replacement code. Callable while locked, so it takes no current PIN. A
/// wrong code is an `Err` carrying the attempts left or the lockout.
#[tauri::command]
pub async fn app_lock_recover(
    app: AppHandle,
    recovery_code: String,
    new_pin: String,
) -> Result<AppLockCodeResult, String> {
    let handle = app.clone();
    with_pool(&app, |pool| async move {
        let next_code = recover_with_pool(&pool, &recovery_code, &new_pin).await?;
        ENABLED.store(true, Ordering::SeqCst);
        LOCKED.store(false, Ordering::SeqCst);
        log::info!("app lock: PIN reset with a recovery code, unlocked");
        emit_state(&handle);
        Ok(AppLockCodeResult { status: status_from(&pool).await?, recovery_code: next_code })
    })
    .await
}

#[tauri::command]
pub async fn app_lock_disable(app: AppHandle, current_pin: String) -> Result<AppLockStatus, String> {
    let handle = app.clone();
    with_pool(&app, |pool| async move {
        require_current_pin(&pool, &current_pin).await?;
        delete_settings(&pool, &DEVICE_LOCAL_SETTINGS).await?;
        ENABLED.store(false, Ordering::SeqCst);
        LOCKED.store(false, Ordering::SeqCst);
        log::info!("app lock: PIN removed");
        emit_state(&handle);
        status_from(&pool).await
    })
    .await
}

/// "Forgot PIN": the only way past a forgotten PIN is to give up the data it
/// was protecting. Erases every content table (the same set as Settings'
/// "Delete all data") plus the paired devices -- otherwise the next sync
/// would hand everything straight back -- and then removes the PIN, all in
/// one transaction. Settings and the encryption key are kept.
#[tauri::command]
pub async fn app_lock_forgot_pin_erase(app: AppHandle) -> Result<AppLockStatus, String> {
    const TABLES: [&str; 10] = [
        "reflection",
        "wellness_check",
        "daily_task_list",
        "not_to_do_list",
        "screen_time_session",
        "bulk_edit_preset",
        "habit_log",
        "habit",
        "sync_cursor",
        "paired_device",
    ];
    let handle = app.clone();
    with_pool(&app, |pool| async move {
        let mut tx = pool.begin().await.map_err(|e| e.to_string())?;
        for table in TABLES {
            sqlx::query(&format!("DELETE FROM {table}"))
                .execute(&mut *tx)
                .await
                .map_err(|e| format!("couldn't clear {table}: {e}"))?;
        }
        for key in DEVICE_LOCAL_SETTINGS {
            sqlx::query("DELETE FROM app_setting WHERE key = ?")
                .bind(key)
                .execute(&mut *tx)
                .await
                .map_err(|e| format!("couldn't clear {key}: {e}"))?;
        }
        tx.commit().await.map_err(|e| e.to_string())?;
        ENABLED.store(false, Ordering::SeqCst);
        LOCKED.store(false, Ordering::SeqCst);
        log::warn!("app lock: forgotten PIN -- all entries and paired devices erased, PIN removed");
        emit_state(&handle);
        if let Err(e) = handle.emit(ERASED_EVENT, ()) {
            log::warn!("app lock: failed to emit erased: {e}");
        }
        status_from(&pool).await
    })
    .await
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hex(bytes: &[u8]) -> String {
        bytes.iter().map(|b| format!("{b:02x}")).collect()
    }

    #[test]
    fn pbkdf2_matches_rfc7914_vectors() {
        assert_eq!(
            hex(&pbkdf2_sha256(b"password", b"salt", 1)),
            "120fb6cffcf8b32c43e7225256c4f837a86548c92ccc35480805987cb70be17b"
        );
        assert_eq!(
            hex(&pbkdf2_sha256(b"password", b"salt", 2)),
            "ae4d0c95af6b46d32d0adff928f06dd02a303f8ef3c251dfd6e2d85a95474c43"
        );
    }

    #[test]
    fn pin_hash_round_trips_and_rejects_others() {
        let stored = hash_secret_with("4821", b"0123456789abcdef", 10);
        assert!(secret_matches("4821", &stored));
        assert!(!secret_matches("4822", &stored));
        assert!(!secret_matches("04821", &stored));
        assert!(!secret_matches("", &stored));
    }

    #[test]
    fn malformed_hash_never_matches() {
        for stored in ["", "garbage", "pbkdf2-sha256$0$AAAA$AAAA", "other$10$AAAA$AAAA", "pbkdf2-sha256$10$!!$AAAA"] {
            assert!(!secret_matches("1234", stored), "{stored}");
        }
    }

    #[test]
    fn random_salt_differs_per_hash() {
        assert_ne!(hash_secret_with("1234", b"aaaaaaaaaaaaaaaa", 5), hash_secret_with("1234", b"bbbbbbbbbbbbbbbb", 5));
    }

    #[test]
    fn new_pin_validation() {
        assert!(validate_new_pin("1234").is_ok());
        assert!(validate_new_pin("123456789012").is_ok());
        assert!(validate_new_pin("123").is_err());
        assert!(validate_new_pin("1234567890123").is_err());
        assert!(validate_new_pin("12a4").is_err());
        assert!(validate_new_pin("12 34").is_err());
        assert!(validate_new_pin("١٢٣٤").is_err()); // non-ASCII digits
    }

    #[test]
    fn first_failure_opens_window_with_max_minus_one() {
        let a = Attempts::FRESH.after_failure(1_000);
        assert_eq!(a, Attempts { window_start_ms: Some(1_000), left: MAX_FAILED_ATTEMPTS - 1 });
        assert_eq!(a.left(1_000), MAX_FAILED_ATTEMPTS - 1);
        assert_eq!(a.locked_out_until(1_000), None);
    }

    #[test]
    fn fifth_failure_locks_out_until_window_end() {
        let mut a = Attempts::FRESH;
        for i in 0..MAX_FAILED_ATTEMPTS as i64 {
            assert_eq!(a.locked_out_until(1_000 + i), None);
            a = a.after_failure(1_000 + i * 60_000);
        }
        assert_eq!(a.left, 0);
        let until = 1_000 + ATTEMPT_WINDOW_MS;
        assert_eq!(a.locked_out_until(1_000 + 5 * 60_000), Some(until));
        assert_eq!(a.locked_out_until(until - 1), Some(until));
        // Window over: full attempts again, no lockout.
        assert_eq!(a.locked_out_until(until), None);
        assert_eq!(a.left(until), MAX_FAILED_ATTEMPTS);
    }

    #[test]
    fn failure_after_window_expiry_starts_a_new_window() {
        let a = Attempts { window_start_ms: Some(0), left: 1 };
        let b = a.after_failure(ATTEMPT_WINDOW_MS + 5);
        assert_eq!(b, Attempts { window_start_ms: Some(ATTEMPT_WINDOW_MS + 5), left: MAX_FAILED_ATTEMPTS - 1 });
    }

    #[test]
    fn clock_set_back_does_not_extend_lockout() {
        let a = Attempts { window_start_ms: Some(10_000_000), left: 0 };
        assert_eq!(a.locked_out_until(5_000_000), None);
        assert_eq!(a.left(5_000_000), MAX_FAILED_ATTEMPTS);
    }

    /// `verify_limited`'s VERIFYING guard is process-global, so the tests
    /// that go through it must not run concurrently.
    static VERIFY_TESTS: std::sync::Mutex<()> = std::sync::Mutex::new(());

    async fn test_pool() -> SqlitePool {
        let pool = sqlx::sqlite::SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .unwrap();
        sqlx::query("CREATE TABLE app_setting (key TEXT PRIMARY KEY, value TEXT)")
            .execute(&pool)
            .await
            .unwrap();
        pool
    }

    #[tokio::test]
    async fn attempts_persist_and_clear() {
        let pool = test_pool().await;
        assert_eq!(read_attempts(&pool, &PIN_LIMITER).await.unwrap(), Attempts::FRESH);
        let a = Attempts { window_start_ms: Some(42), left: 3 };
        write_attempts(&pool, &PIN_LIMITER, a).await.unwrap();
        assert_eq!(read_attempts(&pool, &PIN_LIMITER).await.unwrap(), a);
        write_attempts(&pool, &PIN_LIMITER, Attempts::FRESH).await.unwrap();
        assert_eq!(read_attempts(&pool, &PIN_LIMITER).await.unwrap(), Attempts::FRESH);
    }

    #[tokio::test]
    async fn verify_counts_down_then_refuses_even_the_right_pin() {
        let _serial = VERIFY_TESTS.lock().unwrap_or_else(|e| e.into_inner());
        let pool = test_pool().await;
        write_setting(&pool, PIN_HASH_SETTING, &hash_secret_with("1234", b"0123456789abcdef", 5)).await.unwrap();
        for expected_left in (1..MAX_FAILED_ATTEMPTS).rev() {
            match verify_limited(&pool, &PIN_LIMITER, "0000").await.unwrap() {
                Verify::Wrong { left } => assert_eq!(left, expected_left),
                _ => panic!("expected Wrong"),
            }
        }
        assert!(matches!(verify_limited(&pool, &PIN_LIMITER, "0000").await.unwrap(), Verify::LockedOut { .. }));
        assert!(matches!(verify_limited(&pool, &PIN_LIMITER, "1234").await.unwrap(), Verify::LockedOut { .. }));
    }

    #[tokio::test]
    async fn success_resets_the_window() {
        let _serial = VERIFY_TESTS.lock().unwrap_or_else(|e| e.into_inner());
        let pool = test_pool().await;
        write_setting(&pool, PIN_HASH_SETTING, &hash_secret_with("1234", b"0123456789abcdef", 5)).await.unwrap();
        assert!(matches!(verify_limited(&pool, &PIN_LIMITER, "9999").await.unwrap(), Verify::Wrong { .. }));
        assert!(matches!(verify_limited(&pool, &PIN_LIMITER, "1234").await.unwrap(), Verify::Match));
        assert_eq!(read_attempts(&pool, &PIN_LIMITER).await.unwrap(), Attempts::FRESH);
    }

    // --- Recovery code ---------------------------------------------------

    #[test]
    fn generated_recovery_codes_use_the_alphabet() {
        let a = generate_recovery_code();
        let b = generate_recovery_code();
        assert_eq!(a.len(), RECOVERY_CODE_LEN);
        assert!(a.bytes().all(|c| RECOVERY_ALPHABET.contains(&c)));
        assert_ne!(a, b);
        for ambiguous in ['0', 'O', '1', 'I'] {
            assert!(!RECOVERY_ALPHABET.contains(&(ambiguous as u8)));
        }
    }

    #[test]
    fn recovery_code_display_and_normalization_round_trip() {
        let raw = "ABCDEFGHJKLMNPQR";
        let shown = format_recovery_code(raw);
        assert_eq!(shown, "ABCD-EFGH-JKLM-NPQR");
        assert_eq!(normalize_recovery_code(&shown), raw);
        assert_eq!(normalize_recovery_code(" abcd efgh-jklm  npqr "), raw);
    }

    #[test]
    fn device_local_settings_cover_the_recovery_keys() {
        for key in [
            "app_lock_pin_hash",
            "app_lock_attempts_left",
            "app_lock_window_start_ms",
            "app_lock_recovery_hash",
            "app_lock_recovery_attempts_left",
            "app_lock_recovery_window_start_ms",
        ] {
            assert!(DEVICE_LOCAL_SETTINGS.contains(&key), "{key}");
        }
    }

    async fn pool_with_pin(pin: &str) -> (SqlitePool, String) {
        let pool = test_pool().await;
        write_setting(&pool, PIN_HASH_SETTING, &hash_secret_with(pin, b"0123456789abcdef", 5)).await.unwrap();
        let code = issue_recovery_code(&pool).await.unwrap();
        (pool, code)
    }

    #[tokio::test]
    async fn recovery_replaces_the_pin_and_consumes_the_code() {
        let _serial = VERIFY_TESTS.lock().unwrap_or_else(|e| e.into_inner());
        let (pool, code) = pool_with_pin("1234").await;
        // Some failed PIN attempts that recovery should clear.
        assert!(matches!(verify_limited(&pool, &PIN_LIMITER, "0000").await.unwrap(), Verify::Wrong { .. }));

        // Lower case and no dashes is fine.
        let next = recover_with_pool(&pool, &code.replace('-', "").to_lowercase(), "9876").await.unwrap();
        assert_ne!(next, code);

        assert_eq!(read_attempts(&pool, &PIN_LIMITER).await.unwrap(), Attempts::FRESH);
        assert!(matches!(verify_limited(&pool, &PIN_LIMITER, "1234").await.unwrap(), Verify::Wrong { .. }));
        assert!(matches!(verify_limited(&pool, &PIN_LIMITER, "9876").await.unwrap(), Verify::Match));
        // The used code is dead, the new one works.
        assert!(recover_with_pool(&pool, &code, "5555").await.is_err());
        assert!(recover_with_pool(&pool, &next, "5555").await.is_ok());
    }

    #[tokio::test]
    async fn a_bad_new_pin_does_not_consume_the_code_or_an_attempt() {
        let _serial = VERIFY_TESTS.lock().unwrap_or_else(|e| e.into_inner());
        let (pool, code) = pool_with_pin("1234").await;
        assert!(recover_with_pool(&pool, &code, "12").await.is_err());
        assert_eq!(read_attempts(&pool, &RECOVERY_LIMITER).await.unwrap(), Attempts::FRESH);
        assert!(recover_with_pool(&pool, &code, "9876").await.is_ok());
    }

    #[tokio::test]
    async fn a_wrong_code_leaves_the_pin_alone() {
        let _serial = VERIFY_TESTS.lock().unwrap_or_else(|e| e.into_inner());
        let (pool, _code) = pool_with_pin("1234").await;
        let err = recover_with_pool(&pool, "AAAA-AAAA-AAAA-AAAA", "9876").await.unwrap_err();
        assert!(err.contains("incorrect"), "{err}");
        assert!(matches!(verify_limited(&pool, &PIN_LIMITER, "1234").await.unwrap(), Verify::Match));
        // Malformed input is refused without costing an attempt.
        assert!(recover_with_pool(&pool, "short", "9876").await.is_err());
        assert_eq!(
            read_attempts(&pool, &RECOVERY_LIMITER).await.unwrap().left,
            MAX_FAILED_ATTEMPTS - 1
        );
    }

    #[tokio::test]
    async fn recovery_has_its_own_attempt_window() {
        let _serial = VERIFY_TESTS.lock().unwrap_or_else(|e| e.into_inner());
        let (pool, code) = pool_with_pin("1234").await;
        // Burn every PIN attempt: PIN entry is locked out.
        for _ in 0..MAX_FAILED_ATTEMPTS {
            let _ = verify_limited(&pool, &PIN_LIMITER, "0000").await.unwrap();
        }
        assert!(matches!(verify_limited(&pool, &PIN_LIMITER, "1234").await.unwrap(), Verify::LockedOut { .. }));
        // The recovery code still works.
        assert!(recover_with_pool(&pool, &code, "9876").await.is_ok());
    }

    #[tokio::test]
    async fn recovery_attempts_run_out_even_for_the_right_code() {
        let _serial = VERIFY_TESTS.lock().unwrap_or_else(|e| e.into_inner());
        let (pool, code) = pool_with_pin("1234").await;
        for _ in 0..MAX_FAILED_ATTEMPTS {
            assert!(recover_with_pool(&pool, "AAAA-AAAA-AAAA-AAAA", "9876").await.is_err());
        }
        let err = recover_with_pool(&pool, &code, "9876").await.unwrap_err();
        assert!(err.starts_with("Too many wrong recovery codes"), "{err}");
        // The PIN window was never touched.
        assert_eq!(read_attempts(&pool, &PIN_LIMITER).await.unwrap(), Attempts::FRESH);
    }

    #[tokio::test]
    async fn no_recovery_code_means_recovery_is_refused() {
        let _serial = VERIFY_TESTS.lock().unwrap_or_else(|e| e.into_inner());
        let pool = test_pool().await;
        write_setting(&pool, PIN_HASH_SETTING, &hash_secret_with("1234", b"0123456789abcdef", 5)).await.unwrap();
        assert!(!has_recovery_code(&pool).await.unwrap());
        let err = recover_with_pool(&pool, "AAAA-AAAA-AAAA-AAAA", "9876").await.unwrap_err();
        assert_eq!(err, "This device has no recovery code.");
        assert!(matches!(verify_limited(&pool, &PIN_LIMITER, "1234").await.unwrap(), Verify::Match));
    }
}
