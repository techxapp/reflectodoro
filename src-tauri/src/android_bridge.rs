//! Rust-side handle to `NativeBridgePlugin.kt`
//! (`gen/android/app/src/main/java/com/reflectodoro/app/NativeBridgePlugin.kt`),
//! a plain `@TauriPlugin` class living directly in the app's own Android
//! module rather than a separately published plugin crate -- this bridge is
//! app-internal only (nothing in the frontend calls it), so it skips the
//! `tauri_plugin::Builder`/`android_path()`/build.rs scaffolding real
//! published plugins use for that; `register_android_plugin` just needs a
//! class on the app's own classpath, which `:app`'s own Kotlin sources
//! already are.
#![cfg(target_os = "android")]

use serde_json::Value;
use tauri::{
    ipc::Channel,
    plugin::{mobile::PluginInvokeError, Builder, PluginHandle},
    Manager, Runtime,
};

pub struct AndroidBridge<R: Runtime>(PluginHandle<R>);

impl<R: Runtime> AndroidBridge<R> {
    pub fn ping(&self) -> Result<Value, PluginInvokeError> {
        self.0.run_mobile_plugin("ping", ())
    }

    /// Starts (or refreshes) `BreakSchedulerService`, which just raises
    /// this process's priority and arms the `AlarmManager` backup -- it
    /// never independently decides phase transitions; `run_scheduler` in
    /// lib.rs remains the sole source of truth for that once it's running.
    pub fn start_foreground_service(&self) -> Result<Value, PluginInvokeError> {
        self.0.run_mobile_plugin("startForegroundService", ())
    }

    /// Stops the service and cancels the pending alarm -- called when the
    /// user turns Pomodoro mode off (see commands::set_enabled), so
    /// disabling it actually lets Android reclaim the process instead of
    /// leaving a phantom "running" notification and a dangling alarm.
    pub fn stop_foreground_service(&self) -> Result<Value, PluginInvokeError> {
        self.0.run_mobile_plugin("stopForegroundService", ())
    }

    /// Brings the break to the user's attention -- always, even if the app
    /// is currently visible: a resumed Activity has no OS-level protection
    /// against the user backgrounding it a moment later, so this can't be
    /// skipped just because it happens to be foregrounded right now (see
    /// NativeBridgePlugin.kt's triggerBreakScreen doc comment for the
    /// real-device bug that skipping caused). Called from the Android arm of
    /// overlay::spawn_or_update_overlay. Kotlin itself picks the surface:
    /// the native WindowManager overlay (native_overlay.rs/
    /// NativeOverlayManager.kt) when the "display over other apps" permission
    /// is granted, else falls back to a break notification. `persistent`
    /// mirrors BREAK_NOTIFICATION_PERSISTENT_ENABLED (only relevant to the
    /// notification fallback); `state` is the current OverlayState, passed
    /// through so the native overlay has real content (challenge, slot) to
    /// show the instant it's created rather than a blank frame. Deliberately
    /// not a full-screen-intent/auto-launch -- see that static's doc comment
    /// for why.
    pub fn trigger_break_screen(&self, persistent: bool, state: Value) -> Result<Value, PluginInvokeError> {
        // Sent as a JSON string, not a nested object: Kotlin treats it as
        // opaque (just relaying it into the overlay WebView via
        // JSONObject.quote), so there's no need for a typed Jackson class
        // matching OverlayState's shape on that side.
        self.0.run_mobile_plugin(
            "triggerBreakScreen",
            serde_json::json!({ "persistent": persistent, "state": state.to_string() }),
        )
    }

    /// Clears a break notification / hides the native overlay, whichever
    /// (if either) is currently up -- called from the Android arm of
    /// overlay::close_overlay. Harmless no-op for whichever surface wasn't
    /// actually in use.
    pub fn cancel_break_notification(&self) -> Result<Value, PluginInvokeError> {
        self.0.run_mobile_plugin("cancelBreakNotification", ())
    }

    /// Pushes updated OverlayState into the native overlay's WebView (e.g.
    /// breakit_matched flipping, or a merged slot's new challenge/timer) --
    /// called from every overlay::emit_state on Android. A harmless no-op if
    /// the native overlay isn't currently showing (Kotlin decides that, not
    /// Rust -- see NativeOverlayManager.isShowing).
    pub fn update_native_overlay(&self, state: Value) -> Result<Value, PluginInvokeError> {
        self.0
            .run_mobile_plugin("updateNativeOverlay", serde_json::json!({ "state": state.to_string() }))
    }

    /// Registers the one long-lived Channel Kotlin uses to call back into
    /// Rust whenever the user interacts with the native overlay (submitting
    /// a reflection, attempting the breakit code) -- see
    /// native_overlay.rs::install_channel, called once from lib.rs's setup().
    pub fn init_native_overlay_channel(&self, channel: Channel<Value>) -> Result<Value, PluginInvokeError> {
        self.0
            .run_mobile_plugin("initNativeOverlayChannel", serde_json::json!({ "channel": channel }))
    }

    /// Whether "Display over other apps" is granted -- surfaced to
    /// onboarding/Settings so it only prompts for a grant that's actually
    /// missing.
    pub fn can_draw_overlays(&self) -> Result<Value, PluginInvokeError> {
        self.0.run_mobile_plugin("canDrawOverlays", ())
    }

    /// Deep-links to the system settings screen for the grant -- there is no
    /// in-app runtime-dialog form of this permission.
    pub fn request_draw_overlays_permission(&self) -> Result<Value, PluginInvokeError> {
        self.0.run_mobile_plugin("requestDrawOverlaysPermission", ())
    }

    /// Whether `scheduleNextAlarm` (BreakScheduling.kt) can use
    /// `setAlarmClock`'s real-exact/foreground-launch-exempt path rather
    /// than its degraded inexact fallback -- surfaced to onboarding/Settings
    /// so it only prompts for a grant that's actually missing.
    pub fn can_schedule_exact_alarms(&self) -> Result<Value, PluginInvokeError> {
        self.0.run_mobile_plugin("canScheduleExactAlarms", ())
    }

    /// Deep-links to the system settings screen for the "Alarms & reminders"
    /// grant -- there is no in-app runtime-dialog form of this permission.
    pub fn request_schedule_exact_alarm_permission(&self) -> Result<Value, PluginInvokeError> {
        self.0.run_mobile_plugin("requestScheduleExactAlarmPermission", ())
    }

    /// Requests transient audio focus (`AUDIOFOCUS_GAIN_TRANSIENT`) so any
    /// well-behaved playing app ducks itself -- Android's media.rs arm has
    /// no cross-app query API like SMTC/MPRIS, so this is a request rather
    /// than a query. Called from media::android_impl::pause_playing_sessions.
    pub fn pause_audio_focus(&self) -> Result<Value, PluginInvokeError> {
        self.0.run_mobile_plugin("pauseAudioFocus", ())
    }

    /// Abandons the focus request from `pause_audio_focus`, if one is
    /// outstanding, so whatever ducked for it is free to resume. Called
    /// from media::android_impl::resume_playing_sessions.
    pub fn resume_audio_focus(&self) -> Result<Value, PluginInvokeError> {
        self.0.run_mobile_plugin("resumeAudioFocus", ())
    }
}

pub fn register<R: Runtime>(builder: tauri::Builder<R>) -> tauri::Builder<R> {
    builder.plugin(
        Builder::<R, ()>::new("reflectodoro-android-bridge")
            .setup(|app, api| {
                let handle = api.register_android_plugin("com.reflectodoro.app", "NativeBridgePlugin")?;
                app.manage(AndroidBridge(handle));
                Ok(())
            })
            .build(),
    )
}
