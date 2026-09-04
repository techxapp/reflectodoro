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
    /** Flips true once Rust's run()/run_scheduler has actually started in
     * this process incarnation -- super.onCreate() below chains into
     * WryActivity.onCreate() -> Rust.onActivityCreate(), the JNI entry
     * point tauri::mobile_entry_point wires up. BreakAlarmReceiver reads
     * this to tell "the scheduler is already running somewhere in this
     * process, no need to interrupt the user" apart from "this process was
     * fully dead until this alarm fired, and only a tap-to-open
     * notification can bring the scheduler back" -- see
     * BreakScheduling.kt's postWakeNotification. */
    var schedulerStarted = false

    /** True while this Activity is actually visible/resumed. Read by
     * NativeBridgePlugin.triggerBreakScreen to decide whether a break
     * notification is needed at all -- if the app is already in front of
     * the user, the frontend's own overlay://state listener already
     * handles showing /overlay, and a full-screen-intent notification on
     * top of an already-visible app would just be redundant/jarring. */
    var isResumed = false

    /** Set by BreakAlarmReceiver immediately before it force-launches
     * MainActivity to recover from a dead process (see its onReceive doc
     * comment). By the time Rust's freshly-booted run_scheduler gets around
     * to calling triggerBreakScreen, that forced launch has almost always
     * already flipped isResumed true -- confirmed via logcat on a real
     * device: triggerBreakScreen ran, but isResumed being true made it skip
     * both the notification and the native overlay, even though the user
     * was never actually looking at this app (they were still in whatever
     * they'd backgrounded us for). NativeBridgePlugin.triggerBreakScreen
     * reads and clears this to bypass isResumed for exactly that one call,
     * then goes back to trusting isResumed normally for every later break. */
    var recoveryLaunchPending = false

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
    schedulerStarted = true
  }

  override fun onResume() {
    super.onResume()
    isResumed = true
  }

  override fun onPause() {
    super.onPause()
    isResumed = false
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
