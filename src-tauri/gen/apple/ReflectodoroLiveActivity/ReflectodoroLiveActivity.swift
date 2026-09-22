// Renders the Live Activity the app starts via ios-bridge's
// startOrUpdateLiveActivity. The app can only update it while it's running
// (no push server), so the view relies on self-updating timers
// (`Text(timerInterval:)`) rather than on fresh content, and the bridge sets
// `staleDate` to the phase end so iOS greys it out once it's out of date.

import ActivityKit
import SwiftUI
import WidgetKit

@main
struct ReflectodoroLiveActivityBundle: WidgetBundle {
  var body: some Widget {
    BreakLiveActivity()
  }
}

struct BreakLiveActivity: Widget {
  var body: some WidgetConfiguration {
    ActivityConfiguration(for: BreakActivityAttributes.self) { context in
      LockScreenView(state: context.state, isStale: context.isStale)
        .padding()
        .activityBackgroundTint(Color.black.opacity(0.6))
        .activitySystemActionForegroundColor(.white)
    } dynamicIsland: { context in
      DynamicIsland {
        DynamicIslandExpandedRegion(.leading) {
          Label(title(context.state), systemImage: icon(context.state))
            .font(.headline)
        }
        DynamicIslandExpandedRegion(.trailing) {
          countdown(context.state)
            .font(.title3.monospacedDigit())
            .frame(width: 70)
        }
        DynamicIslandExpandedRegion(.bottom) {
          Text(subtitle(context.state))
            .font(.caption)
            .foregroundStyle(.secondary)
        }
      } compactLeading: {
        Image(systemName: icon(context.state))
      } compactTrailing: {
        countdown(context.state)
          .monospacedDigit()
          .frame(width: 44)
      } minimal: {
        Image(systemName: icon(context.state))
      }
    }
  }
}

private struct LockScreenView: View {
  let state: BreakActivityAttributes.ContentState
  let isStale: Bool

  var body: some View {
    HStack(alignment: .center, spacing: 12) {
      Image(systemName: icon(state))
        .font(.title2)
      VStack(alignment: .leading, spacing: 2) {
        Text(title(state)).font(.headline)
        Text(isStale ? "Open Reflectodoro to refresh" : subtitle(state))
          .font(.caption)
          .foregroundStyle(.secondary)
      }
      Spacer()
      if !isStale {
        countdown(state)
          .font(.title2.monospacedDigit())
          .multilineTextAlignment(.trailing)
          .frame(width: 80)
      }
    }
  }
}

private func isBreak(_ s: BreakActivityAttributes.ContentState) -> Bool {
  s.phase == "break" || s.reflectionPending
}

private func title(_ s: BreakActivityAttributes.ContentState) -> String {
  if s.reflectionPending { return "Reflect to unlock" }
  return s.phase == "break" ? "Break time" : "Focus"
}

private func subtitle(_ s: BreakActivityAttributes.ContentState) -> String {
  if s.reflectionPending { return "Open Reflectodoro: what did you do?" }
  let f = DateFormatter()
  f.timeStyle = .short
  f.dateStyle = .none
  return s.phase == "break"
    ? "Back to work at \(f.string(from: s.phaseEnd))"
    : "Next break \(f.string(from: s.nextBreakStart))–\(f.string(from: s.nextBreakEnd))"
}

private func icon(_ s: BreakActivityAttributes.ContentState) -> String {
  isBreak(s) ? "cup.and.saucer.fill" : "timer"
}

private func countdown(_ s: BreakActivityAttributes.ContentState) -> Text {
  let end = max(s.phaseEnd, Date())
  return Text(timerInterval: Date()...end, countsDown: true)
}
