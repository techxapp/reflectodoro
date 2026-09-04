package com.reflectodoro.app

import android.app.AlarmManager
import android.app.NotificationChannel
import android.app.NotificationManager
import android.app.PendingIntent
import android.content.Context
import android.content.Intent
import android.os.Build
import androidx.core.app.NotificationCompat
import java.util.Calendar

const val WAKE_CHANNEL_ID = "reflectodoro_wake"
const val WAKE_NOTIFICATION_ID = 1002
const val BREAK_CHANNEL_ID = "reflectodoro_break"
const val BREAK_NOTIFICATION_ID = 1003

/** Mirrors grid::slot_for's boundary rule (src-tauri/src/grid.rs) just
 * closely enough to pick the next AlarmManager wake time -- :00-:25 work,
 * :25-:30 break, :30-:55 work, :55-:00 break. No phase/unlock decisions are
 * made here; Rust remains the sole source of truth for OverlayState once
 * it's actually running. This only exists because AlarmManager needs a
 * concrete instant before the Tauri runtime is even alive (e.g. right after
 * boot, or after the process was killed) -- recomputed fresh every call,
 * never persisted. */
private fun nextBoundaryMinute(minute: Int): Int = when {
  minute < 25 -> 25
  minute < 30 -> 30
  minute < 55 -> 55
  else -> 60
}

/** (Re)arms the single next AlarmManager wake, replacing whatever was
 * previously scheduled -- setAlarmClock is one-shot, so BreakSchedulerService
 * and BreakAlarmReceiver both call this every time they run to keep the
 * chain going. */
fun scheduleNextAlarm(context: Context) {
  val now = Calendar.getInstance()
  val boundary = nextBoundaryMinute(now.get(Calendar.MINUTE))
  val next = now.clone() as Calendar
  next.set(Calendar.SECOND, 0)
  next.set(Calendar.MILLISECOND, 0)
  if (boundary == 60) {
    next.set(Calendar.MINUTE, 0)
    next.add(Calendar.HOUR_OF_DAY, 1)
  } else {
    next.set(Calendar.MINUTE, boundary)
  }

  val alarmManager = context.getSystemService(Context.ALARM_SERVICE) as AlarmManager
  val pendingIntent = PendingIntent.getBroadcast(
    context,
    0,
    Intent(context, BreakAlarmReceiver::class.java),
    PendingIntent.FLAG_IMMUTABLE or PendingIntent.FLAG_UPDATE_CURRENT,
  )

  // setAlarmClock, not setExactAndAllowWhileIdle -- confirmed empirically
  // (real device) that a plain BroadcastReceiver's startActivity() only
  // reliably brings the app forward from an idle/Home-screen state, not over
  // another app actively in the foreground: Android's background-activity-
  // launch restrictions (10+) don't exempt broadcasts from a regular exact
  // alarm. setAlarmClock is the one AlarmManager entry point the OS does
  // exempt, because it's the same primitive real alarm-clock apps use to
  // interrupt whatever's currently on screen. Side effect, not a bug:
  // Android shows a small persistent alarm-clock icon in the status bar
  // (tapping it opens MainActivity via showIntent below) for as long as
  // Pomodoro mode is on -- the same thing any real alarm-clock app shows,
  // and a deliberate, confirmed tradeoff for the reliability gain.
  //
  // AOSP docs claim setAlarmClock is exempt from the API 31+
  // SCHEDULE_EXACT_ALARM permission entirely -- disproven on a real Android
  // 12 device (HONOR/MagicOS), which threw a SecurityException here despite
  // that documented exemption (see AndroidManifest.xml's comment on this
  // permission). So this is now actually gated on canScheduleExactAlarms(),
  // with a plain inexact alarm as the fallback when it's not granted --
  // degraded (loses the over-another-app foreground exemption above, and
  // the self-heal only fires on whatever schedule the OS batches inexact
  // alarms to), but this is only ever a backup to run_scheduler's
  // in-process scheduling anyway (see this file's header comment), so
  // degrading beats crashing the whole process.
  val showIntent = PendingIntent.getActivity(
    context,
    3,
    Intent(context, MainActivity::class.java).apply { flags = Intent.FLAG_ACTIVITY_NEW_TASK },
    PendingIntent.FLAG_IMMUTABLE or PendingIntent.FLAG_UPDATE_CURRENT,
  )
  if (canScheduleExactAlarm(context)) {
    try {
      alarmManager.setAlarmClock(AlarmManager.AlarmClockInfo(next.timeInMillis, showIntent), pendingIntent)
      return
    } catch (e: SecurityException) {
      // Belt-and-suspenders: canScheduleExactAlarms() said yes but the OEM
      // enforced it anyway. Fall through to the inexact path below rather
      // than crash.
    }
  }
  alarmManager.set(AlarmManager.RTC_WAKEUP, next.timeInMillis, pendingIntent)
}

/** True on API < 31, where this permission doesn't exist at all. On API 31+,
 * reflects whether the user has granted "Alarms & reminders" for this app --
 * see requestScheduleExactAlarmPermission (NativeBridgePlugin.kt) for the
 * only way to prompt for it (no in-app runtime-dialog form exists). */
fun canScheduleExactAlarm(context: Context): Boolean {
  if (Build.VERSION.SDK_INT < Build.VERSION_CODES.S) return true
  val alarmManager = context.getSystemService(AlarmManager::class.java)
  return alarmManager.canScheduleExactAlarms()
}

/** Only shown when BreakAlarmReceiver fires in a process where no Activity
 * has been created yet this incarnation (MainActivity.schedulerStarted is
 * still false) -- i.e. the process was fully dead and Rust's run_scheduler
 * was never running to notice the boundary on its own. Deliberately generic
 * (not "time for your break", no breakit challenge text): a receiver has no
 * way to know the actual phase or challenge without Rust running, and
 * guessing would risk showing stale or wrong content. Tapping it just opens
 * the app, whose fresh run_scheduler iteration figures out the real state
 * immediately on its own.
 *
 * Posted alongside (not instead of) BreakAlarmReceiver's own auto-launch of
 * MainActivity for this same case -- this notification is the fallback if
 * that auto-launch doesn't actually bring the app forward for any reason
 * (an OS/OEM edge case, or the user simply not noticing it happen). This is
 * the one place in the app that *does* auto-launch unprompted -- see
 * BreakAlarmReceiver's own doc comment for why that's a scoped exception to
 * postBreakNotification's "never auto-launch over active use" rule below,
 * not a reversal of it. */
fun postWakeNotification(context: Context) {
  ensureWakeChannel(context)
  val pendingOpen = PendingIntent.getActivity(
    context,
    0,
    Intent(context, MainActivity::class.java),
    PendingIntent.FLAG_IMMUTABLE or PendingIntent.FLAG_UPDATE_CURRENT,
  )
  val notification = NotificationCompat.Builder(context, WAKE_CHANNEL_ID)
    .setContentTitle("Reflectodoro")
    .setContentText("Tap to reopen -- the background timer was stopped by the system.")
    .setSmallIcon(context.applicationInfo.icon)
    .setPriority(NotificationCompat.PRIORITY_DEFAULT)
    .setAutoCancel(true)
    .setContentIntent(pendingOpen)
    .build()
  val manager = context.getSystemService(NotificationManager::class.java)
  manager.notify(WAKE_NOTIFICATION_ID, notification)
}

private fun ensureWakeChannel(context: Context) {
  if (Build.VERSION.SDK_INT < Build.VERSION_CODES.O) return
  val manager = context.getSystemService(NotificationManager::class.java)
  if (manager.getNotificationChannel(WAKE_CHANNEL_ID) != null) return
  val channel = NotificationChannel(
    WAKE_CHANNEL_ID,
    "Reflectodoro wake-up",
    NotificationManager.IMPORTANCE_DEFAULT,
  )
  channel.description = "Lets you reopen Reflectodoro if its background timer was stopped by the system"
  manager.createNotificationChannel(channel)
}

/** Called from the Android arm of overlay.rs's spawn_or_update_overlay --
 * the Rust/frontend side already handles everything for a break correctly
 * on its own (run_scheduler keeps running regardless, and the single
 * window's overlay://state listener navigates to /overlay even while
 * backgrounded); this exists to actually get the user's attention while
 * they're using the phone for something else, since nothing else does that.
 *
 * Deliberately NOT a full-screen-intent/auto-launching notification: that
 * mechanism only auto-launches over a *locked* screen (confirmed
 * empirically -- with the screen on and unlocked, Android shows it as a
 * heads-up notification either way, full-screen-intent or not). The intent
 * here is specifically to interrupt active phone use, not to wake an
 * idle/locked device the user has already put down -- a locked-screen
 * auto-launch would do the opposite of what's wanted. The IMPORTANCE_HIGH
 * channel below is what actually earns the heads-up peek while the screen
 * is on.
 *
 * `persistent` (mirrors app_setting.break_notification_persistent_enabled)
 * controls whether it can be swiped away or only clears once the break
 * actually resolves (see cancelBreakNotification). When true, this routes
 * through BreakSchedulerService's own foreground notification slot instead
 * of posting a separate one -- confirmed empirically that a plain
 * setOngoing(true) notification NOT backed by a live foreground service can
 * still be swiped away (especially once auto-grouped in the shade with
 * another notification from this app), while one tied to an active
 * foreground service gets a real non-dismissible guarantee from Android.
 * Falls back to a plain dismissible notification if the service somehow
 * isn't running (shouldn't normally happen -- Pomodoro mode being enabled
 * is what gets a break triggered at all, and that's also what starts the
 * service).
 *
 * Deliberately generic content (not the breakit challenge text): by the
 * time the user actually sees this, /overlay or the native overlay has
 * already rendered the real thing underneath. Called unconditionally by
 * triggerBreakScreen (NativeBridgePlugin.kt), even for an app that's already
 * in the foreground -- see that command's doc comment for why a resumed-only
 * skip is wrong here. */
fun postBreakNotification(context: Context, persistent: Boolean) {
  if (persistent && BreakSchedulerService.isRunning()) {
    BreakSchedulerService.enterBreakMode()
    return
  }

  ensureBreakChannel(context)
  val contentIntent = Intent(context, MainActivity::class.java).apply {
    flags = Intent.FLAG_ACTIVITY_NEW_TASK or Intent.FLAG_ACTIVITY_SINGLE_TOP
  }
  val pendingContent = PendingIntent.getActivity(
    context,
    2,
    contentIntent,
    PendingIntent.FLAG_IMMUTABLE or PendingIntent.FLAG_UPDATE_CURRENT,
  )
  val notification = NotificationCompat.Builder(context, BREAK_CHANNEL_ID)
    .setContentTitle("Reflectodoro")
    .setContentText("Break time -- tap to reflect")
    .setSmallIcon(context.applicationInfo.icon)
    .setCategory(NotificationCompat.CATEGORY_ALARM)
    .setPriority(NotificationCompat.PRIORITY_HIGH)
    .setAutoCancel(true)
    .setContentIntent(pendingContent)
    .build()

  val manager = context.getSystemService(NotificationManager::class.java)
  manager.notify(BREAK_NOTIFICATION_ID, notification)
}

/** Called from the Android arm of overlay.rs's close_overlay, so a break
 * notification the user never tapped (they switched back to the app some
 * other way) doesn't linger after it's already resolved. Reverts both
 * possible paths postBreakNotification could have taken -- harmless no-op
 * for whichever one wasn't actually used. */
fun cancelBreakNotification(context: Context) {
  BreakSchedulerService.exitBreakMode()
  val manager = context.getSystemService(NotificationManager::class.java)
  manager.cancel(BREAK_NOTIFICATION_ID)
}

fun ensureBreakChannel(context: Context) {
  if (Build.VERSION.SDK_INT < Build.VERSION_CODES.O) return
  val manager = context.getSystemService(NotificationManager::class.java)
  if (manager.getNotificationChannel(BREAK_CHANNEL_ID) != null) return
  val channel = NotificationChannel(
    BREAK_CHANNEL_ID,
    "Reflectodoro break reminders",
    NotificationManager.IMPORTANCE_HIGH,
  )
  channel.description = "Brings Reflectodoro to the foreground when a break starts"
  manager.createNotificationChannel(channel)
}
