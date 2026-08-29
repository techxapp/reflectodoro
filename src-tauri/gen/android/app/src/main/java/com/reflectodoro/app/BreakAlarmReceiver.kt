package com.reflectodoro.app

import android.content.BroadcastReceiver
import android.content.Context
import android.content.Intent

/** Fires at every grid boundary (see scheduleNextAlarm in
 * BreakScheduling.kt) as a Doze-surviving backup to the foreground service.
 * Deliberately doesn't decide phase/unlock state itself -- see
 * BreakScheduling.kt's postWakeNotification doc comment for why. */
class BreakAlarmReceiver : BroadcastReceiver() {
  override fun onReceive(context: Context, intent: Intent) {
    // Covers "the service died but the process/Activity didn't" -- cheap
    // and idempotent even if it was already running.
    context.startForegroundService(Intent(context, BreakSchedulerService::class.java))

    // Covers "the whole process was dead" -- a plain service restart alone
    // can't revive run_scheduler (it only starts via MainActivity's
    // Activity-creation path, see MainActivity.schedulerStarted), so only
    // do this when that path hasn't run yet in this process incarnation.
    if (!MainActivity.schedulerStarted) {
      postWakeNotification(context)
    }
  }
}
