package com.reflectodoro.app

import android.app.Notification
import android.app.NotificationChannel
import android.app.NotificationManager
import android.app.PendingIntent
import android.app.Service
import android.content.Intent
import android.os.Build
import android.os.IBinder
import androidx.core.app.NotificationCompat

/** Exists purely to raise this process's priority so Android is far less
 * likely to kill it while backgrounded -- it never independently decides
 * phase transitions or shows the break screen itself; run_scheduler in
 * lib.rs remains the sole source of truth for that once it's running. Only
 * job: post the required foreground-service notification and keep
 * scheduleNextAlarm's chain going as a backup. Started from Rust
 * (android_bridge.rs) when Pomodoro mode is on, and re-pinged by
 * BreakAlarmReceiver at every boundary. */
class BreakSchedulerService : Service() {
  companion object {
    const val CHANNEL_ID = "reflectodoro_running"
    const val NOTIFICATION_ID = 1001

    // Set/cleared in onCreate/onDestroy. Lets postBreakNotification /
    // cancelBreakNotification (BreakScheduling.kt) swap this service's own
    // foreground notification between its normal "running" content and
    // break content, instead of posting a separate notification -- a
    // notification tied to a *live foreground service* gets a much
    // stronger non-dismissible guarantee from Android than a plain
    // setOngoing(true) notification does on its own (confirmed
    // empirically: a stand-alone "ongoing" notification, especially once
    // auto-grouped in the shade with another notification from the same
    // app, could still be swiped away in testing). This is the only way
    // found to be genuinely non-dismissible.
    private var instance: BreakSchedulerService? = null

    fun isRunning(): Boolean = instance != null

    fun enterBreakMode() {
      instance?.setBreakMode(true)
    }

    fun exitBreakMode() {
      instance?.setBreakMode(false)
    }
  }

  private var inBreakMode = false

  override fun onBind(intent: Intent?): IBinder? = null

  override fun onCreate() {
    super.onCreate()
    instance = this
    startForeground(NOTIFICATION_ID, buildNotification())
    scheduleNextAlarm(this)
  }

  override fun onStartCommand(intent: Intent?, flags: Int, startId: Int): Int {
    // Re-posting (not just relying on the one from onCreate) keeps this
    // idempotent for every subsequent ping -- from Rust on toggle, from
    // BreakAlarmReceiver, or from BootCompletedReceiver -- without caring
    // whether the service instance was already running. Reads the current
    // inBreakMode rather than resetting it, so a redundant restart mid-break
    // (e.g. BreakAlarmReceiver firing again before the break resolves)
    // can't accidentally revert the notification back to "running" content.
    startForeground(NOTIFICATION_ID, buildNotification())
    scheduleNextAlarm(this)
    return START_STICKY
  }

  override fun onDestroy() {
    super.onDestroy()
    if (instance === this) instance = null
  }

  /** Fires when the user swipes this app's task away from Recents -- by
   * default that kills the process, service included, with nothing to
   * revive it until the next BreakAlarmReceiver-triggered ping (the next
   * grid boundary, potentially tens of minutes away). Restarting
   * immediately here is the standard Android idiom for a foreground
   * service that should survive a Recents-swipe the same way it survives
   * plain backgrounding (Home) -- Recents-swipe was never meant to be a
   * *harder* escape hatch than Home, just a different one. It still isn't
   * unbeatable: Force Stop (Settings) remains the one true kill switch
   * that no app can override, by OS design -- see CLAUDE.md's Kill
   * switches section, same category as Task Manager on Windows. Whether
   * this self-revival actually survives is also OEM-dependent: some
   * manufacturers' battery/process managers (Huawei/Honor's in particular)
   * can still prevent it regardless of what the app does here. */
  override fun onTaskRemoved(rootIntent: Intent?) {
    super.onTaskRemoved(rootIntent)
    // Try/catch: onTaskRemoved fires with the app already in a background
    // state, and API 31+'s background-foreground-service-start restrictions
    // can reject this restart attempt the same way they can reject
    // BreakAlarmReceiver's (see that file's own doc comment for the exact
    // exemption story) -- a Recents-swipe is a routine, frequent user action,
    // so letting that throw uncaught here would crash the app on nearly
    // every swipe-away on an affected device. Nothing else to fall back to
    // here (unlike BreakAlarmReceiver, this isn't the Doze-survival backup
    // path, just the immediate self-revival attempt) -- worst case, the
    // process stays dead until the next AlarmManager-triggered ping, same as
    // if this whole restart attempt had simply been skipped.
    try {
      startForegroundService(Intent(applicationContext, BreakSchedulerService::class.java))
    } catch (e: Exception) {
      // Nothing else this call site can do -- see comment above.
    }
  }

  private fun setBreakMode(breakMode: Boolean) {
    inBreakMode = breakMode
    startForeground(NOTIFICATION_ID, buildNotification())
  }

  private fun buildNotification(): Notification {
    val pendingOpen = PendingIntent.getActivity(
      this,
      0,
      Intent(this, MainActivity::class.java),
      PendingIntent.FLAG_IMMUTABLE or PendingIntent.FLAG_UPDATE_CURRENT,
    )
    if (inBreakMode) {
      ensureBreakChannel(this)
      return NotificationCompat.Builder(this, BREAK_CHANNEL_ID)
        .setContentTitle("Reflectodoro")
        .setContentText("Break time -- tap to reflect")
        .setSmallIcon(applicationInfo.icon)
        .setCategory(NotificationCompat.CATEGORY_ALARM)
        .setPriority(NotificationCompat.PRIORITY_HIGH)
        .setOngoing(true)
        .setContentIntent(pendingOpen)
        .build()
    }
    ensureChannel()
    return NotificationCompat.Builder(this, CHANNEL_ID)
      .setContentTitle("Reflectodoro")
      .setContentText("Pomodoro timer running in the background")
      .setSmallIcon(applicationInfo.icon)
      .setOngoing(true)
      .setContentIntent(pendingOpen)
      .setPriority(NotificationCompat.PRIORITY_LOW)
      .build()
  }

  private fun ensureChannel() {
    if (Build.VERSION.SDK_INT < Build.VERSION_CODES.O) return
    val manager = getSystemService(NotificationManager::class.java)
    if (manager.getNotificationChannel(CHANNEL_ID) != null) return
    val channel = NotificationChannel(
      CHANNEL_ID,
      "Reflectodoro running",
      NotificationManager.IMPORTANCE_LOW,
    )
    channel.description = "Keeps the Pomodoro timer running in the background"
    manager.createNotificationChannel(channel)
  }
}
