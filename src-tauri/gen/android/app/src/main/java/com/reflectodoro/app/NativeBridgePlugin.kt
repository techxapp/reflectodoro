package com.reflectodoro.app

import android.app.Activity
import android.app.AlarmManager
import android.app.PendingIntent
import android.content.Intent
import android.media.AudioAttributes
import android.media.AudioFocusRequest
import android.media.AudioManager
import android.net.Uri
import android.provider.Settings
import app.tauri.annotation.Command
import app.tauri.annotation.InvokeArg
import app.tauri.annotation.TauriPlugin
import app.tauri.plugin.Channel
import app.tauri.plugin.Invoke
import app.tauri.plugin.JSObject
import app.tauri.plugin.Plugin

@InvokeArg
class TriggerBreakScreenArgs {
    var persistent: Boolean = true
    // JSON-encoded OverlayState (+ dev_mode) -- opaque to Kotlin, just
    // relayed into the native overlay's WebView as-is. See
    // overlay_state_json_for_android in overlay.rs.
    lateinit var state: String
}

@InvokeArg
class UpdateNativeOverlayArgs {
    lateinit var state: String
}

@InvokeArg
class InitNativeOverlayChannelArgs {
    lateinit var channel: Channel
}

@TauriPlugin
class NativeBridgePlugin(private val activity: Activity) : Plugin(activity) {
    // Held only between pauseAudioFocus and the matching resumeAudioFocus
    // for the same break -- never persisted, since a fresh process start
    // has nothing outstanding to release anyway.
    private var audioFocusRequest: AudioFocusRequest? = null

    // Set once at app startup (see native_overlay.rs::install_channel) and
    // reused for the lifetime of the process -- this is how the native
    // overlay's WebView (which has no Tauri IPC of its own) gets reflection
    // submissions and breakit attempts back into Rust. See
    // NativeOverlayManager's OverlayJsBridge.
    private var overlayChannel: Channel? = null

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

    /** Always posts the break notification, and additionally shows the
     * native draw-over-other-apps overlay (NativeOverlayManager) when that
     * permission is granted -- deliberately not either/or: the overlay
     * survives Home/app-switching and has no OS-level dismiss gesture at
     * all, but the notification is a real fallback path if the overlay
     * doesn't render for any reason (a permission edge case, an OEM
     * quirk blocking WindowManager.addView outright, etc.) -- the user
     * confirmed they want both rather than relying on the overlay alone.
     * A no-op if the app is already resumed, matching the existing
     * behavior: the frontend's own overlay://state listener already handles
     * that case -- except immediately after BreakAlarmReceiver's recovery
     * auto-launch, where isResumed is already true by the time this runs
     * even though the user never chose to look at this app (confirmed via
     * logcat on a real device). MainActivity.recoveryLaunchPending, read and
     * cleared here, bypasses isResumed for exactly that one call. */
    @Command
    fun triggerBreakScreen(invoke: Invoke) {
        val bypassResumedGuard = MainActivity.recoveryLaunchPending
        if (bypassResumedGuard) MainActivity.recoveryLaunchPending = false
        if (!MainActivity.isResumed || bypassResumedGuard) {
            val args = invoke.parseArgs(TriggerBreakScreenArgs::class.java)
            postBreakNotification(activity, args.persistent)
            if (canDrawOverlaysGranted()) {
                NativeOverlayManager.show(activity, args.state, overlayChannel)
            }
        }
        invoke.resolve(JSObject())
    }

    @Command
    fun cancelBreakNotification(invoke: Invoke) {
        cancelBreakNotification(activity)
        NativeOverlayManager.hide()
        invoke.resolve(JSObject())
    }

    /** Pushes updated OverlayState into the native overlay if it's currently
     * showing -- harmless no-op otherwise (e.g. the notification fallback
     * was used instead, or nothing is open right now). */
    @Command
    fun updateNativeOverlay(invoke: Invoke) {
        if (NativeOverlayManager.isShowing()) {
            val args = invoke.parseArgs(UpdateNativeOverlayArgs::class.java)
            NativeOverlayManager.update(args.state)
        }
        invoke.resolve(JSObject())
    }

    @Command
    fun initNativeOverlayChannel(invoke: Invoke) {
        val args = invoke.parseArgs(InitNativeOverlayChannelArgs::class.java)
        overlayChannel = args.channel
        invoke.resolve(JSObject())
    }

    private fun canDrawOverlaysGranted(): Boolean {
        return Settings.canDrawOverlays(activity)
    }

    /** "Display over other apps" needs this explicit grant on every
     * supported version (minSdk 29 is well past the API 23 floor where the
     * requirement was introduced -- unlike exact-alarm's SDK_INT check,
     * there's no in-range version where this behaves differently). Checked
     * here and requested below via a Settings deep link -- there's no
     * in-app runtime-dialog form of this permission, same as exact-alarm. */
    @Command
    fun canDrawOverlays(invoke: Invoke) {
        val ret = JSObject()
        ret.put("value", canDrawOverlaysGranted())
        invoke.resolve(ret)
    }

    @Command
    fun requestDrawOverlaysPermission(invoke: Invoke) {
        val intent = Intent(Settings.ACTION_MANAGE_OVERLAY_PERMISSION)
        intent.data = Uri.parse("package:" + activity.packageName)
        activity.startActivity(intent)
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
