// Elisa: the native window for the Elisa agent. Spawns `elisa --serve-jsonl` in the chosen project folder.
//
//   ElisaApp [--demo] [--cwd DIR] [--theme dark|light] [--base-url URL [--type T]] [--model M] [--shots DIR]
//            [--script FILE [--seed N] [--script-delay S] [--script-cap S]] [--frame X,Y,W,H] [--continue] [--script-from-final]
//            [--frames DIR [--frames-every S]]
//
// --demo: no model; Elisa uses its scripted demo gateway (`--demo-states`) in a fresh demo project.
// --base-url / --type / --model are passed to Elisa (default: its config, http://127.0.0.1:8898 = streamdense gateway).
// --base-url alone keeps the active connection's kind and model (its key only for the same host); --type
// streamdense|anthropic|openai says what kind of server is there instead.
// --shots DIR (debug): plays the demo and writes PNGs of the window (offscreen render, no screen-recording permission).
// --script FILE: scripted playback of a person typing in this window (Playback.swift); any key or click stops it.
// --frames DIR (debug): the window as DIR/frame-NNNNN.png every S seconds (default 0.25), for a GIF or video of a run.
// --frames-window TITLE (debug): those frames of another window ("Model Scorecard", "Model Connections").
// --scorecard: open the Model Scorecard window at launch. --eval-test MODEL (debug): press Test on that installed
// Ollama model as the local model list does (confirm, then run); --eval-cancel-after S: then press Cancel.
import AppKit
import SwiftUI

@MainActor
enum Boot {
    static let kit = Kit.locate()
    static let model = AppModel(kit: kit)
}

final class AppDelegate: NSObject, NSApplicationDelegate {
    func applicationWillFinishLaunching(_ notification: Notification) {
        // writing to an Elisa that has already exited must fail the write, not kill the app (SIGPIPE's default)
        signal(SIGPIPE, SIG_IGN)
        // before any view asks for a font: the bundled OFL fonts (Silkscreen, Chivo Mono), else the system monospace
        MainActor.assumeIsolated { Fonts.registerFonts(Boot.kit) }
    }

    func applicationDidFinishLaunching(_ notification: Notification) {
        setvbuf(stdout, nil, _IOLBF, 0)
        NSApp.setActivationPolicy(.regular)
        MainActor.assumeIsolated { if let icon = Brand.appIcon { NSApp.applicationIconImage = icon } }
        NSApp.activate(ignoringOtherApps: true)
        MainActor.assumeIsolated { launch() }
    }

    @MainActor private func launch() {
        let m = Boot.model
        let args = CommandLine.arguments
        func opt(_ n: String) -> String? { args.firstIndex(of: n).flatMap { $0 + 1 < args.count ? args[$0 + 1] : nil } }
        func optAll(_ n: String) -> [String] { args.indices.filter { args[$0] == n && $0 + 1 < args.count }.map { args[$0 + 1] } }
        if let kit = Boot.kit {
            print("kit: \(kit.design.path)")
            ShadeKit.shared.load(kit)
            if let p = ShadeKit.shared.problem { print("shade: \(p)") }
        } else {
            print("kit: not found (set ELISA_KIT_DIR to elisa/design); Shade is not drawn")
        }
        print("fonts: \(Kit.fontReport())")
        print("elisa: \(ElisaProcess.binary()?.path ?? "not found")")
        MenuBarItem.shared.install(model: m, kit: Boot.kit)
        if opt("--theme") == "light" { m.dark = false }
        for f in ["--base-url", "--model"] + (opt("--base-url") != nil ? ["--type"] : []) { if let v = opt(f) { m.extraArgs += [f, v] } }
        // --continue: elisa resumes the project's last session
        if args.contains("--continue") { m.extraArgs.append("--continue") }
        // --connections-shot FILE [--connections-select NAME] (debug): the Model Connections window as a PNG, then quit
        if let file = opt("--connections-shot") {
            ConnectionsWindow.shared.shot(m, to: file, select: opt("--connections-select"))
            return
        }
        // --connections: open the Model Connections window at launch
        if args.contains("--connections") { DispatchQueue.main.async { ConnectionsWindow.shared.show(m, select: opt("--connections-select")) } }
        let shots = opt("--shots")
        m.demo = args.contains("--demo") || shots != nil
        m.folder = opt("--cwd")
        if let dir = shots { Shots.shared.run(into: dir, model: m) }
        if let dir = opt("--selftest") { SelfTest.shared.run(into: dir, images: optAll("--selftest-image"), quit: args.contains("--selftest-quit"), model: m) }
        if let dir = opt("--frames") {
            try? FileManager.default.createDirectory(atPath: dir, withIntermediateDirectories: true)
            var n = 0
            let other = opt("--frames-window")
            Timer.scheduledTimer(withTimeInterval: max(0.05, Double(opt("--frames-every") ?? "") ?? 0.25), repeats: true) { _ in
                MainActor.assumeIsolated {
                    let name = String(format: "frame-%05d", n)

                    if let t = other { DebugFrame.snap(title: t, to: (dir as NSString).appendingPathComponent(name + ".png")) } else { Shots.shared.capture(name, into: dir) }
                    n += 1
                }
            }
        }
        // --frame X,Y,W,H: where the window goes (two windows side by side for a two-session run); moved as soon as it
        // exists, so an off-screen test run does not flash on the screen
        if let r = DebugFrame.rect {
            func place() -> Bool {
                guard let w = NSApp.windows.first(where: { $0.title == "Elisa" }) else { return false }
                if w.frame != r { w.setFrame(r, display: true) }
                return true
            }
            _ = place()
            var tries = 0
            Timer.scheduledTimer(withTimeInterval: 0.02, repeats: true) { t in
                MainActor.assumeIsolated { tries += 1; if (place() && tries > 100) || tries > 250 { t.invalidate() } }
            }
        }
        // debug runs: when SwiftUI did not open the scene's window (it sometimes does not at launch), host the main view
        // in a window of its own, as --shots does
        if opt("--frames") != nil || DebugFrame.rect != nil {
            DispatchQueue.main.asyncAfter(deadline: .now() + 2) {
                guard !NSApp.windows.contains(where: { $0.title == "Elisa" }) else { return }
                let w = NSWindow(contentRect: DebugFrame.rect ?? NSRect(x: 0, y: 0, width: 1280, height: 820), styleMask: [.titled, .closable, .resizable, .miniaturizable, .fullSizeContentView], backing: .buffered, defer: false)
                w.title = "Elisa"
                w.titlebarAppearsTransparent = true
                w.contentView = NSHostingView(rootView: MainWindow().environmentObject(m).frame(minWidth: 640, minHeight: 560))
                w.isReleasedWhenClosed = false
                DebugFrame.place(w)
                w.orderFrontRegardless()
                print("frames: no scene window; hosting MainWindow in its own window")
            }
        }
        if args.contains("--scorecard") { DispatchQueue.main.async { ScorecardWindow.shared.show(m) } }
        if let model = opt("--eval-test") {
            DispatchQueue.main.asyncAfter(deadline: .now() + 2) {
                let conns = ConnectionStore()
                conns.load()
                DispatchQueue.main.asyncAfter(deadline: .now() + 1.5) {
                    let t = EvalTarget.ollama(model, conns: conns.items)
                    EvalStore.shared.confirming = t
                    DispatchQueue.main.asyncAfter(deadline: .now() + 1.5) { EvalStore.shared.start(t) }
                    if let s = Double(opt("--eval-cancel-after") ?? "") {
                        DispatchQueue.main.asyncAfter(deadline: .now() + 1.5 + s) { EvalStore.shared.cancel() }
                    }
                }
            }
        }
        if m.demo || m.folder != nil {
            m.start()
            if let f = opt("--script") {
                Playback.shared.run(file: f, seed: UInt64(opt("--seed") ?? "") ?? 1, delay: Double(opt("--script-delay") ?? "") ?? 3,
                                    cap: Double(opt("--script-cap") ?? "") ?? 2700, fromFinal: args.contains("--script-from-final"), model: m)
            }
        } else {
            // no folder yet: the welcome screen (introduction, model connection, folder choice)
            m.welcome = true
        }
    }

    func applicationShouldTerminateAfterLastWindowClosed(_ sender: NSApplication) -> Bool { true }

    func applicationWillTerminate(_ notification: Notification) {
        MainActor.assumeIsolated { Boot.model.stop(); EvalStore.shared.stopNow() }
    }
}

@MainActor
func openFolderPanel(_ m: AppModel) {
    let p = NSOpenPanel()
    p.canChooseDirectories = true
    p.canChooseFiles = false
    p.allowsMultipleSelection = false
    // a new project starts with a new folder: NSOpenPanel hides "New Folder" unless asked
    p.canCreateDirectories = true
    p.prompt = "Open"
    p.message = "Choose the project folder Elisa works in, or make a new one with New Folder"
    if let last = UserDefaults.standard.string(forKey: "lastFolder") { p.directoryURL = URL(fileURLWithPath: last) }
    if p.runModal() == .OK, let u = p.url {
        m.open(folder: u.path)
    } else if !m.welcome && m.phase == .starting && m.cwd.isEmpty {
        m.blocks = [Block(kind: .info("[no folder chosen · File > Open Folder… (⌘O), or File > Demo Session]", .plain))]
        m.phase = .exited
    }
}

@main
struct ElisaMain: App {
    @NSApplicationDelegateAdaptor(AppDelegate.self) var delegate
    @ObservedObject private var m = Boot.model

    var body: some Scene {
        Window("Elisa", id: "main") {
            MainWindow()
                .environmentObject(m)
                // the Shades pause while this window is occluded and slow down while it is not key (ShadePace)
                .background(ShadePaceProbe())
                .frame(minWidth: 640, minHeight: 560)
        }
        .windowStyle(.hiddenTitleBar)
        .defaultSize(width: 1280, height: 820)
        .commands {
            CommandGroup(replacing: .newItem) {
                Button("Open Folder…") { openFolderPanel(m) }.keyboardShortcut("o")
                Button("Demo Session (no model)") { m.demo = true; m.folder = nil; m.start() }
            }
            CommandGroup(after: .pasteboard) {
                Divider()
                Button("Copy Transcript for Debugging") { m.copyTranscript() }.keyboardShortcut("c", modifiers: [.command, .option])
                Button("Attach Images…") { m.pickImages() }.keyboardShortcut("a", modifiers: [.command, .shift]).disabled(m.attachBlocked != nil)
            }
            CommandMenu("Elisa") {
                Button("Model Connections…") { ConnectionsWindow.shared.show(m) }.keyboardShortcut(",")
                Button("Model Scorecard…") { ScorecardWindow.shared.show(m) }
                Divider()
                Button(m.editsShown ? "Hide Edits" : "Show Edits") { m.toggleEdits() }.keyboardShortcut("o", modifiers: .control)
                Button("Previous Edit") { m.editPrev() }.keyboardShortcut("[", modifiers: .command)
                Button("Next Edit") { m.editNext() }.keyboardShortcut("]", modifiers: .command)
                Button("Toggle Theme") { m.toggleTheme() }.keyboardShortcut("l", modifiers: [.command, .shift])
                Divider()
                Button("Interrupt") { m.interrupt() }.keyboardShortcut(".", modifiers: .command)
                Button("Stop Playback") { m.playbackStop?() }.keyboardShortcut(".", modifiers: .control).disabled(m.playbackLine == nil)
                Divider()
                Button("Poke Elisa") { m.care("poke") }.keyboardShortcut("g", modifiers: .control)
                Button("Shade Size: \(Int(m.shadeSize)) px (+10)") { m.stepShadeSize() }.keyboardShortcut("s", modifiers: .control)
                Button(m.shadeHidden ? "Show Shade" : "Hide Shade") { m.shadeHidden.toggle() }.keyboardShortcut("h", modifiers: .control)
                Button("Give Water") { m.care("water") }
                Button("Give a Snack") { m.care("food") }
                Button("Give a Toy") { m.care("toy") }
            }
        }
    }
}
