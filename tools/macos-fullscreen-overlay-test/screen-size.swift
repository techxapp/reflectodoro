// Main screen's pixel size, so the harness can tell "covers the whole screen"
// from "a small window that happens to be on the right Space".
import AppKit
let f = NSScreen.main?.frame ?? .zero
print("\(Int(f.width))x\(Int(f.height))")
