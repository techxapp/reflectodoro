package com.reflectodoro.app

import android.os.Bundle
import androidx.activity.enableEdgeToEdge

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
}
