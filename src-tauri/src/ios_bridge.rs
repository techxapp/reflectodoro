//! Rust-side handle to `ios-bridge/Sources/NativeBridgePlugin.swift`, the
//! iOS counterpart of android_bridge.rs. App-internal only: nothing in the
//! frontend calls it. build.rs links the Swift package into this library.
//!
//! Every Swift method resolves on Tauri's background IPC queue without
//! touching the main thread, so these are safe to call from `.setup()` and
//! from synchronous commands (which run on the main thread) -- see the
//! Swift file's header before adding anything UIKit-based.
//!
//! One exception: `discover_p2p_services` blocks its caller for the length of
//! its browse window, so it must only be called from async code on the tokio
//! pool. Everything else here returns promptly.
#![cfg(target_os = "ios")]

use serde_json::Value;
use tauri::{
    plugin::{mobile::PluginInvokeError, Builder, PluginHandle},
    Manager, Runtime,
};

tauri::ios_plugin_binding!(init_plugin_reflectodoro);

pub struct IosBridge<R: Runtime>(PluginHandle<R>);

/// One Live Activity snapshot; see `BreakActivityAttributes.swift`.
pub struct LiveActivityState {
    pub phase: &'static str,
    pub phase_end: String,
    pub next_break_start: String,
    pub next_break_end: String,
    pub reflection_pending: bool,
}

impl<R: Runtime> IosBridge<R> {
    pub fn get_device_name(&self) -> Result<Value, PluginInvokeError> {
        self.0.run_mobile_plugin("getDeviceName", ())
    }

    pub fn get_system_info(&self) -> Result<Value, PluginInvokeError> {
        self.0.run_mobile_plugin("getSystemInfo", ())
    }

    /// Replaces the pending break notifications with exactly `fire_dates`
    /// (RFC 3339). Idempotent: unchanged dates keep their existing request.
    pub fn schedule_break_notifications(
        &self,
        fire_dates: &[String],
        title: &str,
        body: &str,
    ) -> Result<Value, PluginInvokeError> {
        self.0.run_mobile_plugin(
            "scheduleBreakNotifications",
            serde_json::json!({ "fireDates": fire_dates, "title": title, "body": body }),
        )
    }

    pub fn clear_break_notifications(&self) -> Result<Value, PluginInvokeError> {
        self.0.run_mobile_plugin("clearBreakNotifications", ())
    }

    pub fn start_or_update_live_activity(&self, s: &LiveActivityState) -> Result<Value, PluginInvokeError> {
        self.0.run_mobile_plugin(
            "startOrUpdateLiveActivity",
            serde_json::json!({
                "phase": s.phase,
                "phaseEnd": s.phase_end,
                "nextBreakStart": s.next_break_start,
                "nextBreakEnd": s.next_break_end,
                "reflectionPending": s.reflection_pending,
            }),
        )
    }

    pub fn end_live_activity(&self) -> Result<Value, PluginInvokeError> {
        self.0.run_mobile_plugin("endLiveActivity", ())
    }

    pub fn persist_pomodoro_enabled(&self, enabled: bool) -> Result<Value, PluginInvokeError> {
        self.0
            .run_mobile_plugin("persistPomodoroEnabled", serde_json::json!({ "enabled": enabled }))
    }

    /// Defaults to `true` when nothing was ever persisted.
    pub fn get_persisted_pomodoro_enabled(&self) -> Result<bool, PluginInvokeError> {
        let v: Value = self.0.run_mobile_plugin("getPersistedPomodoroEnabled", ())?;
        Ok(v.get("enabled").and_then(|x| x.as_bool()).unwrap_or(true))
    }

    pub fn persist_pomodoro_snooze_until(&self, until_ms: i64, minutes: u32) -> Result<Value, PluginInvokeError> {
        self.0.run_mobile_plugin(
            "persistPomodoroSnoozeUntil",
            serde_json::json!({ "untilMs": until_ms, "minutes": minutes }),
        )
    }

    /// Returns `(until_ms, minutes)`, same shape as Android's.
    pub fn get_persisted_pomodoro_snooze_until(&self) -> Result<(i64, u32), PluginInvokeError> {
        let v: Value = self.0.run_mobile_plugin("getPersistedPomodoroSnoozeUntil", ())?;
        let until_ms = v.get("untilMs").and_then(|x| x.as_i64()).unwrap_or(0);
        let minutes = v.get("minutes").and_then(|x| x.as_u64()).unwrap_or(0) as u32;
        Ok((until_ms, minutes))
    }

    /// Returns whether other apps' audio was actually interrupted; `false`
    /// (not an error) when iOS refused, e.g. while backgrounded.
    pub fn pause_other_audio(&self) -> Result<(bool, Option<String>), PluginInvokeError> {
        let v: Value = self.0.run_mobile_plugin("pauseOtherAudio", ())?;
        Ok((
            v.get("paused").and_then(|x| x.as_bool()).unwrap_or(false),
            v.get("error").and_then(|x| x.as_str()).map(str::to_owned),
        ))
    }

    pub fn resume_other_audio(&self) -> Result<Value, PluginInvokeError> {
        self.0.run_mobile_plugin("resumeOtherAudio", ())
    }

    pub fn exclude_from_backup(&self, path: &str) -> Result<Value, PluginInvokeError> {
        self.0.run_mobile_plugin("excludeFromBackup", serde_json::json!({ "path": path }))
    }

    /// Advertises this device on the LAN for P2P sync (p2p_sync.rs) via
    /// Bonjour -- `mdns-sd`'s raw multicast sockets would need Apple's
    /// restricted multicast entitlement, so like Android this goes through
    /// the native API instead (see P2pBonjour.swift for why `NetService`
    /// rather than Network.framework). `device_id`/`name`/`platform` become
    /// the service's TXT record; `port` is p2p_sync::PORT, the same Rust TCP
    /// listener desktop peers connect to -- only discovery is native.
    ///
    /// Returns as soon as the publish is dispatched, so it stays safe to call
    /// from `.setup()`.
    pub fn register_p2p_service(&self, device_id: &str, name: &str, platform: &str, port: u16) -> Result<Value, PluginInvokeError> {
        self.0.run_mobile_plugin(
            "registerP2pService",
            serde_json::json!({ "deviceId": device_id, "name": name, "platform": platform, "port": port }),
        )
    }

    /// Stops advertising. Only called as the unregister-before-register half
    /// of `advertise_self`'s idempotency, same as Android's.
    pub fn unregister_p2p_service(&self) -> Result<Value, PluginInvokeError> {
        self.0.run_mobile_plugin("unregisterP2pService", ())
    }

    /// Runs a Bonjour browse burst for `timeout_ms` and returns
    /// `{"devices": [{deviceId, name, platform, host, port}]}` -- deliberately
    /// the exact JSON shape Android's `discover_p2p_services` returns, so
    /// p2p_sync's two mobile arms can share one parser.
    ///
    /// Unlike everything else in this file, this **blocks the calling thread
    /// for the full `timeout_ms`** (~3s) while the burst runs. That's fine
    /// only because its one caller is `browse_lan`, reached from async
    /// commands on the tokio pool -- never from `.setup()` or a synchronous
    /// command, which would be the main thread.
    pub fn discover_p2p_services(&self, timeout_ms: u64) -> Result<Value, PluginInvokeError> {
        self.0
            .run_mobile_plugin("discoverP2pServices", serde_json::json!({ "timeoutMs": timeout_ms }))
    }
}

pub fn register<R: Runtime>(builder: tauri::Builder<R>) -> tauri::Builder<R> {
    builder.plugin(
        Builder::<R, ()>::new("reflectodoro-ios-bridge")
            .setup(|app, api| {
                let handle = api.register_ios_plugin(init_plugin_reflectodoro)?;
                app.manage(IosBridge(handle));
                Ok(())
            })
            .build(),
    )
}
