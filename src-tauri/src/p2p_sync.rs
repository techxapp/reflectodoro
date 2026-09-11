//! P2P LAN device pairing + manual delta sync. Backs Settings' "Paired
//! devices" list and "Import from device" dropdown+Sync button -- see
//! CLAUDE.md's "P2P LAN sync" section for the full feature design. This
//! module is the wire-level mechanics: LAN discovery, the pairing handshake,
//! and the sync session itself.
//!
//! ## Security model
//!
//! Pairing is PIN-based, but the PIN is never key material -- it only
//! authenticates a SPAKE2 password-authenticated key exchange (`spake2`
//! crate), which derives a full ~256-bit shared key. That key, not the PIN
//! or any hash of it, is what encrypts every session afterward (`snow`,
//! Noise `NNpsk0`). This matters because a naive "hash the PIN, use it as a
//! key" design would let anyone who merely *records* the pairing handshake
//! brute-force a 6-digit PIN offline in seconds; SPAKE2's guarantee is that
//! a passive eavesdropper learns nothing usable that way -- every PIN guess
//! requires actively interacting with one of the two real devices in real
//! time. The one residual attack (someone on the LAN guessing PINs live
//! against an open pairing window) is closed by `MAX_PAIRING_ATTEMPTS` +
//! `PAIRING_WINDOW` below: a handful of guesses, then the session must be
//! reopened.
//!
//! The Noise handshake immediately after SPAKE2 also doubles as *key
//! confirmation*: if the PIN typed on the joining device didn't match the
//! host's, the two sides derive different SPAKE2 keys, and the Noise
//! handshake's AEAD authentication fails outright rather than silently
//! producing garbage -- that failure is what `handle_pairing_responder`
//! below reports as "PIN mismatch".
//!
//! ## Wire protocol
//!
//! One TCP connection per pairing attempt or sync session, to `PORT` on the
//! peer's LAN address (found via mDNS -- `mdns-sd` on desktop, `NsdManager`
//! via `android_bridge.rs` on Android). Every message is framed with a
//! `write_frame`/`read_frame` u32-be length prefix.
//!
//! - **Pairing** (dialer writes tag `TAG_PAIRING` first): a raw (unencrypted
//!   -- SPAKE2 messages are safe to send in the clear, that's the whole
//!   point of the protocol) SPAKE2 message exchange, then a Noise `NNpsk0`
//!   handshake keyed by the derived key, then one JSON frame each way
//!   (`PeerIdentity`) over the now-encrypted channel.
//! - **Sync** (dialer writes tag `TAG_SYNC`, then its own device id so the
//!   responder knows which stored `shared_key` to use): a Noise `NNpsk0`
//!   handshake keyed by that peer's stored key, then one JSON frame each way
//!   (`SyncPayload`) carrying delta rows for the five synced tables --
//!   `app_setting` is never included, by design (see CLAUDE.md). Both sides
//!   apply the payload they receive via `import.rs`'s existing merge
//!   functions, always in `ImportMode::Merge`.
//!
//! ## Verified live on one real device pair
//!
//! Pairing and bidirectional sync both completed successfully between a
//! Windows desktop and an Android phone on the same wifi network. That
//! testing is also what surfaced (and got fixes for) several bugs no unit
//! test would have caught: `run_listener` binding only IPv4 while a peer's
//! mDNS resolution could come back with a public IPv6 address instead
//! (fixed by binding both families); two separate cold-start races where
//! `advertise_self` read `device_id`/`device_name` before the database or
//! the frontend's `ensureDeviceName()` had populated them (see
//! `get_or_create_device_id_after_db_ready` and `resync_advertised_name`);
//! and `SyncResult` only ever reporting rows *received*, so a sync that
//! only moved data outward read as "0 rows" even though it worked. Not yet
//! verified: macOS/Linux as either side of a pair, or more than two
//! devices.

use std::net::SocketAddr;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use chrono::{SecondsFormat, Utc};
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use snow::params::NoiseParams;
use spake2::{Ed25519Group, Identity, Password, Spake2};
use tauri::{AppHandle, Manager};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

use crate::db;
use crate::import;

#[cfg(not(any(target_os = "android", target_os = "ios")))]
use mdns_sd::{ServiceDaemon, ServiceEvent, ServiceInfo};

/// TCP port every platform listens on for both pairing and sync connections
/// (disambiguated by the first byte sent -- see `TAG_PAIRING`/`TAG_SYNC`).
/// Arbitrary, chosen to avoid well-known-port collisions.
const PORT: u16 = 47811;
const SERVICE_TYPE: &str = "_reflectodoro._tcp.local.";

const TAG_PAIRING: u8 = 0x01;
const TAG_SYNC: u8 = 0x02;

const MAX_PAIRING_ATTEMPTS: u32 = 5;
const PAIRING_WINDOW: Duration = Duration::from_secs(60);
/// How long a single mDNS browse waits for responses before giving up --
/// applies to both the pairing-candidate list and the "who's online right
/// now" check behind the "Import from device" dropdown.
const BROWSE_WINDOW: Duration = Duration::from_secs(3);

/// Noise's own hard per-message ceiling (protocol-defined, not tunable) --
/// every `read_frame` call site bounds its max length by this (plus a little
/// headroom for Noise's own overhead), which is what actually stops a
/// misbehaving peer from making this device allocate an unbounded buffer
/// from a forged length prefix.
const NOISE_MAX_MSG: usize = 65535;
/// Plaintext chunk size for `send_encrypted_json`/`recv_encrypted_json` --
/// comfortably under `NOISE_MAX_MSG` once Noise's ~16-byte AEAD tag and this
/// module's own framing are accounted for.
const PLAINTEXT_CHUNK: usize = 60_000;

fn noise_params() -> NoiseParams {
    "Noise_NNpsk0_25519_ChaChaPoly_BLAKE2s"
        .parse()
        .expect("static Noise params string is always valid")
}

// --- Managed state ----------------------------------------------------

struct PairingSession {
    pin: String,
    expires_at: Instant,
    attempts: u32,
}

pub struct P2pState {
    #[cfg(not(any(target_os = "android", target_os = "ios")))]
    mdns: Option<ServiceDaemon>,
    pairing_session: Mutex<Option<PairingSession>>,
}

impl P2pState {
    pub fn new() -> Self {
        #[cfg(not(any(target_os = "android", target_os = "ios")))]
        let mdns = match ServiceDaemon::new() {
            Ok(d) => Some(d),
            Err(e) => {
                log::warn!("p2p_sync: mDNS daemon failed to start, LAN discovery disabled: {e}");
                None
            }
        };
        Self {
            #[cfg(not(any(target_os = "android", target_os = "ios")))]
            mdns,
            pairing_session: Mutex::new(None),
        }
    }
}

// --- Wire types ---------------------------------------------------------

#[derive(Serialize, Deserialize)]
struct PeerIdentity {
    device_id: String,
    name: String,
    platform: String,
}

/// Never includes `app_setting` -- settings deliberately never sync between
/// devices (see CLAUDE.md's P2P LAN sync section).
#[derive(Serialize, Deserialize, Default)]
struct SyncPayload {
    reflection: Vec<import::ImportReflectionRow>,
    daily_task_list: Vec<import::ImportTaskListRow>,
    not_to_do_list: Vec<import::ImportNotToDoRow>,
    wellness_check: Vec<import::ImportWellnessCheckRow>,
    screen_time_session: Vec<import::ImportScreenTimeSessionRow>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SyncResult {
    /// Rows *received* from the peer and applied locally -- everything
    /// below this point in the struct describes the incoming side only.
    /// `sent_count` (a single total, not broken down per table -- the UI
    /// only ever shows it as one number) is the other half: rows this
    /// device sent to the peer. Without it, a sync that only moved data in
    /// the send direction (this device had a new change, the peer had
    /// nothing new to send back) reported "0 rows" even though it worked --
    /// confirmed as a real point of user confusion in live testing.
    pub sent_count: usize,
    pub reflection_count: usize,
    pub task_list_count: usize,
    pub not_to_do_list_count: usize,
    pub wellness_check_count: usize,
    pub screen_time_session_count: usize,
    pub merged_slot_count: usize,
    pub screen_time_duplicate_count: usize,
    pub wellness_check_duplicate_count: usize,
}

#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct DiscoveredDevice {
    pub device_id: String,
    pub name: String,
    pub platform: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PairedDeviceInfo {
    pub device_id: String,
    pub name: String,
    pub platform: String,
    pub paired_at: String,
    pub last_sync_at: Option<String>,
    /// Populated by `get_paired_devices`/`browse_online_paired_devices` from
    /// a fresh mDNS browse -- not stored, always a live snapshot.
    pub online: bool,
}

// --- Small helpers --------------------------------------------------------

fn now_iso() -> String {
    Utc::now().to_rfc3339_opts(SecondsFormat::Millis, true)
}

fn hex_encode(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn hex_decode(s: &str) -> Result<Vec<u8>, String> {
    if s.len() % 2 != 0 {
        return Err("invalid hex string (odd length)".to_string());
    }
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&s[i..i + 2], 16).map_err(|e| e.to_string()))
        .collect()
}

fn random_numeric_pin(len: usize) -> String {
    use rand::Rng;
    let mut rng = rand::thread_rng();
    (0..len).map(|_| char::from(b'0' + rng.gen_range(0..10u8))).collect()
}

fn random_hex_id(bytes: usize) -> String {
    use rand::RngCore;
    let mut buf = vec![0u8; bytes];
    rand::thread_rng().fill_bytes(&mut buf);
    hex_encode(&buf)
}

/// This device's own stable identity, generated lazily on first use and
/// cached in `app_setting` (key `p2p_device_id`) rather than via migration
/// SQL, which can't produce randomness. Mirrors breakit.rs's own
/// hand-rolled-random-string approach rather than pulling in a `uuid` crate.
pub async fn get_or_create_device_id(app: &AppHandle) -> Result<String, String> {
    let pool = db::open_direct_pool(app).await?;
    if let Some(id) = sqlx::query_scalar::<_, String>("SELECT value FROM app_setting WHERE key = 'p2p_device_id'")
        .fetch_optional(&pool)
        .await
        .map_err(|e| e.to_string())?
    {
        return Ok(id);
    }
    let id = random_hex_id(16);
    sqlx::query("INSERT INTO app_setting (key, value) VALUES ('p2p_device_id', ?)")
        .bind(&id)
        .execute(&pool)
        .await
        .map_err(|e| e.to_string())?;
    Ok(id)
}

async fn get_device_name(app: &AppHandle) -> String {
    let Ok(pool) = db::open_direct_pool(app).await else {
        return String::new();
    };
    sqlx::query_scalar::<_, String>("SELECT value FROM app_setting WHERE key = 'device_name'")
        .fetch_optional(&pool)
        .await
        .ok()
        .flatten()
        .unwrap_or_default()
}

fn platform_str() -> String {
    std::env::consts::OS.to_string()
}

/// `get_or_create_device_id`, retried with backoff -- **only** for
/// `advertise_self`'s startup call, which runs synchronously from `.setup()`
/// on every boot, including cold starts. `db::open_direct_pool` opens a
/// plain `sqlx` connection against `pomodoro.db`, which doesn't create the
/// file or apply migrations itself -- that only happens once
/// tauri-plugin-sql's own pool is actually used, which in practice means
/// "once the frontend's `getDb()`/`Database.load()` runs", an async
/// webview-boot round trip that this synchronous Rust setup path can easily
/// win on a cold start. Confirmed live on a real Android device: the very
/// first advertise_self attempt hit `SQLITE_CANTOPEN` (db file didn't exist
/// yet) -- the same class of race CLAUDE.md already documents for
/// `breakit_config`'s cold-start config race, just one step earlier (the db
/// file itself, not just a missing row). Every other `db::open_direct_pool`
/// caller (import.rs's user-triggered import, this module's own
/// per-connection handlers) runs long after the app has been open and used,
/// so they never hit this -- advertise_self is the only caller that races
/// the very first moment of process startup.
async fn get_or_create_device_id_after_db_ready(app: &AppHandle) -> Result<String, String> {
    const MAX_ATTEMPTS: u32 = 10;
    const RETRY_DELAY: Duration = Duration::from_millis(750);

    let mut last_err = String::new();
    for attempt in 0..MAX_ATTEMPTS {
        match get_or_create_device_id(app).await {
            Ok(id) => return Ok(id),
            Err(e) => {
                last_err = e;
                tokio::time::sleep(RETRY_DELAY).await;
                let _ = attempt;
            }
        }
    }
    Err(last_err)
}

// --- Framing ---------------------------------------------------------------

async fn write_frame<W: AsyncWriteExt + Unpin>(w: &mut W, data: &[u8]) -> std::io::Result<()> {
    w.write_u32(data.len() as u32).await?;
    w.write_all(data).await
}

async fn read_frame<R: AsyncReadExt + Unpin>(r: &mut R, max_len: u32) -> std::io::Result<Vec<u8>> {
    let len = r.read_u32().await?;
    if len > max_len {
        return Err(std::io::Error::new(std::io::ErrorKind::InvalidData, "frame exceeds max length"));
    }
    let mut buf = vec![0u8; len as usize];
    r.read_exact(&mut buf).await?;
    Ok(buf)
}

/// Serializes `value`, splits it into `PLAINTEXT_CHUNK`-sized pieces (a sync
/// payload can exceed Noise's single-message ceiling), and sends a small
/// header frame (chunk count) followed by one Noise-encrypted frame per
/// chunk.
async fn send_encrypted_json<T: Serialize, S: AsyncWriteExt + Unpin>(
    stream: &mut S,
    transport: &mut snow::TransportState,
    value: &T,
) -> Result<(), String> {
    let plaintext = serde_json::to_vec(value).map_err(|e| e.to_string())?;
    let chunks: Vec<&[u8]> = if plaintext.is_empty() {
        vec![&plaintext[..]]
    } else {
        plaintext.chunks(PLAINTEXT_CHUNK).collect()
    };

    let mut buf = vec![0u8; NOISE_MAX_MSG];
    let header = (chunks.len() as u32).to_be_bytes();
    let n = transport.write_message(&header, &mut buf).map_err(|e| e.to_string())?;
    write_frame(stream, &buf[..n]).await.map_err(|e| e.to_string())?;

    for chunk in chunks {
        let n = transport.write_message(chunk, &mut buf).map_err(|e| e.to_string())?;
        write_frame(stream, &buf[..n]).await.map_err(|e| e.to_string())?;
    }
    Ok(())
}

async fn recv_encrypted_json<T: DeserializeOwned, S: AsyncReadExt + Unpin>(
    stream: &mut S,
    transport: &mut snow::TransportState,
) -> Result<T, String> {
    let header_ct = read_frame(stream, NOISE_MAX_MSG as u32 + 64).await.map_err(|e| e.to_string())?;
    let mut header_buf = vec![0u8; NOISE_MAX_MSG];
    let n = transport.read_message(&header_ct, &mut header_buf).map_err(|e| e.to_string())?;
    if n != 4 {
        return Err("malformed sync header".to_string());
    }
    let count = u32::from_be_bytes([header_buf[0], header_buf[1], header_buf[2], header_buf[3]]);

    let mut plaintext = Vec::new();
    let mut buf = vec![0u8; NOISE_MAX_MSG];
    for _ in 0..count {
        let ct = read_frame(stream, NOISE_MAX_MSG as u32 + 64).await.map_err(|e| e.to_string())?;
        let n = transport.read_message(&ct, &mut buf).map_err(|e| e.to_string())?;
        plaintext.extend_from_slice(&buf[..n]);
    }
    serde_json::from_slice(&plaintext).map_err(|e| e.to_string())
}

// --- LAN discovery (desktop: mdns-sd; Android: NsdManager via the bridge) --

/// Spawns advertising (or re-advertising) as a background task. Called from
/// `.setup()` at startup, and again via `resync_advertised_name` whenever
/// the frontend learns a real `device_name` -- either late, on cold start
/// (`ensureDeviceName` in db.ts runs async after the webview boots, which
/// can easily lose the race against this synchronous-from-setup() call --
/// same class of cold-start race as `get_or_create_device_id_after_db_ready`
/// above, confirmed live: a fresh Android install advertised with a blank
/// name because `get_device_name` read `app_setting.device_name` before
/// `ensureDeviceName` had written anything to it), or later, whenever the
/// user renames this device in Settings. Both platform variants
/// unregister any previous registration before registering fresh, making
/// repeat calls idempotent rather than erroring or leaving stale
/// duplicate entries.
#[cfg(not(any(target_os = "android", target_os = "ios")))]
pub fn advertise_self(app: &AppHandle) {
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        let device_id = match get_or_create_device_id_after_db_ready(&app).await {
            Ok(id) => id,
            Err(e) => {
                log::error!("p2p_sync: couldn't determine device id, LAN advertisement skipped: {e}");
                return;
            }
        };
        let name = get_device_name(&app).await;
        let platform = platform_str();

        let mdns = match app.state::<P2pState>().mdns.clone() {
            Some(m) => m,
            None => return,
        };

        let host_name = format!("{device_id}.local.");
        // Fullname is deterministic (device_id never changes), so this can
        // be reconstructed rather than needing to track the prior
        // ServiceInfo -- errors here (nothing registered yet, the common
        // first-call case) are expected and harmless.
        let _ = mdns.unregister(&format!("{device_id}.{SERVICE_TYPE}"));

        let props: Vec<(&str, &str)> = vec![("device_id", device_id.as_str()), ("name", name.as_str()), ("platform", platform.as_str())];
        let info = match ServiceInfo::new(SERVICE_TYPE, &device_id, &host_name, "", PORT, &props[..]) {
            Ok(i) => i.enable_addr_auto(),
            Err(e) => {
                log::error!("p2p_sync: failed to build mDNS service info: {e}");
                return;
            }
        };
        match mdns.register(info) {
            Ok(()) => log::info!("p2p_sync: advertising as {device_id} ({name}) on the LAN"),
            Err(e) => log::error!("p2p_sync: failed to register mDNS service: {e}"),
        }
    });
}

#[cfg(target_os = "android")]
pub fn advertise_self(app: &AppHandle) {
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        let device_id = match get_or_create_device_id_after_db_ready(&app).await {
            Ok(id) => id,
            Err(e) => {
                log::error!("p2p_sync: couldn't determine device id, LAN advertisement skipped: {e}");
                return;
            }
        };
        let name = get_device_name(&app).await;
        let bridge = app.state::<crate::android_bridge::AndroidBridge<tauri::Wry>>();
        // Harmless no-op on the common first-call case (nothing registered
        // yet) -- see this function's doc comment on the desktop variant.
        if let Err(e) = bridge.unregister_p2p_service() {
            log::warn!("p2p_sync: NSD unregister-before-reregister failed (fine on first call): {e:?}");
        }
        match bridge.register_p2p_service(&device_id, &name, &platform_str(), PORT) {
            Ok(_) => log::info!("p2p_sync: NSD registration dispatched for {device_id} ({name})"),
            Err(e) => log::error!("p2p_sync: NSD registration failed: {e:?}"),
        }
    });
}

/// Called from the frontend once it knows the real `device_name` -- after
/// `ensureDeviceName()` resolves on boot, and after Settings saves a
/// user-edited device name -- so a peer's "Paired devices" list doesn't keep
/// showing a stale or blank name until this device's next restart. See
/// `advertise_self`'s doc comment for why the first advertisement so
/// commonly races an as-yet-unknown name.
#[tauri::command]
pub fn resync_advertised_name(app: AppHandle) {
    advertise_self(&app);
}

#[cfg(not(any(target_os = "android", target_os = "ios")))]
async fn browse_lan(app: &AppHandle, window: Duration) -> Result<Vec<(DiscoveredDevice, SocketAddr)>, String> {
    let mdns = app
        .state::<P2pState>()
        .mdns
        .clone()
        .ok_or_else(|| "LAN discovery is unavailable on this device".to_string())?;
    let receiver = mdns.browse(SERVICE_TYPE).map_err(|e| e.to_string())?;

    let mut found = Vec::new();
    let deadline = tokio::time::Instant::now() + window;
    loop {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        if remaining.is_zero() {
            break;
        }
        match tokio::time::timeout(remaining, receiver.recv_async()).await {
            Ok(Ok(ServiceEvent::ServiceResolved(info))) => {
                let device_id = info.get_property_val_str("device_id").unwrap_or_default().to_string();
                let name = info.get_property_val_str("name").unwrap_or_default().to_string();
                let platform = info.get_property_val_str("platform").unwrap_or_default().to_string();
                if device_id.is_empty() {
                    continue;
                }
                // Prefer IPv4: a peer can advertise both families, and an
                // IPv6 entry here is sometimes a globally-routable address
                // rather than a LAN-local one (confirmed live: the same
                // laptop resolved to its LAN IPv4 in one browse and a public
                // IPv6 in another) -- IPv4 is the reliable "same wifi" match
                // this feature actually assumes. `run_listener` binds both
                // families regardless, so an IPv6-only peer still works.
                let addresses = info.get_addresses();
                let addr = addresses.iter().find(|a| a.is_ipv4()).or_else(|| addresses.iter().next());
                if let Some(addr) = addr {
                    found.push((DiscoveredDevice { device_id, name, platform }, SocketAddr::new(*addr, info.get_port())));
                }
            }
            Ok(Ok(_)) => continue,
            Ok(Err(_)) => break,
            Err(_) => break,
        }
    }
    let _ = mdns.stop_browse(SERVICE_TYPE);
    Ok(found)
}

#[cfg(target_os = "android")]
async fn browse_lan(app: &AppHandle, window: Duration) -> Result<Vec<(DiscoveredDevice, SocketAddr)>, String> {
    let bridge = app.state::<crate::android_bridge::AndroidBridge<tauri::Wry>>();
    let value = bridge
        .discover_p2p_services(window.as_millis() as u64)
        .map_err(|e| format!("{e:?}"))?;
    // Wrapped in {"devices": [...]} on the Kotlin side (NativeBridgePlugin.kt's
    // discoverP2pServices) since Invoke.resolve() expects a JSObject, not a
    // bare array.
    let entries = value.get("devices").and_then(|v| v.as_array()).cloned().unwrap_or_default();
    let mut found = Vec::new();
    for entry in entries {
        let device_id = entry.get("deviceId").and_then(|v| v.as_str()).unwrap_or_default().to_string();
        let name = entry.get("name").and_then(|v| v.as_str()).unwrap_or_default().to_string();
        let platform = entry.get("platform").and_then(|v| v.as_str()).unwrap_or_default().to_string();
        let host = entry.get("host").and_then(|v| v.as_str()).unwrap_or_default();
        let port = entry.get("port").and_then(|v| v.as_u64()).unwrap_or(PORT as u64) as u16;
        if device_id.is_empty() || host.is_empty() {
            continue;
        }
        let Ok(ip) = host.parse() else { continue };
        found.push((DiscoveredDevice { device_id, name, platform }, SocketAddr::new(ip, port)));
    }
    Ok(found)
}

async fn resolve_peer(app: &AppHandle, device_id: &str) -> Result<SocketAddr, String> {
    let found = browse_lan(app, BROWSE_WINDOW).await?;
    found
        .into_iter()
        .find(|(d, _)| d.device_id == device_id)
        .map(|(_, addr)| addr)
        .ok_or_else(|| "device not found on the network (is it online and on the same wifi?)".to_string())
}

// --- DB access -------------------------------------------------------------

async fn already_paired_ids(app: &AppHandle) -> Result<Vec<String>, String> {
    let pool = db::open_direct_pool(app).await?;
    sqlx::query_scalar::<_, String>("SELECT device_id FROM paired_device")
        .fetch_all(&pool)
        .await
        .map_err(|e| e.to_string())
}

async fn load_paired_device_secret(app: &AppHandle, peer_device_id: &str) -> Result<(String, Option<String>), String> {
    let pool = db::open_direct_pool(app).await?;
    let row = sqlx::query_as::<_, (String, Option<String>)>(
        "SELECT shared_key, last_sync_at FROM paired_device WHERE device_id = ?",
    )
    .bind(peer_device_id)
    .fetch_optional(&pool)
    .await
    .map_err(|e| e.to_string())?;
    row.ok_or_else(|| format!("device {peer_device_id} is not paired with this device"))
}

async fn save_paired_device(app: &AppHandle, peer: &PeerIdentity, shared_key_hex: &str, paired_at: &str) -> Result<(), String> {
    let pool = db::open_direct_pool(app).await?;
    sqlx::query(
        "INSERT INTO paired_device (device_id, name, platform, shared_key, paired_at, last_sync_at)
         VALUES (?, ?, ?, ?, ?, NULL)
         ON CONFLICT(device_id) DO UPDATE SET
            name = excluded.name, platform = excluded.platform,
            shared_key = excluded.shared_key, paired_at = excluded.paired_at",
    )
    .bind(&peer.device_id)
    .bind(&peer.name)
    .bind(&peer.platform)
    .bind(shared_key_hex)
    .bind(paired_at)
    .execute(&pool)
    .await
    .map_err(|e| e.to_string())?;
    Ok(())
}

async fn update_last_sync_at(app: &AppHandle, peer_device_id: &str, sync_started_at: &str) -> Result<(), String> {
    let pool = db::open_direct_pool(app).await?;
    sqlx::query("UPDATE paired_device SET last_sync_at = ? WHERE device_id = ?")
        .bind(sync_started_at)
        .bind(peer_device_id)
        .execute(&pool)
        .await
        .map_err(|e| e.to_string())?;
    Ok(())
}

/// `since = None` (a device never synced with this peer before) selects
/// every row -- an empty-string lower bound sorts before every real ISO
/// timestamp, so it doubles as "no cursor yet" without a separate branch.
async fn build_delta_payload(app: &AppHandle, since: Option<&str>) -> Result<SyncPayload, String> {
    let pool = db::open_direct_pool(app).await?;
    let since = since.unwrap_or("");

    let reflection = sqlx::query_as::<_, (String, String, String, String)>(
        "SELECT created_at, slot_start_at, text, COALESCE(updated_at, created_at)
         FROM reflection WHERE COALESCE(updated_at, created_at) > ?",
    )
    .bind(since)
    .fetch_all(&pool)
    .await
    .map_err(|e| e.to_string())?
    .into_iter()
    .map(|(created_at, slot_start_at, text, updated_at)| import::ImportReflectionRow {
        created_at,
        slot_start_at,
        text,
        updated_at: Some(updated_at),
    })
    .collect();

    let daily_task_list = sqlx::query_as::<_, (String, String, String)>(
        "SELECT date, content, updated_at FROM daily_task_list WHERE updated_at > ?",
    )
    .bind(since)
    .fetch_all(&pool)
    .await
    .map_err(|e| e.to_string())?
    .into_iter()
    .map(|(date, content, updated_at)| import::ImportTaskListRow { date, content, updated_at })
    .collect();

    let not_to_do_list = sqlx::query_as::<_, (String, String, String)>(
        "SELECT date, content, updated_at FROM not_to_do_list WHERE updated_at > ?",
    )
    .bind(since)
    .fetch_all(&pool)
    .await
    .map_err(|e| e.to_string())?
    .into_iter()
    .map(|(date, content, updated_at)| import::ImportNotToDoRow { date, content, updated_at })
    .collect();

    let wellness_check = sqlx::query_as::<_, (String, i64, i64, i64, i64, String)>(
        "SELECT slot_start_at, relaxed_eyes, exercise, drank_water, washroom, created_at
         FROM wellness_check WHERE created_at > ?",
    )
    .bind(since)
    .fetch_all(&pool)
    .await
    .map_err(|e| e.to_string())?
    .into_iter()
    .map(|(slot_start_at, relaxed_eyes, exercise, drank_water, washroom, created_at)| import::ImportWellnessCheckRow {
        slot_start_at,
        relaxed_eyes,
        exercise,
        drank_water,
        washroom,
        created_at,
    })
    .collect();

    let screen_time_session = sqlx::query_as::<_, (String, String, String, String, String, String)>(
        "SELECT app_id, display_name, platform, device_name, started_at, ended_at
         FROM screen_time_session WHERE started_at > ?",
    )
    .bind(since)
    .fetch_all(&pool)
    .await
    .map_err(|e| e.to_string())?
    .into_iter()
    .map(|(app_id, display_name, platform, device_name, started_at, ended_at)| import::ImportScreenTimeSessionRow {
        app_id,
        display_name,
        platform,
        device_name,
        started_at,
        ended_at,
    })
    .collect();

    Ok(SyncPayload { reflection, daily_task_list, not_to_do_list, wellness_check, screen_time_session })
}

async fn apply_payload(app: &AppHandle, payload: &SyncPayload) -> Result<SyncResult, String> {
    let pool = db::open_direct_pool(app).await?;
    let mut tx = pool.begin().await.map_err(|e| format!("failed to start sync-apply transaction: {e}"))?;

    let merged_slot_count = import::import_reflections(&mut tx, &payload.reflection, import::ImportMode::Merge).await?;
    let wellness_check_duplicate_count =
        import::import_wellness_checks(&mut tx, &payload.wellness_check, import::ImportMode::Merge).await?;

    let daily_task_rows: Vec<(&str, &str, &str)> =
        payload.daily_task_list.iter().map(|r| (r.date.as_str(), r.content.as_str(), r.updated_at.as_str())).collect();
    import::merge_import_day_rows(&mut tx, "daily_task_list", &daily_task_rows, import::ImportMode::Merge).await?;

    let not_to_do_rows: Vec<(&str, &str, &str)> =
        payload.not_to_do_list.iter().map(|r| (r.date.as_str(), r.content.as_str(), r.updated_at.as_str())).collect();
    import::merge_import_day_rows(&mut tx, "not_to_do_list", &not_to_do_rows, import::ImportMode::Merge).await?;

    let screen_time_duplicate_count = import::import_screen_time_sessions(&mut tx, &payload.screen_time_session).await?;

    tx.commit().await.map_err(|e| format!("failed to commit sync-apply transaction: {e}"))?;

    Ok(SyncResult {
        // Set by the caller (sync_with_device_inner) where the outgoing
        // payload is known; apply_payload only ever sees the incoming side.
        sent_count: 0,
        reflection_count: payload.reflection.len(),
        task_list_count: payload.daily_task_list.len(),
        not_to_do_list_count: payload.not_to_do_list.len(),
        wellness_check_count: payload.wellness_check.len(),
        screen_time_session_count: payload.screen_time_session.len(),
        merged_slot_count,
        screen_time_duplicate_count,
        wellness_check_duplicate_count,
    })
}

// --- TCP listener: accepts both pairing and sync connections --------------

/// Binds both IPv4 (`0.0.0.0`) and IPv6 (`[::]`) wildcard listeners and runs
/// an accept loop on each that binds successfully, rather than a single
/// IPv4-only listener -- confirmed live (real two-device test) that a peer
/// discovered via mDNS can resolve to *either* family, sometimes for the
/// same device across two separate browses (an mDNS responder can answer
/// with a global IPv6 address that isn't purely LAN-scoped). An IPv4-only
/// listener silently refuses any connection attempt that lands on the IPv6
/// address `browse_lan`/discovery happened to pick that time.
///
/// Whether the two binds conflict depends on the OS's default
/// `IPV6_V6ONLY` setting (Windows/macOS: separate by default, so both binds
/// succeed independently; Linux: often dual-stack by default, so the IPv6
/// bind alone already serves IPv4 traffic and the IPv4 bind fails with
/// "address in use") -- both outcomes are treated as success as long as at
/// least one listener is up.
pub async fn run_listener(app: AppHandle) {
    let v6 = TcpListener::bind(("::", PORT)).await.ok();
    let v4 = match TcpListener::bind(("0.0.0.0", PORT)).await {
        Ok(l) => Some(l),
        Err(e) if v6.is_some() => {
            // Very likely just the dual-stack-already-covers-it case
            // described above, not a real problem -- only worth a warning,
            // not an error, and only when it's not accompanied by the v6
            // bind also having failed (that combination is handled below).
            log::warn!("p2p_sync: IPv4 listener bind failed (IPv6 listener already covers this port on this OS, most likely): {e}");
            None
        }
        Err(e) => {
            log::error!("p2p_sync: failed to bind an IPv4 listener on port {PORT}, and no IPv6 listener is up either -- LAN pairing/sync will not work: {e}");
            None
        }
    };

    if v4.is_none() && v6.is_none() {
        return;
    }
    log::info!("p2p_sync: listening on port {PORT} (IPv4={}, IPv6={})", v4.is_some(), v6.is_some());

    if let Some(listener) = v4 {
        let app = app.clone();
        tauri::async_runtime::spawn(accept_loop(app, listener));
    }
    if let Some(listener) = v6 {
        tauri::async_runtime::spawn(accept_loop(app, listener));
    }
}

async fn accept_loop(app: AppHandle, listener: TcpListener) {
    loop {
        let (stream, peer_addr) = match listener.accept().await {
            Ok(v) => v,
            Err(e) => {
                log::warn!("p2p_sync: accept() failed: {e}");
                continue;
            }
        };
        let app = app.clone();
        tauri::async_runtime::spawn(async move {
            if let Err(e) = handle_connection(app, stream, peer_addr).await {
                log::warn!("p2p_sync: connection from {peer_addr} failed: {e}");
            }
        });
    }
}

async fn handle_connection(app: AppHandle, mut stream: TcpStream, peer_addr: SocketAddr) -> Result<(), String> {
    let mut tag = [0u8; 1];
    stream.read_exact(&mut tag).await.map_err(|e| e.to_string())?;
    match tag[0] {
        TAG_PAIRING => handle_pairing_responder(app, stream, peer_addr).await,
        TAG_SYNC => handle_sync_responder(app, stream, peer_addr).await,
        other => Err(format!("unknown connection tag {other}")),
    }
}

async fn handle_pairing_responder(app: AppHandle, mut stream: TcpStream, peer_addr: SocketAddr) -> Result<(), String> {
    let msg_b = read_frame(&mut stream, 4096).await.map_err(|e| e.to_string())?;

    let pin = {
        let state = app.state::<P2pState>();
        let mut guard = state.pairing_session.lock().unwrap();
        match guard.as_mut() {
            Some(session) if session.expires_at > Instant::now() && session.attempts < MAX_PAIRING_ATTEMPTS => {
                session.attempts += 1;
                Some(session.pin.clone())
            }
            _ => None,
        }
    };

    let pin = match pin {
        Some(p) => p,
        None => {
            let mut resp = vec![0x01u8];
            resp.extend_from_slice(b"not accepting pairing right now");
            let _ = write_frame(&mut stream, &resp).await;
            return Err(format!("rejected pairing attempt from {peer_addr}: no open pairing session"));
        }
    };

    let password = Password::new(pin.as_bytes());
    let id_a = Identity::new(b"reflectodoro-pairing-host");
    let id_b = Identity::new(b"reflectodoro-pairing-joiner");
    let (s2_a, msg_a) = Spake2::<Ed25519Group>::start_a(&password, &id_a, &id_b);

    let mut resp = vec![0x00u8];
    resp.extend_from_slice(&msg_a);
    write_frame(&mut stream, &resp).await.map_err(|e| e.to_string())?;

    let key = s2_a.finish(&msg_b).map_err(|e| format!("spake2 finish failed: {e:?}"))?;

    let mut hs = snow::Builder::new(noise_params())
        .psk(0, &key)
        .build_responder()
        .map_err(|e| e.to_string())?;
    let msg1 = read_frame(&mut stream, NOISE_MAX_MSG as u32 + 64).await.map_err(|e| e.to_string())?;
    let mut buf = vec![0u8; NOISE_MAX_MSG];
    if hs.read_message(&msg1, &mut buf).is_err() {
        // Wrong PIN: this device's derived key differs from the joiner's, so
        // the Noise AEAD tag fails to verify -- see this module's doc
        // comment's "Security model" section. The attempt above already
        // counted against MAX_PAIRING_ATTEMPTS.
        return Err(format!("pairing handshake failed from {peer_addr}: PIN mismatch"));
    }
    let mut buf2 = vec![0u8; NOISE_MAX_MSG];
    let n = hs.write_message(&[], &mut buf2).map_err(|e| e.to_string())?;
    write_frame(&mut stream, &buf2[..n]).await.map_err(|e| e.to_string())?;
    let mut transport = hs.into_transport_mode().map_err(|e| e.to_string())?;

    let self_id = get_or_create_device_id(&app).await?;
    let self_name = get_device_name(&app).await;
    let self_identity = PeerIdentity { device_id: self_id, name: self_name, platform: platform_str() };

    let peer_identity: PeerIdentity = recv_encrypted_json(&mut stream, &mut transport).await?;
    send_encrypted_json(&mut stream, &mut transport, &self_identity).await?;

    let shared_key_hex = hex_encode(&key);
    let paired_at = now_iso();
    save_paired_device(&app, &peer_identity, &shared_key_hex, &paired_at).await?;

    // One successful pairing closes the window -- caps how many devices can
    // pair against a single PIN broadcast to exactly one.
    {
        let state = app.state::<P2pState>();
        *state.pairing_session.lock().unwrap() = None;
    }

    log::info!("p2p_sync: paired with {} ({})", peer_identity.name, peer_identity.device_id);
    Ok(())
}

async fn handle_sync_responder(app: AppHandle, mut stream: TcpStream, peer_addr: SocketAddr) -> Result<(), String> {
    let dialer_id_bytes = read_frame(&mut stream, 256).await.map_err(|e| e.to_string())?;
    let dialer_id = String::from_utf8(dialer_id_bytes).map_err(|e| e.to_string())?;

    let (shared_key_hex, last_sync_at) = load_paired_device_secret(&app, &dialer_id)
        .await
        .map_err(|e| format!("sync attempt from unpaired device {dialer_id} ({peer_addr}): {e}"))?;
    let key = hex_decode(&shared_key_hex)?;

    let mut hs = snow::Builder::new(noise_params())
        .psk(0, &key)
        .build_responder()
        .map_err(|e| e.to_string())?;
    let msg1 = read_frame(&mut stream, NOISE_MAX_MSG as u32 + 64).await.map_err(|e| e.to_string())?;
    let mut buf = vec![0u8; NOISE_MAX_MSG];
    hs.read_message(&msg1, &mut buf).map_err(|e| format!("sync handshake failed from {dialer_id}: {e}"))?;
    let mut buf2 = vec![0u8; NOISE_MAX_MSG];
    let n = hs.write_message(&[], &mut buf2).map_err(|e| e.to_string())?;
    write_frame(&mut stream, &buf2[..n]).await.map_err(|e| e.to_string())?;
    let mut transport = hs.into_transport_mode().map_err(|e| e.to_string())?;

    // Captured before either side reads/builds its delta, exactly like the
    // dialer -- see build_delta_payload's doc comment and sync_with_device.
    let sync_started_at = now_iso();

    let incoming: SyncPayload = recv_encrypted_json(&mut stream, &mut transport).await?;
    let outgoing = build_delta_payload(&app, last_sync_at.as_deref()).await?;
    let sent_count = sync_payload_row_count(&outgoing);
    send_encrypted_json(&mut stream, &mut transport, &outgoing).await?;

    let received_count = sync_payload_row_count(&incoming);
    apply_payload(&app, &incoming).await?;
    update_last_sync_at(&app, &dialer_id, &sync_started_at).await?;

    log::info!("p2p_sync: completed inbound sync with {dialer_id} (sent={sent_count}, received={received_count})");
    Ok(())
}

// --- Tauri commands (dialer side / frontend-facing) ------------------------

/// Opens a `PAIRING_WINDOW`-long pairing session and returns the PIN to
/// show the user. Only one session is tracked at a time -- starting a new
/// one replaces (invalidates) any still-open previous one.
#[tauri::command]
pub async fn start_pairing(app: AppHandle) -> Result<String, String> {
    let pin = random_numeric_pin(6);
    let state = app.state::<P2pState>();
    *state.pairing_session.lock().unwrap() = Some(PairingSession {
        pin: pin.clone(),
        expires_at: Instant::now() + PAIRING_WINDOW,
        attempts: 0,
    });
    log::info!("p2p_sync: start_pairing: pairing window open for {PAIRING_WINDOW:?}");
    Ok(pin)
}

/// Closes an open pairing session early (e.g. the user backed out of the
/// "Pair a new device" dialog before entering a PIN anywhere).
#[tauri::command]
pub fn cancel_pairing(app: AppHandle) {
    let state = app.state::<P2pState>();
    *state.pairing_session.lock().unwrap() = None;
}

/// Devices currently advertising on the LAN that aren't already paired with
/// this one -- the list the "Pair a new device" flow picks a target from.
#[tauri::command]
pub async fn browse_pairing_candidates(app: AppHandle) -> Result<Vec<DiscoveredDevice>, String> {
    let self_id = get_or_create_device_id(&app).await?;
    let paired = already_paired_ids(&app).await?;
    let found = browse_lan(&app, BROWSE_WINDOW).await?;
    let candidates: Vec<DiscoveredDevice> = found
        .into_iter()
        .map(|(d, _)| d)
        .filter(|d| d.device_id != self_id && !paired.contains(&d.device_id))
        .collect();
    log::info!("p2p_sync: browse_pairing_candidates: found {} candidate(s)", candidates.len());
    Ok(candidates)
}

/// Dials `device_id` (found via a prior `browse_pairing_candidates` call)
/// and runs the joiner side of the SPAKE2+Noise pairing handshake using
/// `pin` -- the PIN shown on the *other* device's "Pair a new device"
/// screen. Logs entry/outcome (`confirm_pairing_inner` does the actual
/// work) -- this is the dialer side of pairing, which previously had zero
/// log visibility on failure (only the responder side logged anything),
/// making a failed pairing attempt from either device hard to diagnose.
#[tauri::command]
pub async fn confirm_pairing(app: AppHandle, device_id: String, pin: String) -> Result<PairedDeviceInfo, String> {
    log::info!("p2p_sync: confirm_pairing: dialing {device_id}");
    match confirm_pairing_inner(&app, &device_id, &pin).await {
        Ok(info) => {
            log::info!("p2p_sync: confirm_pairing: paired with {} ({})", info.name, info.device_id);
            Ok(info)
        }
        Err(e) => {
            log::warn!("p2p_sync: confirm_pairing: failed pairing with {device_id}: {e}");
            Err(e)
        }
    }
}

async fn confirm_pairing_inner(app: &AppHandle, device_id: &str, pin: &str) -> Result<PairedDeviceInfo, String> {
    let peer_addr = resolve_peer(app, device_id).await?;
    log::info!("p2p_sync: confirm_pairing: resolved {device_id} at {peer_addr}");

    let mut stream = TcpStream::connect(peer_addr).await.map_err(|e| e.to_string())?;
    stream.write_u8(TAG_PAIRING).await.map_err(|e| e.to_string())?;

    let password = Password::new(pin.as_bytes());
    let id_a = Identity::new(b"reflectodoro-pairing-host");
    let id_b = Identity::new(b"reflectodoro-pairing-joiner");
    let (s2_b, msg_b) = Spake2::<Ed25519Group>::start_b(&password, &id_a, &id_b);
    write_frame(&mut stream, &msg_b).await.map_err(|e| e.to_string())?;

    let resp = read_frame(&mut stream, 4096).await.map_err(|e| e.to_string())?;
    let (status, rest) = resp.split_first().ok_or("empty pairing response")?;
    if *status != 0x00 {
        return Err(String::from_utf8_lossy(rest).to_string());
    }
    let key = s2_b.finish(rest).map_err(|_| "pairing failed -- incorrect PIN".to_string())?;

    let mut hs = snow::Builder::new(noise_params())
        .psk(0, &key)
        .build_initiator()
        .map_err(|e| e.to_string())?;
    let mut buf = vec![0u8; NOISE_MAX_MSG];
    let n = hs.write_message(&[], &mut buf).map_err(|e| e.to_string())?;
    write_frame(&mut stream, &buf[..n]).await.map_err(|e| e.to_string())?;
    let msg2 = read_frame(&mut stream, NOISE_MAX_MSG as u32 + 64).await.map_err(|e| e.to_string())?;
    let mut buf2 = vec![0u8; NOISE_MAX_MSG];
    hs.read_message(&msg2, &mut buf2).map_err(|_| "pairing failed -- incorrect PIN".to_string())?;
    let mut transport = hs.into_transport_mode().map_err(|e| e.to_string())?;

    let self_id = get_or_create_device_id(&app).await?;
    let self_name = get_device_name(&app).await;
    let self_identity = PeerIdentity { device_id: self_id, name: self_name, platform: platform_str() };

    send_encrypted_json(&mut stream, &mut transport, &self_identity).await?;
    let peer_identity: PeerIdentity = recv_encrypted_json(&mut stream, &mut transport).await?;

    let shared_key_hex = hex_encode(&key);
    let paired_at = now_iso();
    save_paired_device(&app, &peer_identity, &shared_key_hex, &paired_at).await?;

    Ok(PairedDeviceInfo {
        device_id: peer_identity.device_id,
        name: peer_identity.name,
        platform: peer_identity.platform,
        paired_at,
        last_sync_at: None,
        online: true,
    })
}

/// Every paired device, each annotated with whether it's currently visible
/// on the LAN (a fresh short browse, not a stored flag).
#[tauri::command]
pub async fn get_paired_devices(app: AppHandle) -> Result<Vec<PairedDeviceInfo>, String> {
    let pool = db::open_direct_pool(&app).await?;
    let rows = sqlx::query_as::<_, (String, String, String, String, Option<String>)>(
        "SELECT device_id, name, platform, paired_at, last_sync_at FROM paired_device ORDER BY name COLLATE NOCASE",
    )
    .fetch_all(&pool)
    .await
    .map_err(|e| e.to_string())?;

    let online_ids: Vec<String> = browse_lan(&app, BROWSE_WINDOW)
        .await
        .unwrap_or_default()
        .into_iter()
        .map(|(d, _)| d.device_id)
        .collect();

    Ok(rows
        .into_iter()
        .map(|(device_id, name, platform, paired_at, last_sync_at)| {
            let online = online_ids.contains(&device_id);
            PairedDeviceInfo { device_id, name, platform, paired_at, last_sync_at, online }
        })
        .collect())
}

/// Just the currently-online subset -- what the "Import from device"
/// dropdown populates itself from.
#[tauri::command]
pub async fn browse_online_paired_devices(app: AppHandle) -> Result<Vec<PairedDeviceInfo>, String> {
    Ok(get_paired_devices(app).await?.into_iter().filter(|d| d.online).collect())
}

#[tauri::command]
pub async fn forget_paired_device(app: AppHandle, device_id: String) -> Result<(), String> {
    let pool = db::open_direct_pool(&app).await?;
    sqlx::query("DELETE FROM paired_device WHERE device_id = ?")
        .bind(&device_id)
        .execute(&pool)
        .await
        .map_err(|e| e.to_string())?;
    Ok(())
}

/// Runs the dialer (initiator) side of a sync session with an already-paired
/// device: connects, authenticates+encrypts via Noise keyed by the stored
/// pairing secret, exchanges deltas since this pair's `last_sync_at`, and
/// applies what it receives -- always `ImportMode::Merge`, never `Replace`.
/// Logs entry/outcome, same reasoning as `confirm_pairing`.
#[tauri::command]
pub async fn sync_with_device(app: AppHandle, device_id: String) -> Result<SyncResult, String> {
    log::info!("p2p_sync: sync_with_device: starting sync with {device_id}");
    match sync_with_device_inner(&app, &device_id).await {
        Ok(result) => {
            log::info!(
                "p2p_sync: sync_with_device: completed with {device_id} (sent={}, received: reflection={} task_list={} not_to_do_list={} wellness_check={} screen_time_session={})",
                result.sent_count,
                result.reflection_count,
                result.task_list_count,
                result.not_to_do_list_count,
                result.wellness_check_count,
                result.screen_time_session_count,
            );
            Ok(result)
        }
        Err(e) => {
            log::warn!("p2p_sync: sync_with_device: failed syncing with {device_id}: {e}");
            Err(e)
        }
    }
}

async fn sync_with_device_inner(app: &AppHandle, device_id: &str) -> Result<SyncResult, String> {
    let peer_addr = resolve_peer(app, device_id).await?;
    log::info!("p2p_sync: sync_with_device: resolved {device_id} at {peer_addr}");
    let (shared_key_hex, last_sync_at) = load_paired_device_secret(app, device_id).await?;
    let key = hex_decode(&shared_key_hex)?;
    let self_id = get_or_create_device_id(app).await?;

    let mut stream = TcpStream::connect(peer_addr).await.map_err(|e| e.to_string())?;
    stream.write_u8(TAG_SYNC).await.map_err(|e| e.to_string())?;
    write_frame(&mut stream, self_id.as_bytes()).await.map_err(|e| e.to_string())?;

    let mut hs = snow::Builder::new(noise_params())
        .psk(0, &key)
        .build_initiator()
        .map_err(|e| e.to_string())?;
    let mut buf = vec![0u8; NOISE_MAX_MSG];
    let n = hs.write_message(&[], &mut buf).map_err(|e| e.to_string())?;
    write_frame(&mut stream, &buf[..n]).await.map_err(|e| e.to_string())?;
    let msg2 = read_frame(&mut stream, NOISE_MAX_MSG as u32 + 64).await.map_err(|e| e.to_string())?;
    let mut buf2 = vec![0u8; NOISE_MAX_MSG];
    hs.read_message(&msg2, &mut buf2).map_err(|e| e.to_string())?;
    let mut transport = hs.into_transport_mode().map_err(|e| e.to_string())?;

    // Captured before querying deltas (not "now" after the transfer
    // completes) -- so nothing created mid-transfer is missed on the next
    // sync, same snapshot-then-query pattern the file-based export already
    // uses. The responder does the same in handle_sync_responder.
    let sync_started_at = now_iso();

    let outgoing = build_delta_payload(&app, last_sync_at.as_deref()).await?;
    let sent_count = sync_payload_row_count(&outgoing);
    send_encrypted_json(&mut stream, &mut transport, &outgoing).await?;
    let incoming: SyncPayload = recv_encrypted_json(&mut stream, &mut transport).await?;

    let result = apply_payload(&app, &incoming).await?;
    update_last_sync_at(&app, &device_id, &sync_started_at).await?;

    Ok(SyncResult { sent_count, ..result })
}

fn sync_payload_row_count(payload: &SyncPayload) -> usize {
    payload.reflection.len()
        + payload.daily_task_list.len()
        + payload.not_to_do_list.len()
        + payload.wellness_check.len()
        + payload.screen_time_session.len()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hex_round_trips() {
        let bytes = [0u8, 1, 254, 255, 16, 128];
        assert_eq!(hex_decode(&hex_encode(&bytes)).unwrap(), bytes);
    }

    #[test]
    fn hex_decode_rejects_odd_length() {
        assert!(hex_decode("abc").is_err());
    }

    #[test]
    fn random_pin_is_right_length_and_numeric() {
        let pin = random_numeric_pin(6);
        assert_eq!(pin.len(), 6);
        assert!(pin.chars().all(|c| c.is_ascii_digit()));
    }

    #[tokio::test]
    async fn frame_round_trips_over_a_duplex_stream() {
        let (mut a, mut b) = tokio::io::duplex(4096);
        let payload = b"hello reflectodoro".to_vec();
        write_frame(&mut a, &payload).await.unwrap();
        let received = read_frame(&mut b, 4096).await.unwrap();
        assert_eq!(received, payload);
    }

    #[tokio::test]
    async fn read_frame_rejects_a_frame_over_the_max_length() {
        let (mut a, mut b) = tokio::io::duplex(4096);
        write_frame(&mut a, &[0u8; 100]).await.unwrap();
        let result = read_frame(&mut b, 50).await;
        assert!(result.is_err());
    }

    /// The core security property from this module's doc comment: matching
    /// PINs on both sides derive the same SPAKE2 key, and that key lets the
    /// following Noise handshake complete -- while a wrong PIN on one side
    /// derives a *different* key, which makes the Noise handshake fail
    /// outright (the AEAD tag doesn't verify) rather than silently
    /// succeeding with mismatched keys.
    #[test]
    fn matching_pins_derive_the_same_key_and_complete_the_noise_handshake() {
        let id_a = Identity::new(b"reflectodoro-pairing-host");
        let id_b = Identity::new(b"reflectodoro-pairing-joiner");

        let (s2_a, msg_a) = Spake2::<Ed25519Group>::start_a(&Password::new(b"123456"), &id_a, &id_b);
        let (s2_b, msg_b) = Spake2::<Ed25519Group>::start_b(&Password::new(b"123456"), &id_a, &id_b);

        let key_a = s2_a.finish(&msg_b).unwrap();
        let key_b = s2_b.finish(&msg_a).unwrap();
        assert_eq!(key_a, key_b, "matching PINs must derive identical keys");

        let params = noise_params();
        let mut initiator = snow::Builder::new(params.clone()).psk(0, &key_b).build_initiator().unwrap();
        let mut responder = snow::Builder::new(params).psk(0, &key_a).build_responder().unwrap();

        let mut buf1 = vec![0u8; NOISE_MAX_MSG];
        let n1 = initiator.write_message(&[], &mut buf1).unwrap();
        let mut buf2 = vec![0u8; NOISE_MAX_MSG];
        responder.read_message(&buf1[..n1], &mut buf2).expect("responder must accept the initiator's first message");

        let mut buf3 = vec![0u8; NOISE_MAX_MSG];
        let n3 = responder.write_message(&[], &mut buf3).unwrap();
        let mut buf4 = vec![0u8; NOISE_MAX_MSG];
        initiator.read_message(&buf3[..n3], &mut buf4).expect("initiator must accept the responder's reply");

        assert!(initiator.into_transport_mode().is_ok());
        assert!(responder.into_transport_mode().is_ok());
    }

    #[test]
    fn mismatched_pins_fail_the_noise_handshake_rather_than_silently_diverging() {
        let id_a = Identity::new(b"reflectodoro-pairing-host");
        let id_b = Identity::new(b"reflectodoro-pairing-joiner");

        let (s2_a, msg_a) = Spake2::<Ed25519Group>::start_a(&Password::new(b"111111"), &id_a, &id_b);
        let (s2_b, msg_b) = Spake2::<Ed25519Group>::start_b(&Password::new(b"999999"), &id_a, &id_b);

        let key_a = s2_a.finish(&msg_b).unwrap();
        let key_b = s2_b.finish(&msg_a).unwrap();
        assert_ne!(key_a, key_b, "different PINs must derive different keys");

        let params = noise_params();
        let mut initiator = snow::Builder::new(params.clone()).psk(0, &key_b).build_initiator().unwrap();
        let mut responder = snow::Builder::new(params).psk(0, &key_a).build_responder().unwrap();

        let mut buf1 = vec![0u8; NOISE_MAX_MSG];
        let n1 = initiator.write_message(&[], &mut buf1).unwrap();
        let mut buf2 = vec![0u8; NOISE_MAX_MSG];
        let result = responder.read_message(&buf1[..n1], &mut buf2);
        assert!(result.is_err(), "a mismatched key must make the Noise handshake fail, not silently succeed");
    }

    #[tokio::test]
    async fn encrypted_json_round_trips_including_a_multi_chunk_payload() {
        let id_a = Identity::new(b"a");
        let id_b = Identity::new(b"b");
        let (s2_a, msg_a) = Spake2::<Ed25519Group>::start_a(&Password::new(b"pw"), &id_a, &id_b);
        let (s2_b, msg_b) = Spake2::<Ed25519Group>::start_b(&Password::new(b"pw"), &id_a, &id_b);
        let key_a = s2_a.finish(&msg_b).unwrap();
        let key_b = s2_b.finish(&msg_a).unwrap();

        let params = noise_params();
        let mut hs_a = snow::Builder::new(params.clone()).psk(0, &key_a).build_initiator().unwrap();
        let mut hs_b = snow::Builder::new(params).psk(0, &key_b).build_responder().unwrap();
        let mut buf = vec![0u8; NOISE_MAX_MSG];
        let n = hs_a.write_message(&[], &mut buf).unwrap();
        let mut buf2 = vec![0u8; NOISE_MAX_MSG];
        hs_b.read_message(&buf[..n], &mut buf2).unwrap();
        let n = hs_b.write_message(&[], &mut buf2).unwrap();
        let mut buf3 = vec![0u8; NOISE_MAX_MSG];
        hs_a.read_message(&buf2[..n], &mut buf3).unwrap();
        let mut transport_a = hs_a.into_transport_mode().unwrap();
        let mut transport_b = hs_b.into_transport_mode().unwrap();

        // Larger than PLAINTEXT_CHUNK so this exercises the multi-frame path,
        // not just a single small message.
        let big_text = "x".repeat(PLAINTEXT_CHUNK * 2 + 500);
        let payload = SyncPayload {
            reflection: vec![import::ImportReflectionRow {
                created_at: "2026-01-01T00:00:00.000Z".to_string(),
                slot_start_at: "2026-01-01T00:00:00.000Z".to_string(),
                text: big_text.clone(),
                updated_at: Some("2026-01-01T00:00:00.000Z".to_string()),
            }],
            ..Default::default()
        };

        let (mut a, mut b) = tokio::io::duplex(1024 * 1024);
        let send = async { send_encrypted_json(&mut a, &mut transport_a, &payload).await.unwrap() };
        let recv = async { recv_encrypted_json::<SyncPayload, _>(&mut b, &mut transport_b).await.unwrap() };
        let (_, received) = tokio::join!(send, recv);

        assert_eq!(received.reflection.len(), 1);
        assert_eq!(received.reflection[0].text, big_text);
    }
}
