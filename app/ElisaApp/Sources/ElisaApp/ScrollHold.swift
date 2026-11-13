// Scroll hold (kit Main Window v2, "SCROLLED UP"): once the user scrolls up, new output no longer moves the transcript;
// a pill at the transcript's bottom-right counts the unseen lines, click (or End) jumps down and sticks again.
// Scrolling back to the bottom re-sticks too. The position comes from the NSScrollView behind SwiftUI's ScrollView
// (macOS 14 has no scroll-geometry API), observed through its clip view's bounds.
import AppKit
import SwiftUI

/// placed in the scroll view's content: finds the enclosing NSScrollView and reports the distance from the bottom.
/// It owns the "pinned to the bottom" state: only a user scroll (wheel / trackpad, scroller drag, paging keys) can
/// unpin. A bounds change that is not user input (content growth, layout, SwiftUI's own scrollTo) never unpins; while
/// pinned, any growth of the document (a tall card laid out after the scrollTo already ran) snaps back to the bottom
/// synchronously, so the distance is never read in the window between "content grew" and "scrolled to the new end".
struct ScrollWatcher: NSViewRepresentable {
    let onScroll: @MainActor (CGFloat) -> Void

    func makeNSView(context: Context) -> WatchView { WatchView(onScroll) }
    func updateNSView(_ v: WatchView, context: Context) { v.onScroll = onScroll }

    @MainActor
    final class WatchView: NSView {
        var onScroll: @MainActor (CGFloat) -> Void
        private weak var watched: NSScrollView?
        private var tokens: [NSObjectProtocol] = []
        private var monitor: Any?
        /// the transcript sticks to its newest line; only user scrolling clears it
        private var pinned = true
        /// set while a user scroll is in flight (live scroll, or the wheel / key event being dispatched right now)
        private var liveScroll = false
        private var inputPending = false
        private var snapping = false

        init(_ f: @escaping @MainActor (CGFloat) -> Void) { onScroll = f; super.init(frame: .zero) }
        required init?(coder: NSCoder) { nil }

        override func viewDidMoveToWindow() {
            super.viewDidMoveToWindow()
            guard let sv = enclosingScrollView, sv !== watched else { return }
            stopWatching()
            watched = sv
            ScrollHold.view = sv
            ScrollHold.watcher = self
            sv.contentView.postsBoundsChangedNotifications = true
            let nc = NotificationCenter.default
            // queue nil: delivered synchronously while the change is posted, so the user-input flag is still valid
            func on(_ n: Notification.Name, _ obj: Any?, _ f: @escaping @MainActor (WatchView) -> Void) {
                tokens.append(nc.addObserver(forName: n, object: obj, queue: nil) { [weak self] _ in
                    MainActor.assumeIsolated { if let s = self { f(s) } }
                })
            }
            on(NSView.boundsDidChangeNotification, sv.contentView) { $0.boundsChanged() }
            on(NSScrollView.willStartLiveScrollNotification, sv) { $0.liveScroll = true }
            on(NSScrollView.didEndLiveScrollNotification, sv) { $0.liveScroll = false; $0.boundsChanged() }
            if let doc = sv.documentView {
                doc.postsFrameChangedNotifications = true
                on(NSView.frameDidChangeNotification, doc) { $0.contentResized() }
            }
            // wheel / trackpad (incl. momentum) and paging keys aimed at this scroll view: the scroll they cause happens
            // during their dispatch, so the flag is cleared on the next main-queue turn
            monitor = NSEvent.addLocalMonitorForEvents(matching: [.scrollWheel, .keyDown]) { [weak self] e in
                MainActor.assumeIsolated {
                    guard let self, let sv = self.watched, e.window === sv.window else { return }
                    if e.type == .scrollWheel {
                        guard sv.bounds.contains(sv.convert(e.locationInWindow, from: nil)) else { return }
                    } else {
                        // page up/down, home, up/down arrows (End is the pill's jump-down shortcut)
                        guard [116, 121, 115, 126, 125].contains(e.keyCode) else { return }
                    }
                    self.markUserInput()
                }
                return e
            }
        }

        /// the next scroll comes from the user (also used by --script playback to scroll up "as a user would")
        func markUserInput() {
            inputPending = true
            DispatchQueue.main.async { [weak self] in self?.inputPending = false }
        }

        // the content view (this view's frame tracks the transcript's) grew or shrank
        override func setFrameSize(_ s: NSSize) { super.setFrameSize(s); contentResized() }

        private func contentResized() {
            guard let sv = watched, let doc = sv.documentView else { return }
            if pinned { snapToBottom() } else { onScroll(ScrollHold.distance(sv, doc)) }
        }

        private func boundsChanged() {
            guard !snapping, let sv = watched, let doc = sv.documentView else { return }
            let d = ScrollHold.distance(sv, doc)
            if liveScroll || inputPending {
                pinned = d < ScrollHold.holdAt
            } else if pinned {
                // not the user: layout moved the view off the end, put it back
                if d > 0.5 { snapToBottom(); return }
            } else if d < ScrollHold.holdAt {
                pinned = true   // programmatic jump to the end (pill, End, re-stick)
            }
            onScroll(pinned ? 0 : d)
        }

        private func snapToBottom() {
            guard !snapping, let sv = watched, let doc = sv.documentView else { return }
            snapping = true
            defer { snapping = false }
            let clip = sv.contentView
            let y = doc.isFlipped ? max(doc.bounds.minY, doc.bounds.maxY - clip.bounds.height) : doc.bounds.minY
            if abs(clip.bounds.origin.y - y) > 0.5 {
                clip.scroll(to: NSPoint(x: clip.bounds.origin.x, y: y))
                sv.reflectScrolledClipView(clip)
            }
            onScroll(0)
        }

        private func stopWatching() {
            tokens.forEach { NotificationCenter.default.removeObserver($0) }
            tokens = []
            if let m = monitor { NSEvent.removeMonitor(m); monitor = nil }
        }

        deinit {
            MainActor.assumeIsolated { stopWatching() }
        }
    }
}

@MainActor
enum ScrollHold {
    /// the transcript's scroll view (--script playback scrolls it)
    static weak var view: NSScrollView?
    static weak var watcher: ScrollWatcher.WatchView?
    /// the "new output" pill's action while it shows (--selftest presses it through this)
    static var jump: (() -> Void)?
    /// distance from the bottom (points) at which a user scroll counts as "scrolled up"
    static let holdAt: CGFloat = 24

    static func distance(_ sv: NSScrollView, _ doc: NSView) -> CGFloat {
        let vis = sv.contentView.documentVisibleRect
        let b = doc.bounds
        return max(0, doc.isFlipped ? b.maxY - vis.maxY : vis.minY - b.minY)
    }

    /// scroll up by `pt` points, as a user would (--script playback)
    static func scrollUp(_ pt: CGFloat) {
        guard let sv = view, let doc = sv.documentView else { return }
        watcher?.markUserInput()
        var o = sv.contentView.bounds.origin
        o.y += doc.isFlipped ? -pt : pt
        sv.contentView.scroll(to: o)
        sv.reflectScrolledClipView(sv.contentView)
    }
}

/// "↓ new output · 12 more lines" (kit: right = dock + 28, bottom 8, 11.5 mono, radius 12, bg2, separator border, fg)
struct NewOutputPill: View {
    @EnvironmentObject var m: AppModel
    @Environment(\.pal) var p
    let jump: () -> Void
    var body: some View {
        let lines = m.heldLines == 1 ? "1 more line" : "\(m.heldLines) more lines"
        Button(action: jump) {
            (Text("↓ ").foregroundStyle(p.acc) + Text(m.heldNew ? "new output" : lines).foregroundStyle(p.fg)
             + Text(m.heldNew ? " · \(lines)" : "").foregroundStyle(p.tx))
                .font(Fonts.mono(11.5))
                .padding(.vertical, 3).padding(.horizontal, 10)
                .background(Capsule().fill(p.bg2))
                .overlay(Capsule().strokeBorder(m.heldNew ? p.acc : p.sep, lineWidth: 1))
                .fixedSize()
        }
        .buttonStyle(.plain)
        .keyboardShortcut(.end, modifiers: [])
        .help("Jump to the newest output (End)")
    }
}
