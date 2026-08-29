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

    /// Whether `scheduleNextAlarm` (BreakScheduling.kt) will get to use a
    /// true exact alarm rather than its inexact fallback -- surfaced to the
    /// onboarding screen so it only prompts for a grant that's actually
    /// missing.
    pub fn can_schedule_exact_alarms(&self) -> Result<Value, PluginInvokeError> {
        self.0.run_mobile_plugin("canScheduleExactAlarms", ())
    }

    /// Opens the system settings screen for the exact-alarm grant -- there
    /// is no in-app runtime-dialog form of this permission, unlike
    /// POST_NOTIFICATIONS.
    pub fn request_exact_alarm_permission(&self) -> Result<Value, PluginInvokeError> {
        self.0.run_mobile_plugin("requestExactAlarmPermission", ())
    }

    /// Posts a break notification if the app isn't already visible (a no-op
    /// otherwise, decided on the Kotlin side via MainActivity.isResumed) --
    /// called from the Android arm of overlay::spawn_or_update_overlay.
    /// `persistent` mirrors BREAK_NOTIFICATION_PERSISTENT_ENABLED: whether
    /// the notification can be swiped away or only clears once the break
    /// actually resolves. Deliberately not a full-screen-intent/auto-launch
    /// -- see that static's doc comment for why.
    pub fn trigger_break_screen(&self, persistent: bool) -> Result<Value, PluginInvokeError> {
        self.0
            .run_mobile_plugin("triggerBreakScreen", serde_json::json!({ "persistent": persistent }))
    }

    /// Clears a break notification the user resolved some other way than
    /// tapping it -- called from the Android arm of overlay::close_overlay.
    pub fn cancel_break_notification(&self) -> Result<Value, PluginInvokeError> {
        self.0.run_mobile_plugin("cancelBreakNotification", ())
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
