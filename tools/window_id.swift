// Prints the CGWindowID of the largest on-screen window owned by the given pid (0 if none).
import CoreGraphics
import Foundation

guard CommandLine.arguments.count > 1, let pid = Int32(CommandLine.arguments[1]) else {
    print(0); exit(1)
}
let list = (CGWindowListCopyWindowInfo([.optionOnScreenOnly], kCGNullWindowID) as? [[String: Any]]) ?? []
var best = (id: 0, area: 0.0)
for w in list {
    guard (w[kCGWindowOwnerPID as String] as? Int32) == pid,
          (w[kCGWindowLayer as String] as? Int) == 0,
          let b = w[kCGWindowBounds as String] as? [String: Double],
          let id = w[kCGWindowNumber as String] as? Int else { continue }
    let area = (b["Width"] ?? 0) * (b["Height"] ?? 0)
    if area > best.area { best = (id, area) }
}
print(best.id)
