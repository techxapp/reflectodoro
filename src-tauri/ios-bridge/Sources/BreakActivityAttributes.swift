// Shared by the bridge (which starts/updates the Live Activity) and the
// ReflectodoroLiveActivity widget extension (which renders it) -- the
// extension's project.yml entry compiles this same file, so the two can
// never drift apart. Must not import Tauri.

#if canImport(ActivityKit)
import ActivityKit
import Foundation

@available(iOS 16.1, *)
public struct BreakActivityAttributes: ActivityAttributes {
  public struct ContentState: Codable, Hashable {
    /// "work" or "break".
    public var phase: String
    /// When the current phase ends (the next grid boundary).
    public var phaseEnd: Date
    public var nextBreakStart: Date
    public var nextBreakEnd: Date
    /// A break is open and still waiting for a reflection.
    public var reflectionPending: Bool

    public init(phase: String, phaseEnd: Date, nextBreakStart: Date, nextBreakEnd: Date, reflectionPending: Bool) {
      self.phase = phase
      self.phaseEnd = phaseEnd
      self.nextBreakStart = nextBreakStart
      self.nextBreakEnd = nextBreakEnd
      self.reflectionPending = reflectionPending
    }
  }

  public init() {}
}
#endif
