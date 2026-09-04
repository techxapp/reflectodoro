package com.reflectodoro.app

import android.graphics.Rect
import android.os.Bundle
import android.view.View
import android.webkit.WebView
import androidx.activity.enableEdgeToEdge
import androidx.webkit.WebSettingsCompat
import androidx.webkit.WebViewFeature

class MainActivity : TauriActivity() {
  companion object {
    /** Timestamp (epoch ms) of the most recent `reportSchedulerHeartbeat`
     * call from Rust's `run_scheduler` loop -- that loop is capped to iterate
     * at least every `ANDROID_POLL_INTERVAL` (20s, see lib.rs) regardless of
     * phase, and reports in on every iteration, so this staying fresh is
     * "the scheduler is actually alive right now", not just "an Activity was
     * created at some point in this process incarnation". BreakAlarmReceiver
     * reads this (via isSchedulerAlive) to tell that apart from "this process
     * is fully dead / the scheduler task died without taking the process down
     * with it, and only a tap-to-open notification can bring it back" -- see
     * BreakScheduling.kt's postWakeNotification.
     *
     * Deliberately a liveness signal, not a one-time "did onCreate ever run"
     * flag (which is what this used to be, as `schedulerStarted`): a flag set
     * once and never cleared stays true forever once an Activity has existed
     * in this process, even if the scheduler task itself later dies without
     * aborting the whole process -- which would permanently disable the one
     * mechanism meant to recover from exactly that. */
    @Volatile
    var lastSchedulerHeartbeatAt: Long = 0

    /** Generous relative to the ~20s heartbeat cadence -- tolerates Doze/
     * scheduling jitter delaying an individual heartbeat without false-
     * negatively treating a briefly-delayed-but-alive scheduler as dead. */
    private const val HEARTBEAT_STALE_THRESHOLD_MS = 5 * 60 * 1000L

    fun isSchedulerAlive(): Boolean {
      return lastSchedulerHeartbeatAt != 0L &&
        System.currentTimeMillis() - lastSchedulerHeartbeatAt < HEARTBEAT_STALE_THRESHOLD_MS
    }

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
