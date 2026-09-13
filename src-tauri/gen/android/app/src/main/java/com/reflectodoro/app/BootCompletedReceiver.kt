package com.reflectodoro.app

import android.content.BroadcastReceiver
import android.content.Context
import android.content.Intent

/** Re-arms the scheduler after a reboot -- unless the user had deliberately
 * turned Pomodoro mode off, in which case re-arming it here would force the
 * service/notification/alarms back on despite `set_enabled(false)` having
 * explicitly stopped all three (see commands::set_enabled). That flag lives
 * in Rust's `POMODORO_ENABLED`, which isn't persisted anywhere Rust-side
 * (it resets to its default of true on every fresh process start) -- and a
 * freshly booted process has no Tauri/Rust runtime running yet for this
 * receiver to ask anyway. So NativeBridgePlugin.persistPomodoroEnabled
 * mirrors the toggle into a plain SharedPreferences flag every time it
 * changes, purely so this receiver has something to read before Rust exists
 * in this process incarnation. Defaults to enabled (true) so a fresh install
 * that has never touched the toggle keeps today's always-re-arm behavior. */
object PomodoroEnabledPref {
  const val PREFS_NAME = "reflectodoro_prefs"
  const val PREF_POMODORO_ENABLED = "pomodoro_enabled"
  // Epoch-millis resume time for an in-progress snooze (see
  // NativeBridgePlugin.persistPomodoroSnoozeUntil / commands::snooze_pomodoro
  // in Rust), 0 = no snooze pending. Persisted so a snooze survives the
  // process being killed while backgrounded (some OEM skins kill a
  // foreground-service process outright when the user swipes it from
  // Recent Apps) -- Rust's own POMODORO_SNOOZE_UNTIL_MS atomic resets to 0
  // on every fresh process start otherwise, silently cancelling the pause.
  const val PREF_SNOOZE_UNTIL_MS = "pomodoro_snooze_until_ms"
  // The originally chosen snooze duration (30/60/90/120), persisted purely
  // so the main window's dropdown can redisplay the right selected <option>
  // after a restore -- Rust's SnoozeInfo.minutes drives the <select>'s
  // value, and 0 (the Rust atomic's own default) matches none of the
  // dropdown's fixed option values, which left it rendering blank/empty
  // rather than falling back to any option at all.
  const val PREF_SNOOZE_MINUTES = "pomodoro_snooze_minutes"

  fun isEnabled(context: Context): Boolean {
    return context.getSharedPreferences(PREFS_NAME, Context.MODE_PRIVATE)
      .getBoolean(PREF_POMODORO_ENABLED, true)
  }

  fun getSnoozeUntilMs(context: Context): Long {
    return context.getSharedPreferences(PREFS_NAME, Context.MODE_PRIVATE)
      .getLong(PREF_SNOOZE_UNTIL_MS, 0L)
  }

  fun getSnoozeMinutes(context: Context): Int {
    return context.getSharedPreferences(PREFS_NAME, Context.MODE_PRIVATE)
      .getInt(PREF_SNOOZE_MINUTES, 0)
  }
}

class BootCompletedReceiver : BroadcastReceiver() {
  override fun onReceive(context: Context, intent: Intent) {
    if (intent.action != Intent.ACTION_BOOT_COMPLETED) return
    if (!PomodoroEnabledPref.isEnabled(context)) return
    context.startForegroundService(Intent(context, BreakSchedulerService::class.java))
  }
}
