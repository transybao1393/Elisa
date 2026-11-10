// Shade, drawn by the brand kit's own renderer: elisa-shade.js (v2) over elisa-character.js (v1), run as-is in
// JavaScriptCore (the scripts only need a `window` global). The SVG it returns is drawn by AppKit's native SVG support
// (CoreSVG, vector) straight into a bitmap at the screen's backing scale: the 22 px status face, the 24 px mini reply
// marker and the dock are all sharp on Retina, never a bitmap upscale. One renderer, three builds (kit layout.json):
// full (dock, props on), mini (<= 24 px: no rim, highlight, paws or props; vertical motion x0.45), face (ears + eyes).
import AppKit
import JavaScriptCore
import SwiftUI

@MainActor
final class ShadeKit {
    static let shared = ShadeKit()
    private var svgFn: JSValue?
    private var ctx: JSContext?
    private var durs: [String: Double] = [:]
    /// small builds (face, mini) always: bounded, a few MB at most
    private var cache: [String: NSImage] = [:]
    private var order: [String] = []
    private let cacheMax = 400
    /// large builds (dock) only when their whole loop fits the byte budget (a held pose such as the sleeping one does; a 30 fps
    /// awake loop does not, and is drawn per frame as before rather than thrashing the cache)
    private var big: [String: (img: NSImage, bytes: Int)] = [:]
    private var bigOrder: [String] = []
    private var bigBytes = 0
    private let bigBudget = 32 << 20
    private(set) var problem: String?

    func load(_ kit: Kit) {
        guard let c1 = kit.text("05-character/renderer/elisa-character.js"), let c2 = kit.text("05-character/renderer/elisa-shade.js") else {
            problem = "kit renderer not found (05-character/renderer)"; return
        }
        guard let c = JSContext() else { problem = "JavaScriptCore unavailable"; return }
        c.exceptionHandler = { _, e in FileHandle.standardError.write(Data("elisa-app: renderer: \(e?.toString() ?? "?")\n".utf8)) }
        c.evaluateScript("var window = this;")
        c.evaluateScript(c1)
        c.evaluateScript(c2)
        guard let es = c.objectForKeyedSubscript("ElisaShade"), !es.isUndefined else { problem = "ElisaShade did not load"; return }
        svgFn = es.objectForKeyedSubscript("svg")
        if let st = es.objectForKeyedSubscript("STATES")?.toDictionary() as? [String: Any] {
            for (k, v) in st { if let d = (v as? [String: Any])?["dur"] as? Double { durs[k] = d } }
        }
        ctx = c
    }

    var ready: Bool { svgFn != nil }

    /// the frame rate for a calm loop (idle: a slow breath, a blink): low enough that the whole loop fits the frame
    /// cache, so after the first loop no frame is rendered again (no JavaScript, no CoreSVG), only a cached bitmap drawn.
    /// Small builds (face, mini) 5 fps; the dock 4–8 fps by its size (Medium 8, Large 7). Every tick re-runs a SwiftUI
    /// update of the window, so the idle rates are what an idle window costs (about 2-3 % CPU at these rates).
    func calmFps(_ state: String, pt: CGFloat, pad: CGFloat, scale: CGFloat) -> Double {
        if pt <= 32 { return 5 }
        let px = pt * (1 + 2 * pad) * scale
        let loopBytes = Double(px * px * 4) * dur(state) / 1000
        return max(4, min(8, (Double(bigBudget) / loopBytes).rounded(.down)))
    }
    func dur(_ st: String) -> Double { durs[st] ?? 2000 }

    /// the kit's SVG for `state` at loop fraction `t` (the renderer's own call: `ElisaShade.svg(state, t, opts)`)
    func svg(_ state: String, t: Double, size: Int, detail: String, bg: String?, props: Bool) -> String? {
        guard let f = svgFn else { return nil }
        var o: [String: Any] = ["size": size, "detail": detail, "noProps": !props]
        if let bg { o["bg"] = bg }
        return f.call(withArguments: [durs[state] != nil ? state : "idle", t, o])?.toString()
    }

    /// Shade as an image `pt` points square (+ `pad` headroom on each side for jumps / ears), drawn as vectors into a
    /// bitmap of exactly pt * scale pixels. `loop` = frames in the state's loop at the current rate (sizes the cache
    /// decision for large builds); a frame is keyed by its loop fraction `t`, so the same pose at any rate is shared
    func image(_ state: String, t: Double, loop: Int, pt: CGFloat, detail: String, bg: String?, props: Bool, pad: CGFloat, scale: CGFloat) -> NSImage? {
        let small = pt <= 32
        let key = "\(state)|\(t)|\(pt)|\(detail)|\(bg ?? "")|\(props)|\(pad)|\(scale)"
        if small, let i = cache[key] { return i }
        if !small, let e = big[key] { return e.img }
        let px = max(1, Int((pt * (1 + 2 * pad) * scale).rounded()))
        let bytes = px * px * 4
        let keepBig = !small && loop * bytes <= bigBudget
        guard let s = svg(state, t: t, size: Int(pt), detail: detail, bg: bg, props: props) else { return nil }
        let img = ShadeKit.raster(ShadeKit.padded(s, pad), pt: pt * (1 + 2 * pad), scale: scale)
        if small, let img {
            cache[key] = img
            order.append(key)
            if order.count > cacheMax { cache.removeValue(forKey: order.removeFirst()) }
        } else if keepBig, let img {
            big[key] = (img, bytes)
            bigOrder.append(key)
            bigBytes += bytes
            while bigBytes > bigBudget, !bigOrder.isEmpty {
                if let old = big.removeValue(forKey: bigOrder.removeFirst()) { bigBytes -= old.bytes }
            }
        }
        return img
    }

    /// widen the SVG's viewBox by `pad` (a fraction of its width) on every side: the kit draws with overflow:visible
    /// (jumps leave the artboard); an image is clipped to its viewBox, so the room is made explicit
    static func padded(_ s: String, _ pad: CGFloat) -> String {
        guard pad > 0, let r = s.range(of: "viewBox=\""), let e = s[r.upperBound...].firstIndex(of: "\"") else { return s }
        let n = s[r.upperBound..<e].split(separator: " ").compactMap { Double($0) }
        guard n.count == 4 else { return s }
        let p = n[2] * Double(pad)
        let vb = "\(n[0] - p) \(n[1] - p) \(n[2] + 2 * p) \(n[3] + 2 * p)"
        return s.replacingCharacters(in: r.upperBound..<e, with: vb)
    }

    /// vector draw (CoreSVG) into a bitmap of pt * scale pixels; the image reports `pt` points
    static func raster(_ svg: String, pt: CGFloat, scale: CGFloat) -> NSImage? {
        guard let src = NSImage(data: Data(svg.utf8)) else { return nil }
        let px = max(1, Int((pt * scale).rounded()))
        guard let rep = NSBitmapImageRep(bitmapDataPlanes: nil, pixelsWide: px, pixelsHigh: px, bitsPerSample: 8, samplesPerPixel: 4,
                                         hasAlpha: true, isPlanar: false, colorSpaceName: .deviceRGB, bytesPerRow: 0, bitsPerPixel: 0) else { return nil }
        NSGraphicsContext.saveGraphicsState()
        NSGraphicsContext.current = NSGraphicsContext(bitmapImageRep: rep)
        NSGraphicsContext.current?.imageInterpolation = .high
        src.draw(in: NSRect(x: 0, y: 0, width: px, height: px), from: .zero, operation: .sourceOver, fraction: 1)
        NSGraphicsContext.restoreGraphicsState()
        rep.size = NSSize(width: pt, height: pt)
        let img = NSImage(size: rep.size)
        img.addRepresentation(rep)
        return img
    }
}

/// How fast the Shades may animate, from the main window's state: occluded / minimized / app hidden = paused (no
/// timeline ticks at all), not key = a lower rate, and a sleeping Shade holds one pose (curled, eyes shut, mid-breath)
/// that is drawn once per state change. Each tick of a Shade's TimelineView re-runs the window's SwiftUI update and
/// display pass; that is what kept an idle window with a sleeping Shade at ~50 % CPU. Awake in the
/// key window: rates unchanged.
@MainActor
final class ShadePace: ObservableObject {
    static let shared = ShadePace()
    @Published private(set) var visible = true
    @Published private(set) var key = true
    private weak var window: NSWindow?
    private var tokens: [NSObjectProtocol] = []

    /// awake, window not key
    static let unfocusedFps: Double = 8
    /// --pace-front (debug, for CPU measurements): pace as in the key window in front, whatever the window's state
    nonisolated static let front = CommandLine.arguments.contains("--pace-front")

    /// the frame rate for a Shade that would run at `base` showing `state`; 0 = paused (one still frame)
    func fps(_ base: Double, state: String) -> Double {
        if state == "sleeping" { return 0 }
        // --frames / --selftest render the window offscreen and expect it to animate as if in front
        if DebugFrame.scale != nil || ShadePace.front { return base }
        if !visible { return 0 }
        return key ? base : min(base, ShadePace.unfocusedFps)
    }

    func track(_ w: NSWindow?) {
        guard let w, w !== window else { return }
        window = w
        for t in tokens { NotificationCenter.default.removeObserver(t) }
        let nc = NotificationCenter.default
        let winNames: [Notification.Name] = [NSWindow.didChangeOcclusionStateNotification, NSWindow.didBecomeKeyNotification,
                                             NSWindow.didResignKeyNotification, NSWindow.didMiniaturizeNotification,
                                             NSWindow.didDeminiaturizeNotification]
        let appNames: [Notification.Name] = [NSApplication.didHideNotification, NSApplication.didUnhideNotification,
                                             NSApplication.didBecomeActiveNotification, NSApplication.didResignActiveNotification]
        tokens = winNames.map { nc.addObserver(forName: $0, object: w, queue: .main) { [weak self] _ in MainActor.assumeIsolated { self?.update() } } }
            + appNames.map { nc.addObserver(forName: $0, object: nil, queue: .main) { [weak self] _ in MainActor.assumeIsolated { self?.update() } } }
        update()
    }

    private func update() {
        guard let w = window else { return }
        let v = w.occlusionState.contains(.visible) && !w.isMiniaturized && !NSApp.isHidden
        let k = w.isKeyWindow
        if v != visible { visible = v }
        if k != key { key = k }
    }
}

/// tells ShadePace which window the Shades live in (attached to the main window's content)
struct ShadePaceProbe: NSViewRepresentable {
    final class Probe: NSView {
        override func viewDidMoveToWindow() {
            super.viewDidMoveToWindow()
            ShadePace.shared.track(window)
        }
    }
    func makeNSView(context: Context) -> Probe { Probe() }
    func updateNSView(_ v: Probe, context: Context) {}
}

/// One Shade, animated with its state's loop (the kit's `t = elapsed / dur`), at `fps` (lowered or paused by
/// ShadePace when the window is in the background or the Shade sleeps).
struct ShadeImage: View {
    let state: String
    let since: Date
    let size: CGFloat
    var detail = "full"
    var bg: String?
    var props = false
    var animate = true
    var pad: CGFloat = 0.15
    var fps: Double = 24
    @Environment(\.displayScale) private var scale
    @ObservedObject private var pace = ShadePace.shared

    /// the paced rate; idle (nothing happening) runs at the calm rate, whose whole loop is cached
    private func rate(_ px: CGFloat) -> Double {
        let r = pace.fps(fps, state: state)
        guard state == "idle", r > 0, DebugFrame.scale == nil else { return r }
        return min(r, ShadeKit.shared.calmFps(state, pt: size, pad: pad, scale: px))
    }

    var body: some View {
        let px = DebugFrame.scale ?? max(1, scale)
        let rate = rate(px)
        let run = animate && rate > 0
        let side = size * (1 + 2 * pad)
        ShadeFrames(cfg: .init(state: state, since: since, size: size, detail: detail, bg: bg, props: props, pad: pad,
                               fps: run ? rate : 0, still: !animate, scale: px))
            .frame(width: side, height: side)
            .frame(width: size, height: size)
            .allowsHitTesting(false)
    }
}

/// The Shade's frames in an AppKit view with its own timer at the frame rate: a tick redraws only this view (a cached
/// bitmap, see ShadeKit.image), never the window's SwiftUI graph. (A SwiftUI TimelineView tick re-ran the window's
/// SwiftUI update, ~3-5 % CPU for an idle window at the calm rates; this costs a fraction of that.) Paused (fps 0) it
/// holds one frame and runs no timer; off a window it runs none either. It draws in draw(_:), so cacheDisplay
/// (--frames, --selftest) captures it.
struct ShadeFrames: NSViewRepresentable {
    struct Cfg: Equatable {
        var state: String, since: Date, size: CGFloat, detail: String, bg: String?, props: Bool, pad: CGFloat
        /// 0 = paused
        var fps: Double
        /// not animated at all (Reduce Motion, an older reply): the loop's middle pose
        var still: Bool
        var scale: CGFloat
    }
    let cfg: Cfg

    final class View: NSView {
        var cfg: Cfg? { didSet { if cfg != oldValue { restart() } } }
        private var timer: Timer?
        private var img: NSImage?
        private var frameNo = -1

        override var isFlipped: Bool { true }
        override func hitTest(_ point: NSPoint) -> NSView? { nil }
        /// on screen a tick only swaps the layer's bitmap (no CPU drawing); draw(_:) is for cacheDisplay captures
        override var wantsUpdateLayer: Bool { true }
        override func updateLayer() {
            guard let l = layer else { return }
            l.contentsGravity = .resize
            l.minificationFilter = .trilinear
            l.contents = img?.cgImage(forProposedRect: nil, context: nil, hints: nil)
        }

        override func viewDidMoveToWindow() {
            super.viewDidMoveToWindow()
            restart()
        }

        private func restart() {
            timer?.invalidate()
            timer = nil
            frameNo = -1
            tick()
            guard let c = cfg, c.fps > 0, window != nil else { return }
            let t = Timer(timeInterval: 1 / c.fps, repeats: true) { [weak self] _ in MainActor.assumeIsolated { self?.tick() } }
            t.tolerance = 0.2 / c.fps
            RunLoop.main.add(t, forMode: .common)
            timer = t
        }

        /// the frame for now (the kit's `t = elapsed / dur`, on a grid of `fps` frames per second from `since`)
        private func tick() {
            guard let c = cfg else { return }
            let run = c.fps > 0
            let f = run ? c.fps : 24
            let n = max(1, Int((ShadeKit.shared.dur(c.state) / 1000 * f).rounded(.up)))
            let i = run ? Int(max(0, Date().timeIntervalSince(c.since)) * f + 0.001) % n : n / 2
            guard i != frameNo else { return }
            frameNo = i
            let t = c.still ? 0.5 : Double(i) / Double(n)
            img = ShadeKit.shared.image(c.state, t: t, loop: run ? n : 1, pt: c.size, detail: c.detail, bg: c.bg, props: c.props, pad: c.pad, scale: c.scale)
            needsDisplay = true
        }

        override func draw(_ dirtyRect: NSRect) {
            NSGraphicsContext.current?.imageInterpolation = .high
            img?.draw(in: bounds, from: .zero, operation: .sourceOver, fraction: 1, respectFlipped: true, hints: nil)
        }

        deinit { timer?.invalidate() }
    }

    func makeNSView(context: Context) -> View {
        let v = View()
        v.cfg = cfg
        return v
    }

    func updateNSView(_ v: View, context: Context) { v.cfg = cfg }
}

/// the 22 px status-bar face (kit v2 svg-statusbar: ears and eyes, 12 states); `bg` is the window background, which
/// the sad / sleepy lids are painted in
/// While Shade is hidden, a click on the face brings it back ("Show Shade"); hiding flashes the face 3 times in the
/// accent (skipped under Reduce Motion) and, on the first hide each launch, shows a ~2.5 s toast above it.
struct StatusFace: View {
    @EnvironmentObject var m: AppModel
    @Environment(\.pal) var p
    @Environment(\.accessibilityReduceMotion) var reduceMotion
    @State private var flash = false
    @State private var toast = false
    @State private var cue = 0
    /// the toast shows once per launch
    private static var toastShown = false
    var body: some View {
        let f = m.layout.statusFace
        ShadeImage(state: m.shade.face, since: m.shade.since, size: f, detail: "face", bg: p.bgHex, animate: !reduceMotion, pad: 0.12, fps: 16)
            .frame(width: f, height: f)
            .background(RoundedRectangle(cornerRadius: 5).fill(p.acc.opacity(flash ? 0.35 : 0)).padding(-2))
            .overlay(RoundedRectangle(cornerRadius: 5).strokeBorder(p.acc, lineWidth: 1.5).padding(-2).opacity(flash ? 1 : 0))
            .contentShape(Rectangle())
            .onTapGesture { if m.shadeHidden { m.shadeHidden = false } }
            .help(m.shadeHidden ? "Show Shade" : "")
            .onHover { inside in if m.shadeHidden { if inside { NSCursor.pointingHand.push() } else { NSCursor.pop() } } }
            .overlay(alignment: .bottomLeading) {
                if toast {
                    (Text("Shade hidden").foregroundStyle(p.fg) + Text(" · click the eyes to bring it back").foregroundStyle(p.fg2))
                        .font(Fonts.mono(11.5)).lineLimit(1).fixedSize()
                        .padding(.vertical, 5).padding(.horizontal, 10)
                        .background(RoundedRectangle(cornerRadius: 6).fill(p.bg2))
                        .overlay(RoundedRectangle(cornerRadius: 6).strokeBorder(p.acc, lineWidth: 1))
                        .shadow(color: .black.opacity(p.dark ? 0.45 : 0.15), radius: 8, y: 3)
                        .offset(x: -6, y: -(f + 8))
                        .transition(.opacity)
                        .allowsHitTesting(false)
                }
            }
            .padding(EdgeInsets(top: -2, leading: -3, bottom: -2, trailing: -2))
            .onChange(of: m.shadeHidden) { _, hidden in
                cue += 1
                let n = cue
                guard hidden else { flash = false; withAnimation(.easeOut(duration: 0.15)) { toast = false }; return }
                if !StatusFace.toastShown {
                    StatusFace.toastShown = true
                    withAnimation(.easeOut(duration: 0.12)) { toast = true }
                    DispatchQueue.main.asyncAfter(deadline: .now() + 2.5) {
                        if cue == n { withAnimation(.easeIn(duration: 0.35)) { toast = false } }
                    }
                }
                if !reduceMotion {
                    // 3 quick pulses in the accent: on 0.16 s, off 0.12 s
                    for i in 0..<6 {
                        DispatchQueue.main.asyncAfter(deadline: .now() + Double(i / 2) * 0.28 + (i % 2 == 0 ? 0 : 0.16)) {
                            if cue == n { withAnimation(.easeInOut(duration: 0.06)) { flash = i % 2 == 0 } }
                        }
                    }
                }
            }
    }
}

/// the 24 px mini build beside each reply: the live reply plays the live state, older replies hold idle
struct ReplyMarker: View {
    @EnvironmentObject var m: AppModel
    @Environment(\.accessibilityReduceMotion) var reduceMotion
    let live: Bool
    var body: some View {
        ShadeImage(state: live ? m.shade.state : "idle", since: live ? m.shade.since : .distantPast, size: m.layout.replyMarker,
                   detail: "mini", animate: live && !reduceMotion, pad: 0.15, fps: 16)
    }
}


// MARK: - the dock (layout.json companion: bottom-right of the transcript column)

/// the three care-tray icons are the kit prototype's own (08-source/Elisa Prototype.dc.html, care tray)
private let trayIcons: [(String, String, String)] = [
    ("water", "Give water", ##"<svg xmlns="http://www.w3.org/2000/svg" width="18" height="18" viewBox="0 0 24 24" fill="none" stroke="#000" stroke-width="1.8" stroke-linejoin="round"><path d="M10 2h4v3h-4z"/><path d="M9 5h6l1 3v12a2 2 0 0 1-2 2h-4a2 2 0 0 1-2-2V8z"/><path d="M8.5 13h7" stroke-opacity=".6"/></svg>"##),
    ("food", "Give food", ##"<svg xmlns="http://www.w3.org/2000/svg" width="18" height="18" viewBox="0 0 24 24" fill="none" stroke="#000" stroke-width="1.8" stroke-linejoin="round"><path d="M4 9h16l-1.5 11h-13z"/><path d="M4 9l-1-3 6 1M20 9l1-3-6 1"/><circle cx="10" cy="7.5" r="1.2"/><circle cx="14" cy="7.5" r="1.2"/></svg>"##),
    ("toy", "Give a toy", ##"<svg xmlns="http://www.w3.org/2000/svg" width="18" height="18" viewBox="0 0 24 24" fill="none" stroke="#000" stroke-width="1.8" stroke-linecap="round"><circle cx="11" cy="11" r="7"/><path d="M6 8c3 1 6 4 8 9M5 13c3 0 6 2 7 5M9 4.5c2 2 5 3 9 3"/><path d="M17 16c2 1 3 3 5 3"/></svg>"##),
]

/// the hide button's eye-with-slash, drawn in the care-tray icons' stroke style
private let hideIcon = ##"<svg xmlns="http://www.w3.org/2000/svg" width="18" height="18" viewBox="0 0 24 24" fill="none" stroke="#000" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round"><path d="M2 12s3.5-6.5 10-6.5S22 12 22 12s-3.5 6.5-10 6.5S2 12 2 12z"/><circle cx="12" cy="12" r="3"/><path d="M4 20L20 4"/></svg>"##

/// the resize handle's two-way diagonal arrow, drawn in the care-tray icons' stroke style
private let resizeIcon = ##"<svg xmlns="http://www.w3.org/2000/svg" width="18" height="18" viewBox="0 0 24 24" fill="none" stroke="#000" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round"><path d="M6 18L18 6"/><path d="M6 11.5V18h6.5"/><path d="M11.5 6H18v6.5"/></svg>"##

/// the diagonal (NW-SE) resize cursor: the public frame-resize cursor on macOS 15+, AppKit's window-corner one before
@MainActor private let resizeCursor: NSCursor = {
    if #available(macOS 15.0, *) { return NSCursor.frameResize(position: .topLeft, directions: .all) }
    let sel = NSSelectorFromString("_windowResizeNorthWestSouthEastCursor")
    if NSCursor.responds(to: sel), let c = NSCursor.perform(sel)?.takeUnretainedValue() as? NSCursor { return c }
    return .crosshair
}()

private func templateImage(_ svg: String) -> NSImage? {
    let i = NSImage(data: Data(svg.utf8))
    i?.isTemplate = true
    return i
}

struct Dock: View {
    @EnvironmentObject var m: AppModel
    @Environment(\.pal) var p
    /// Reduce Motion: no movement (no loop, no pop spring, no fidget), only the bubble and the pose
    @Environment(\.accessibilityReduceMotion) var reduceMotion
    let size: CGFloat
    /// size factor against the Medium dock size: the bubble and the side buttons scale with the character
    var scale: CGFloat = 1
    /// the width Shade's spot gives the bubble (it wraps inside this, never truncates)
    let room: CGFloat
    @State private var hover: String?
    /// Shade's size when the current resize drag began (nil = not dragging)
    @State private var dragFrom: CGFloat?
    @State private var cursorPushed = false

    var body: some View {
        let s = m.shade
        let L = m.layout
        VStack(alignment: .trailing, spacing: L.bubbleGap) {
            if !s.verb.isEmpty { bubble(s).padding(.trailing, (size * 0.18).rounded()).offset(y: s.pop ? L.bubbleLift * scale : 0) }
            HStack(alignment: .bottom, spacing: 6) {
                VStack(spacing: L.trayGap * scale) {
                    ForEach(trayIcons, id: \.0) { icon in trayButton(icon) }
                    resizeHandle()
                    hideButton()
                }
                .padding(.bottom, L.trayPadBottom * scale)
                ZStack {
                    glow(s)
                    // a long tool's fidget borrows the listening pose for a moment (the bubble stays the tool's)
                    ShadeImage(state: s.fidget && !reduceMotion ? "listening" : s.state, since: s.since, size: size, detail: "full", props: true,
                               animate: !reduceMotion, pad: 0.25, fps: 30)
                    actProp(s)
                }
                .frame(width: size, height: size)
                .scaleEffect(s.pop ? L.attentionScale : 1, anchor: UnitPoint(x: 0.5, y: 0.88))
                .contentShape(Rectangle())
                .onTapGesture { m.care("poke") }
                .help("Poke Elisa")
            }
        }
        .animation(reduceMotion ? nil : .timingCurve(0.34, 1.7, 0.64, 1, duration: 0.35), value: s.pop)
    }

    /// the activities the kit has no prop for get a small SF Symbol in the kit's prop spot (beside the right paw):
    /// WAITING (the gateway queue) an hourglass. Static: no timer, no CPU while it shows.
    @ViewBuilder private func actProp(_ s: ShadeFrame) -> some View {
        let sym: String? = s.verb == "WAITING" ? "hourglass" : nil
        if let sym {
            Image(systemName: sym)
                .font(.system(size: size * 0.17, weight: .semibold))
                .foregroundStyle(p.acc)
                .padding(size * 0.035)
                .background(Circle().fill(p.bg2))
                .overlay(Circle().strokeBorder(p.sep, lineWidth: 1))
                .position(x: size * 0.82, y: size * 0.62)
                .accessibilityHidden(true)
        }
    }

    private func toneColor(_ t: String) -> Color { t == "error" ? p.err : t == "warn" ? p.warn : p.acc }

    private func bubble(_ s: ShadeFrame) -> some View {
        let border = s.pop ? p.acc : p.sep
        let L = m.layout
        // the spot's room less this bubble's own trailing inset (it keeps the tail over the character)
        let k = scale
        let maxW = room - (size * 0.18).rounded()
        let verb = Text(s.verb.uppercased()).font(Fonts.pixel(10 * k)).tracking(10 * k * 0.06).foregroundStyle(toneColor(s.tone))
        let line = s.target.isEmpty ? verb : verb + Text("\u{00A0}" + s.target).font(Fonts.mono(11.5 * k)).foregroundStyle(p.fg2)
        let r = L.bubbleRadius * k
        return FitWidth(maxWidth: maxW - 2 * L.bubblePad.1 * k) { line.multilineTextAlignment(.leading) }
        .padding(.vertical, L.bubblePad.0 * k).padding(.horizontal, L.bubblePad.1 * k)
        .background(RoundedRectangle(cornerRadius: r).fill(p.bg2))
        .overlay(RoundedRectangle(cornerRadius: r).strokeBorder(border, lineWidth: 1))
        .background(RoundedRectangle(cornerRadius: r + 3).fill(s.pop ? p.sel : .clear).padding(-3))
        .overlay(alignment: .bottomTrailing) {
            BubbleTail().fill(p.bg2).overlay(BubbleTail(open: true).stroke(border, lineWidth: 1))
                .frame(width: 12 * k, height: 6 * k).offset(x: -(size * 0.32), y: 5.5 * k)
        }
    }

    private func glow(_ s: ShadeFrame) -> some View {
        let (hex, a): (String, Double) = s.state == "error" ? (p.errHex, 0.28) : s.state == "permission" ? (p.warnHex, 0.26) : (p.accHex, p.dark ? 0.24 : 0.16)
        let c = Color(nsColor: parseColor(hex)).opacity(a)
        let op: Double = s.pop ? 1 : (s.state == "idle" || s.state == "sleeping") ? 0 : 0.5
        return Rectangle().fill(EllipticalGradient(colors: [c, c.opacity(0)], center: .center))
            .frame(width: size * 0.84, height: size * 0.78)
            .position(x: size / 2, y: size * 0.57)
            .opacity(op)
            .animation(.easeInOut(duration: 0.3), value: op)
    }

    private func trayButton(_ icon: (String, String, String)) -> some View {
        let on = ["water": "drinking", "food": "eating", "toy": "playing"][icon.0] == m.shade.state
        let lit = on || hover == icon.0
        return Button { m.care(icon.0) } label: {
            Group { if let img = templateImage(icon.2) { Image(nsImage: img).resizable().frame(width: 18 * scale, height: 18 * scale) } }
                .foregroundStyle(lit ? p.acc : p.fg2)
                .frame(width: m.layout.trayButton * scale, height: m.layout.trayButton * scale)
                .background(RoundedRectangle(cornerRadius: m.layout.trayRadius * scale).fill(p.bg2))
                .overlay(RoundedRectangle(cornerRadius: m.layout.trayRadius * scale).strokeBorder(lit ? p.acc : p.sep, lineWidth: 1))
        }
        .buttonStyle(.plain)
        .help(icon.1)
        .onHover { hover = $0 ? icon.0 : (hover == icon.0 ? nil : hover) }
    }

    /// drag to resize Shade continuously (Medium..Large); up-left grows, down-right shrinks (the dock is pinned
    /// bottom-right); double-click resets to Medium; ctrl-s steps +10 px; VoiceOver increment/decrement step 10 px
    private func resizeHandle() -> some View {
        let lit = hover == "size" || dragFrom != nil
        let b = m.layout.trayButton * scale
        return Group { if let img = templateImage(resizeIcon) { Image(nsImage: img).resizable().frame(width: 18 * scale, height: 18 * scale) } }
            .foregroundStyle(lit ? p.acc : p.fg2)
            .frame(width: b, height: b)
            .background(RoundedRectangle(cornerRadius: m.layout.trayRadius * scale).fill(p.bg2))
            .overlay(RoundedRectangle(cornerRadius: m.layout.trayRadius * scale).strokeBorder(lit ? p.acc : p.sep, lineWidth: 1))
            .contentShape(Rectangle())
            .gesture(DragGesture(minimumDistance: 1, coordinateSpace: .global)
                .onChanged { g in
                    let from = dragFrom ?? m.shadeSize
                    if dragFrom == nil { dragFrom = from }
                    // projection on the up-left diagonal: the character's top-left corner follows the pointer
                    m.setShadeSize(from - (g.translation.width + g.translation.height) / 2)
                }
                .onEnded { _ in
                    dragFrom = nil
                    if hover != "size" { cursor(false) }
                })
            .simultaneousGesture(TapGesture(count: 2).onEnded { m.setShadeSize(m.shadeRange.lowerBound) })
            .help("Drag to resize")
            .onHover { inside in
                hover = inside ? "size" : (hover == "size" ? nil : hover)
                if inside { cursor(true) } else if dragFrom == nil { cursor(false) }
            }
            .accessibilityElement()
            .accessibilityLabel("Resize Shade")
            .accessibilityValue("\(Int(m.shadeSize)) px")
            .accessibilityAdjustableAction { d in
                switch d {
                case .increment: m.setShadeSize(m.shadeSize + 10)
                case .decrement: m.setShadeSize(m.shadeSize - 10)
                @unknown default: break
                }
            }
    }

    private func cursor(_ on: Bool) {
        if on, !cursorPushed { resizeCursor.push(); cursorPushed = true }
        if !on, cursorPushed { NSCursor.pop(); cursorPushed = false }
    }

    /// hides Shade (character, bubble and this column); the status-bar face brings it back
    private func hideButton() -> some View {
        let lit = hover == "hide"
        let b = m.layout.trayButton * scale
        return Button { m.shadeHidden = true } label: {
            Group { if let img = templateImage(hideIcon) { Image(nsImage: img).resizable().frame(width: 18 * scale, height: 18 * scale) } }
                .foregroundStyle(lit ? p.acc : p.fg2)
                .frame(width: b, height: b)
                .background(RoundedRectangle(cornerRadius: m.layout.trayRadius * scale).fill(p.bg2))
                .overlay(RoundedRectangle(cornerRadius: m.layout.trayRadius * scale).strokeBorder(lit ? p.acc : p.sep, lineWidth: 1))
        }
        .buttonStyle(.plain)
        .help("Hide Shade (ctrl-h; click the eyes in the status bar to bring it back)")
        .onHover { hover = $0 ? "hide" : (hover == "hide" ? nil : hover) }
    }
}

/// the bubble's tail, pointing down (open = the two outer edges only, for the border)
struct BubbleTail: Shape {
    var open = false
    func path(in r: CGRect) -> Path {
        var p = Path()
        p.move(to: CGPoint(x: r.minX, y: r.minY))
        p.addLine(to: CGPoint(x: r.midX, y: r.maxY))
        p.addLine(to: CGPoint(x: r.maxX, y: r.minY))
        if !open { p.closeSubpath() }
        return p
    }
}

/// sizes its one child to its ideal width when that fits in `maxWidth`, else wraps it at `maxWidth` and takes the
/// height (and the widest line) the wrapped text needs: the bubble grows to fit and never truncates
struct FitWidth: SwiftUI.Layout {
    let maxWidth: CGFloat
    func sizeThatFits(proposal: ProposedViewSize, subviews: Subviews, cache: inout ()) -> CGSize {
        guard let v = subviews.first else { return .zero }
        let ideal = v.sizeThatFits(ProposedViewSize.unspecified)
        if ideal.width <= maxWidth { return ideal }
        return v.sizeThatFits(ProposedViewSize(width: maxWidth, height: nil))
    }
    func placeSubviews(in bounds: CGRect, proposal: ProposedViewSize, subviews: Subviews, cache: inout ()) {
        subviews.first?.place(at: bounds.origin, proposal: ProposedViewSize(width: bounds.width, height: bounds.height))
    }
}
