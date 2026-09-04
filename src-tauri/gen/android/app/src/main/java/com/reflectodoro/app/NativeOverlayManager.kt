package com.reflectodoro.app

import android.annotation.SuppressLint
import android.content.Context
import android.graphics.PixelFormat
import android.graphics.Rect
import android.os.Handler
import android.os.Looper
import android.view.Gravity
import android.view.ViewTreeObserver
import android.view.WindowManager
import android.webkit.JavascriptInterface
import android.webkit.WebView
import android.webkit.WebViewClient
import androidx.webkit.WebSettingsCompat
import androidx.webkit.WebViewFeature
import app.tauri.plugin.Channel

/** Owns the "draw over other apps" break overlay -- a plain WebView added
 * directly to WindowManager (TYPE_APPLICATION_OVERLAY), not one of Tauri's
 * own windows. This is what lets the break screen survive the user pressing
 * Home or switching apps: a WindowManager overlay isn't tied to any
 * Activity's lifecycle the way the single-Activity Tauri webview is, so it
 * keeps floating on top of whatever's in the foreground until this code
 * removes it -- which only happens once Rust's unlock formula
 * (OverlayState::unlocked) is actually satisfied. There is deliberately no
 * user-facing dismiss affordance anywhere in this class or in
 * native_overlay.html: unlike a notification, a raw WindowManager window has
 * no OS-level swipe-to-dismiss gesture at all, so non-dismissibility falls
 * out of the mechanism itself rather than needing extra flags to opt into.
 *
 * All public entry points hop onto the main thread themselves (WindowManager
 * and WebView both require it) so callers -- NativeBridgePlugin's command
 * handlers, which run off the main thread -- don't have to remember to. */
object NativeOverlayManager {
  private val mainHandler = Handler(Looper.getMainLooper())
  private var webView: WebView? = null
  private var windowManager: WindowManager? = null
  private var keyboardLayoutListener: ViewTreeObserver.OnGlobalLayoutListener? = null

  fun isShowing(): Boolean = webView != null

  @SuppressLint("SetJavaScriptEnabled")
  fun show(context: Context, stateJson: String, channel: Channel?) {
    mainHandler.post {
      if (webView != null) {
        pushState(stateJson)
        return@post
      }

      val appContext = context.applicationContext
      val wm = appContext.getSystemService(Context.WINDOW_SERVICE) as WindowManager

      val wv = WebView(appContext)
      wv.settings.javaScriptEnabled = true
      wv.settings.domStorageEnabled = false
      // See MainActivity.onWebViewCreate for why: keeps this WebView's own
      // dark styling (native_overlay.html) as the only source of truth
      // instead of layering the system's automatic darkening on top of it.
      if (WebViewFeature.isFeatureSupported(WebViewFeature.ALGORITHMIC_DARKENING)) {
        WebSettingsCompat.setAlgorithmicDarkeningAllowed(wv.settings, false)
      }
      wv.addJavascriptInterface(OverlayJsBridge(channel), "AndroidBridge")
      wv.webViewClient = object : WebViewClient() {
        override fun onPageFinished(view: WebView?, url: String?) {
          pushState(stateJson)
        }
      }
      wv.loadUrl("file:///android_asset/native_overlay.html")

      // TYPE_APPLICATION_OVERLAY unconditionally: it's an API 26+ type, and
      // minSdk 29 is already past that floor -- no pre-26 fallback needed.
      //
      // No FLAG_NOT_FOCUSABLE / FLAG_NOT_TOUCH_MODAL: this overlay must be
      // able to receive touches (so the app underneath can't be interacted
      // with) and keyboard focus (so the reflection/breakit fields work),
      // unlike a click-through "chat heads"-style overlay.
      val params = WindowManager.LayoutParams(
        WindowManager.LayoutParams.MATCH_PARENT,
        WindowManager.LayoutParams.MATCH_PARENT,
        WindowManager.LayoutParams.TYPE_APPLICATION_OVERLAY,
        WindowManager.LayoutParams.FLAG_LAYOUT_IN_SCREEN,
        PixelFormat.TRANSLUCENT,
      )
      params.gravity = Gravity.TOP or Gravity.START
      params.softInputMode = WindowManager.LayoutParams.SOFT_INPUT_ADJUST_RESIZE

      wm.addView(wv, params)
      webView = wv
      windowManager = wm

      // softInputMode's ADJUST_RESIZE/ADJUST_PAN only auto-applies to
      // "application" window types -- TYPE_APPLICATION_OVERLAY is a system
      // window type, so it never resizes/pans on its own when the IME shows.
      // A real device (API 29) confirmed this: native_overlay.html's
      // visualViewport-based spacer never grew beyond its static floor, and
      // the keyboard covered the focused field. getWindowVisibleDisplayFrame
      // () is the older, window-type-independent mechanism (the same one
      // keyboard-visibility libraries use for popups/overlays) -- it reports
      // the actual IME-adjusted visible frame for this window regardless, so
      // this measures the real keyboard height and feeds it to the page
      // directly.
      val listener = ViewTreeObserver.OnGlobalLayoutListener {
        val visibleFrame = Rect()
        wv.getWindowVisibleDisplayFrame(visibleFrame)
        val screenHeightPx = wv.resources.displayMetrics.heightPixels
        val keyboardPx = (screenHeightPx - visibleFrame.bottom).coerceAtLeast(0)
        val keyboardDp = keyboardPx / wv.resources.displayMetrics.density
        wv.evaluateJavascript("window.__setKeyboardInset && window.__setKeyboardInset($keyboardDp)", null)
      }
      wv.viewTreeObserver.addOnGlobalLayoutListener(listener)
      keyboardLayoutListener = listener
    }
  }

  fun update(stateJson: String) {
    mainHandler.post { pushState(stateJson) }
  }

  private fun pushState(stateJson: String) {
    val wv = webView ?: return
    // Encode as a JS string literal via a JSON string, not naive quote
    // escaping -- state text (a saved reflection draft, the breakit
    // challenge) can contain characters that would otherwise break out of a
    // hand-escaped '...' literal.
    val encoded = org.json.JSONObject.quote(stateJson)
    wv.evaluateJavascript("window.__updateOverlayState($encoded)", null)
  }

  fun hide() {
    mainHandler.post {
      val wv = webView ?: return@post
      val wm = windowManager ?: return@post
      keyboardLayoutListener?.let {
        if (wv.viewTreeObserver.isAlive) wv.viewTreeObserver.removeOnGlobalLayoutListener(it)
      }
      keyboardLayoutListener = null
      wm.removeView(wv)
      wv.destroy()
      webView = null
      windowManager = null
      // The main Activity's WebView was fully covered by this overlay for
      // the whole break -- on some devices (Honor/MediaTek confirmed) the
      // region it covered can be left showing a stale composited frame
      // (still black) until forced to redraw. See
      // MainActivity.forceRedrawMainWindow for detail.
      MainActivity.forceRedrawMainWindow()
    }
  }
}

/** Bridges native_overlay.html's JS back into Rust via the Channel Rust
 * registered once at startup (see native_overlay.rs::install_channel) --
 * this WebView has no Tauri IPC of its own, so this @JavascriptInterface is
 * the entire path back. Methods run on a WebView-internal thread, not the
 * main thread, matching how Channel.sendObject is meant to be called. */
class OverlayJsBridge(private val channel: Channel?) {
  @JavascriptInterface
  fun submitReflection(text: String) {
    channel?.sendObject(mapOf("kind" to "submit_reflection", "text" to text))
  }

  @JavascriptInterface
  fun breakitAttempt(input: String) {
    channel?.sendObject(mapOf("kind" to "breakit_attempt", "input" to input))
  }

  @JavascriptInterface
  fun saveTaskList(content: String) {
    channel?.sendObject(mapOf("kind" to "save_task_list", "content" to content))
  }

  @JavascriptInterface
  fun saveNotToDoList(content: String) {
    channel?.sendObject(mapOf("kind" to "save_not_to_do_list", "content" to content))
  }

  @JavascriptInterface
  fun devForceClose() {
    channel?.sendObject(mapOf("kind" to "dev_force_close"))
  }
}
