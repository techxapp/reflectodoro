package com.reflectodoro.app

import android.app.Activity
import android.app.AlarmManager
import android.app.PendingIntent
import android.content.Intent
import android.media.AudioAttributes
import android.media.AudioFocusRequest
import android.media.AudioManager
import android.net.Uri
import android.os.Build
import android.provider.Settings
import app.tauri.annotation.Command
import app.tauri.annotation.InvokeArg
import app.tauri.annotation.TauriPlugin
import app.tauri.plugin.Invoke
import app.tauri.plugin.JSObject
import app.tauri.plugin.Plugin

@InvokeArg
class TriggerBreakScreenArgs {
    var persistent: Boolean = true
}

@TauriPlugin
class NativeBridgePlugin(private val activity: Activity) : Plugin(activity) {
    // Held only between pauseAudioFocus and the matching resumeAudioFocus
    // for the same break -- never persisted, since a fresh process start
    // has nothing outstanding to release anyway.
    private var audioFocusRequest: AudioFocusRequest? = null

    @Command
    fun ping(invoke: Invoke) {
        val ret = JSObject()
        ret.put("pong", true)
        invoke.resolve(ret)
    }

    @Command
    fun startForegroundService(invoke: Invoke) {
        activity.startForegroundService(Intent(activity, BreakSchedulerService::class.java))
        invoke.resolve(JSObject())
    }

    @Command
    fun stopForegroundService(invoke: Invoke) {
        activity.stopService(Intent(activity, BreakSchedulerService::class.java))
        val alarmManager = activity.getSystemService(AlarmManager::class.java)
        val pendingIntent = PendingIntent.getBroadcast(
            activity,
            0,
            Intent(activity, BreakAlarmReceiver::class.java),
            PendingIntent.FLAG_IMMUTABLE or PendingIntent.FLAG_NO_CREATE,
        )
        pendingIntent?.let { alarmManager.cancel(it) }
        invoke.resolve(JSObject())
    }

    /** No special permission exists below API 31 -- exact alarms always
     * work there, matching scheduleNextAlarm's own SDK_INT check in
     * BreakScheduling.kt. */
    @Command
    fun canScheduleExactAlarms(invoke: Invoke) {
        val alarmManager = activity.getSystemService(AlarmManager::class.java)
        val can = Build.VERSION.SDK_INT < Build.VERSION_CODES.S || alarmManager.canScheduleExactAlarms()
        val ret = JSObject()
        ret.put("value", can)
        invoke.resolve(ret)
    }

    /** Deep-links to the system settings screen for this one permission --
     * there is no in-app runtime-dialog form of this grant, unlike
     * POST_NOTIFICATIONS. A no-op below API 31, where the setting doesn't
     * exist because it isn't needed. */
    @Command
    fun requestExactAlarmPermission(invoke: Invoke) {
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.S) {
            val intent = Intent(Settings.ACTION_REQUEST_SCHEDULE_EXACT_ALARM)
            intent.data = Uri.parse("package:" + activity.packageName)
            activity.startActivity(intent)
        }
        invoke.resolve(JSObject())
    }

    @Command
    fun triggerBreakScreen(invoke: Invoke) {
        if (!MainActivity.isResumed) {
            val args = invoke.parseArgs(TriggerBreakScreenArgs::class.java)
            postBreakNotification(activity, args.persistent)
        }
        invoke.resolve(JSObject())
    }

    @Command
    fun cancelBreakNotification(invoke: Invoke) {
        cancelBreakNotification(activity)
        invoke.resolve(JSObject())
    }

    /** Requests transient audio focus so any well-behaved playing app
     * (Spotify, YouTube Music, etc.) receives AUDIOFOCUS_LOSS_TRANSIENT and
     * pauses itself -- the platform's audio-focus courtesy contract, not a
     * cross-app query the way SMTC/MPRIS offer on Windows/Linux. No special
     * permission needed. Only stores the request (for resumeAudioFocus to
     * release later) if it was actually granted -- AUDIOFOCUS_REQUEST_FAILED
     * means nothing to hold onto. */
    @Command
    fun pauseAudioFocus(invoke: Invoke) {
        val audioManager = activity.getSystemService(AudioManager::class.java)
        val attributes = AudioAttributes.Builder()
            .setUsage(AudioAttributes.USAGE_MEDIA)
            .setContentType(AudioAttributes.CONTENT_TYPE_MUSIC)
            .build()
        val request = AudioFocusRequest.Builder(AudioManager.AUDIOFOCUS_GAIN_TRANSIENT)
            .setAudioAttributes(attributes)
            .setOnAudioFocusChangeListener { }
            .build()
        if (audioManager.requestAudioFocus(request) == AudioManager.AUDIOFOCUS_REQUEST_GRANTED) {
            audioFocusRequest = request
        }
        invoke.resolve(JSObject())
    }

    /** Abandons the focus request from pauseAudioFocus, if one is
     * outstanding -- releases the transient hold so whatever ducked for it
     * is free to resume. Only releases apps that ducked for *this specific*
     * request, unlike a blind play/pause toggle, so it can't resume media
     * that was already paused before the break started. No-op if nothing
     * is outstanding (request was denied, or this is called twice). */
    @Command
    fun resumeAudioFocus(invoke: Invoke) {
        audioFocusRequest?.let {
            val audioManager = activity.getSystemService(AudioManager::class.java)
            audioManager.abandonAudioFocusRequest(it)
            audioFocusRequest = null
        }
        invoke.resolve(JSObject())
    }
}
