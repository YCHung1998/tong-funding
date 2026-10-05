// Posts a left click at window-relative point (x, y) in the largest window of <pid>.
import CoreGraphics
import Foundation

let a = CommandLine.arguments
guard a.count > 3, let pid = Int32(a[1]), let x = Double(a[2]), let y = Double(a[3]) else {
    FileHandle.standardError.write("usage: window_click <pid> <x> <y>\n".data(using: .utf8)!); exit(1)
}
let list = (CGWindowListCopyWindowInfo([.optionOnScreenOnly], kCGNullWindowID) as? [[String: Any]]) ?? []
var origin: CGPoint? = nil
var bestArea = 0.0
for w in list {
    guard (w[kCGWindowOwnerPID as String] as? Int32) == pid, (w[kCGWindowLayer as String] as? Int) == 0,
          let b = w[kCGWindowBounds as String] as? [String: Double] else { continue }
    let area = (b["Width"] ?? 0) * (b["Height"] ?? 0)
    if area > bestArea { bestArea = area; origin = CGPoint(x: b["X"] ?? 0, y: b["Y"] ?? 0) }
}
guard let o = origin else { FileHandle.standardError.write("no window\n".data(using: .utf8)!); exit(2) }
let p = CGPoint(x: o.x + x, y: o.y + y)
for type in [CGEventType.mouseMoved, .leftMouseDown, .leftMouseUp] {
    let e = CGEvent(mouseEventSource: nil, mouseType: type, mouseCursorPosition: p, mouseButton: .left)
    e?.post(tap: .cghidEventTap)
    usleep(80_000)
}
print("clicked at \(p)")
