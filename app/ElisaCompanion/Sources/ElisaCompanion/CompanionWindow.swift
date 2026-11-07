// The floating window: borderless, transparent, draggable, optionally always on top. It never takes focus from the
// terminal (non-activating panel) and lets clicks through everywhere except Shade, the care tray and the bubble.
import AppKit
import WebKit

/// borderless panel: drag anywhere on it to move it (a press that does not move stays a click for the page)
final class CompanionPanel: NSPanel {
    private var dragFrom: NSPoint?
    private var originFrom: NSPoint = .zero

    init(size: NSSize) {
        super.init(contentRect: NSRect(origin: .zero, size: size), styleMask: [.borderless, .nonactivatingPanel],
                   backing: .buffered, defer: false)
        isOpaque = false
        backgroundColor = .clear
        hasShadow = false
        isFloatingPanel = true
        becomesKeyOnlyIfNeeded = true
        hidesOnDeactivate = false
        collectionBehavior = [.canJoinAllSpaces, .fullScreenAuxiliary]
    }

    override var canBecomeKey: Bool { true }
    override var canBecomeMain: Bool { false }

    override func sendEvent(_ e: NSEvent) {
        switch e.type {
        case .leftMouseDown:
            dragFrom = NSEvent.mouseLocation
            originFrom = frame.origin
        case .leftMouseDragged:
            if let from = dragFrom {
                let p = NSEvent.mouseLocation
                let (dx, dy) = (p.x - from.x, p.y - from.y)
                if hypot(dx, dy) > 3 { setFrameOrigin(NSPoint(x: originFrom.x + dx, y: originFrom.y + dy)) }
            }
        case .leftMouseUp:
            dragFrom = nil
        default:
            break
        }
        super.sendEvent(e)
    }
}

/// clicks on Shade work on the first click, without activating the app first
final class CompanionWebView: WKWebView {
    override func acceptsFirstMouse(for event: NSEvent?) -> Bool { true }
}

/// the dock size (layout.json companion.sizes) -> window size: room for the bubble's max width on the left, the 1.22x
/// attention pop and a jump above, the care tray beside Shade
func windowSize(dock s: Int) -> NSSize {
    let d = CGFloat(s)
    let w = max(d + 120 + (d * 0.18).rounded(), 30 + 6 + d * 1.22) + 32
    let h = d + 96
    return NSSize(width: w.rounded(), height: h.rounded())
}
