// Stands in for "the user is watching a full-screen video" -- a *separate*
// process that puts itself into real macOS fullscreen, which is what gives it
// its own dedicated Space. That separateness is the whole point: the macOS
// restriction this test exists for only applies to another *app's* full-screen
// Space, so the break overlay can't be tested against a fullscreen window of
// its own process.
//
// Deliberately drives itself (NSWindow.toggleFullScreen) rather than scripting
// a real app such as Safari: no Accessibility/Automation permission is needed,
// so the test runs unattended on a clean machine.
//
// Always self-terminates (VICTIM_SECONDS, default 30) and leaves fullscreen on
// the way out, so a failed or interrupted run can never strand the user in an
// abandoned fullscreen Space.
import AppKit

let app = NSApplication.shared
app.setActivationPolicy(.regular)

final class VictimDelegate: NSObject, NSApplicationDelegate {
    var window: NSWindow!

    func applicationDidFinishLaunching(_ note: Notification) {
        let screen = NSScreen.main!.frame
        window = NSWindow(
            contentRect: NSRect(x: 100, y: 100, width: screen.width / 2, height: screen.height / 2),
            styleMask: [.titled, .closable, .resizable],
            backing: .buffered, defer: false)
        window.title = "FULLSCREEN VICTIM (Reflectodoro overlay test)"
        window.collectionBehavior = [.fullScreenPrimary]
        let view = NSView(frame: window.contentView!.bounds)
        view.wantsLayer = true
        view.layer?.backgroundColor = NSColor.systemGreen.cgColor
        window.contentView = view
        window.makeKeyAndOrderFront(nil)
        app.activate(ignoringOtherApps: true)

        DispatchQueue.main.asyncAfter(deadline: .now() + 0.6) {
            self.window.toggleFullScreen(nil)
            DispatchQueue.main.asyncAfter(deadline: .now() + 1.5) {
                // The harness waits for this line before starting the break.
                print("victim: ready isFullScreen=\(self.window.styleMask.contains(.fullScreen))")
            }
        }

        let lifetime = ProcessInfo.processInfo.environment["VICTIM_SECONDS"].flatMap(Double.init) ?? 30
        DispatchQueue.main.asyncAfter(deadline: .now() + lifetime) {
            if self.window.styleMask.contains(.fullScreen) { self.window.toggleFullScreen(nil) }
            DispatchQueue.main.asyncAfter(deadline: .now() + 1.0) { NSApp.terminate(nil) }
        }
    }
}

let delegate = VictimDelegate()
app.delegate = delegate
setbuf(stdout, nil)
app.run()
