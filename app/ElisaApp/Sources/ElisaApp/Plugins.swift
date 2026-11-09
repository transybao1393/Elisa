// Plugins (docs/reference/mcp-and-plugins.md "Plugins", events in elisa/src/jsonl.rs): the Plugins window (Elisa › Plugins…) lists what
// is installed and sends the session's `{"type":"plugin","op":…}` requests; the Plugin Setup window shows a
// `plugin_preview` (what an add / update / trust would install and allow) or a `project_trust` (what a repository
// wants to add) and answers the yes / no `approve` (question) that follows.
//
// With no session (no folder open) the list comes from `elisa plugin list --json` and enable / disable / remove run
// as `elisa plugin OP NAME`; add, update and trust need the setup screen's yes / no, so they wait for a session.
import AppKit
import SwiftUI

struct PluginServer: Hashable {
    var name = "", runs = "container"
    var installed = false, running = false
    var tools: Int?, tokens: Int?
    /// "ok" | "failed" | "not installed" (an older Elisa sends none)
    var state: String?
    /// why it did not start (docker's message and a hint), with state "failed"
    var error: String?
    var host: Bool { runs == "host" }
    var failed: Bool { state == "failed" || error != nil }
}

struct PluginInfo: Identifiable {
    var name = "", version = "", description = "", source = "user"
    var enabled = false, trusted = false
    var servers: [PluginServer] = []
    var rules = 0
    var commands: [String] = []
    var notes: [String] = []
    var id: String { source + ":" + name }
    var project: Bool { source == "project" }
    /// the servers' tool count and token cost, when any server's is known
    var tools: Int? { let t = servers.compactMap(\.tools); return t.isEmpty ? nil : t.reduce(0, +) }
    var tokens: Int? { let t = servers.compactMap(\.tokens); return t.isEmpty ? nil : t.reduce(0, +) }

    init(_ j: [String: Any]) {
        name = j["name"] as? String ?? ""
        version = j["version"] as? String ?? ""
        description = j["description"] as? String ?? ""
        source = j["source"] as? String ?? "user"
        enabled = j["enabled"] as? Bool ?? false
        trusted = j["trusted"] as? Bool ?? false
        rules = (j["rules"] as? NSNumber)?.intValue ?? 0
        commands = j["commands"] as? [String] ?? []
        notes = j["notes"] as? [String] ?? []
        servers = (j["servers"] as? [[String: Any]] ?? []).map { s in
            PluginServer(name: s["name"] as? String ?? "", runs: s["runs"] as? String ?? "container",
                         installed: s["installed"] as? Bool ?? false, running: s["running"] as? Bool ?? false,
                         tools: (s["tools"] as? NSNumber)?.intValue, tokens: (s["tokens"] as? NSNumber)?.intValue,
                         state: s["state"] as? String, error: (s["error"] as? String).flatMap { $0.isEmpty ? nil : $0 })
        }
    }

    /// on / off / changed, as `/plugin list` words it
    var state: (text: String, tone: Tone) {
        if project { return trusted ? ("trusted", .ok) : ("not trusted yet", .warn) }
        if !trusted { return ("changed since you approved it · off until trusted", .warn) }
        if enabled && servers.contains(where: \.failed) { return ("on, but NOT WORKING: a server failed to start", .err) }
        return enabled ? ("on", .ok) : ("off", .plain)
    }
}

/// `plugin_preview` {op, name, text, preview: {…}}: the setup screen
struct PluginPreview {
    struct Server: Hashable {
        var name = "", runs = "container", installer: String?, package = "", version = "", image: String?
        var imagePresent: Bool?
        var command: [String] = []
        var mounts: [Mount] = []
        var env: [String] = []
        var tools = ""
        var host: Bool { runs == "host" }
    }
    struct Mount: Hashable { var path: String; var write: Bool }
    struct Rule: Hashable { var level: String; var pattern: String; var broad: Bool }
    /// `text`: the full prompt the command sends to the model (an older Elisa sends none)
    struct Command: Hashable { var name: String; var description: String; var args: String = ""; var text: String = "" }
    var op = "add", name = "", text = ""
    var version = "", oldVersion: String?, description = "", source = ""
    var host = false
    var servers: [Server] = []
    var rules: [Rule] = []
    var commands: [Command] = []
    var notes: [String] = []
    var problems: [String] = []

    init(_ e: [String: Any]) {
        op = e["op"] as? String ?? "add"
        name = e["name"] as? String ?? ""
        text = e["text"] as? String ?? ""
        let p = e["preview"] as? [String: Any] ?? [:]
        version = p["version"] as? String ?? ""
        oldVersion = p["old_version"] as? String
        description = p["description"] as? String ?? ""
        source = p["source"] as? String ?? ""
        host = p["host"] as? Bool ?? false
        servers = (p["servers"] as? [[String: Any]] ?? []).map { s in
            Server(name: s["name"] as? String ?? "", runs: s["runs"] as? String ?? "container", installer: s["installer"] as? String,
                   package: s["package"] as? String ?? "", version: s["version"] as? String ?? "", image: s["image"] as? String,
                   imagePresent: s["image_present"] as? Bool, command: s["command"] as? [String] ?? [],
                   mounts: (s["mounts"] as? [[String: Any]] ?? []).map { Mount(path: $0["path"] as? String ?? "", write: $0["write"] as? Bool ?? false) },
                   env: s["env"] as? [String] ?? [], tools: s["tools"] as? String ?? "")
        }
        rules = (p["rules"] as? [[String: Any]] ?? []).map { Rule(level: $0["level"] as? String ?? "", pattern: $0["pattern"] as? String ?? "", broad: $0["broad"] as? Bool ?? false) }
        commands = (p["commands"] as? [[String: Any]] ?? []).map {
            Command(name: $0["name"] as? String ?? "", description: $0["description"] as? String ?? "",
                    args: $0["args"] as? String ?? "", text: $0["text"] as? String ?? "")
        }
        notes = p["notes"] as? [String] ?? []
        problems = p["problems"] as? [String] ?? []
        missingImages = p["missing_images"] as? [String] ?? []
    }

    /// listed images that are not on this machine (Elisa can download them after a yes)
    var missingImages: [String] = []

    var verb: String { op == "update" ? "Update" : op == "trust" ? "Trust" : "Install" }
}

/// `project_trust` {path, hash, text, commands, rules, plugins}
struct ProjectTrust {
    struct Command: Hashable { var name: String; var description: String; var source: String }
    struct Rule: Hashable { var level: String; var pattern: String; var source: String }
    var path = "", text = ""
    var commands: [Command] = []
    var rules: [Rule] = []
    var plugins: [String] = []

    init(_ e: [String: Any]) {
        path = e["path"] as? String ?? ""
        text = e["text"] as? String ?? ""
        commands = (e["commands"] as? [[String: Any]] ?? []).map { Command(name: $0["name"] as? String ?? "", description: $0["description"] as? String ?? "", source: $0["source"] as? String ?? "") }
        rules = (e["rules"] as? [[String: Any]] ?? []).map { Rule(level: $0["level"] as? String ?? "", pattern: $0["pattern"] as? String ?? "", source: $0["source"] as? String ?? "") }
        plugins = e["plugins"] as? [String] ?? []
    }
}

@MainActor
final class PluginStore: ObservableObject {
    static let shared = PluginStore()

    @Published private(set) var plugins: [PluginInfo] = []
    @Published private(set) var loaded = false
    /// why the list could not be read (CLI failed)
    @Published private(set) var listError: String?
    /// the last outcome, one line: `plugin_result`, a CLI run, or "sent …" while the session works on it
    @Published var status: (text: String, ok: Bool)?
    @Published private(set) var preview: PluginPreview?
    @Published private(set) var trust: ProjectTrust?
    /// a plugin whose Remove was pressed and waits for the confirm
    @Published var confirmRemove: String?
    /// a CLI operation running (no session)
    @Published private(set) var working = false
    /// bumped by every `plugins` event: a CLI list started before it does not overwrite the session's newer one
    private var gen = 0

    // MARK: events from the session

    func event(_ type: String, _ e: [String: Any], _ m: AppModel) {
        switch type {
        case "plugins":
            gen += 1
            plugins = (e["plugins"] as? [[String: Any]] ?? []).map(PluginInfo.init)
            loaded = true
            listError = nil
        case "plugin_preview":
            preview = PluginPreview(e)
            trust = nil
            PluginSetupWindow.shared.show(m, byEvent: true)
        case "project_trust":
            trust = ProjectTrust(e)
            preview = nil
            PluginSetupWindow.shared.show(m, byEvent: true)
        case "plugin_result":
            // installed but a server did not start (`working: false`): an error, not a success
            let ok = (e["ok"] as? Bool ?? false) && (e["working"] as? Bool ?? true)
            let text = e["text"] as? String ?? ""
            // the last line says what happened (the lines above are the servers' probe); a failed server's own line
            // (with docker's reason) is the one that says why
            let lines = text.split(separator: "\n").map(String.init)
            let why = ok ? nil : lines.first { $0.localizedCaseInsensitiveContains("fail") || $0.localizedCaseInsensitiveContains("not working") }
            status = ((why.map { $0 + " · " } ?? "") + (lines.last ?? text), ok)
            // a setup that could not go ahead (problems) stays open so its problems can be read
            if preview?.problems.isEmpty ?? true { preview = nil }
        default: break
        }
    }

    /// the yes / no `approve` after a preview or a project's trust prompt: the setup window answers it
    func question(_ m: AppModel) {
        if preview != nil || trust != nil { PluginSetupWindow.shared.show(m, byEvent: true) }
        // --plugin-setup-answer yes|no (debug): press the setup window's button after 3 s
        let a = CommandLine.arguments
        if let i = a.firstIndex(of: "--plugin-setup-answer"), i + 1 < a.count {
            let yes = a[i + 1] == "yes"
            DispatchQueue.main.asyncAfter(deadline: .now() + 3) { if m.approval?.question == true { print("plugins: setup answered \(yes ? "yes" : "no") (--plugin-setup-answer)"); self.answer(yes, m) } }
        }
    }

    func answer(_ yes: Bool, _ m: AppModel) {
        if m.approval?.question == true { m.answer(word: yes ? "yes" : "no") }
        let p = preview, t = trust
        if !yes { status = (p.map { "\($0.verb.lowercased()) \($0.name): not now" } ?? (t != nil ? "project not trusted" : "not now"), false) }
        else if let p { status = (["update": "updating", "trust": "trusting"][p.op, default: "installing"] + " \(p.name)…", true) }
        if t != nil { trust = nil }
        if p != nil && (p?.problems.isEmpty ?? true) { preview = nil }
        PluginSetupWindow.shared.close()
    }

    /// the setup window was closed: a pending question is answered no
    func setupClosed(_ m: AppModel) {
        if m.approval?.question == true { m.answer(word: "no") }
        preview = nil
        trust = nil
    }

    func sessionEnded() {
        preview = nil
        trust = nil
        PluginSetupWindow.shared.close()
    }

    // MARK: actions

    /// enable / disable / remove / update / trust: through the session when there is one, else (enable, disable,
    /// remove) with `elisa plugin OP NAME`
    func act(_ op: String, _ name: String, _ m: AppModel) {
        confirmRemove = nil
        if m.sessionOpen {
            m.sendPlugin(op, name)
            status = ("sent: /plugin \(op) \(name)", true)
            return
        }
        guard ["enable", "disable", "remove"].contains(op) else { status = ("open a folder to \(op) plugins", false); return }
        working = true
        status = ("elisa plugin \(op) \(name)…", true)
        Self.cli(["plugin", op, name], cwd: "/") { code, out, err in
            self.working = false
            let line = (code == 0 ? out : (err.isEmpty ? out : err)).split(separator: "\n").last.map(String.init) ?? "exit \(code)"
            self.status = (line, code == 0)
            self.refresh(m)
        }
    }

    /// "Add plugin…": a local folder, then the session's add (the setup screen follows)
    func add(_ m: AppModel) {
        guard m.sessionOpen else { status = ("open a folder to add plugins", false); return }
        let p = NSOpenPanel()
        p.canChooseDirectories = true
        p.canChooseFiles = false
        p.allowsMultipleSelection = false
        p.prompt = "Choose"
        p.message = "Choose a plugin folder (it holds a plugin.toml). Local folders only."
        if p.runModal() == .OK, let u = p.url { add(path: u.path, m) }
    }

    func add(path: String, _ m: AppModel) {
        m.sendPlugin("add", path)
        status = ("sent: /plugin add \((path as NSString).abbreviatingWithTildeInPath)", true)
    }

    /// the list from `elisa plugin list --json` (no session needed): in the session's folder when there is one, so
    /// the project's plugins show too; the session's own `plugins` events replace it as they come
    func refresh(_ m: AppModel) {
        let cwd = m.sessionOpen && !m.cwd.isEmpty ? m.cwd : "/"
        let started = gen
        Self.cli(["plugin", "list", "--json"], cwd: cwd) { code, out, err in
            guard started == self.gen else { return }
            guard code == 0, let d = out.data(using: .utf8), let j = (try? JSONSerialization.jsonObject(with: d)) as? [String: Any], let list = j["plugins"] as? [[String: Any]] else {
                self.listError = "could not read the plugin list: " + (err.split(separator: "\n").last.map(String.init) ?? "exit \(code)")
                self.loaded = true
                return
            }
            self.plugins = list.map(PluginInfo.init)
            self.listError = nil
            self.loaded = true
        }
    }

    /// `elisa ARGS` off the main thread, its stdin closed (nothing can be asked), then `done` on the main thread
    static func cli(_ args: [String], cwd: String, done: @escaping @MainActor (Int32, String, String) -> Void) {
        guard let bin = ElisaProcess.binary() else {
            done(127, "", "elisa binary not found: build it (cd elisa && cargo build --release) or set ELISA_BIN")
            return
        }
        DispatchQueue.global(qos: .userInitiated).async {
            let p = Process()
            p.executableURL = bin
            p.arguments = args
            p.currentDirectoryURL = URL(fileURLWithPath: cwd)
            let out = Pipe(), err = Pipe()
            p.standardOutput = out
            p.standardError = err
            p.standardInput = FileHandle.nullDevice
            var code: Int32 = 127
            var o = Data(), e = Data()
            do {
                try p.run()
                // stderr on its own thread: a full pipe would block the child while stdout is read
                let g = DispatchGroup()
                g.enter()
                DispatchQueue.global().async { e = err.fileHandleForReading.readDataToEndOfFile(); g.leave() }
                o = out.fileHandleForReading.readDataToEndOfFile()
                g.wait()
                p.waitUntilExit()
                code = p.terminationStatus
            } catch {
                e = Data("cannot run elisa: \(error.localizedDescription)".utf8)
            }
            let (so, se, c) = (String(decoding: o, as: UTF8.self), String(decoding: e, as: UTF8.self), code)
            DispatchQueue.main.async { MainActor.assumeIsolated { done(c, so, se) } }
        }
    }
}

// MARK: - the Plugins window

struct PluginsView: View {
    @ObservedObject var store = PluginStore.shared
    @ObservedObject var m: AppModel
    @Environment(\.pal) var p

    var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            VStack(alignment: .leading, spacing: 6) {
                Pixel("Plugins", size: 11, color: p.acc)
                Text(verbatim: "Each plugin brings MCP servers, slash commands and permission rules from a local folder.")
                    .font(Fonts.mono(12)).foregroundStyle(p.tx)
                Text(verbatim: m.sessionOpen ? "Changes apply to this session at once." : "Open a folder to add, update or trust plugins. Enable, disable and remove work now.")
                    .font(Fonts.mono(11)).foregroundStyle(p.fg2)
            }
            .padding(.horizontal, 22).padding(.top, 30).padding(.bottom, 14)
            Rectangle().fill(p.sep).frame(height: 1)
            if let e = store.listError {
                Text(verbatim: e).font(Fonts.mono(12)).foregroundStyle(p.err).padding(22)
                Spacer(minLength: 0)
            } else if !store.loaded {
                Text(verbatim: "reading…").font(Fonts.mono(12)).foregroundStyle(p.fg2).padding(22)
                Spacer(minLength: 0)
            } else if store.plugins.isEmpty {
                Text(verbatim: "No plugins installed. Add plugin… installs one from a local folder.").font(Fonts.mono(12)).foregroundStyle(p.fg2).padding(22)
                Spacer(minLength: 0)
            } else {
                ScrollView {
                    LazyVStack(alignment: .leading, spacing: 0) {
                        ForEach(store.plugins) { pl in
                            row(pl)
                            Rectangle().fill(p.sep.opacity(0.6)).frame(height: 1)
                        }
                    }
                }
            }
            Rectangle().fill(p.sep).frame(height: 1)
            HStack(spacing: 12) {
                SmallBtn(title: "Add plugin…", primary: m.sessionOpen) { store.add(m) }
                    .disabled(!m.sessionOpen)
                    .help(m.sessionOpen ? "Choose a local plugin folder; its setup screen shows before anything is installed" : "Open a folder to manage plugins")
                if let s = store.status {
                    Text(verbatim: s.text).font(Fonts.mono(11)).foregroundStyle(s.ok ? p.ok : p.warn).lineLimit(2).textSelection(.enabled)
                } else if !m.sessionOpen {
                    Text(verbatim: "open a folder to add plugins").font(Fonts.mono(11)).foregroundStyle(p.fg2)
                }
                Spacer()
                Button { store.refresh(m) } label: { Text(verbatim: "↻ reload").font(Fonts.mono(11)).foregroundStyle(p.acc) }.buttonStyle(.plain)
            }
            .padding(.horizontal, 22).padding(.vertical, 9)
        }
        .background(p.bg)
        .ignoresSafeArea()
        .frame(minWidth: 760, minHeight: 420)
        .onAppear { store.refresh(m) }
    }

    private func row(_ pl: PluginInfo) -> some View {
        let st = pl.state
        return VStack(alignment: .leading, spacing: 7) {
            HStack(alignment: .firstTextBaseline, spacing: 10) {
                Text(verbatim: pl.name).font(Fonts.mono(13, .semibold)).foregroundStyle(p.fg)
                Text(verbatim: pl.version).font(Fonts.mono(11.5)).foregroundStyle(p.fg2)
                tag(pl.project ? "project" : "user", p.fg2)
                Text(verbatim: st.text).font(Fonts.mono(11.5, .semibold)).foregroundStyle(color(st.tone))
                Spacer(minLength: 8)
                actions(pl)
            }
            if !pl.description.isEmpty {
                Text(verbatim: pl.description).font(Fonts.mono(12)).foregroundStyle(p.tx).fixedSize(horizontal: false, vertical: true)
            }
            ForEach(pl.servers, id: \.self) { s in
                HStack(alignment: .firstTextBaseline, spacing: 8) {
                    Text(verbatim: "server").font(Fonts.mono(11)).foregroundStyle(p.fg2).frame(width: 64, alignment: .leading)
                    Text(verbatim: s.name).font(Fonts.mono(11.5, .semibold)).foregroundStyle(p.code)
                    if s.host {
                        Text(verbatim: "⚠ runs on this Mac").font(Fonts.mono(11, .semibold)).foregroundStyle(p.warn)
                    } else {
                        Text(verbatim: "runs in container").font(Fonts.mono(11)).foregroundStyle(p.tx)
                    }
                    if !s.installed { Text(verbatim: "not installed").font(Fonts.mono(11)).foregroundStyle(p.warn) }
                    if s.failed { Text(verbatim: "FAILED to start").font(Fonts.mono(11, .semibold)).foregroundStyle(p.err) }
                    else if s.running { Text(verbatim: "running").font(Fonts.mono(11)).foregroundStyle(p.ok) }
                    Text(verbatim: s.failed ? "its tools are missing" : toolText(s.tools, s.tokens)).font(Fonts.mono(11)).foregroundStyle(p.fg2)
                }
                if let e = s.error {
                    Text(verbatim: e).font(Fonts.mono(11)).foregroundStyle(p.err).textSelection(.enabled)
                        .fixedSize(horizontal: false, vertical: true).padding(.leading, 72)
                }
            }
            let facts = [pl.rules > 0 ? "\(pl.rules) rule\(pl.rules == 1 ? "" : "s")" : "no rules",
                         pl.commands.isEmpty ? "no commands" : "commands: " + pl.commands.joined(separator: " "),
                         pl.servers.count > 1 ? "all servers: " + toolText(pl.tools, pl.tokens) : nil].compactMap { $0 }
            Text(verbatim: facts.joined(separator: " · ")).font(Fonts.mono(11)).foregroundStyle(p.fg2).textSelection(.enabled)
            ForEach(pl.notes, id: \.self) { n in
                Text(verbatim: "note: \(n)").font(Fonts.mono(11)).foregroundStyle(p.warn).fixedSize(horizontal: false, vertical: true).textSelection(.enabled)
            }
            if pl.project {
                Text(verbatim: "Comes with this project (.elisa/plugins). Commands and deny / ask rules only.").font(Fonts.mono(11)).foregroundStyle(p.fg2)
            }
            if store.confirmRemove == pl.name {
                HStack(spacing: 8) {
                    Text(verbatim: "Remove \(pl.name)? Its copy in ~/.elisa/plugins and its installed packages are deleted.")
                        .font(Fonts.mono(11)).foregroundStyle(p.tx).fixedSize(horizontal: false, vertical: true)
                    SmallBtn(title: "Remove", primary: true) { store.act("remove", pl.name, m) }
                    SmallBtn(title: "Not now") { store.confirmRemove = nil }
                }
                .padding(8)
                .background(RoundedRectangle(cornerRadius: 6).fill(p.bg2))
                .overlay(RoundedRectangle(cornerRadius: 6).strokeBorder(p.sep, lineWidth: 1))
            }
        }
        .padding(.horizontal, 22).padding(.vertical, 12)
    }

    @ViewBuilder private func actions(_ pl: PluginInfo) -> some View {
        let session = m.sessionOpen
        HStack(spacing: 6) {
            if pl.project {
                if !pl.trusted {
                    SmallBtn(title: "Trust…") { store.act("trust", "project", m) }.disabled(!session).opacity(session ? 1 : 0.5)
                        .help(session ? "Review the project's commands and rules" : "Open a folder to manage plugins")
                }
            } else {
                if !pl.trusted {
                    SmallBtn(title: "Trust…", primary: true) { store.act("trust", pl.name, m) }.disabled(!session).opacity(session ? 1 : 0.5)
                        .help(session ? "Review its files again; it stays off until you trust them" : "Open a folder to manage plugins")
                }
                Toggle("", isOn: Binding(get: { pl.enabled }, set: { store.act($0 ? "enable" : "disable", pl.name, m) }))
                    .toggleStyle(.switch).controlSize(.mini).labelsHidden().tint(p.acc)
                    .disabled(store.working || !pl.trusted)
                    .help(pl.enabled ? "On: click to disable" : "Off: click to enable")
                SmallBtn(title: "Update…") { store.act("update", pl.name, m) }.disabled(!session).opacity(session ? 1 : 0.5)
                    .help(session ? "Read its source folder again; the setup screen shows what changes" : "Open a folder to manage plugins")
                SmallBtn(title: "Remove") { store.confirmRemove = pl.name }.disabled(store.working)
            }
        }
    }

    private func tag(_ t: String, _ c: Color) -> some View {
        Text(verbatim: t).font(Fonts.mono(10.5)).foregroundStyle(c)
            .padding(.horizontal, 5).padding(.vertical, 1)
            .overlay(RoundedRectangle(cornerRadius: 4).strokeBorder(p.sep, lineWidth: 1))
    }

    private func color(_ t: Tone) -> Color { switch t { case .ok: p.ok; case .warn: p.warn; case .err: p.err; case .plain: p.fg2 } }

    private func toolText(_ tools: Int?, _ tokens: Int?) -> String {
        guard let n = tools else { return "tools known once it runs" }
        return "\(n) tool\(n == 1 ? "" : "s")" + (tokens.map { " · ~\(k($0)) tokens" } ?? "")
    }
}

private struct PluginsRoot: View {
    @ObservedObject var m: AppModel
    var body: some View { PluginsView(m: m).environment(\.pal, m.pal).preferredColorScheme(m.dark ? .dark : .light) }
}

@MainActor
final class PluginsWindow {
    static let shared = PluginsWindow()
    private var window: NSWindow?

    func show(_ m: AppModel) {
        if let w = window { w.contentView = NSHostingView(rootView: PluginsRoot(m: m)); Front.user(w); return }
        let w = NSWindow(contentRect: NSRect(x: 0, y: 0, width: 900, height: 600), styleMask: [.titled, .closable, .resizable, .miniaturizable, .fullSizeContentView], backing: .buffered, defer: false)
        w.title = "Plugins"
        w.titlebarAppearsTransparent = true
        w.titleVisibility = .hidden
        w.isReleasedWhenClosed = false
        w.minSize = NSSize(width: 760, height: 420)
        w.contentView = NSHostingView(rootView: PluginsRoot(m: m))
        if !DebugFrame.place(w) { w.center() }
        Front.user(w)
        window = w
    }
}

// MARK: - the setup window (plugin_preview / project_trust)

struct PluginSetupView: View {
    @ObservedObject var store = PluginStore.shared
    @ObservedObject var m: AppModel
    @Environment(\.pal) var p

    /// the yes / no question is open (it follows the preview; none comes when the preview has problems)
    private var asking: Bool { m.approval?.question == true && !pullAsking }
    /// the open question is Elisa's own "download this image?" (the preview lists a missing image)
    private var pullAsking: Bool { m.approval.map { $0.question && $0.tool.hasPrefix("sandbox pull") } ?? false }

    var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            // the title stays out of the scroll view (a scroll view under the title bar drew blank in captures)
            header.padding(.horizontal, 22).padding(.top, 30).padding(.bottom, 12)
            Rectangle().fill(p.sep).frame(height: 1)
            ScrollView {
                VStack(alignment: .leading, spacing: 14) {
                    if let pv = store.preview { preview(pv) }
                    else if let t = store.trust { trust(t) }
                    else { Text(verbatim: "Nothing to set up.").font(Fonts.mono(12)).foregroundStyle(p.fg2) }
                }
                .padding(.horizontal, 22).padding(.top, 14).padding(.bottom, 16)
                .frame(maxWidth: .infinity, alignment: .leading)
            }
            Rectangle().fill(p.sep).frame(height: 1)
            footer.padding(.horizontal, 22).padding(.vertical, 10)
        }
        .background(p.bg)
        .ignoresSafeArea()
        .frame(minWidth: 640, minHeight: 420)
    }

    @ViewBuilder private var footer: some View {
        HStack(spacing: 10) {
            if pullAsking, let a = m.approval {
                Text(verbatim: a.summary).font(Fonts.mono(11)).foregroundStyle(p.tx).lineLimit(3).fixedSize(horizontal: false, vertical: true)
                Spacer()
                SmallBtn(title: "Not now") { m.answer(word: "no") }
                SmallBtn(title: "Download", primary: true) { m.answer(word: "yes"); store.status = ("downloading…", true) }
            } else if let pv = store.preview, !pv.problems.isEmpty {
                Text(verbatim: pv.missingImages.isEmpty ? "Cannot \(pv.verb.lowercased()) now: fix the problems above, then try again."
                     : "Cannot \(pv.verb.lowercased()) now: download the image above (Elisa checks again after it), or try later.").font(Fonts.mono(11)).foregroundStyle(p.warn)
                Spacer()
                SmallBtn(title: "Close") { PluginSetupWindow.shared.close() }
            } else {
                Text(verbatim: asking ? (store.trust != nil ? "Kept in ~/.elisa, never in the repository. Any change asks again." : "Nothing is installed until you say yes.")
                     : "waiting for Elisa…").font(Fonts.mono(11)).foregroundStyle(p.fg2)
                Spacer()
                SmallBtn(title: "Not now") { store.answer(false, m) }.disabled(!asking)
                SmallBtn(title: store.preview?.verb ?? "Trust", primary: true) { store.answer(true, m) }.disabled(!asking).opacity(asking ? 1 : 0.5)
            }
        }
    }

    private func section(_ t: String) -> some View { Pixel(t, size: 9, color: p.fg2).padding(.top, 4) }

    private func line(_ label: String, _ value: String, color: Color? = nil) -> some View {
        HStack(alignment: .firstTextBaseline, spacing: 8) {
            Text(verbatim: label).font(Fonts.mono(11)).foregroundStyle(p.fg2).frame(width: 96, alignment: .leading)
            Text(verbatim: value).font(Fonts.mono(11.5)).foregroundStyle(color ?? p.tx).textSelection(.enabled).fixedSize(horizontal: false, vertical: true)
        }
    }

    private func warnBox<C: View>(_ c: Color, @ViewBuilder _ content: () -> C) -> some View {
        VStack(alignment: .leading, spacing: 4) { content() }
            .padding(.horizontal, 10).padding(.vertical, 7)
            .frame(maxWidth: .infinity, alignment: .leading)
            .background(RoundedRectangle(cornerRadius: 6).fill(c.opacity(0.12)))
            .overlay(RoundedRectangle(cornerRadius: 6).strokeBorder(c.opacity(0.6), lineWidth: 1))
    }

    @ViewBuilder private var header: some View {
        VStack(alignment: .leading, spacing: 6) {
            if let pv = store.preview {
                Pixel(pv.op == "update" ? "Update plugin" : pv.op == "trust" ? "Trust plugin" : "Install plugin", size: 11, color: p.acc)
                HStack(alignment: .firstTextBaseline, spacing: 8) {
                    Text(verbatim: pv.name).font(Fonts.mono(16, .semibold)).foregroundStyle(p.fg)
                    Text(verbatim: pv.oldVersion.map { "\($0) → \(pv.version)" } ?? pv.version).font(Fonts.mono(12)).foregroundStyle(p.fg2)
                }
            } else if let t = store.trust {
                Pixel("Trust this project?", size: 11, color: p.acc)
                Text(verbatim: (t.path as NSString).abbreviatingWithTildeInPath).font(Fonts.mono(14, .semibold)).foregroundStyle(p.fg).textSelection(.enabled)
                    .lineLimit(2).truncationMode(.middle)
            } else {
                Pixel("Plugin setup", size: 11, color: p.acc)
            }
        }
        .frame(maxWidth: .infinity, alignment: .leading)
    }

    @ViewBuilder private func preview(_ pv: PluginPreview) -> some View {
        VStack(alignment: .leading, spacing: 6) {
            if !pv.description.isEmpty { Text(verbatim: pv.description).font(Fonts.mono(12)).foregroundStyle(p.tx) }
            if !pv.source.isEmpty { Text(verbatim: "from \((pv.source as NSString).abbreviatingWithTildeInPath)").font(Fonts.mono(11)).foregroundStyle(p.fg2).textSelection(.enabled) }
        }
        if pv.host {
            warnBox(p.warn) {
                Text(verbatim: "⚠ Runs directly on this Mac").font(Fonts.mono(13, .semibold)).foregroundStyle(p.warn)
                Text(verbatim: "A server below is not in a container. It can read and change anything you can. Elisa installs nothing for it.")
                    .font(Fonts.mono(11.5)).foregroundStyle(p.tx).fixedSize(horizontal: false, vertical: true)
            }
        }
        if !pv.problems.isEmpty {
            warnBox(p.err) {
                Text(verbatim: "Cannot \(pv.verb.lowercased()) now").font(Fonts.mono(13, .semibold)).foregroundStyle(p.err)
                ForEach(pv.problems, id: \.self) { pr in
                    Text(verbatim: pr).font(Fonts.mono(11.5)).foregroundStyle(p.tx).textSelection(.enabled).fixedSize(horizontal: false, vertical: true)
                }
            }
        }
        section(pv.servers.isEmpty ? "Servers: none" : "Servers")
        ForEach(pv.servers, id: \.self) { s in
            VStack(alignment: .leading, spacing: 4) {
                HStack(alignment: .firstTextBaseline, spacing: 8) {
                    Text(verbatim: s.name).font(Fonts.mono(12.5, .semibold)).foregroundStyle(p.code)
                    if s.host {
                        Text(verbatim: "⚠ runs on this Mac, not in a container").font(Fonts.mono(11.5, .semibold)).foregroundStyle(p.warn)
                    } else {
                        Text(verbatim: "runs in a container with no network").font(Fonts.mono(11.5)).foregroundStyle(p.tx)
                    }
                }
                if !s.host {
                    line("package", "\(s.package) \(s.version)")
                    if let i = s.installer { line("installer", i) }
                    if let img = s.image {
                        line("image", img + (s.imagePresent == true ? " (on this machine)" : s.imagePresent == false ? " (not on this machine)" : ""),
                             color: s.imagePresent == false ? p.warn : nil)
                    }
                    if s.imagePresent == false, let img = s.image, pv.missingImages.contains(img) { download(img, pv) }
                    line("mounts", s.mounts.isEmpty ? "none" : s.mounts.map { "\($0.path) (\($0.write ? "read + write" : "read-only"))" }.joined(separator: ", "),
                         color: s.mounts.contains(where: \.write) ? p.warn : nil)
                }
                line("command", s.command.joined(separator: " "))
                line("environment", s.env.isEmpty ? "none" : s.env.joined(separator: ", "))
                line("tools", s.tools)
            }
            .padding(10)
            .frame(maxWidth: .infinity, alignment: .leading)
            .background(RoundedRectangle(cornerRadius: 6).fill(p.bg2))
            .overlay(RoundedRectangle(cornerRadius: 6).strokeBorder(s.host ? p.warn.opacity(0.7) : p.sep, lineWidth: 1))
        }
        section(pv.rules.isEmpty ? "Permission rules: none" : "Permission rules it adds")
        ForEach(pv.rules, id: \.self) { r in
            HStack(alignment: .firstTextBaseline, spacing: 8) {
                Text(verbatim: r.level).font(Fonts.mono(11.5, .semibold)).foregroundStyle(r.level == "allow" ? p.ok : r.level == "deny" ? p.err : p.warn).frame(width: 48, alignment: .leading)
                Text(verbatim: r.pattern).font(Fonts.mono(11.5)).foregroundStyle(p.code).textSelection(.enabled)
                if r.broad { Text(verbatim: "⚠ broad: covers every call of this kind").font(Fonts.mono(11, .semibold)).foregroundStyle(p.warn) }
            }
        }
        if !pv.commands.isEmpty {
            section(pv.commands.contains { !$0.text.isEmpty } ? "Commands (each one's text is sent to the model)" : "Commands")
            ForEach(pv.commands, id: \.self) { c in CommandText(c: c) }
        }
        ForEach(pv.notes, id: \.self) { n in
            Text(verbatim: "note: \(n)").font(Fonts.mono(11)).foregroundStyle(p.warn).fixedSize(horizontal: false, vertical: true)
        }
    }

    /// a missing listed image: "Download image" (the session asks first, or its pending question is answered yes),
    /// its progress, then the preview comes again
    @ObservedObject private var sandbox = SandboxStore.shared

    @ViewBuilder private func download(_ img: String, _ pv: PluginPreview) -> some View {
        HStack(alignment: .firstTextBaseline, spacing: 8) {
            Text(verbatim: "").frame(width: 96, alignment: .leading)
            if let pr = sandbox.pulling[img] {
                Text(verbatim: "downloading \(img) · \(pr)").font(Fonts.mono(11.5)).foregroundStyle(p.acc)
                SmallBtn(title: "Cancel") { sandbox.cancel(m) }
            } else {
                SmallBtn(title: "Download image" + (sandbox.size(img).map { " (~\($0) MB)" } ?? ""), primary: true) { sandbox.downloadForPreview(img, pv, m) }
                    .help("Downloads \(img) into Colima's VM (nothing is installed on this Mac), then checks the plugin again")
                Text(verbatim: "into Colima's VM; nothing is installed on this Mac").font(Fonts.mono(11)).foregroundStyle(p.fg2)
            }
        }
        .onAppear { if sandbox.images.isEmpty { sandbox.refresh(m) } }
    }

    @ViewBuilder private func trust(_ t: ProjectTrust) -> some View {
        VStack(alignment: .leading, spacing: 6) {
            Text(verbatim: "This repository brings its own commands and permission rules. They load only if you trust them.")
                .font(Fonts.mono(12)).foregroundStyle(p.tx).fixedSize(horizontal: false, vertical: true)
        }
        if !t.plugins.isEmpty {
            section("Project plugins")
            Text(verbatim: t.plugins.joined(separator: ", ")).font(Fonts.mono(12)).foregroundStyle(p.code)
        }
        section(t.commands.isEmpty ? "Commands: none" : "Commands")
        ForEach(t.commands, id: \.self) { c in
            HStack(alignment: .firstTextBaseline, spacing: 8) {
                Text(verbatim: c.name).font(Fonts.mono(11.5, .semibold)).foregroundStyle(p.code).frame(minWidth: 96, alignment: .leading)
                Text(verbatim: c.description.isEmpty ? "—" : c.description).font(Fonts.mono(11.5)).foregroundStyle(p.tx)
                Text(verbatim: c.source).font(Fonts.mono(11)).foregroundStyle(p.fg2)
            }
        }
        section(t.rules.isEmpty ? "Permission rules: none" : "Permission rules")
        ForEach(t.rules, id: \.self) { r in
            HStack(alignment: .firstTextBaseline, spacing: 8) {
                Text(verbatim: r.level).font(Fonts.mono(11.5, .semibold)).foregroundStyle(r.level == "deny" ? p.err : p.warn).frame(width: 48, alignment: .leading)
                Text(verbatim: r.pattern).font(Fonts.mono(11.5)).foregroundStyle(p.code).textSelection(.enabled)
                Text(verbatim: r.source).font(Fonts.mono(11)).foregroundStyle(p.fg2)
            }
        }
        section("Full text")
        Text(verbatim: t.text).font(Fonts.mono(11)).foregroundStyle(p.tx).textSelection(.enabled)
            .padding(10).frame(maxWidth: .infinity, alignment: .leading)
            .background(RoundedRectangle(cornerRadius: 6).fill(p.bg2))
    }
}

/// a plugin command in the setup screen: name, arguments and description, and the full prompt it sends to the model,
/// collapsed by default ("show text")
private struct CommandText: View {
    @Environment(\.pal) var p
    let c: PluginPreview.Command
    /// --plugin-commands-open (debug): every command's text shown
    @State private var open = CommandLine.arguments.contains("--plugin-commands-open")
    var body: some View {
        VStack(alignment: .leading, spacing: 4) {
            HStack(alignment: .firstTextBaseline, spacing: 8) {
                Text(verbatim: c.name + (c.args.isEmpty ? "" : " " + c.args)).font(Fonts.mono(11.5, .semibold)).foregroundStyle(p.code)
                    .frame(minWidth: 96, alignment: .leading)
                Text(verbatim: c.description.isEmpty ? "—" : c.description).font(Fonts.mono(11.5)).foregroundStyle(p.tx)
                    .fixedSize(horizontal: false, vertical: true)
                Spacer(minLength: 8)
                if !c.text.isEmpty {
                    let n = c.text.split(separator: "\n", omittingEmptySubsequences: false).count
                    Button { open.toggle() } label: {
                        Text(verbatim: open ? "▾ hide text" : "▸ show text (\(n) line\(n == 1 ? "" : "s"))").font(Fonts.mono(11)).foregroundStyle(p.acc)
                    }
                    .buttonStyle(.plain)
                    .help("The full prompt this command sends to the model")
                }
            }
            if open {
                Text(verbatim: c.text).font(Fonts.mono(11)).foregroundStyle(p.tx).textSelection(.enabled)
                    .fixedSize(horizontal: false, vertical: true)
                    .padding(8).frame(maxWidth: .infinity, alignment: .leading)
                    .background(RoundedRectangle(cornerRadius: 6).fill(p.bg2))
                    .overlay(RoundedRectangle(cornerRadius: 6).strokeBorder(p.sep, lineWidth: 1))
            }
        }
    }
}

private struct PluginSetupRoot: View {
    @ObservedObject var m: AppModel
    var body: some View { PluginSetupView(m: m).environment(\.pal, m.pal).preferredColorScheme(m.dark ? .dark : .light) }
}

@MainActor
final class PluginSetupWindow: NSObject, NSWindowDelegate {
    static let shared = PluginSetupWindow()
    private var window: NSWindow?
    private weak var model: AppModel?
    /// close() in progress: not the user closing it (no "no" answer from windowWillClose)
    private var closing = false

    /// `byEvent`: opened by a session event (plugin_preview, project_trust, the yes / no question): shown without
    /// taking focus (Front.event); else the person asked for it and it comes to the front
    func show(_ m: AppModel, byEvent: Bool = false) {
        model = m
        if let w = window {
            if !w.isVisible { w.contentView = NSHostingView(rootView: PluginSetupRoot(m: m)) }
            // already up: an event leaves it where it is
            if byEvent { if !w.isVisible { Front.event(w) } } else { Front.user(w) }
            return
        }
        let w = NSWindow(contentRect: NSRect(x: 0, y: 0, width: 720, height: 640), styleMask: [.titled, .closable, .resizable, .fullSizeContentView], backing: .buffered, defer: false)
        w.title = "Plugin Setup"
        w.titlebarAppearsTransparent = true
        w.titleVisibility = .hidden
        w.isReleasedWhenClosed = false
        w.minSize = NSSize(width: 640, height: 420)
        w.delegate = self
        w.contentView = NSHostingView(rootView: PluginSetupRoot(m: m))
        if !DebugFrame.place(w) { w.center() }
        if byEvent { Front.event(w) } else { Front.user(w) }
        window = w
    }

    /// the chat's "details" link
    func reopen(_ m: AppModel) {
        if PluginStore.shared.preview != nil || PluginStore.shared.trust != nil { show(m) }
    }

    func close() {
        guard let w = window, w.isVisible else { return }
        closing = true
        w.close()
        closing = false
    }

    func windowWillClose(_ notification: Notification) {
        guard !closing, let m = model else { return }
        PluginStore.shared.setupClosed(m)
    }
}
