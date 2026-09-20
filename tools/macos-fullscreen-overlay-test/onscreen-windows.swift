// Lists the windows the WindowServer considers on-screen -- which, crucially,
// means "on the *active* Space". That makes this an oracle no AppKit readback
// inside the app itself can be: `isOnActiveSpace`, `isVisible`, the collection
// behavior and the window level all read back correct on a window that is in
// fact parked on another Space (exactly what the shipped bug looked like in a
// real user's log), whereas absence from this list is ground truth.
//
// Needs no permissions: only kCGWindowName would require Screen Recording, and
// nothing here reads it.
//
// Usage: onscreen-windows [owner-substring]
import CoreGraphics
import Foundation

let opts: CGWindowListOption = [.optionOnScreenOnly, .excludeDesktopElements]
guard let infos = CGWindowListCopyWindowInfo(opts, kCGNullWindowID) as? [[String: Any]] else {
    FileHandle.standardError.write("CGWindowListCopyWindowInfo returned nil\n".data(using: .utf8)!)
    exit(2)
}
let filter = CommandLine.arguments.count > 1 ? CommandLine.arguments[1].lowercased() : ""
for w in infos {
    let owner = w[kCGWindowOwnerName as String] as? String ?? "?"
    if !filter.isEmpty && !owner.lowercased().contains(filter) { continue }
    let layer = w[kCGWindowLayer as String] as? Int ?? -99999
    var x = 0, y = 0, width = 0, height = 0
    if let d = w[kCGWindowBounds as String] as? [String: Any],
       let r = CGRect(dictionaryRepresentation: d as CFDictionary) {
        x = Int(r.origin.x); y = Int(r.origin.y); width = Int(r.width); height = Int(r.height)
    }
    // Machine-readable, one window per line, for the shell harness to parse.
    print("owner=\(owner)\tlayer=\(layer)\tx=\(x)\ty=\(y)\tw=\(width)\th=\(height)")
}
