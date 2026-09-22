// Swift side of ios_bridge.rs. Tauri runs these on its background "ipc"
// queue while the Rust caller blocks on the reply -- and some callers are on
// the main thread (`.setup()`, synchronous commands). So no method here may
// hop to the main thread: nothing UIKit, only thread-safe Foundation /
// UserNotifications / ActivityKit calls.

import AVFoundation
import Foundation
import Tauri
import UserNotifications
import WebKit

#if canImport(ActivityKit)
import ActivityKit
#endif

private let breakIdPrefix = "break-"
/// Bundled in the app target (gen/apple/Sounds). iOS falls back to the
/// default sound if it's ever missing, and plays nothing on silent mode.
private let breakSoundName = "break-beep.caf"
/// Bump whenever the notification content changes: pending requests carrying
/// an older version are treated as stale and rescheduled, instead of being
/// left alone because their identifier already matches.
private let breakContentVersion = 2
private let enabledKey = "pomodoroEnabled"
private let snoozeUntilKey = "pomodoroSnoozeUntilMs"
private let snoozeMinutesKey = "pomodoroSnoozeMinutes"

class ScheduleBreakNotificationsArgs: Decodable {
  let fireDates: [String]
  let title: String
  let body: String
}

class LiveActivityArgs: Decodable {
  let phase: String
  let phaseEnd: String
  let nextBreakStart: String
  let nextBreakEnd: String
  let reflectionPending: Bool
}

class PersistEnabledArgs: Decodable {
  let enabled: Bool
}

class PersistSnoozeArgs: Decodable {
  let untilMs: Int64
  let minutes: Int
}

class PathArgs: Decodable {
  let path: String
}

private func parseIso(_ s: String) -> Date? {
  let f = ISO8601DateFormatter()
  f.formatOptions = [.withInternetDateTime]
  return f.date(from: s)
}

/// Hardware identifier, e.g. "iPhone15,2" (the simulator reports its host
/// architecture, so it names the simulated model via an env var instead).
private func machineIdentifier() -> String {
  if let sim = ProcessInfo.processInfo.environment["SIMULATOR_MODEL_IDENTIFIER"] {
    return sim
  }
  var info = utsname()
  uname(&info)
  return withUnsafeBytes(of: &info.machine) { raw in
    String(decoding: raw.prefix(while: { $0 != 0 }), as: UTF8.self)
  }
}

class NativeBridgePlugin: Plugin {
  @objc public func ping(_ invoke: Invoke) {
    invoke.resolve(["ok": true])
  }

  /// "iPhone" / "iPad" -- since iOS 16 the user-assigned device name needs a
  /// restricted entitlement, and UIDevice would need the main thread anyway.
  @objc public func getDeviceName(_ invoke: Invoke) {
    let id = machineIdentifier()
    let family = ["iPhone", "iPad", "iPod"].first { id.hasPrefix($0) } ?? "iOS device"
    invoke.resolve(["name": family])
  }

  @objc public func getSystemInfo(_ invoke: Invoke) {
    let v = ProcessInfo.processInfo.operatingSystemVersion
    invoke.resolve([
      "osVersion": "\(v.majorVersion).\(v.minorVersion).\(v.patchVersion)",
      "model": machineIdentifier(),
    ])
  }

  // --- Break notifications ---------------------------------------------

  /// Keeps exactly `fireDates` pending as break notifications: stale ones
  /// are removed, missing ones added, existing ones left alone. Identifiers
  /// are derived from the fire date, so repeated calls never duplicate.
  @objc public func scheduleBreakNotifications(_ invoke: Invoke) throws {
    let args = try invoke.parseArgs(ScheduleBreakNotificationsArgs.self)
    var wanted: [String: Date] = [:]
    for s in args.fireDates {
      if let d = parseIso(s), d > Date() {
        wanted[breakIdPrefix + s] = d
      }
    }
    let center = UNUserNotificationCenter.current()
    center.getPendingNotificationRequests { pending in
      let ours = pending.filter { $0.identifier.hasPrefix(breakIdPrefix) }
      // Current-version requests still wanted are kept; everything else of
      // ours (no longer wanted, or scheduled with older content) is replaced.
      let pendingIds = Set(
        ours.filter { $0.content.userInfo["v"] as? Int == breakContentVersion }.map(\.identifier)
      ).intersection(wanted.keys)
      let stale = Set(ours.map(\.identifier)).subtracting(pendingIds)
      if !stale.isEmpty {
        center.removePendingNotificationRequests(withIdentifiers: Array(stale))
      }
      let calendar = Calendar.current
      var added = 0
      for (id, date) in wanted where !pendingIds.contains(id) {
        let content = UNMutableNotificationContent()
        content.title = args.title
        content.body = args.body
        content.sound = UNNotificationSound(named: UNNotificationSoundName(breakSoundName))
        content.threadIdentifier = "breaks"
        content.userInfo = ["v": breakContentVersion]
        if #available(iOS 15.0, *) {
          content.interruptionLevel = .active
        }
        let components = calendar.dateComponents([.year, .month, .day, .hour, .minute, .second], from: date)
        let trigger = UNCalendarNotificationTrigger(dateMatching: components, repeats: false)
        center.add(UNNotificationRequest(identifier: id, content: content, trigger: trigger))
        added += 1
      }
      invoke.resolve(["pending": wanted.count, "added": added, "removed": stale.count])
    }
  }

  @objc public func clearBreakNotifications(_ invoke: Invoke) {
    let center = UNUserNotificationCenter.current()
    center.getPendingNotificationRequests { pending in
      let ids = pending.map(\.identifier).filter { $0.hasPrefix(breakIdPrefix) }
      center.removePendingNotificationRequests(withIdentifiers: ids)
      center.getDeliveredNotifications { delivered in
        let deliveredIds = delivered.map(\.request.identifier).filter { $0.hasPrefix(breakIdPrefix) }
        center.removeDeliveredNotifications(withIdentifiers: deliveredIds)
        invoke.resolve(["removed": ids.count])
      }
    }
  }

  // --- Live Activity -----------------------------------------------------

  /// Starts the Live Activity or updates the running one. Can only *start*
  /// while the app is in the foreground (an ActivityKit rule) -- a failure
  /// then is expected and reported, not thrown.
  @objc public func startOrUpdateLiveActivity(_ invoke: Invoke) throws {
    let args = try invoke.parseArgs(LiveActivityArgs.self)
    #if canImport(ActivityKit)
    if #available(iOS 16.2, *) {
      guard let phaseEnd = parseIso(args.phaseEnd),
            let nextStart = parseIso(args.nextBreakStart),
            let nextEnd = parseIso(args.nextBreakEnd)
      else {
        invoke.reject("invalid Live Activity dates")
        return
      }
      guard ActivityAuthorizationInfo().areActivitiesEnabled else {
        invoke.resolve(["status": "disabled"])
        return
      }
      let state = BreakActivityAttributes.ContentState(
        phase: args.phase, phaseEnd: phaseEnd, nextBreakStart: nextStart, nextBreakEnd: nextEnd,
        reflectionPending: args.reflectionPending)
      let content = ActivityContent(state: state, staleDate: phaseEnd)
      Task {
        let running = Activity<BreakActivityAttributes>.activities
        if let current = running.first {
          for extra in running.dropFirst() {
            await extra.end(nil, dismissalPolicy: .immediate)
          }
          await current.update(content)
          invoke.resolve(["status": "updated"])
          return
        }
        do {
          _ = try Activity.request(attributes: BreakActivityAttributes(), content: content, pushType: nil)
          invoke.resolve(["status": "started"])
        } catch {
          invoke.resolve(["status": "failed", "error": "\(error)"])
        }
      }
      return
    }
    #endif
    invoke.resolve(["status": "unsupported"])
  }

  @objc public func endLiveActivity(_ invoke: Invoke) {
    #if canImport(ActivityKit)
    if #available(iOS 16.2, *) {
      Task {
        for activity in Activity<BreakActivityAttributes>.activities {
          await activity.end(nil, dismissalPolicy: .immediate)
        }
        invoke.resolve()
      }
      return
    }
    #endif
    invoke.resolve()
  }

  // --- Pomodoro on/off + snooze, surviving process kills -------------------

  @objc public func persistPomodoroEnabled(_ invoke: Invoke) throws {
    let args = try invoke.parseArgs(PersistEnabledArgs.self)
    UserDefaults.standard.set(args.enabled, forKey: enabledKey)
    invoke.resolve()
  }

  @objc public func getPersistedPomodoroEnabled(_ invoke: Invoke) {
    let enabled = UserDefaults.standard.object(forKey: enabledKey) as? Bool ?? true
    invoke.resolve(["enabled": enabled])
  }

  @objc public func persistPomodoroSnoozeUntil(_ invoke: Invoke) throws {
    let args = try invoke.parseArgs(PersistSnoozeArgs.self)
    UserDefaults.standard.set(args.untilMs, forKey: snoozeUntilKey)
    UserDefaults.standard.set(args.minutes, forKey: snoozeMinutesKey)
    invoke.resolve()
  }

  @objc public func getPersistedPomodoroSnoozeUntil(_ invoke: Invoke) {
    let untilMs = (UserDefaults.standard.object(forKey: snoozeUntilKey) as? NSNumber)?.int64Value ?? 0
    let minutes = UserDefaults.standard.integer(forKey: snoozeMinutesKey)
    invoke.resolve(["untilMs": untilMs, "minutes": minutes])
  }

  // --- Media pause-on-break ------------------------------------------------

  /// True only between a successful `pauseOtherAudio` and the matching
  /// `resumeOtherAudio`, so a release never deactivates a session we
  /// didn't activate.
  private var audioSessionActive = false

  /// Activating a non-mixable session interrupts whatever other app is
  /// playing (the iOS counterpart of Android's transient audio focus). We
  /// never play anything ourselves. `.playback` rather than the default
  /// `.soloAmbient` so the interruption doesn't depend on the silent switch.
  /// Fails with `cannotInterruptOthers` while the app is backgrounded.
  @objc public func pauseOtherAudio(_ invoke: Invoke) {
    let session = AVAudioSession.sharedInstance()
    do {
      try session.setCategory(.playback, mode: .default, options: [])
      try session.setActive(true)
      audioSessionActive = true
      invoke.resolve(["paused": true])
    } catch {
      invoke.resolve(["paused": false, "error": "\(error)"])
    }
  }

  /// `.notifyOthersOnDeactivation` lets apps we interrupted resume; it can't
  /// resume anything that was already paused before the break.
  @objc public func resumeOtherAudio(_ invoke: Invoke) {
    guard audioSessionActive else {
      invoke.resolve(["resumed": false])
      return
    }
    audioSessionActive = false
    do {
      try AVAudioSession.sharedInstance().setActive(false, options: .notifyOthersOnDeactivation)
      invoke.resolve(["resumed": true])
    } catch {
      invoke.resolve(["resumed": false, "error": "\(error)"])
    }
  }

  // --- Backup exclusion ----------------------------------------------------

  /// iOS's equivalent of android:allowBackup="false": keeps pomodoro.db (and
  /// everything else in the app's config dir) out of iCloud/Finder backups.
  @objc public func excludeFromBackup(_ invoke: Invoke) throws {
    let args = try invoke.parseArgs(PathArgs.self)
    var url = URL(fileURLWithPath: args.path, isDirectory: true)
    var values = URLResourceValues()
    values.isExcludedFromBackup = true
    do {
      try FileManager.default.createDirectory(at: url, withIntermediateDirectories: true)
      try url.setResourceValues(values)
      invoke.resolve()
    } catch {
      invoke.reject("couldn't exclude \(args.path) from backup: \(error)")
    }
  }
}

@_cdecl("init_plugin_reflectodoro")
func initPlugin() -> Plugin {
  return NativeBridgePlugin()
}
