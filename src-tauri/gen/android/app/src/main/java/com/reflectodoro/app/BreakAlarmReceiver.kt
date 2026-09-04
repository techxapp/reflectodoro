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
    //
    // Wrapped in try/catch: the foreground-service-start exemption this
    // alarm chain relies on (see scheduleNextAlarm's own doc comment) only
    // applies to the exact setAlarmClock path. When exact alarms aren't
    // granted, scheduleNextAlarm falls back to a plain inexact
    // alarmManager.set(), which carries no such exemption -- so on API 31+
    // with "Alarms & reminders" not granted, startForegroundService() here
    // throws ForegroundServiceStartNotAllowedException and would otherwise
    // crash the whole app on every single grid boundary (every 25-30
    // minutes) for as long as that permission stays ungranted. Falling back
    // to postWakeNotification -- the same recovery notification already
    // used below for "the whole process was dead" -- degrades gracefully
    // instead of crashing.
    var serviceStartFailed = false
    try {
      context.startForegroundService(Intent(context, BreakSchedulerService::class.java))
    } catch (e: Exception) {
      serviceStartFailed = true
    }

    // Covers "the whole process was dead" (a plain service restart alone
    // can't revive run_scheduler, it only starts via MainActivity's
    // Activity-creation path) as well as "the process is alive but the
    // scheduler task itself died without taking it down" -- see
    // MainActivity.isSchedulerAlive's doc comment for why this checks a
    // recent heartbeat rather than a one-time "did onCreate ever run" flag.
    if (!MainActivity.isSchedulerAlive()) {
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
    } else if (serviceStartFailed) {
      // The scheduler heartbeat is recent, so run_scheduler is presumably
      // still running in-process and should notice this boundary on its own
      // -- the failed restart above was likely a no-op attempt on a service
      // that didn't actually need reviving. Still worth a visible nudge in
      // case that assumption is wrong on this particular device.
      postWakeNotification(context)
    }
  }
}
