package com.reflectodoro.app

import android.content.BroadcastReceiver
import android.content.Context
import android.content.Intent
import android.os.Handler
import android.os.Looper
import android.util.Log

/** Fires at every grid boundary (see scheduleNextAlarm in
 * BreakScheduling.kt) as a Doze-surviving backup to the foreground service.
 * Deliberately doesn't decide phase/unlock state itself -- see
 * BreakScheduling.kt's postWakeNotification doc comment for why. */
class BreakAlarmReceiver : BroadcastReceiver() {
  companion object {
    private const val TAG = "BreakAlarmReceiver"

    /** How long a live scheduler gets to answer the wake with a heartbeat.
     * It normally answers in well under a second. Kept well inside the ~10s a
     * goAsync() receiver is allowed, since recover()'s startActivity relies on
     * this broadcast still being in flight for its background-launch
     * allowance. */
    private const val WAKE_ANSWER_TIMEOUT_MS = 5_000L
  }

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
    // crash the whole app on every single grid boundary. Whether the app then
    // needs recovering is decided below, the same way either way.
    try {
      context.startForegroundService(Intent(context, BreakSchedulerService::class.java))
    } catch (e: Exception) {
      Log.w(TAG, "startForegroundService failed: $e")
      // The service is what re-arms the next alarm (setAlarmClock is
      // one-shot). Without it, re-arm here, or the chain ends now -- recover()
      // no longer relaunches the app outside a break, so nothing else would.
      scheduleNextAlarm(context)
    }

    // No scheduler has ever run in this process: it was dead (this receiver
    // started it fresh), or only the service was restarted -- run_scheduler
    // only starts via MainActivity's Activity-creation path.
    if (!MainActivity.hasSchedulerReported()) {
      Log.i(TAG, "no scheduler in this process -- recovering")
      recover(context)
      return
    }

    // A scheduler did run here, but it may have died since without taking the
    // process with it. Its age alone can't tell us: it sleeps on
    // CLOCK_MONOTONIC, which freezes in deep sleep, so after any screen-off
    // stretch a live scheduler's last heartbeat is minutes old. Judging by
    // age used to relaunch the app over whatever the user was doing (plus a
    // "Tap to reopen" notification) at nearly every boundary, break end
    // included. Instead: wake it -- which also makes it act on this boundary
    // right away rather than up to MOBILE_POLL_INTERVAL of awake time later --
    // and see whether it answers.
    val receivedAt = System.currentTimeMillis()
    val channel = NativeBridgePlugin.sharedChannel
    if (channel == null) {
      Log.w(TAG, "no Rust channel to wake the scheduler with -- waiting for its own poll")
    } else {
      try {
        channel.sendObject(mapOf("kind" to "scheduler_wake"))
      } catch (e: Exception) {
        Log.w(TAG, "scheduler_wake send failed: $e")
      }
    }

    val pending = goAsync()
    Handler(Looper.getMainLooper()).postDelayed({
      try {
        if (MainActivity.lastSchedulerHeartbeatAt >= receivedAt) {
          Log.i(TAG, "scheduler answered the wake -- alive, nothing to recover")
        } else {
          Log.w(TAG, "scheduler didn't answer within ${WAKE_ANSWER_TIMEOUT_MS}ms -- recovering")
          recover(context)
        }
      } finally {
        pending.finish()
      }
    }, WAKE_ANSWER_TIMEOUT_MS)
  }

  /** Whether a recovery launch would have a break to show. Outside a break
   * (most concretely the break-end boundary, which fires this receiver just as
   * often as break start) a dead scheduler has nothing to do until the next
   * break -- the alarm chain is kept going by BreakSchedulerService either way,
   * and the next break-start boundary recovers then. Relaunching anyway pulled
   * the app over whatever the user had gone back to, at every break end. Same
   * for Pomodoro mode off or snoozed (night pause included): no break opens. */
  private fun recoveryHasBreakToShow(context: Context): Boolean {
    if (!PomodoroEnabledPref.isEnabled(context)) return false
    if (PomodoroEnabledPref.getSnoozeUntilMs(context) > System.currentTimeMillis()) return false
    return isInBreakNow(context)
  }

  private fun recover(context: Context) {
    if (!recoveryHasBreakToShow(context)) {
      Log.i(TAG, "no live scheduler, but not inside an active break -- not relaunching")
      return
    }
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
