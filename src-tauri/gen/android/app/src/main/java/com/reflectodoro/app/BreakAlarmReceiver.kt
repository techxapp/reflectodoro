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

      // Deliberately DOES auto-launch to the foreground here, unlike every
      // other break trigger in this app -- confirmed explicitly with the
      // user as a scoped exception to the "never auto-launch over active
      // use" rule (see BreakScheduling.kt's postBreakNotification doc
      // comment), limited to this one recovery case: the process was
      // killed (most commonly by an OEM battery/process manager -- see
      // CLAUDE.md) badly enough that even the foreground service didn't
      // survive to show the real break/overlay UI, so there is no other
      // way back in short of the user noticing and tapping the wake
      // notification above, possibly tens of minutes later. No
      // FLAG_SHOW_WHEN_LOCKED/FLAG_TURN_SCREEN_ON here -- if the screen is
      // off/locked this just queues the activity to be shown on unlock
      // rather than forcing the screen on, so the "never wake an idle/
      // locked device" constraint from that same decision still holds. This
      // only reliably reaches the foreground -- even over another app
      // actively running -- because scheduleNextAlarm (BreakScheduling.kt)
      // arms this receiver via AlarmManager.setAlarmClock, which is on
      // Android's documented background-activity-launch exemption list.
      // Confirmed empirically on a real device that a plain exact alarm is
      // NOT exempt: this startActivity() only worked from an idle/Home-
      // screen state before that switch, not over a foreground app.
      val launchIntent = Intent(context, MainActivity::class.java).apply {
        flags = Intent.FLAG_ACTIVITY_NEW_TASK
      }
      context.startActivity(launchIntent)
    }
  }
}
