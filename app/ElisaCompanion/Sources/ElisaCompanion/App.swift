// Elisa companion: Shade in a small floating window, following Elisa's live state over ~/.elisa/companion.sock.
//
//   ElisaCompanion [--size small|medium|large] [--no-top] [--socket PATH] [--shots DIR] [--snap-dir DIR] [--log-states]
//
// --log-states: print each state line received from Elisa (smoke tests).
//
// --shots DIR (debug): plays idle, thinking, editing, permission, celebrating and playing, writes one PNG of the page per
// state into DIR (an in-process render: no screen-recording permission needed), then quits.
// --snap-dir DIR (debug): the same kind of PNG for each state Elisa sends that holds for 1.3 s (first time per state).
import AppKit
import Darwin
import SwiftUI
import WebKit

@MainActor
final class Controller: NSObject, ObservableObject, WKScriptMessageHandler, WKNavigationDelegate {
    static let shared = Controller()

    @Published var alwaysOnTop = true { didSet { panel?.level = alwaysOnTop ? .floating : .normal } }
    @Published var connected = false
    @Published var stateName = "idle"

    private(set) var panel: CompanionPanel?
    private var web: CompanionWebView?
    private var server: LinkServer?
    private var hitRects: [NSRect] = []
    private var hitTimer: Timer?
    private var ready = false
    private var pending: [String] = []
    private var dock = 128
    private var snapDir: String?
    private var snapped: Set<String> = []

    func launch() {
        setvbuf(stdout, nil, _IOLBF, 0) // line-buffered log, also when redirected to a file
        let args = CommandLine.arguments
        func opt(_ n: String) -> String? { args.firstIndex(of: n).flatMap { $0 + 1 < args.count ? args[$0 + 1] : nil } }
        if args.contains("--no-top") { alwaysOnTop = false }

        guard let kit = Kit.locate() else { fail("brand kit not found: set ELISA_KIT_DIR to elisa/design") }
        let sizes = kit.dockSizes()
        let want = (opt("--size") ?? "medium").capitalized
        dock = sizes[want] ?? sizes["Medium"] ?? 128
        print("kit: \(kit.design.path)")
        print("fonts: \(Kit.fontReport())")

        guard let url = Bundle.module.url(forResource: "companion", withExtension: "html"),
              let tpl = try? String(contentsOf: url, encoding: .utf8) else { fail("companion.html missing from the bundle") }
        let html: String
        do { html = try kit.page(template: tpl, size: dock) } catch { fail("\(error)") }

        // the window, bottom-right of the main screen the first time; then where the user left it
        let size = windowSize(dock: dock)
        let p = CompanionPanel(size: size)
        p.level = alwaysOnTop ? .floating : .normal
        if !p.setFrameUsingName("ElisaCompanion"), let vf = NSScreen.main?.visibleFrame {
            p.setFrameOrigin(NSPoint(x: vf.maxX - size.width - 24, y: vf.minY + 24))
        }
        p.setContentSize(size)
        p.setFrameAutosaveName("ElisaCompanion")

        let cfg = WKWebViewConfiguration()
        cfg.userContentController.add(self, name: "companion")
        let w = CompanionWebView(frame: NSRect(origin: .zero, size: size), configuration: cfg)
        w.setValue(false, forKey: "drawsBackground")
        w.underPageBackgroundColor = .clear
        w.navigationDelegate = self
        w.autoresizingMask = [.width, .height]
        p.contentView = w
        w.loadHTMLString(html, baseURL: nil)
        panel = p
        web = w
        p.orderFrontRegardless()
        print("window \(p.windowNumber) \(Int(size.width))x\(Int(size.height)) (dock \(dock) px)")

        // click-through outside Shade / tray / bubble
        hitTimer = Timer.scheduledTimer(withTimeInterval: 1.0 / 20, repeats: true) { _ in
            MainActor.assumeIsolated { Controller.shared.updateClickThrough() }
        }

        let s = LinkServer(path: opt("--socket") ?? LinkServer.defaultPath())
        s.onState = { [weak self] j in self?.applyState(j) }
        s.onConnected = { [weak self] on in
            self?.connected = on
            self?.js("companion.connected(\(on))")
        }
        do { try s.start() } catch { fail("\(error)") }
        server = s

        snapDir = opt("--snap-dir")
        if let dir = opt("--shots") { shots(into: dir) }
    }

    func quit() {
        server?.stop()
        NSApp.terminate(nil)
    }

    func shutdown() { server?.stop() }

    private func fail(_ msg: String) -> Never {
        FileHandle.standardError.write(("elisa-companion: " + msg + "\n").data(using: .utf8)!)
        exit(1)
    }

    // MARK: - page

    private func js(_ code: String) {
        guard ready else { pending.append(code); return }
        web?.evaluateJavaScript(code, completionHandler: nil)
    }

    private func applyState(_ j: [String: Any]) {
        var m: [String: Any] = [:]
        for k in ["state", "verb", "target", "tone", "theme"] { if let v = j[k] as? String { m[k] = v } }
        m["pop"] = j["pop"] as? Bool ?? false
        guard let st = m["state"] as? String,
              let d = try? JSONSerialization.data(withJSONObject: m), let s = String(data: d, encoding: .utf8) else { return }
        stateName = st
        if CommandLine.arguments.contains("--log-states") { print("state \(st) \(m["verb"] ?? "") \(m["target"] ?? "")") }
        js("companion.apply(\(s))")
        snapLive(st)
    }

    nonisolated func userContentController(_ uc: WKUserContentController, didReceive msg: WKScriptMessage) {
        MainActor.assumeIsolated {
            guard let b = msg.body as? [String: Any], let type = b["type"] as? String else { return }
            switch type {
            case "ready":
                ready = true
                for c in pending { web?.evaluateJavaScript(c, completionHandler: nil) }
                pending.removeAll()
            case "care":
                guard let a = b["action"] as? String, ["poke", "water", "food", "toy"].contains(a) else { return }
                // Elisa owns the state when connected (it answers with the new state line); alone, play it here
                if server?.sendCare(a) != true { js("companion.localCare('\(a)')") }
            case "hit":
                let rs = (b["rects"] as? [[Double]]) ?? []
                hitRects = rs.filter { $0.count == 4 }.map { NSRect(x: $0[0], y: $0[1], width: $0[2], height: $0[3]) }
            default:
                break
            }
        }
    }

    private func updateClickThrough() {
        guard let p = panel, let v = web else { return }
        let m = p.convertPoint(fromScreen: NSEvent.mouseLocation)
        // page coordinates: top-left origin
        let pt = NSPoint(x: m.x, y: v.bounds.height - m.y)
        let inside = hitRects.contains { $0.insetBy(dx: -4, dy: -4).contains(pt) }
        if p.ignoresMouseEvents == inside { p.ignoresMouseEvents = !inside }
    }

    // MARK: - --shots

    private func shots(into dir: String) {
        try? FileManager.default.createDirectory(atPath: dir, withIntermediateDirectories: true)
        let scenes: [(String, [String: Any])] = [
            ("idle", ["state": "idle", "verb": "", "target": "", "tone": "accent", "pop": false]),
            ("thinking", ["state": "thinking", "verb": "THINKING", "target": "give me a sec", "tone": "accent", "pop": false]),
            ("editing", ["state": "editing", "verb": "EDITING", "target": "split.rs", "tone": "accent", "pop": false]),
            ("permission", ["state": "permission", "verb": "NEEDS YOU", "target": "cargo test", "tone": "warn", "pop": true]),
            ("celebrating", ["state": "celebrating", "verb": "ALL DONE!", "target": "tests green", "tone": "accent", "pop": true]),
            ("playing", ["state": "playing", "verb": "PLAY", "target": "yarn ball", "tone": "accent", "pop": true]),
        ]
        func step(_ i: Int) {
            guard i < scenes.count else { print("shots: done"); quit(); return }
            var m = scenes[i].1
            m["theme"] = "dark"
            applyState(m)
            DispatchQueue.main.asyncAfter(deadline: .now() + 1.4) {
                self.snapshot(to: (dir as NSString).appendingPathComponent("companion-\(scenes[i].0).png")) { step(i + 1) }
            }
        }
        DispatchQueue.main.asyncAfter(deadline: .now() + 1.0) { step(0) }
    }

    /// --snap-dir DIR: the first time a state from Elisa holds for 1.3 s, write DIR/companion-<state>.png
    private func snapLive(_ st: String) {
        guard let dir = snapDir, !snapped.contains(st) else { return }
        snapped.insert(st)
        DispatchQueue.main.asyncAfter(deadline: .now() + 1.3) {
            guard self.stateName == st else { self.snapped.remove(st); return }
            try? FileManager.default.createDirectory(atPath: dir, withIntermediateDirectories: true)
            self.snapshot(to: (dir as NSString).appendingPathComponent("companion-\(st).png")) {}
        }
    }

    /// the page rendered in-process to a PNG (no screen-recording permission needed). The window itself is
    /// transparent; for the picture the theme's window background (colors.json bg) is painted behind Shade.
    private func snapshot(to path: String, then done: @escaping () -> Void) {
        web?.evaluateJavaScript("companion.backdrop(true)") { _, _ in
            self.web?.takeSnapshot(with: WKSnapshotConfiguration()) { img, _ in
                if let img, let tiff = img.tiffRepresentation, let rep = NSBitmapImageRep(data: tiff),
                   let png = rep.representation(using: .png, properties: [:]) {
                    try? png.write(to: URL(fileURLWithPath: path))
                    print("shot: \(path)")
                }
                self.web?.evaluateJavaScript("companion.backdrop(false)", completionHandler: nil)
                done()
            }
        }
    }
}

final class AppDelegate: NSObject, NSApplicationDelegate {
    private var signalSources: [DispatchSourceSignal] = []

    func applicationDidFinishLaunching(_ notification: Notification) {
        NSApp.setActivationPolicy(.accessory) // no Dock icon: the window and the menu-bar item are the app
        for sig in [SIGINT, SIGTERM, SIGHUP] {
            signal(sig, SIG_IGN)
            let s = DispatchSource.makeSignalSource(signal: sig, queue: .main)
            s.setEventHandler { MainActor.assumeIsolated { Controller.shared.quit() } }
            s.resume()
            signalSources.append(s)
        }
        MainActor.assumeIsolated { Controller.shared.launch() }
    }

    func applicationWillTerminate(_ notification: Notification) {
        MainActor.assumeIsolated { Controller.shared.shutdown() }
    }
}

@main
struct ElisaCompanionApp: App {
    @NSApplicationDelegateAdaptor(AppDelegate.self) var delegate
    @ObservedObject private var c = Controller.shared

    var body: some Scene {
        MenuBarExtra("Elisa", systemImage: "pawprint.fill") {
            Text(c.connected ? "Elisa connected · \(c.stateName)" : "Elisa not connected")
            Divider()
            Toggle("Always on top", isOn: $c.alwaysOnTop)
            Button("Quit Elisa companion") { c.quit() }.keyboardShortcut("q")
        }
    }
}
