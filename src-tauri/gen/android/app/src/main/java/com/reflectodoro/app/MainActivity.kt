package com.reflectodoro.app

import android.graphics.Rect
import android.os.Bundle
import android.os.Process
import android.util.Log
import android.view.View
import androidx.activity.OnBackPressedCallback
import android.webkit.WebView
import androidx.activity.enableEdgeToEdge
import androidx.webkit.WebSettingsCompat
import androidx.webkit.WebViewFeature

class MainActivity : TauriActivity() {
  companion object {
    /** Wall-clock time (epoch ms) of the most recent `reportSchedulerHeartbeat`
     * from Rust's `run_scheduler` loop, which reports at the top of every
     * iteration. 0 means no scheduler has ever run in this process.
     *
     * BreakAlarmReceiver doesn't judge liveness by how *old* this is: the
     * scheduler sleeps on CLOCK_MONOTONIC, which freezes in deep sleep, so a
     * perfectly alive scheduler routinely goes many minutes without reporting
     * while the screen is off. A "stale after 5 minutes" rule used to read that
     * as dead and relaunch the app (plus a "Tap to reopen" notification) at
     * nearly every boundary. The receiver instead wakes the scheduler and checks
     * for a heartbeat *newer than the alarm*. */
    @Volatile
    var lastSchedulerHeartbeatAt: Long = 0

    fun hasSchedulerReported(): Boolean = lastSchedulerHeartbeatAt != 0L

    /** Set once in onWebViewCreate below and read by
     * NativeOverlayManager.hide() to force a redraw of the main window's
     * WebView right as the native break overlay (a separate
     * TYPE_APPLICATION_OVERLAY WebView covering the whole screen) is torn
     * down. On this device family (Honor/MediaTek), the region the overlay
     * covered can be left showing a stale composited frame -- still black --
     * after wm.removeView() until something forces SurfaceFlinger to
     * recomposite it. The nav tabs are the clearest symptom: their DOM never
     * changes across a route change (they live in +layout.svelte, outside
     * the routed content), so they're the part of the page least likely to
     * get an incidental repaint on their own -- the active tab does change
     * (its class differs per route) and repaints fine, which is why only it
     * stays visible. */
    private var mainWebView: WebView? = null

    fun forceRedrawMainWindow() {
      val wv = mainWebView ?: return
      wv.post {
        wv.invalidate()
        (wv.parent as? View)?.invalidate()
        wv.requestLayout()
      }
    }
  }

  override fun onCreate(savedInstanceState: Bundle?) {
    enableEdgeToEdge()
    super.onCreate(savedInstanceState)
    // Back leaves the app the way Home does, on every version. Android 12+
    // already moves a root task to the back here, but 10-11 finish the
    // Activity, which now ends the process (see onDestroy) -- and with it the
    // scheduler, until the next break's alarm cold-starts it again over
    // whatever the user is doing.
    onBackPressedDispatcher.addCallback(this, object : OnBackPressedCallback(true) {
      override fun handleOnBackPressed() {
        moveTaskToBack(true)
      }
    })
  }

  /** A real destroy (swiped from Recents, or finished) while the foreground
   * service keeps the process alive leaves Tauri with no Activity, and every
   * Rust->Kotlin plugin call after that -- the scheduler makes one every
   * MOBILE_POLL_INTERVAL -- hits wry's `first_activity_id().expect("no
   * available activity")` and aborts the process (release builds are
   * panic = "abort"). So the process died anyway, ~20s later, silently, and
   * the next break-start alarm had to cold-start the app over whatever the
   * user was doing. Ending it here does the same thing on purpose, and gets
   * the reason into the log first. Configuration changes are excluded: wry
   * keeps its references across those. */
  override fun onDestroy() {
    super.onDestroy()
    if (isChangingConfigurations) return
    Log.i("Reflectodoro/Activity", "MainActivity destroyed -- ending the process")
    try {
      NativeBridgePlugin.sharedChannel?.sendObject(mapOf("kind" to "activity_destroyed"))
    } catch (e: Exception) {
      Log.w("Reflectodoro/Activity", "activity_destroyed send failed: $e")
    }
    Process.killProcess(Process.myPid())
  }

  /** enableEdgeToEdge() above (WindowCompat.setDecorFitsSystemWindows(false))
   * means the classic windowSoftInputMode adjustResize/adjustPan never
   * kicks in -- confirmed on a real API 29 device: window.innerHeight and
   * visualViewport.height stayed identical with the keyboard visibly
   * covering the page. Same fix as the native break overlay
   * (NativeOverlayManager.show): measure the real keyboard height via
   * getWindowVisibleDisplayFrame, which isn't gated on
   * decorFitsSystemWindows/softInputMode the way the automatic resize is,
   * and push it to the page directly. */
  override fun onWebViewCreate(webView: WebView) {
    super.onWebViewCreate(webView)
    mainWebView = webView

    // Belt-and-suspenders: keeps app.css's own prefers-color-scheme dark
    // theme as the only source of truth instead of layering the system
    // WebView's automatic darkening on top of it. Confirmed NOT the cause of
    // the black-nav-tabs bug on the Honor test device (its own
    // HwForceDarkManager already reports force-dark disabled for this app),
    // but harmless to keep off regardless -- see forceRedrawMainWindow above
    // for the actual fix.
    if (WebViewFeature.isFeatureSupported(WebViewFeature.ALGORITHMIC_DARKENING)) {
      WebSettingsCompat.setAlgorithmicDarkeningAllowed(webView.settings, false)
    }

    webView.viewTreeObserver.addOnGlobalLayoutListener {
      val visibleFrame = Rect()
      webView.getWindowVisibleDisplayFrame(visibleFrame)
      val screenHeightPx = webView.resources.displayMetrics.heightPixels
      val keyboardPx = (screenHeightPx - visibleFrame.bottom).coerceAtLeast(0)
      val keyboardDp = keyboardPx / webView.resources.displayMetrics.density
      webView.evaluateJavascript("window.__setKeyboardInset && window.__setKeyboardInset($keyboardDp)", null)
    }
  }
}
