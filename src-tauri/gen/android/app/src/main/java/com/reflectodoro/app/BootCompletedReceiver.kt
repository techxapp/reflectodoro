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

  fun isEnabled(context: Context): Boolean {
    return context.getSharedPreferences(PREFS_NAME, Context.MODE_PRIVATE)
      .getBoolean(PREF_POMODORO_ENABLED, true)
  }
}

class BootCompletedReceiver : BroadcastReceiver() {
  override fun onReceive(context: Context, intent: Intent) {
    if (intent.action != Intent.ACTION_BOOT_COMPLETED) return
    if (!PomodoroEnabledPref.isEnabled(context)) return
    context.startForegroundService(Intent(context, BreakSchedulerService::class.java))
  }
}
