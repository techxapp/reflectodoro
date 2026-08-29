package com.reflectodoro.app

import android.content.BroadcastReceiver
import android.content.Context
import android.content.Intent

/** Re-arms the scheduler after a reboot. No persisted "was Pomodoro mode
 * on" flag exists anywhere -- POMODORO_ENABLED in lib.rs isn't persisted on
 * any platform, it resets to its default of true on every fresh process
 * start -- so this just always restarts the foreground service, matching
 * that same default rather than inventing a new one. */
class BootCompletedReceiver : BroadcastReceiver() {
  override fun onReceive(context: Context, intent: Intent) {
    if (intent.action != Intent.ACTION_BOOT_COMPLETED) return
    context.startForegroundService(Intent(context, BreakSchedulerService::class.java))
  }
}
