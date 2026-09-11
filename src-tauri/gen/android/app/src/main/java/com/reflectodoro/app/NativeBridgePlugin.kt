package com.reflectodoro.app

import android.app.Activity
import android.app.AlarmManager
import android.app.PendingIntent
import android.content.Context
import android.content.Intent
import android.media.AudioAttributes
import android.media.AudioFocusRequest
import android.media.AudioManager
import android.net.Uri
import android.net.nsd.NsdManager
import android.net.nsd.NsdServiceInfo
import android.os.Build
import android.os.Handler
import android.os.Looper
import android.provider.Settings
import android.util.Log
import app.tauri.annotation.Command
import app.tauri.annotation.InvokeArg
import app.tauri.annotation.TauriPlugin
import app.tauri.plugin.Channel
import app.tauri.plugin.Invoke
import app.tauri.plugin.JSArray
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

@InvokeArg
class PersistPomodoroEnabledArgs {
    var enabled: Boolean = true
}

@InvokeArg
class RegisterP2pServiceArgs {
    lateinit var deviceId: String
    lateinit var name: String
    lateinit var platform: String
    var port: Int = 0
}

@InvokeArg
class DiscoverP2pServicesArgs {
    var timeoutMs: Long = 3000
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

    // LAN discovery for P2P device pairing/sync (p2p_sync.rs) -- Android has
    // no portable Rust mDNS crate, so this goes through NsdManager instead
    // of the pure-Rust mdns-sd path desktop uses. Held only between
    // registerP2pService and unregisterP2pService, mirroring
    // audioFocusRequest's lifetime pattern above.
    private var nsdRegistrationListener: NsdManager.RegistrationListener? = null
    private val nsdManager: NsdManager by lazy {
        activity.getSystemService(Context.NSD_SERVICE) as NsdManager
    }

    companion object {
        // Trailing dot matches the format NsdManager expects (mirrors
        // p2p_sync.rs's SERVICE_TYPE constant on the Rust side -- both must
        // agree for desktop and Android instances to discover each other).
        private const val P2P_SERVICE_TYPE = "_reflectodoro._tcp."
    }

    @Command
    fun ping(invoke: Invoke) {
        val ret = JSObject()
        ret.put("pong", true)
        invoke.resolve(ret)
    }

    /** Android's equivalent of commands::get_hostname's Windows/Linux paths
     * (COMPUTERNAME / /proc/sys/kernel/hostname) -- there's no traditional
     * hostname on Android, but Settings.Global.DEVICE_NAME is the same
     * user-assigned name shown in the system Bluetooth/Wi-Fi UI (e.g.
     * "Priya's Phone"), which is the closest equivalent and, being
     * user-chosen, more useful than a raw model string. Falls back to
     * Build.MODEL (e.g. "YAL-AL00") when that setting is unset -- reading
     * Settings.Global needs no special permission, only writing does. Called
     * from commands::get_hostname's Android arm; see ensureDeviceName in
     * db.ts for why a real value here matters (it's what P2P sync shows for
     * this device on a peer's "Paired devices" list, see p2p_sync.rs). */
    @Command
    fun getDeviceName(invoke: Invoke) {
        val name = Settings.Global.getString(activity.contentResolver, Settings.Global.DEVICE_NAME)
            ?.trim()
            ?.takeIf { it.isNotEmpty() }
            ?: Build.MODEL
        val ret = JSObject()
        ret.put("value", name)
        invoke.resolve(ret)
    }

    /** Called from every iteration of Rust's run_scheduler loop (which is
     * capped to run at least every ANDROID_POLL_INTERVAL, 20s, regardless of
     * phase -- see lib.rs). Lets MainActivity.isSchedulerAlive() tell "the
     * scheduler is actually alive right now" apart from "an Activity merely
     * existed at some point in this process incarnation". */
    @Command
    fun reportSchedulerHeartbeat(invoke: Invoke) {
        MainActivity.lastSchedulerHeartbeatAt = System.currentTimeMillis()
        invoke.resolve(JSObject())
    }

    /** Persists the Pomodoro-mode on/off toggle to SharedPreferences (not
     * Rust's own app_setting table -- this needs to be readable by
     * BootCompletedReceiver, which runs before any Tauri/Rust runtime exists
     * in a freshly booted process) so a reboot can respect a user's
     * deliberate choice to turn it off, instead of BootCompletedReceiver
     * always re-arming the scheduler/notifications/alarms on the assumption
     * that "off" was never persisted anywhere. See
     * PomodoroEnabledPref.isEnabled (BootCompletedReceiver.kt). */
    @Command
    fun persistPomodoroEnabled(invoke: Invoke) {
        val args = invoke.parseArgs(PersistPomodoroEnabledArgs::class.java)
        activity.getSharedPreferences(PomodoroEnabledPref.PREFS_NAME, Activity.MODE_PRIVATE)
            .edit()
            .putBoolean(PomodoroEnabledPref.PREF_POMODORO_ENABLED, args.enabled)
            .apply()
        invoke.resolve(JSObject())
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
     *
     * Unconditional now -- an earlier version skipped both whenever
     * MainActivity.isResumed was true, on the theory that the frontend's own
     * overlay://state listener already shows /overlay for an app the user is
     * already looking at, so nothing else was needed. That's true for the
     * instant the break starts, but not after: unlike the desktop overlay
     * window, a resumed Activity has no OS-level protection against Home /
     * Recents / switching apps at all, so a break that started while the app
     * happened to be open was trivially escapable the moment the user
     * background it -- confirmed on a real device (the native overlay never
     * showed even though "Display over other apps" was granted). The isResumed
     * check and its one-off MainActivity.recoveryLaunchPending bypass (for the
     * dead-process recovery relaunch, which hit the identical bug in a
     * narrower case) are gone along with it -- nothing left to bypass. */
    @Command
    fun triggerBreakScreen(invoke: Invoke) {
        val args = invoke.parseArgs(TriggerBreakScreenArgs::class.java)
        postBreakNotification(activity, args.persistent)
        if (canDrawOverlaysGranted()) {
            NativeOverlayManager.show(activity, args.state, overlayChannel)
        }
        invoke.resolve(JSObject())
    }

    @Command
    fun cancelBreakNotification(invoke: Invoke) {
        cancelBreakNotification(activity)
        // The native overlay is a WindowManager layer drawn over whatever
        // app the user was actually in (Home, another app) -- unlike the
        // regular in-app /overlay route, hiding it does not by itself bring
        // MainActivity back in front of anything. Without this, a reflection
        // submitted while backgrounded would close the overlay straight back
        // to Home/the other app, and the wellness check-in that close_overlay
        // just triggered (checkin://slot, routed by +layout.svelte) would be
        // rendered on a webview nobody is looking at. Captured before hide()
        // since isShowing() always reads false afterward.
        val wasShowingNativeOverlay = NativeOverlayManager.isShowing()
        NativeOverlayManager.hide()
        if (wasShowingNativeOverlay) {
            val intent = Intent(activity, MainActivity::class.java).apply {
                flags = Intent.FLAG_ACTIVITY_NEW_TASK or Intent.FLAG_ACTIVITY_SINGLE_TOP
            }
            activity.startActivity(intent)
        }
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

    /** Whether scheduleNextAlarm (BreakScheduling.kt) can use setAlarmClock's
     * real-exact/foreground-launch-exempt path rather than its degraded
     * inexact fallback -- surfaced to onboarding/Settings so it only prompts
     * for a grant that's actually missing. Always true on API < 31. */
    @Command
    fun canScheduleExactAlarms(invoke: Invoke) {
        val ret = JSObject()
        ret.put("value", canScheduleExactAlarm(activity))
        invoke.resolve(ret)
    }

    /** Deep-links to the system settings screen for the "Alarms & reminders"
     * grant -- same no-in-app-dialog situation as requestDrawOverlaysPermission.
     * Requires SCHEDULE_EXACT_ALARM to be declared in the manifest (it is) --
     * without that declaration this action/screen doesn't exist for the app
     * at all. */
    @Command
    fun requestScheduleExactAlarmPermission(invoke: Invoke) {
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.S) {
            val intent = Intent(Settings.ACTION_REQUEST_SCHEDULE_EXACT_ALARM)
            intent.data = Uri.parse("package:" + activity.packageName)
            activity.startActivity(intent)
        }
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

    /** Advertises this device on the LAN for P2P sync -- called once at
     * startup from p2p_sync::advertise_self's Android arm, mirroring
     * desktop's always-on mdns-sd registration. `deviceId`/`name`/`platform`
     * become the service's TXT record; `port` is the same Rust TCP listener
     * (p2p_sync::PORT) desktop peers connect to -- only discovery goes
     * through Kotlin, the wire protocol itself doesn't. Registration
     * failures (e.g. no active network) are swallowed here the same way
     * desktop's ServiceDaemon::new() failure is: LAN sync becomes
     * unavailable rather than crashing anything. */
    @Command
    fun registerP2pService(invoke: Invoke) {
        val args = invoke.parseArgs(RegisterP2pServiceArgs::class.java)
        val serviceInfo = NsdServiceInfo().apply {
            serviceName = args.deviceId
            serviceType = P2P_SERVICE_TYPE
            port = args.port
            setAttribute("device_id", args.deviceId)
            setAttribute("name", args.name)
            setAttribute("platform", args.platform)
        }
        // Logged (android.util.Log, not routed through Rust's log:: -- this
        // fires from an NsdManager callback, not synchronously from the
        // Invoke that dispatched registerService, so there's no live
        // `invoke`/command context left to report through) since registration
        // itself is async: registerService returning without an exception
        // only means the request was dispatched, not that the OS actually
        // registered it -- a silently-discarded callback result is exactly
        // the class of bug CLAUDE.md's "Debugging from production logs"
        // section calls out (log both the attempt and its outcome).
        val listener = object : NsdManager.RegistrationListener {
            override fun onServiceRegistered(info: NsdServiceInfo) {
                Log.i("Reflectodoro/P2pSync", "NSD service registered: ${info.serviceName}")
            }
            override fun onRegistrationFailed(info: NsdServiceInfo, errorCode: Int) {
                Log.e("Reflectodoro/P2pSync", "NSD registration failed for ${info.serviceName}, errorCode=$errorCode")
            }
            override fun onServiceUnregistered(info: NsdServiceInfo) {
                Log.i("Reflectodoro/P2pSync", "NSD service unregistered: ${info.serviceName}")
            }
            override fun onUnregistrationFailed(info: NsdServiceInfo, errorCode: Int) {
                Log.e("Reflectodoro/P2pSync", "NSD unregistration failed for ${info.serviceName}, errorCode=$errorCode")
            }
        }
        try {
            nsdManager.registerService(serviceInfo, NsdManager.PROTOCOL_DNS_SD, listener)
            nsdRegistrationListener = listener
        } catch (e: Exception) {
            // No network / NSD unavailable on this device -- same "log and
            // carry on" tolerance as every other best-effort setup path.
        }
        invoke.resolve(JSObject())
    }

    /** Not currently called anywhere (this device's advertisement stays up
     * for the process lifetime, same as desktop) -- kept for symmetry with
     * registerP2pService/desktop's unused-but-available stop_browse path. */
    @Command
    fun unregisterP2pService(invoke: Invoke) {
        nsdRegistrationListener?.let {
            try {
                nsdManager.unregisterService(it)
            } catch (e: Exception) {
                // Already unregistered/never succeeded -- nothing to clean up.
            }
            nsdRegistrationListener = null
        }
        invoke.resolve(JSObject())
    }

    /** Runs an NSD discovery burst for `timeoutMs`, resolving every
     * `_reflectodoro._tcp` instance found (device_id/name/platform from its
     * TXT record, host/port from resolution), and resolves with
     * `{"devices": [...]}` -- the Android equivalent of desktop's short
     * mdns-sd browse window (p2p_sync::browse_lan). Wrapped in an object
     * rather than a bare array since Invoke.resolve() expects a JSObject;
     * see android_bridge.rs's discover_p2p_services for the Rust side that
     * unwraps this. */
    @Command
    fun discoverP2pServices(invoke: Invoke) {
        val args = invoke.parseArgs(DiscoverP2pServicesArgs::class.java)
        val results = java.util.Collections.synchronizedList(mutableListOf<JSObject>())

        // NsdManager only allows one resolveService() in flight at a time
        // (a second concurrent call fails with FAILURE_ALREADY_ACTIVE) --
        // confirmed live on a real device: onServiceFound fires more than
        // once for the very same instance (this device's own
        // self-advertisement, and a peer on another interface), and firing
        // resolveService for each concurrently produced exactly that
        // failure before eventually succeeding on retry. Serialize through
        // a small pending queue instead, and skip re-queuing a service name
        // already resolved or in flight this discovery burst.
        val resolveQueue = java.util.ArrayDeque<NsdServiceInfo>()
        val seenServiceNames = java.util.Collections.synchronizedSet(mutableSetOf<String>())
        var resolving = false

        fun resolveNext() {
            synchronized(resolveQueue) {
                if (resolving || resolveQueue.isEmpty()) return
                resolving = true
            }
            val next = synchronized(resolveQueue) { resolveQueue.poll() } ?: run {
                resolving = false
                return
            }
            nsdManager.resolveService(next, object : NsdManager.ResolveListener {
                override fun onResolveFailed(info: NsdServiceInfo, errorCode: Int) {
                    Log.e("Reflectodoro/P2pSync", "NSD resolve failed for ${info.serviceName}, errorCode=$errorCode")
                    resolving = false
                    resolveNext()
                }
                override fun onServiceResolved(info: NsdServiceInfo) {
                    val attrs = info.attributes
                    fun attr(key: String): String =
                        attrs[key]?.let { String(it, Charsets.UTF_8) } ?: ""
                    val entry = JSObject()
                    entry.put("deviceId", attr("device_id"))
                    entry.put("name", attr("name"))
                    entry.put("platform", attr("platform"))
                    entry.put("host", info.host?.hostAddress ?: "")
                    entry.put("port", info.port)
                    results.add(entry)
                    Log.i("Reflectodoro/P2pSync", "NSD resolved: ${attr("name")} (${attr("device_id")}) at ${info.host?.hostAddress}:${info.port}")
                    resolving = false
                    resolveNext()
                }
            })
        }

        val discoveryListener = object : NsdManager.DiscoveryListener {
            override fun onDiscoveryStarted(serviceType: String) {
                Log.i("Reflectodoro/P2pSync", "NSD discovery started for $serviceType")
            }
            override fun onServiceFound(service: NsdServiceInfo) {
                if (!seenServiceNames.add(service.serviceName)) return
                synchronized(resolveQueue) { resolveQueue.add(service) }
                resolveNext()
            }
            override fun onServiceLost(service: NsdServiceInfo) {}
            override fun onDiscoveryStopped(serviceType: String) {}
            override fun onStartDiscoveryFailed(serviceType: String, errorCode: Int) {
                Log.e("Reflectodoro/P2pSync", "NSD discovery failed to start for $serviceType, errorCode=$errorCode")
            }
            override fun onStopDiscoveryFailed(serviceType: String, errorCode: Int) {}
        }

        try {
            nsdManager.discoverServices(P2P_SERVICE_TYPE, NsdManager.PROTOCOL_DNS_SD, discoveryListener)
        } catch (e: Exception) {
            Log.e("Reflectodoro/P2pSync", "NSD discoverServices threw", e)
            invoke.resolve(JSObject().put("devices", JSArray()))
            return
        }

        Handler(Looper.getMainLooper()).postDelayed({
            try {
                nsdManager.stopServiceDiscovery(discoveryListener)
            } catch (e: Exception) {
                // Already stopped, or never fully started -- fine either way.
            }
            val ret = JSObject()
            ret.put("devices", JSArray(results.toList()))
            invoke.resolve(ret)
        }, args.timeoutMs)
    }
}
