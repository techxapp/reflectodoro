//! Where the desktop at-rest key (see crypto.rs) lives, and the user-facing
//! controls over that: CLAUDE.md's "Encryption at rest" -> "Key storage".
//!
//! Two places, exactly one of which holds the key at a time:
//! - **The OS vault** (Keychain / Credential Manager / Secret Service, via
//!   `keyring`). The default: the key is generated automatically and the user
//!   never types anything.
//! - **A password-protected file** (`encryption.key` in `app_config_dir()`).
//!   Used when the vault can't persist a key, or when the user moves the key
//!   there from Settings. The file holds the key wrapped under an Argon2id-
//!   derived key-encryption key, so it is useless without the password. The
//!   password is asked for on every launch and only the unwrapped key is kept,
//!   in memory, for the life of the process -- never the password itself.
//!
//! Moving between the two moves the *same* 32-byte key, so no row is ever
//! re-encrypted. Builds before this module wrote the file as the raw key in
//! plaintext base64 ("legacy"); such a file is still read, but the app stays
//! locked until the user wraps it with a password (or moves it to the vault).
//!
//! **Never auto-create a key over existing data.** The old loader generated a
//! fresh key whenever it found none -- including when the vault was merely
//! unreachable (Secret Service not up yet at login, a denied Keychain prompt)
//! or its entry had gone missing, which silently orphaned every encrypted row
//! and then wrote the new key to a file that shadowed the real one forever
//! after. Now a missing key over a database that already holds `enc1:` rows
//! is the `KeyMissing` state: the UI offers Retry, restore from a backed-up
//! key, or an explicit, confirmed reset.
//!
//! Android has none of this -- its key is a non-exportable Keystore key (see
//! crypto.rs), with no vault/file split and no "denied" state -- so every
//! command here reports `mode: "keystore"` or refuses there.

use serde::Serialize;
use tauri::AppHandle;

/// Prefix of the error `FieldCipher::resolve` returns while the key is
/// locked. db.ts's `isKeyLockedError` matches on it, so the frontend can tell
/// "unlock first" apart from a real failure.
#[cfg(not(target_os = "android"))]
pub const LOCKED_PREFIX: &str = "KEY_LOCKED:";

/// Emitted (no payload) whenever the key's state changes, so every window's
/// unlock modal and Settings card refetch `get_key_storage_status`.
#[cfg(not(target_os = "android"))]
const STATE_EVENT: &str = "crypto://state";

/// `app_setting` key remembering where the desktop key lives ("vault" /
/// "file"), so a refused vault isn't asked again every launch (see
/// `resolve`). Device-local: excluded from data export/import (db.ts's
/// `exportAllData`, import.rs), since another device's answer says nothing
/// about where *this* device's key is.
pub const LOCATION_SETTING: &str = "encryption_key_location";

#[derive(Serialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct KeyStorageStatus {
    /// "vault" | "password_file" | "keystore" (Android) | "none" (not settled)
    mode: &'static str,
    /// "unlocked" | "locked" | "needs_password" | "key_missing"
    state: &'static str,
    /// `needs_password` because a pre-password plaintext key file was found,
    /// as opposed to a brand-new key the vault wouldn't keep.
    legacy: bool,
    /// Whether the OS vault answered the last time it was asked. Cached rather
    /// than re-probed per status call: on an unsigned macOS build, each vault
    /// read can put a Keychain prompt in front of the user.
    vault_available: bool,
}

// --- Android ---------------------------------------------------------------

#[cfg(target_os = "android")]
mod platform {
    use super::*;

    const UNSUPPORTED: &str = "Android keeps its key in the Keystore; there is nothing to manage here";

    pub async fn status(_app: &AppHandle) -> Result<KeyStorageStatus, String> {
        Ok(KeyStorageStatus { mode: "keystore", state: "unlocked", legacy: false, vault_available: true })
    }
    pub async fn ensure_resolved(_app: &AppHandle) {}
    pub fn is_locked() -> bool {
        false
    }
    pub async fn retry(_app: &AppHandle) -> Result<(), String> {
        Err(UNSUPPORTED.into())
    }
    pub async fn unlock(_app: &AppHandle, _password: String) -> Result<(), String> {
        Err(UNSUPPORTED.into())
    }
    pub async fn set_password(_app: &AppHandle, _password: String) -> Result<(), String> {
        Err(UNSUPPORTED.into())
    }
    pub async fn change_password(_app: &AppHandle, _old: String, _new: String) -> Result<(), String> {
        Err(UNSUPPORTED.into())
    }
    pub async fn migrate_to_file(_app: &AppHandle, _password: String) -> Result<(), String> {
        Err(UNSUPPORTED.into())
    }
    pub async fn migrate_to_vault(_app: &AppHandle) -> Result<(), String> {
        Err(UNSUPPORTED.into())
    }
    pub async fn reveal(_app: &AppHandle, _password: Option<String>) -> Result<String, String> {
        Err(UNSUPPORTED.into())
    }
    pub async fn restore(
        _app: &AppHandle,
        _key: String,
        _target: String,
        _password: Option<String>,
    ) -> Result<(), String> {
        Err(UNSUPPORTED.into())
    }
    pub async fn reset(_app: &AppHandle, _target: String, _password: Option<String>) -> Result<(), String> {
        Err(UNSUPPORTED.into())
    }
}

// --- Desktop ---------------------------------------------------------------

#[cfg(not(target_os = "android"))]
pub(crate) use platform::key;

#[cfg(not(target_os = "android"))]
mod platform {
    use super::*;
    use crate::crypto::{decrypt_with_key, KEY_LEN};
    use base64::engine::general_purpose::STANDARD as BASE64;
    use base64::Engine as _;
    use chacha20poly1305::aead::{Aead, AeadCore, KeyInit, OsRng, Payload};
    use chacha20poly1305::XChaCha20Poly1305;
    use serde::Deserialize;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Mutex;
    use tauri::Emitter;
    use zeroize::Zeroizing;

    type Key = [u8; KEY_LEN];

    /// Identifies this app's entry in the OS credential store. Deliberately
    /// the same bundle identifier used for `app_config_dir()`/`app_log_dir()`.
    const KEYRING_SERVICE: &str = "com.reflectodoro.app";
    const KEYRING_ENTRY: &str = "db-encryption-key";

    /// Alongside `pomodoro.db` in `app_config_dir()`.
    const KEY_FILE: &str = "encryption.key";

    /// Argon2id cost for new key files: 64 MiB, 3 passes -- OWASP's
    /// recommended floor, roughly half a second on a current laptop. Paid
    /// once per launch (unlock) and on Settings actions, never per field.
    const KDF_M_KIB: u32 = 64 * 1024;
    const KDF_T: u32 = 3;
    const KDF_P: u32 = 1;
    /// Upper bounds accepted when *reading* a file's parameters, so a
    /// tampered file can't make an unlock attempt allocate or spin forever.
    const KDF_MAX_M_KIB: u32 = 1024 * 1024;
    const KDF_MAX_T: u32 = 16;
    const KDF_MAX_P: u32 = 16;
    const SALT_LEN: usize = 16;
    const NONCE_LEN: usize = 24;
    /// Binds the wrapped key to this purpose/format version.
    const KEYFILE_AAD: &[u8] = b"reflectodoro/keyfile/v1";

    pub const MIN_PASSWORD_CHARS: usize = 8;

    // --- State ---------------------------------------------------------

    #[derive(Clone, Copy, PartialEq, Eq, Debug)]
    enum Location {
        Vault,
        File,
    }

    #[derive(Clone, Copy, PartialEq, Eq, Debug)]
    enum KeyState {
        Unlocked(Location),
        /// A wrapped key file exists and the vault has no key: needs the password.
        FileLocked,
        /// A key is in hand (`Inner::pending`) but isn't stored anywhere
        /// acceptable yet: either a legacy plaintext file, or a new key the
        /// vault wouldn't persist. Locked until it's wrapped with a password
        /// (or, if the vault now works, moved there).
        NeedsPassword { legacy: bool },
        /// No usable key anywhere, but the database already holds ciphertext.
        KeyMissing,
    }

    struct Inner {
        /// `None` until the first resolution finishes.
        state: Option<KeyState>,
        /// Set exactly when `state` is `Unlocked`.
        key: Option<Key>,
        pending: Option<Key>,
        vault_available: bool,
    }

    static INNER: Mutex<Inner> = Mutex::new(Inner { state: None, key: None, pending: None, vault_available: false });
    /// Serializes resolution and every key-management command, so two windows
    /// (or a command racing startup's resolution) can never interleave a
    /// vault write with a file delete.
    static OP_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());
    /// Mirror of "resolved and not unlocked", readable synchronously by
    /// screen_time.rs's flush path.
    static LOCKED: AtomicBool = AtomicBool::new(false);

    fn locked_error() -> String {
        format!("{LOCKED_PREFIX} the encryption key is locked -- unlock it to continue")
    }

    fn set_state(app: &AppHandle, state: KeyState, key: Option<Key>, pending: Option<Key>) {
        {
            let mut inner = INNER.lock().unwrap();
            inner.state = Some(state);
            inner.key = key;
            inner.pending = pending;
        }
        LOCKED.store(!matches!(state, KeyState::Unlocked(_)), Ordering::SeqCst);
        log::info!("crypto: key state is now {state:?}");
        if let Err(e) = app.emit(STATE_EVENT, ()) {
            log::warn!("crypto: failed to emit {STATE_EVENT}: {e}");
        }
    }

    fn set_vault_available(available: bool) {
        INNER.lock().unwrap().vault_available = available;
    }

    fn current_state() -> Option<KeyState> {
        INNER.lock().unwrap().state
    }

    fn current_key() -> Option<Key> {
        INNER.lock().unwrap().key
    }

    /// `FieldCipher::resolve`'s key source. The fast path is one mutex read;
    /// the first call per process resolves where the key lives.
    pub(crate) async fn key(app: &AppHandle) -> Result<Key, String> {
        if let Some(k) = current_key() {
            return Ok(k);
        }
        ensure_resolved(app).await;
        current_key().ok_or_else(locked_error)
    }

    pub fn is_locked() -> bool {
        LOCKED.load(Ordering::SeqCst)
    }

    pub async fn ensure_resolved(app: &AppHandle) {
        if current_state().is_some() {
            return;
        }
        let _guard = OP_LOCK.lock().await;
        if current_state().is_none() {
            resolve(app).await;
        }
    }

    /// Runs with `OP_LOCK` held.
    ///
    /// **The vault is only asked when it might actually hold the key.** On an
    /// unsigned macOS build every Keychain read the user has refused puts the
    /// permission prompt back up, so once the key is known to live in the
    /// password-protected file (`app_setting.encryption_key_location` =
    /// `file`) -- or, on an install from before that flag existed, once a key
    /// file is simply present, which only ever happened after the vault failed
    /// -- the vault is not touched at all, not even to probe it. Moving back
    /// to the vault is an explicit Settings action.
    async fn resolve(app: &AppHandle) {
        let flag = match read_location_flag(app).await {
            Ok(flag) => flag,
            Err(e) => {
                log::warn!("crypto: couldn't read the remembered key location ({e})");
                None
            }
        };
        let app_blocking = app.clone();
        let file = blocking(move || read_key_file(&app_blocking)).await;
        let skip_vault = match flag {
            Some(Location::File) => true,
            Some(Location::Vault) => false,
            None => !matches!(file, Ok(KeyFile::Missing)),
        };

        let mut probe = Probe { vault: Ok(None), vault_skipped: skip_vault, file };
        if !skip_vault {
            probe.vault = blocking(keyring_get).await;
        }

        // Pre-flag installs only: a plaintext key file with no record of where
        // the key lives is *probably* the key in use, but an old build could
        // leave one behind next to a vault key after a single flaky launch.
        // Prove it against real ciphertext before skipping the vault for good.
        if flag.is_none() && skip_vault {
            if let Ok(KeyFile::Legacy(k)) = &probe.file {
                if let Ok(Some(sample)) = sample_encrypted_value(app).await {
                    if decrypt_with_key(k, &sample).is_err() {
                        log::warn!("crypto: the old key file doesn't match the data; checking the OS vault after all");
                        probe.vault_skipped = false;
                        probe.vault = blocking(keyring_get).await;
                    }
                }
            }
        }

        if let Err(e) = &probe.vault {
            log::warn!("crypto: OS credential store unreadable ({e})");
        }
        set_vault_available(!probe.vault_skipped && probe.vault.is_ok());

        // Only worth a database round trip when no key was found at all.
        let has_data = if probe.needs_data_check() {
            match sample_encrypted_value(app).await {
                Ok(sample) => sample.is_some(),
                Err(e) => {
                    // Can't tell -- assume there is data, so a key is never
                    // generated over rows it can't read.
                    log::warn!("crypto: couldn't check for existing encrypted data ({e}); assuming some exists");
                    true
                }
            }
        } else {
            false
        };

        match decide(&probe, has_data) {
            Decision::UseVault(k) => {
                persist_location(app, Location::Vault).await;
                set_state(app, KeyState::Unlocked(Location::Vault), Some(k), None)
            }
            Decision::FileLocked => {
                persist_location(app, Location::File).await;
                set_state(app, KeyState::FileLocked, None, None)
            }
            Decision::Legacy(k) => {
                log::warn!("crypto: found an unprotected key file; locked until it is given a password");
                persist_location(app, Location::File).await;
                set_state(app, KeyState::NeedsPassword { legacy: true }, None, Some(k))
            }
            Decision::KeyMissing => {
                log::warn!("crypto: no usable key found, but the database holds encrypted data; not generating a new key");
                set_state(app, KeyState::KeyMissing, None, None)
            }
            Decision::CreateNewInFile => {
                log::info!("crypto: no key yet and the key is set to live in a file; waiting for a password");
                set_state(app, KeyState::NeedsPassword { legacy: false }, None, Some(random_key()))
            }
            Decision::CreateNew => {
                let k = random_key();
                let stored = blocking(move || keyring_set_verified(&k)).await;
                match stored {
                    Ok(()) => {
                        log::info!("crypto: generated a new at-rest key and stored it in the OS credential store");
                        set_vault_available(true);
                        persist_location(app, Location::Vault).await;
                        set_state(app, KeyState::Unlocked(Location::Vault), Some(k), None);
                    }
                    Err(e) => {
                        log::warn!(
                            "crypto: OS credential store did not persist the key ({e}); \
                             waiting for a password to protect a key file instead"
                        );
                        set_vault_available(false);
                        // Remembered right away, before any password is set:
                        // otherwise a restart before the user answers would
                        // ask the vault (and re-prompt) all over again.
                        persist_location(app, Location::File).await;
                        set_state(app, KeyState::NeedsPassword { legacy: false }, None, Some(k));
                    }
                }
            }
        }
    }

    struct Probe {
        vault: Result<Option<Key>, String>,
        /// The vault wasn't asked (see `resolve`) -- `vault` is a placeholder.
        vault_skipped: bool,
        file: Result<KeyFile, String>,
    }

    impl Probe {
        fn needs_data_check(&self) -> bool {
            !matches!(self.vault, Ok(Some(_))) && matches!(self.file, Ok(KeyFile::Missing))
        }
    }

    #[derive(Debug, PartialEq)]
    enum Decision {
        UseVault(Key),
        FileLocked,
        Legacy(Key),
        KeyMissing,
        CreateNew,
        /// Nothing to lose and the vault is off-limits: a new key that goes
        /// straight to a password-protected file.
        CreateNewInFile,
    }

    /// Pure, so the "never create a key over existing data" rule is unit
    /// tested without a vault or a filesystem. The vault wins when both hold
    /// a key: only an interrupted migration leaves both, and then they're
    /// the same key.
    fn decide(probe: &Probe, has_encrypted_data: bool) -> Decision {
        if let Ok(Some(k)) = probe.vault {
            return Decision::UseVault(k);
        }
        match &probe.file {
            // Unreadable or corrupt: don't overwrite what might be the only
            // copy of the key. The user can still restore or reset explicitly.
            Err(_) => Decision::KeyMissing,
            Ok(KeyFile::Wrapped(_)) => Decision::FileLocked,
            Ok(KeyFile::Legacy(k)) => Decision::Legacy(*k),
            Ok(KeyFile::Missing) if has_encrypted_data => Decision::KeyMissing,
            Ok(KeyFile::Missing) if probe.vault_skipped => Decision::CreateNewInFile,
            Ok(KeyFile::Missing) => Decision::CreateNew,
        }
    }

    // --- Remembered location (app_setting.encryption_key_location) ----------

    async fn read_location_flag(app: &AppHandle) -> Result<Option<Location>, String> {
        let pool = crate::db::open_direct_pool(app).await?;
        let value: Result<Option<String>, _> = sqlx::query_scalar("SELECT value FROM app_setting WHERE key = ?")
            .bind(LOCATION_SETTING)
            .fetch_optional(&pool)
            .await;
        pool.close().await;
        Ok(match value.map_err(|e| e.to_string())?.as_deref() {
            Some("vault") => Some(Location::Vault),
            Some("file") => Some(Location::File),
            _ => None,
        })
    }

    /// Best-effort: a failed write only means the next launch works it out
    /// from what's on disk again, as a pre-flag install does.
    async fn persist_location(app: &AppHandle, location: Location) {
        let value = match location {
            Location::Vault => "vault",
            Location::File => "file",
        };
        let result = async {
            let pool = crate::db::open_direct_pool(app).await?;
            let r = sqlx::query(
                "INSERT INTO app_setting (key, value) VALUES (?, ?)
                 ON CONFLICT(key) DO UPDATE SET value = excluded.value WHERE value <> excluded.value",
            )
            .bind(LOCATION_SETTING)
            .bind(value)
            .execute(&pool)
            .await;
            pool.close().await;
            r.map(|_| ()).map_err(|e| e.to_string())
        }
        .await;
        if let Err(e) = result {
            log::warn!("crypto: couldn't remember the key location ({e})");
        }
    }

    // --- Commands --------------------------------------------------------

    pub async fn status(app: &AppHandle) -> Result<KeyStorageStatus, String> {
        ensure_resolved(app).await;
        let inner = INNER.lock().unwrap();
        let (mode, state, legacy) = match inner.state {
            Some(KeyState::Unlocked(Location::Vault)) => ("vault", "unlocked", false),
            Some(KeyState::Unlocked(Location::File)) => ("password_file", "unlocked", false),
            Some(KeyState::FileLocked) => ("password_file", "locked", false),
            Some(KeyState::NeedsPassword { legacy }) => ("none", "needs_password", legacy),
            Some(KeyState::KeyMissing) => ("none", "key_missing", false),
            None => ("none", "key_missing", false),
        };
        Ok(KeyStorageStatus { mode, state, legacy, vault_available: inner.vault_available })
    }

    /// Re-runs resolution from scratch -- "Retry" after granting vault
    /// access, or once Secret Service is up. A no-op while unlocked.
    pub async fn retry(app: &AppHandle) -> Result<(), String> {
        let _guard = OP_LOCK.lock().await;
        if matches!(current_state(), Some(KeyState::Unlocked(_))) {
            return Ok(());
        }
        resolve(app).await;
        Ok(())
    }

    pub async fn unlock(app: &AppHandle, password: String) -> Result<(), String> {
        let password = Zeroizing::new(password);
        let _guard = OP_LOCK.lock().await;
        ensure_resolved_locked(app).await;
        match current_state() {
            Some(KeyState::Unlocked(_)) => return Ok(()),
            Some(KeyState::FileLocked) => {}
            _ => return Err("There is no password-protected key file to unlock".into()),
        }
        let app_blocking = app.clone();
        let k = blocking(move || unwrap_file(&app_blocking, &password)).await?;
        set_state(app, KeyState::Unlocked(Location::File), Some(k), None);
        Ok(())
    }

    /// Protects a `NeedsPassword` key (legacy plaintext file or a new key the
    /// vault wouldn't keep) with a password, replacing any plaintext file.
    pub async fn set_password(app: &AppHandle, password: String) -> Result<(), String> {
        let password = Zeroizing::new(password);
        validate_new_password(&password)?;
        let _guard = OP_LOCK.lock().await;
        ensure_resolved_locked(app).await;
        let Some(KeyState::NeedsPassword { .. }) = current_state() else {
            return Err("The encryption key doesn't need a new password right now".into());
        };
        let k = INNER.lock().unwrap().pending.ok_or("no pending key")?;
        let app_blocking = app.clone();
        blocking(move || write_wrapped_file(&app_blocking, &k, &password)).await?;
        persist_location(app, Location::File).await;
        set_state(app, KeyState::Unlocked(Location::File), Some(k), None);
        Ok(())
    }

    pub async fn change_password(app: &AppHandle, old: String, new: String) -> Result<(), String> {
        let (old, new) = (Zeroizing::new(old), Zeroizing::new(new));
        validate_new_password(&new)?;
        let _guard = OP_LOCK.lock().await;
        let (Some(KeyState::Unlocked(Location::File)), Some(k)) = (current_state(), current_key()) else {
            return Err("The key isn't in an unlocked password-protected file".into());
        };
        let app_blocking = app.clone();
        blocking(move || {
            if unwrap_file(&app_blocking, &old)? != k {
                return Err("The key file no longer matches the key in use".to_string());
            }
            write_wrapped_file(&app_blocking, &k, &new)
        })
        .await?;
        log::info!("crypto: key file password changed");
        Ok(())
    }

    pub async fn migrate_to_file(app: &AppHandle, password: String) -> Result<(), String> {
        let password = Zeroizing::new(password);
        validate_new_password(&password)?;
        let _guard = OP_LOCK.lock().await;
        let (Some(KeyState::Unlocked(Location::Vault)), Some(k)) = (current_state(), current_key()) else {
            return Err("The key isn't unlocked in the OS vault".into());
        };
        let app_blocking = app.clone();
        blocking(move || {
            write_wrapped_file(&app_blocking, &k, &password)?;
            // The vault would win over the file on the next launch, so a copy
            // left behind there would make this move silently not happen.
            if let Err(e) = keyring_delete() {
                let _ = remove_key_file(&app_blocking);
                return Err(format!("Couldn't remove the key from the OS vault ({e}); nothing was changed"));
            }
            Ok(())
        })
        .await?;
        persist_location(app, Location::File).await;
        set_state(app, KeyState::Unlocked(Location::File), Some(k), None);
        Ok(())
    }

    /// Also accepted from `NeedsPassword`: the forced set-password prompt
    /// offers "use the OS vault instead" when the vault works now.
    pub async fn migrate_to_vault(app: &AppHandle) -> Result<(), String> {
        let _guard = OP_LOCK.lock().await;
        ensure_resolved_locked(app).await;
        let k = match current_state() {
            Some(KeyState::Unlocked(Location::File)) => current_key(),
            Some(KeyState::NeedsPassword { .. }) => INNER.lock().unwrap().pending,
            _ => None,
        }
        .ok_or("The key isn't available to move to the OS vault")?;
        let app_blocking = app.clone();
        blocking(move || {
            if let Err(e) = keyring_set_verified(&k) {
                set_vault_available(false);
                return Err(format!("The OS vault didn't keep the key ({e})"));
            }
            set_vault_available(true);
            if let Err(e) = remove_key_file(&app_blocking) {
                // A file left behind is harmless (the vault wins on load), but
                // a legacy one is the plaintext key -- roll back rather than
                // report success while it's still on disk.
                let _ = keyring_delete();
                return Err(format!("Couldn't remove the key file ({e}); nothing was changed"));
            }
            Ok(())
        })
        .await?;
        persist_location(app, Location::Vault).await;
        set_state(app, KeyState::Unlocked(Location::Vault), Some(k), None);
        Ok(())
    }

    /// Returns the raw key as base64 for the user to back up. File mode
    /// re-checks the password against the file; vault mode relies on the UI's
    /// confirm. Never logged.
    pub async fn reveal(app: &AppHandle, password: Option<String>) -> Result<String, String> {
        let password = password.map(Zeroizing::new);
        let _guard = OP_LOCK.lock().await;
        let (Some(KeyState::Unlocked(location)), Some(k)) = (current_state(), current_key()) else {
            return Err(locked_error());
        };
        if location == Location::File {
            let password = password.ok_or("Enter the key file password to show the key")?;
            let app_blocking = app.clone();
            if blocking(move || unwrap_file(&app_blocking, &password)).await? != k {
                return Err("The key file no longer matches the key in use".into());
            }
        }
        log::info!("crypto: encryption key revealed to the user");
        Ok(BASE64.encode(k))
    }

    /// Stores a backed-up key, after proving it decrypts this database's
    /// existing data -- a typo or another device's key would otherwise make
    /// every row unreadable.
    pub async fn restore(
        app: &AppHandle,
        key_b64: String,
        target: String,
        password: Option<String>,
    ) -> Result<(), String> {
        let key_b64 = Zeroizing::new(key_b64);
        let password = password.map(Zeroizing::new);
        let compact: Zeroizing<String> = Zeroizing::new(key_b64.chars().filter(|c| !c.is_whitespace()).collect());
        let k = decode_key(&compact).map_err(|_| "That isn't a valid encryption key".to_string())?;
        let location = parse_target(&target)?;
        if location == Location::File {
            validate_new_password(password.as_deref().ok_or("Choose a password for the key file")?)?;
        }

        let _guard = OP_LOCK.lock().await;
        if let Some(sample) = sample_encrypted_value(app).await? {
            if decrypt_with_key(&k, &sample).is_err() {
                return Err("This key doesn't match your existing data".into());
            }
        }
        store_key(app, k, location, password).await?;
        log::info!("crypto: key restored from backup into {location:?}");
        set_state(app, KeyState::Unlocked(location), Some(k), None);
        Ok(())
    }

    /// "Forgot password" / key-missing escape: a brand-new key. Existing
    /// encrypted rows become unreadable -- the UI confirms that twice, and the
    /// Entries page's delete-day buttons clear them afterwards. Refused while
    /// unlocked, where it could only ever destroy readable data.
    pub async fn reset(app: &AppHandle, target: String, password: Option<String>) -> Result<(), String> {
        let password = password.map(Zeroizing::new);
        let location = parse_target(&target)?;
        if location == Location::File {
            validate_new_password(password.as_deref().ok_or("Choose a password for the key file")?)?;
        }
        let _guard = OP_LOCK.lock().await;
        ensure_resolved_locked(app).await;
        if !matches!(current_state(), Some(KeyState::FileLocked | KeyState::KeyMissing)) {
            return Err("The key can only be reset while it's locked or missing".into());
        }
        let k = random_key();
        store_key(app, k, location, password).await?;
        log::warn!("crypto: encryption key reset by the user; previously encrypted rows are now unreadable");
        set_state(app, KeyState::Unlocked(location), Some(k), None);
        Ok(())
    }

    // --- Helpers ---------------------------------------------------------

    /// `ensure_resolved` for callers already holding `OP_LOCK`.
    async fn ensure_resolved_locked(app: &AppHandle) {
        if current_state().is_none() {
            resolve(app).await;
        }
    }

    async fn blocking<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> T {
        // keyring (D-Bus, Keychain) and Argon2 both block; keep them off the
        // async runtime's worker threads.
        tauri::async_runtime::spawn_blocking(f).await.expect("key store task panicked")
    }

    fn parse_target(target: &str) -> Result<Location, String> {
        match target {
            "vault" => Ok(Location::Vault),
            "file" => Ok(Location::File),
            other => Err(format!("unknown key location {other:?}")),
        }
    }

    fn validate_new_password(password: &str) -> Result<(), String> {
        if password.chars().count() < MIN_PASSWORD_CHARS {
            return Err(format!("Use at least {MIN_PASSWORD_CHARS} characters"));
        }
        Ok(())
    }

    /// Writes `k` to `location` and removes the other location's copy. The
    /// vault wins on load, so a stale vault entry after a file store is the
    /// one leftover that matters; it's removed best-effort (the vault may be
    /// the very thing that's unreachable).
    async fn store_key(
        app: &AppHandle,
        k: Key,
        location: Location,
        password: Option<Zeroizing<String>>,
    ) -> Result<(), String> {
        let app_blocking = app.clone();
        blocking(move || match location {
            Location::Vault => {
                if let Err(e) = keyring_set_verified(&k) {
                    set_vault_available(false);
                    return Err(format!("The OS vault didn't keep the key ({e})"));
                }
                set_vault_available(true);
                if let Err(e) = remove_key_file(&app_blocking) {
                    log::warn!("crypto: couldn't remove the old key file ({e})");
                }
                Ok(())
            }
            Location::File => {
                let password = password.ok_or("Choose a password for the key file")?;
                write_wrapped_file(&app_blocking, &k, &password)?;
                if let Err(e) = keyring_delete() {
                    log::warn!("crypto: couldn't remove the old OS vault entry ({e})");
                }
                Ok(())
            }
        })
        .await?;
        persist_location(app, location).await;
        Ok(())
    }

    /// One `enc1:` value from this database, if any -- both "is there data a
    /// new key would orphan?" and the known ciphertext a restored key must
    /// decrypt. Tables in rough order of how likely they are to hold one.
    async fn sample_encrypted_value(app: &AppHandle) -> Result<Option<String>, String> {
        const COLUMNS: &[(&str, &str)] = &[
            ("reflection", "text"),
            ("daily_task_list", "content"),
            ("not_to_do_list", "content"),
            ("wellness_check", "relaxed_eyes"),
            ("screen_time_session", "app_id"),
            ("bulk_edit_preset", "text"),
            ("paired_device", "name"),
        ];
        let pool = crate::db::open_direct_pool(app).await?;
        let mut found = None;
        for (table, column) in COLUMNS {
            let sql = format!("SELECT {column} FROM {table} WHERE {column} LIKE 'enc1:%' LIMIT 1");
            match sqlx::query_scalar::<_, String>(&sql).fetch_optional(&pool).await {
                Ok(Some(v)) => {
                    found = Some(v);
                    break;
                }
                Ok(None) => {}
                Err(e) => {
                    pool.close().await;
                    return Err(format!("couldn't read {table}: {e}"));
                }
            }
        }
        pool.close().await;
        Ok(found)
    }

    fn random_key() -> Key {
        use chacha20poly1305::aead::rand_core::RngCore;
        let mut key = [0u8; KEY_LEN];
        OsRng.fill_bytes(&mut key);
        key
    }

    fn decode_key(encoded: &str) -> Result<Key, String> {
        let bytes = BASE64.decode(encoded).map_err(|e| format!("stored key is not valid base64: {e}"))?;
        bytes.try_into().map_err(|_| "stored key is not the expected length".to_string())
    }

    // --- OS vault --------------------------------------------------------

    fn keyring_entry() -> Result<keyring::Entry, String> {
        keyring::Entry::new(KEYRING_SERVICE, KEYRING_ENTRY).map_err(|e| e.to_string())
    }

    /// `Ok(None)` means "no entry yet", which is not an error -- distinct
    /// from `Err`, which means the store itself couldn't be reached.
    fn keyring_get() -> Result<Option<Key>, String> {
        match keyring_entry()?.get_password() {
            Ok(encoded) => decode_key(&encoded).map(Some),
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(e) => Err(e.to_string()),
        }
    }

    /// Verified, not assumed: keyring built without a real backend silently
    /// substitutes an in-memory store, and a key that evaporates on exit would
    /// leave every row written this session unreadable next launch.
    fn keyring_set_verified(key: &Key) -> Result<(), String> {
        keyring_entry()?.set_password(&BASE64.encode(key)).map_err(|e| e.to_string())?;
        // Deliberately a fresh Entry, not the one just written through:
        // reading back via the same handle could be satisfied by an
        // in-process cache and would prove nothing about persistence.
        match keyring_get()? {
            Some(stored) if stored == *key => Ok(()),
            Some(_) => Err("credential store returned a different key than was just written".to_string()),
            None => Err("credential store reported no entry immediately after writing one".to_string()),
        }
    }

    fn keyring_delete() -> Result<(), String> {
        match keyring_entry()?.delete_credential() {
            Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
            Err(e) => Err(e.to_string()),
        }
    }

    // --- Key file --------------------------------------------------------

    #[derive(Debug, PartialEq)]
    enum KeyFile {
        Missing,
        /// Pre-password builds: the raw key as base64.
        Legacy(Key),
        /// The JSON body of a `WrappedKeyFile`.
        Wrapped(String),
    }

    #[derive(Serialize, Deserialize)]
    struct WrappedKeyFile {
        v: u32,
        kdf: String,
        m: u32,
        t: u32,
        p: u32,
        salt: String,
        nonce: String,
        ct: String,
    }

    fn key_file_path(app: &AppHandle) -> Result<std::path::PathBuf, String> {
        use tauri::Manager;
        let dir = app.path().app_config_dir().map_err(|e| format!("no app config dir: {e}"))?;
        std::fs::create_dir_all(&dir).map_err(|e| format!("couldn't create app config dir: {e}"))?;
        Ok(dir.join(KEY_FILE))
    }

    fn read_key_file(app: &AppHandle) -> Result<KeyFile, String> {
        let path = key_file_path(app)?;
        match std::fs::read_to_string(&path) {
            Ok(contents) => parse_key_file(&contents),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(KeyFile::Missing),
            Err(e) => Err(format!("couldn't read key file: {e}")),
        }
    }

    fn parse_key_file(contents: &str) -> Result<KeyFile, String> {
        let trimmed = contents.trim();
        if trimmed.starts_with('{') {
            // Validate the shape now, so a corrupt file surfaces as
            // KeyMissing at startup rather than as a "wrong password" later.
            serde_json::from_str::<WrappedKeyFile>(trimmed).map_err(|e| format!("key file is corrupt: {e}"))?;
            Ok(KeyFile::Wrapped(trimmed.to_string()))
        } else {
            decode_key(trimmed).map(KeyFile::Legacy)
        }
    }

    fn unwrap_file(app: &AppHandle, password: &str) -> Result<Key, String> {
        match read_key_file(app)? {
            KeyFile::Wrapped(json) => unwrap_key(&json, password),
            KeyFile::Legacy(_) => Err("The key file isn't password-protected".into()),
            KeyFile::Missing => Err("The key file is missing".into()),
        }
    }

    fn remove_key_file(app: &AppHandle) -> Result<(), String> {
        match std::fs::remove_file(key_file_path(app)?) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(e.to_string()),
        }
    }

    /// Temp file + rename, so a crash mid-write can't leave a truncated file
    /// in place of the only copy of the key; then read back and unwrapped
    /// before anything is allowed to depend on it.
    fn write_wrapped_file(app: &AppHandle, key: &Key, password: &str) -> Result<(), String> {
        use std::io::Write;
        let json = wrap_key(key, password)?;
        let path = key_file_path(app)?;
        let tmp = path.with_extension("key.tmp");
        {
            let mut f = std::fs::File::create(&tmp).map_err(|e| format!("couldn't write key file: {e}"))?;
            // Windows already restricts %APPDATA% to the owning user by
            // default ACL; Unix umasks vary enough to set it explicitly.
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                f.set_permissions(std::fs::Permissions::from_mode(0o600))
                    .map_err(|e| format!("couldn't restrict key file permissions: {e}"))?;
            }
            f.write_all(json.as_bytes()).map_err(|e| format!("couldn't write key file: {e}"))?;
            f.sync_all().map_err(|e| format!("couldn't flush key file: {e}"))?;
        }
        std::fs::rename(&tmp, &path).map_err(|e| format!("couldn't replace key file: {e}"))?;
        if unwrap_file(app, password)? != *key {
            return Err("key file didn't read back as the key just written".into());
        }
        Ok(())
    }

    fn derive_kek(password: &str, salt: &[u8], m: u32, t: u32, p: u32) -> Result<Zeroizing<Key>, String> {
        use argon2::{Algorithm, Argon2, Params, Version};
        let params = Params::new(m, t, p, Some(KEY_LEN)).map_err(|e| format!("bad key file parameters: {e}"))?;
        let mut kek = Zeroizing::new([0u8; KEY_LEN]);
        Argon2::new(Algorithm::Argon2id, Version::V0x13, params)
            .hash_password_into(password.as_bytes(), salt, kek.as_mut())
            .map_err(|e| format!("key derivation failed: {e}"))?;
        Ok(kek)
    }

    fn wrap_key(key: &Key, password: &str) -> Result<String, String> {
        wrap_key_with_params(key, password, KDF_M_KIB, KDF_T, KDF_P)
    }

    fn wrap_key_with_params(key: &Key, password: &str, m: u32, t: u32, p: u32) -> Result<String, String> {
        use chacha20poly1305::aead::rand_core::RngCore;
        let mut salt = [0u8; SALT_LEN];
        OsRng.fill_bytes(&mut salt);
        let kek = derive_kek(password, &salt, m, t, p)?;
        let nonce = XChaCha20Poly1305::generate_nonce(&mut OsRng);
        let ct = XChaCha20Poly1305::new(kek.as_ref().into())
            .encrypt(&nonce, Payload { msg: key, aad: KEYFILE_AAD })
            .map_err(|e| format!("key wrapping failed: {e}"))?;
        serde_json::to_string(&WrappedKeyFile {
            v: 1,
            kdf: "argon2id".into(),
            m,
            t,
            p,
            salt: BASE64.encode(salt),
            nonce: BASE64.encode(nonce),
            ct: BASE64.encode(ct),
        })
        .map_err(|e| e.to_string())
    }

    fn unwrap_key(json: &str, password: &str) -> Result<Key, String> {
        let f: WrappedKeyFile = serde_json::from_str(json).map_err(|e| format!("key file is corrupt: {e}"))?;
        if f.v != 1 || f.kdf != "argon2id" {
            return Err(format!("unsupported key file format (v{}, {})", f.v, f.kdf));
        }
        if f.m > KDF_MAX_M_KIB || f.t > KDF_MAX_T || f.p > KDF_MAX_P {
            return Err("key file parameters are out of range".into());
        }
        let salt = BASE64.decode(&f.salt).map_err(|_| "key file is corrupt (salt)")?;
        let nonce = BASE64.decode(&f.nonce).map_err(|_| "key file is corrupt (nonce)")?;
        if nonce.len() != NONCE_LEN {
            return Err("key file is corrupt (nonce length)".into());
        }
        let ct = BASE64.decode(&f.ct).map_err(|_| "key file is corrupt (ciphertext)")?;
        let kek = derive_kek(password, &salt, f.m, f.t, f.p)?;
        // AEAD failure here is overwhelmingly a wrong password; a modified
        // file fails the same way, and is equally unrecoverable by retyping.
        let plain = Zeroizing::new(
            XChaCha20Poly1305::new(kek.as_ref().into())
                .decrypt(nonce.as_slice().into(), Payload { msg: &ct, aad: KEYFILE_AAD })
                .map_err(|_| "Wrong password".to_string())?,
        );
        plain.as_slice().try_into().map_err(|_| "key file holds a key of the wrong length".to_string())
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        /// Tests use tiny Argon2 costs: the format and the AEAD are what's
        /// under test, and unwrap reads the costs from the file itself.
        fn wrap_fast(key: &Key, password: &str) -> String {
            wrap_key_with_params(key, password, 64, 1, 1).unwrap()
        }

        #[test]
        fn wrapped_key_round_trips() {
            let key = [3u8; KEY_LEN];
            let json = wrap_fast(&key, "correct horse");
            assert_eq!(unwrap_key(&json, "correct horse").unwrap(), key);
        }

        #[test]
        fn wrong_password_is_rejected() {
            let json = wrap_fast(&[3u8; KEY_LEN], "correct horse");
            assert_eq!(unwrap_key(&json, "wrong horse").unwrap_err(), "Wrong password");
        }

        #[test]
        fn wrapped_file_never_contains_the_key() {
            let key = [3u8; KEY_LEN];
            let json = wrap_fast(&key, "correct horse");
            assert!(!json.contains(&BASE64.encode(key)));
        }

        #[test]
        fn tampered_ciphertext_is_rejected() {
            let json = wrap_fast(&[3u8; KEY_LEN], "correct horse");
            let mut f: WrappedKeyFile = serde_json::from_str(&json).unwrap();
            let mut ct = BASE64.decode(&f.ct).unwrap();
            ct[0] ^= 1;
            f.ct = BASE64.encode(ct);
            assert!(unwrap_key(&serde_json::to_string(&f).unwrap(), "correct horse").is_err());
        }

        #[test]
        fn every_wrap_uses_a_fresh_salt_and_nonce() {
            let a: WrappedKeyFile = serde_json::from_str(&wrap_fast(&[3u8; KEY_LEN], "pw123456")).unwrap();
            let b: WrappedKeyFile = serde_json::from_str(&wrap_fast(&[3u8; KEY_LEN], "pw123456")).unwrap();
            assert_ne!(a.salt, b.salt);
            assert_ne!(a.nonce, b.nonce);
        }

        #[test]
        fn out_of_range_kdf_parameters_are_refused_before_deriving() {
            let json = wrap_fast(&[3u8; KEY_LEN], "correct horse");
            let mut f: WrappedKeyFile = serde_json::from_str(&json).unwrap();
            f.m = KDF_MAX_M_KIB + 1;
            let err = unwrap_key(&serde_json::to_string(&f).unwrap(), "correct horse").unwrap_err();
            assert!(err.contains("out of range"));
        }

        #[test]
        fn legacy_plaintext_file_is_recognized() {
            let key = [5u8; KEY_LEN];
            assert_eq!(parse_key_file(&format!("{}\n", BASE64.encode(key))).unwrap(), KeyFile::Legacy(key));
        }

        #[test]
        fn wrapped_file_is_recognized_and_corrupt_json_is_an_error() {
            let json = wrap_fast(&[3u8; KEY_LEN], "correct horse");
            assert!(matches!(parse_key_file(&json).unwrap(), KeyFile::Wrapped(_)));
            assert!(parse_key_file("{\"v\":1").is_err());
            assert!(parse_key_file("not base64 at all!").is_err());
        }

        fn probe(vault: Result<Option<Key>, String>, file: Result<KeyFile, String>) -> Probe {
            Probe { vault, vault_skipped: false, file }
        }

        #[test]
        fn vault_key_wins_over_any_file() {
            let k = [1u8; KEY_LEN];
            let p = probe(Ok(Some(k)), Ok(KeyFile::Legacy([2u8; KEY_LEN])));
            assert_eq!(decide(&p, true), Decision::UseVault(k));
            assert!(!p.needs_data_check());
        }

        /// The data-loss bug this module exists to close: an unreachable
        /// vault (or a vanished entry) over existing ciphertext must never
        /// produce a fresh key.
        #[test]
        fn never_creates_a_key_over_existing_data() {
            assert_eq!(decide(&probe(Err("denied".into()), Ok(KeyFile::Missing)), true), Decision::KeyMissing);
            assert_eq!(decide(&probe(Ok(None), Ok(KeyFile::Missing)), true), Decision::KeyMissing);
        }

        #[test]
        fn creates_a_key_only_when_nothing_exists() {
            assert_eq!(decide(&probe(Ok(None), Ok(KeyFile::Missing)), false), Decision::CreateNew);
            assert_eq!(decide(&probe(Err("no backend".into()), Ok(KeyFile::Missing)), false), Decision::CreateNew);
        }

        #[test]
        fn file_states_map_to_locked_legacy_and_missing() {
            let k = [4u8; KEY_LEN];
            assert_eq!(decide(&probe(Ok(None), Ok(KeyFile::Wrapped("{}".into()))), false), Decision::FileLocked);
            assert_eq!(decide(&probe(Err("x".into()), Ok(KeyFile::Legacy(k))), false), Decision::Legacy(k));
            // A corrupt/unreadable file is never overwritten automatically.
            assert_eq!(decide(&probe(Ok(None), Err("corrupt".into())), false), Decision::KeyMissing);
        }

        /// The Keychain-reprompt fix: with the vault off-limits, a fresh key
        /// must go to a file, never be offered to the vault.
        #[test]
        fn skipped_vault_never_leads_to_a_vault_write() {
            let p = Probe { vault: Ok(None), vault_skipped: true, file: Ok(KeyFile::Missing) };
            assert_eq!(decide(&p, false), Decision::CreateNewInFile);
            assert_eq!(decide(&p, true), Decision::KeyMissing);
        }

        #[test]
        fn short_passwords_are_refused() {
            assert!(validate_new_password("1234567").is_err());
            assert!(validate_new_password("12345678").is_ok());
        }
    }
}

// --- Commands --------------------------------------------------------------

pub use platform::{ensure_resolved, is_locked};

#[tauri::command]
pub async fn get_key_storage_status(app: AppHandle) -> Result<KeyStorageStatus, String> {
    platform::status(&app).await
}

#[tauri::command]
pub async fn retry_key_resolution(app: AppHandle) -> Result<(), String> {
    platform::retry(&app).await
}

#[tauri::command]
pub async fn unlock_key_file(app: AppHandle, password: String) -> Result<(), String> {
    platform::unlock(&app, password).await
}

#[tauri::command]
pub async fn set_key_file_password(app: AppHandle, password: String) -> Result<(), String> {
    platform::set_password(&app, password).await
}

#[tauri::command]
pub async fn change_key_file_password(app: AppHandle, old_password: String, new_password: String) -> Result<(), String> {
    platform::change_password(&app, old_password, new_password).await
}

#[tauri::command]
pub async fn migrate_key_to_file(app: AppHandle, password: String) -> Result<(), String> {
    platform::migrate_to_file(&app, password).await
}

#[tauri::command]
pub async fn migrate_key_to_vault(app: AppHandle) -> Result<(), String> {
    platform::migrate_to_vault(&app).await
}

#[tauri::command]
pub async fn reveal_encryption_key(app: AppHandle, password: Option<String>) -> Result<String, String> {
    platform::reveal(&app, password).await
}

#[tauri::command]
pub async fn restore_encryption_key(
    app: AppHandle,
    key: String,
    target: String,
    password: Option<String>,
) -> Result<(), String> {
    platform::restore(&app, key, target, password).await
}

#[tauri::command]
pub async fn reset_encryption_key(app: AppHandle, target: String, password: Option<String>) -> Result<(), String> {
    platform::reset(&app, target, password).await
}
