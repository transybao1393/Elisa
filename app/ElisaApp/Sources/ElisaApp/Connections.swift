// Model Connections: the window over ~/.config/elisa/providers.toml (elisa/src/provider.rs). Elisa owns the file: this
// window reads it with `elisa --providers-json`, writes it with `--providers-save` and checks a draft with
// `--provider-test -`, so the rules live in one place. A key typed here goes to Elisa on stdin, never on a command line,
// and is never read back: the JSON says only `has_key`.
import AppKit
import SwiftUI

struct Connection: Identifiable, Equatable {
    let id = UUID()
    var name = ""
    /// the saved name, so a rename keeps the saved key
    var was = ""
    var type = "openai"
    var baseURL = ""
    var model = ""
    var keyEnv = ""
    /// typed in this window, not saved yet
    var newKey = ""
    var hasKey = false
    var removeKey = false
    var envSet = false
    var maxCtx = ""
    /// the model can see pictures: nil = Auto (Elisa works it out), true / false = the user says
    var vision: Bool? = nil
    var command = ""
    var url = ""
    var tool = ""
    /// the old per-connection web setting: kept as read so a save does not drop it; Elisa ignores it now (web access
    /// is config.toml `web`, and each search asks in the chat)
    var web = ""
    var searxngURL = ""

    /// nil = stays on this machine; else where the conversation goes (as `Profile::leaves_machine`)
    var leaves: String? {
        if type == "mcp" {
            if !url.isEmpty { return Connection.isLocal(url) ? "the MCP server at \(url)" : Connection.host(url) }
            return "the MCP server \(Connection.split(command).first ?? "")"
        }
        return Connection.isLocal(baseURL) ? nil : Connection.host(baseURL)
    }

    static func host(_ u: String) -> String { URLComponents(string: u)?.host ?? u }
    static func isLocal(_ u: String) -> Bool {
        let h = host(u)
        return h == "localhost" || h == "::1" || h.hasPrefix("127.")
    }

    /// a command line as words: spaces split, single or double quotes keep them
    static func split(_ s: String) -> [String] {
        var out: [String] = [], cur = "", quote: Character?, any = false
        for c in s {
            if let q = quote { if c == q { quote = nil } else { cur.append(c) } }
            else if c == "\"" || c == "'" { quote = c; any = true }
            else if c == " " { if any || !cur.isEmpty { out.append(cur) }; cur = ""; any = false }
            else { cur.append(c) }
        }
        if any || !cur.isEmpty { out.append(cur) }
        return out
    }

    static func join(_ a: [String]) -> String { a.map { $0.contains(" ") || $0.isEmpty ? "\"\($0)\"" : $0 }.joined(separator: " ") }

    init() {}

    init(json j: [String: Any]) {
        name = j["name"] as? String ?? ""
        was = name
        type = j["type"] as? String ?? "openai"
        baseURL = j["base_url"] as? String ?? ""
        model = j["model"] as? String ?? ""
        keyEnv = j["api_key_env"] as? String ?? ""
        hasKey = j["has_key"] as? Bool ?? false
        envSet = j["env_set"] as? Bool ?? false
        maxCtx = (j["max_ctx"] as? NSNumber).map { "\($0.intValue)" } ?? ""
        vision = j["vision"] as? Bool
        command = Connection.join(j["command"] as? [String] ?? [])
        url = j["url"] as? String ?? ""
        tool = j["tool"] as? String ?? ""
        web = j["web"] as? String ?? ""
        searxngURL = j["searxng_url"] as? String ?? ""
    }

    var json: [String: Any] {
        var o: [String: Any] = ["name": name, "was": was, "type": type]
        let mcp = type == "mcp"
        o["base_url"] = mcp ? "" : baseURL
        o["model"] = model
        o["api_key_env"] = keyEnv
        o["command"] = mcp && url.isEmpty ? Connection.split(command) : [String]()
        o["url"] = mcp ? url : ""
        o["tool"] = mcp ? tool : ""
        if let n = Int(maxCtx.trimmingCharacters(in: .whitespaces)) { o["max_ctx"] = n }
        if let v = vision { o["vision"] = v }
        o["web"] = web
        o["searxng_url"] = web == "searxng" ? searxngURL : ""
        if !newKey.isEmpty { o["api_key"] = newKey } else if removeKey { o["api_key"] = "" }
        return o
    }

    struct Preset {
        let label, name, type, base, model, env, ctx, command: String
        /// the dim right-hand column of the Add menu: where it connects
        var hint: String {
            if type == "mcp" { return name == "mcp-remote" ? "streamable HTTP" : "stdio command" }
            let h = base.replacingOccurrences(of: "http://", with: "").replacingOccurrences(of: "https://", with: "")
            let host = h.split(separator: "/").first.map(String.init) ?? ""
            return host.isEmpty ? "your server URL" : host
        }
    }
    static let localPresets = [
        Preset(label: "Streamdense gateway", name: "streamdense", type: "streamdense", base: "http://127.0.0.1:8898", model: "qwen38-27b", env: "", ctx: "", command: ""),
        Preset(label: "Ollama", name: "ollama", type: "openai", base: "http://127.0.0.1:11434/v1", model: "", env: "", ctx: "8192", command: ""),
        Preset(label: "LM Studio", name: "lm-studio", type: "openai", base: "http://127.0.0.1:1234/v1", model: "", env: "", ctx: "8192", command: ""),
        Preset(label: "MLX server", name: "mlx", type: "openai", base: "http://127.0.0.1:8081/v1", model: "", env: "", ctx: "8192", command: ""),
        Preset(label: "llama.cpp server", name: "llama-cpp", type: "openai", base: "http://127.0.0.1:8080/v1", model: "", env: "", ctx: "8192", command: ""),
    ]
    static let cloudPresets = [
        Preset(label: "Anthropic (Claude)", name: "claude", type: "anthropic", base: "https://api.anthropic.com", model: "claude-sonnet-5-5", env: "ANTHROPIC_API_KEY", ctx: "200000", command: ""),
        Preset(label: "OpenAI", name: "openai", type: "openai", base: "https://api.openai.com/v1", model: "", env: "OPENAI_API_KEY", ctx: "131072", command: ""),
        Preset(label: "OpenRouter", name: "openrouter", type: "openai", base: "https://openrouter.ai/api/v1", model: "", env: "OPENROUTER_API_KEY", ctx: "131072", command: ""),
        Preset(label: "Other OpenAI-compatible", name: "custom", type: "openai", base: "https://", model: "", env: "", ctx: "131072", command: ""),
    ]
    /// the context size buttons: (label, max_ctx)
    static let contextSizes = [("8K", "8192"), ("16K", "16384"), ("32K", "32768"), ("64K", "65536"), ("128K", "131072"), ("200K", "200000"), ("1M", "1000000")]

    /// the Add menu's sections, in order
    static let presetSections: [(String, [Preset])] = [("On this machine", localPresets), ("Online", cloudPresets), ("Through MCP", mcpPresets)]
    static let mcpPresets = [
        Preset(label: "MCP server (command)", name: "mcp-local", type: "mcp", base: "", model: "", env: "", ctx: "8192", command: "server --stdio"),
        Preset(label: "MCP server (URL)", name: "mcp-remote", type: "mcp", base: "", model: "", env: "", ctx: "8192", command: ""),
    ]

    init(_ p: Preset) {
        name = p.name; type = p.type; baseURL = p.base; model = p.model; keyEnv = p.env; maxCtx = p.ctx; command = p.command
        if p.name == "mcp-remote" { url = "https://" }
    }
}

@MainActor
final class ConnectionStore: ObservableObject {
    @Published var items: [Connection] = []
    @Published var active = ""
    @Published var selected: UUID?
    @Published var path = "~/.config/elisa/providers.toml"
    @Published var note: (text: String, ok: Bool)?
    @Published var busy = false
    @Published var dirty = false
    /// the Add menu is open, and its highlighted row
    @Published var menuOpen = false
    @Published var menuSel = 0
    var menuItems: [Connection.Preset] { Connection.presetSections.flatMap(\.1) }
    private var saved: [Connection] = []
    private var savedActive = ""

    /// Reads the saved connections. `keep`: the name to select afterwards, else the one selected now (each read makes
    /// new ids, and the file may list the connections in another order, so the selection follows the name, not a
    /// position).
    func load(keep: String? = nil, then: (() -> Void)? = nil) {
        let keep = keep ?? items.first { $0.id == selected }?.name
        run(["--providers-json"], nil) { [weak self] j in
            guard let self else { return }
            if j["ok"] as? Bool == false { self.note = (j["detail"] as? String ?? "cannot read the connections", false); then?(); return }
            self.path = ((j["path"] as? String) ?? self.path)
            self.active = j["active"] as? String ?? ""
            self.items = (j["providers"] as? [[String: Any]] ?? []).map(Connection.init(json:))
            self.saved = self.items
            self.savedActive = self.active
            self.dirty = false
            if let keep, let c = self.items.first(where: { $0.name == keep }) {
                self.selected = c.id
            } else if self.selected == nil || !self.items.contains(where: { $0.id == self.selected }) {
                self.selected = self.items.first(where: { $0.name == self.active })?.id ?? self.items.first?.id
            }
            then?()
        }
    }

    func changed() { dirty = items != saved || active != savedActive }

    func add(_ p: Connection.Preset) {
        var c = Connection(p)
        var n = 2
        while items.contains(where: { $0.name == c.name }) { c.name = "\(p.name)-\(n)"; n += 1 }
        items.append(c)
        selected = c.id
        note = nil
        changed()
    }

    func delete(_ id: UUID) {
        guard let i = items.firstIndex(where: { $0.id == id }) else { return }
        if active == items[i].name { active = "" }
        items.remove(at: i)
        selected = items.first?.id
        note = nil
        changed()
    }

    /// the models a local server has, by its URL (`--provider-models`): what the Model field offers to pick
    struct ServerModels {
        struct Item: Hashable { let name: String; let bytes: Int; let params: String; let vision: Bool? }
        var loading = true
        var server = ""
        var items: [Item] = []
        var error: String?
    }
    @Published var models: [String: ServerModels] = [:]

    /// Asks the connection's server what it has (Ollama's list, else its /models). Not `busy`: the buttons stay usable.
    func listModels(_ c: Connection) {
        guard let bin = ElisaProcess.binary(), let data = try? JSONSerialization.data(withJSONObject: c.json) else { return }
        let url = c.baseURL
        var m = models[url] ?? ServerModels(); m.loading = true; models[url] = m
        DispatchQueue.global(qos: .userInitiated).async {
            let j = Self.call(bin, ["--provider-models"], data)
            DispatchQueue.main.async { [weak self] in
                guard let self else { return }
                var r = ServerModels(loading: false)
                if j["ok"] as? Bool == true {
                    r.server = j["server"] as? String ?? ""
                    r.items = (j["models"] as? [[String: Any]] ?? []).map {
                        ServerModels.Item(name: $0["name"] as? String ?? "?", bytes: ($0["bytes"] as? NSNumber)?.intValue ?? 0,
                                          params: $0["params"] as? String ?? "", vision: $0["vision"] as? Bool)
                    }
                } else {
                    r.error = j["detail"] as? String ?? "no answer"
                }
                self.models[url] = r
            }
        }
    }

    func test(_ c: Connection) {
        note = ("testing \(c.name)…", true)
        run(["--provider-test", "-"], c.json) { [weak self] j in
            self?.note = (j["detail"] as? String ?? "no answer", j["ok"] as? Bool ?? false)
        }
    }

    func save(then: @escaping () -> Void) {
        let body: [String: Any] = ["active": active, "providers": items.map(\.json)]
        run(["--providers-save"], body) { [weak self] j in
            guard let self else { return }
            let ok = j["ok"] as? Bool ?? false
            self.note = (j["detail"] as? String ?? "no answer", ok)
            if ok {
                let (keepName, keepNote) = (self.items.first { $0.id == self.selected }?.name, self.note)
                self.load(keep: keepName) {
                    self.note = keepNote
                    then()
                }
            }
        }
    }

    /// longest a call may take: a test that starts an MCP command can wait 30 s for its setup
    nonisolated private static let runTimeout: TimeInterval = 90

    /// Runs Elisa off the main thread; the last line of its stdout is JSON. The body goes on stdin. Any failure (no
    /// binary, a crash, no JSON, a hang) comes back as {"ok": false, "detail": why}, never as a silent no-op.
    private func run(_ args: [String], _ body: [String: Any]?, done: @escaping ([String: Any]) -> Void) {
        guard let bin = ElisaProcess.binary() else {
            note = ("elisa binary not found: build it (cd elisa && cargo build --release) or set ELISA_BIN", false)
            return
        }
        let data: Data?
        if let body {
            guard let d = try? JSONSerialization.data(withJSONObject: body) else { note = ("cannot encode the connections", false); return }
            data = d
        } else { data = nil }
        busy = true
        DispatchQueue.global(qos: .userInitiated).async {
            let r = Self.call(bin, args, data)
            DispatchQueue.main.async { [weak self] in
                self?.busy = false
                done(r)
            }
        }
    }

    nonisolated static func call(_ bin: URL, _ args: [String], _ data: Data?) -> [String: Any] {
        let p = Process()
        p.executableURL = bin
        p.arguments = args
        let (out, inp, err) = (Pipe(), Pipe(), Pipe())
        p.standardOutput = out
        p.standardInput = inp
        p.standardError = err
        do { try p.run() } catch { return ["ok": false, "detail": "cannot start elisa: \(error.localizedDescription)"] }
        // a hung call (a server that never answers) is stopped rather than leaving the window busy forever
        let timer = DispatchWorkItem { if p.isRunning { p.terminate() } }
        DispatchQueue.global().asyncAfter(deadline: .now() + runTimeout, execute: timer)
        defer { timer.cancel() }
        // stderr is drained alongside stdout so a chatty error can't fill its pipe and stall the process
        nonisolated(unsafe) var errData = Data()
        let errDone = DispatchSemaphore(value: 0)
        DispatchQueue.global().async { errData = err.fileHandleForReading.readDataToEndOfFile(); errDone.signal() }
        if let data { try? inp.fileHandleForWriting.write(contentsOf: data) }  // fails, not crashes, if elisa exited
        try? inp.fileHandleForWriting.close()
        let outData = out.fileHandleForReading.readDataToEndOfFile()
        p.waitUntilExit()
        errDone.wait()
        let lastJSON = String(decoding: outData, as: UTF8.self).split(separator: "\n").last { $0.hasPrefix("{") }
        if let line = lastJSON, let j = try? JSONSerialization.jsonObject(with: Data(line.utf8)) as? [String: Any] { return j }
        if p.terminationReason == .uncaughtSignal && p.terminationStatus == SIGTERM {
            return ["ok": false, "detail": "elisa did not answer within \(Int(runTimeout)) s, so it was stopped"]
        }
        let why = String(decoding: errData, as: UTF8.self).split(separator: "\n").last { !$0.trimmingCharacters(in: .whitespaces).isEmpty }
        if let why { return ["ok": false, "detail": "elisa: \(why)"] }
        return ["ok": false, "detail": "elisa exited (\(p.terminationStatus)) without an answer: is the binary older than this app?"]
    }
}

// MARK: - views

private struct Field<C: View>: View {
    @Environment(\.pal) var p
    let label: String
    var hint = ""
    @ViewBuilder let content: () -> C
    var body: some View {
        VStack(alignment: .leading, spacing: 5) {
            Pixel(label, size: 9, color: p.fg2)
            content()
            if !hint.isEmpty { Text(verbatim: hint).font(Fonts.mono(11.5)).foregroundStyle(p.fg2).fixedSize(horizontal: false, vertical: true) }
        }
    }
}

/// a text field as the app's input box draws one: the accent border shows focus
private struct Input: ViewModifier {
    @Environment(\.pal) var p
    @FocusState private var focus: Bool
    func body(content: Content) -> some View {
        content.textFieldStyle(.plain).font(Fonts.mono(13)).foregroundStyle(p.fg)
            .focused($focus)
            .padding(.horizontal, 10).padding(.vertical, 7)
            .background(RoundedRectangle(cornerRadius: 6).fill(p.inputBg))
            .overlay(RoundedRectangle(cornerRadius: 6).strokeBorder(focus ? p.acc : p.sep, lineWidth: 1))
            .contentShape(Rectangle())
            .onTapGesture { focus = true }
    }
}

private struct Btn: View {
    @Environment(\.pal) var p
    let title: String
    /// the selected look: tint and accent border
    var on = false
    var danger = false
    var enabled = true
    let action: () -> Void
    var body: some View {
        Button(action: action) {
            // one line at any width: the footer's path label gives way instead
            Text(verbatim: title).font(Fonts.mono(13, on ? .semibold : .regular)).lineLimit(1).fixedSize()
                .foregroundStyle(!enabled ? p.fg2 : on ? p.fg : danger ? p.err : p.tx)
                .padding(.horizontal, 12).padding(.vertical, 5)
                .background(RoundedRectangle(cornerRadius: 5).fill(on ? p.sel : .clear))
                .overlay(RoundedRectangle(cornerRadius: 5).strokeBorder(on ? p.acc : p.sep, lineWidth: 1))
                .contentShape(Rectangle())
        }
        .buttonStyle(.plain)
        .disabled(!enabled)
    }
}

/// the Add menu: drawn like the slash menu (`SlashMenu`, Views.swift), with its keys
private struct AddMenu: View {
    @ObservedObject var s: ConnectionStore
    @Environment(\.pal) var p
    @FocusState private var focus: Bool

    var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            ForEach(Connection.presetSections, id: \.0) { title, list in
                Pixel(title, size: 9, color: p.fg2).padding(.horizontal, 8).padding(.top, 8).padding(.bottom, 3)
                ForEach(list, id: \.name) { pr in row(pr) }
            }
            Rectangle().fill(p.sep).frame(height: 1).padding(.top, 6)
            Text(verbatim: "↑↓ move · enter add · esc close").font(Fonts.mono(11)).foregroundStyle(p.fg2).padding(.horizontal, 8).padding(.top, 4)
        }
        .padding(6)
        .frame(width: 470)
        .background(RoundedRectangle(cornerRadius: 8).fill(p.bg2).shadow(color: .black.opacity(0.4), radius: 16, y: 12))
        .overlay(RoundedRectangle(cornerRadius: 8).strokeBorder(p.sep, lineWidth: 1))
        .focusable().focusEffectDisabled().focused($focus)
        .onAppear { focus = true }
        .onKeyPress(phases: .down) { k in
            let n = s.menuItems.count
            switch k.key {
            case .upArrow: s.menuSel = (s.menuSel + n - 1) % n
            case .downArrow: s.menuSel = (s.menuSel + 1) % n
            case .return: pick(s.menuItems[s.menuSel])
            case .escape: s.menuOpen = false
            default: return .ignored
            }
            return .handled
        }
    }

    private func row(_ pr: Connection.Preset) -> some View {
        let i = s.menuItems.firstIndex { $0.name == pr.name } ?? 0
        return HStack(spacing: 14) {
            Text(verbatim: pr.label).font(Fonts.mono(12.5, .medium)).foregroundStyle(i == s.menuSel ? p.acc : p.fg).frame(width: 196, alignment: .leading)
            Text(verbatim: pr.hint).font(Fonts.mono(12.5)).foregroundStyle(p.fg2).lineLimit(1)
            Spacer(minLength: 0)
        }
        .padding(.horizontal, 8).padding(.vertical, 3)
        .background(RoundedRectangle(cornerRadius: 4).fill(i == s.menuSel ? p.sel : .clear))
        .contentShape(Rectangle())
        .onHover { if $0 { s.menuSel = i } }
        .onTapGesture { pick(pr) }
    }

    private func pick(_ pr: Connection.Preset) {
        s.menuOpen = false
        s.add(pr)
    }
}

/// follows the theme toggle while the window is open
private struct ConnectionsRoot: View {
    @ObservedObject var s: ConnectionStore
    @ObservedObject var m: AppModel
    var body: some View {
        ConnectionsView(s: s, m: m).environment(\.pal, m.pal).preferredColorScheme(m.dark ? .dark : .light)
    }
}

/// which connection Elisa uses: filled in the accent colour, else an outline
private struct ActiveDot: View {
    @Environment(\.pal) var p
    let on: Bool
    var body: some View {
        ZStack {
            if on { Circle().fill(p.acc) } else { Circle().strokeBorder(p.gut, lineWidth: 1.5) }
        }
        .frame(width: 9, height: 9)
        .frame(width: 16, height: 22)
        .contentShape(Rectangle())
    }
}

struct ConnectionsView: View {
    @ObservedObject var s: ConnectionStore
    @ObservedObject var m: AppModel
    @ObservedObject private var evals = EvalStore.shared
    @Environment(\.pal) var p
    /// the connection whose Custom size button was pressed (a preset number still shows its field)
    @State private var customCtx: UUID?

    var body: some View {
        HStack(spacing: 0) {
            sidebar.frame(width: 268).background(p.bg2)
            Rectangle().fill(p.sep).frame(width: 1)
            VStack(spacing: 0) {
                detail.padding(.horizontal, 24).padding(.top, 30).padding(.bottom, 16).frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .topLeading)
                Rectangle().fill(p.sep).frame(height: 1)
                footer.padding(.horizontal, 24).padding(.vertical, 14)
            }
            .background(p.bg)
        }
        .overlay {
            if s.menuOpen {
                Color.black.opacity(0.001).onTapGesture { s.menuOpen = false }
                AddMenu(s: s).padding(.leading, 12).padding(.bottom, 54).frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .bottomLeading)
            }
        }
        .ignoresSafeArea().frame(minWidth: 820, minHeight: 840)
    }

    private var sidebar: some View {
        VStack(alignment: .leading, spacing: 0) {
            Pixel("Model connections", size: 11, color: p.acc).padding(.top, 34).padding(.bottom, 4).padding(.horizontal, 16)
            HStack(spacing: 5) {
                Text(verbatim: "Elisa uses the one marked").font(Fonts.mono(11.5)).foregroundStyle(p.fg2)
                Circle().fill(p.acc).frame(width: 7, height: 7)
            }
            .padding(.horizontal, 16).padding(.bottom, 14)
            ScrollView {
                VStack(alignment: .leading, spacing: 2) {
                    defaultRow
                    ForEach(s.items) { c in row(c) }
                }
                .padding(.horizontal, 8)
            }
            Spacer(minLength: 0)
            Btn(title: "+ Add connection", on: s.menuOpen) { s.menuSel = 0; s.menuOpen.toggle() }.padding(12)
        }
    }

    private var defaultRow: some View {
        Button { s.active = ""; s.changed() } label: {
            HStack(spacing: 8) {
                ActiveDot(on: s.active.isEmpty)
                VStack(alignment: .leading, spacing: 2) {
                    Text(verbatim: "config.toml").font(Fonts.mono(13)).foregroundStyle(p.tx)
                    Text(verbatim: "no saved connection").font(Fonts.mono(11)).foregroundStyle(p.fg2)
                }
                Spacer(minLength: 0)
            }
            .padding(.horizontal, 8).padding(.vertical, 6).contentShape(Rectangle())
        }
        .buttonStyle(.plain)
    }

    private func row(_ c: Connection) -> some View {
        let on = s.selected == c.id, act = s.active == c.name && !c.name.isEmpty
        return HStack(spacing: 8) {
            Button { s.active = c.name; s.changed() } label: {
                ActiveDot(on: act)
            }
            .buttonStyle(.plain)
            VStack(alignment: .leading, spacing: 2) {
                Text(verbatim: c.name.isEmpty ? "(unnamed)" : c.name).font(Fonts.mono(13)).foregroundStyle(on ? p.acc : p.tx).lineLimit(1)
                Text(verbatim: (c.leaves == nil ? "this machine" : c.type == "mcp" ? "through MCP" : "online")).font(Fonts.mono(11)).foregroundStyle(c.leaves == nil ? p.ok : p.warn)
                if let score = evals.scoreLine(c.model) {
                    Text(verbatim: score).font(Fonts.mono(10.5)).foregroundStyle(p.code).lineLimit(1)
                }
            }
            Spacer(minLength: 0)
        }
        .padding(.horizontal, 8).padding(.vertical, 6)
        .background(RoundedRectangle(cornerRadius: 4).fill(on ? p.sel : .clear))
        .contentShape(Rectangle())
        .onTapGesture { s.selected = c.id; s.note = nil }
    }

    private func bind<T>(_ c: Connection, _ k: WritableKeyPath<Connection, T>) -> Binding<T> {
        Binding(get: { s.items.first { $0.id == c.id }?[keyPath: k] ?? c[keyPath: k] },
                set: { v in
                    guard let i = s.items.firstIndex(where: { $0.id == c.id }) else { return }
                    let wasActive = s.active == s.items[i].name
                    s.items[i][keyPath: k] = v
                    if wasActive { s.active = s.items[i].name }
                    s.changed()
                })
    }

    @ViewBuilder private var detail: some View {
        if let c = s.items.first(where: { $0.id == s.selected }) {
            VStack(alignment: .leading, spacing: 18) {
                HStack(alignment: .top) {
                    Text(verbatim: c.name.isEmpty ? "New connection" : c.name).font(Fonts.mono(15, .semibold)).foregroundStyle(p.fg)
                    Spacer()
                    privacy(c)
                }
                Field(label: "Name") { TextField("", text: bind(c, \.name)).modifier(Input()) }
                Field(label: "How it connects", hint: kindHint(c.type)) {
                    HStack(spacing: 4) {
                        ForEach([("streamdense", "Streamdense"), ("anthropic", "Anthropic API"), ("openai", "OpenAI API"), ("mcp", "MCP")], id: \.0) { k, label in
                            Btn(title: label, on: c.type == k) { bind(c, \.type).wrappedValue = k }
                        }
                    }
                }
                if c.type == "mcp" { mcpFields(c) } else {
                    Field(label: "Server URL", hint: c.type == "openai" ? "Through the version path; Elisa adds /chat/completions." : "") {
                        TextField("", text: bind(c, \.baseURL)).modifier(Input())
                    }
                }
                if c.type == "mcp" {
                    HStack(alignment: .top, spacing: 14) {
                        Field(label: "Model (optional)") { TextField("", text: bind(c, \.model)).modifier(Input()) }
                        Field(label: "Tool (optional)", hint: "Blank = the server's only tool.") { TextField("", text: bind(c, \.tool)).modifier(Input()) }
                    }
                } else {
                    Field(label: "Model") { TextField("", text: bind(c, \.model)).modifier(Input()) }
                    if c.type == "openai" && Connection.isLocal(c.baseURL) { localModels(c) }
                }
                measured(c)
                contextField(c)
                visionField(c)
                keyFields(c)
            }
        } else {
            VStack(alignment: .leading, spacing: 8) {
                Text(verbatim: "config.toml").font(Fonts.mono(15, .semibold)).foregroundStyle(p.fg)
                Text(verbatim: "With no saved connection, Elisa uses base_url and model from config.toml. By default that is the Streamdense gateway at http://127.0.0.1:8898. Add a connection for another local server, an online API or an MCP server.").font(Fonts.mono(13)).foregroundStyle(p.tx).fixedSize(horizontal: false, vertical: true)
            }
        }
    }

    /// what was measured on this Mac with this connection's model (the scorecard), and a Test for it
    private func measured(_ c: Connection) -> some View {
        let saved = !s.dirty && !c.was.isEmpty && c.was == c.name
        return Field(label: "Measured on this Mac") {
            HStack(spacing: 10) {
                if let score = evals.scoreLine(c.model) {
                    Text(verbatim: "\(c.model): \(score)").font(Fonts.mono(12)).foregroundStyle(p.tx)
                } else {
                    Text(verbatim: c.model.isEmpty ? "no model set" : "\(c.model): not tested yet").font(Fonts.mono(12)).foregroundStyle(p.fg2)
                }
                Spacer(minLength: 0)
                Button { ScorecardWindow.shared.show(m) } label: { Text(verbatim: "Scorecard ›").font(Fonts.mono(11.5)).foregroundStyle(p.acc) }.buttonStyle(.plain)
            }
            EvalControl(target: .connection(c), blocked: c.model.isEmpty ? "Set a model to test it." : saved ? nil : "Save the connection to test it.")
        }
        .onAppear { evals.loadOnce() }
    }

    /// the models on this machine's server, to pick from: asked when the connection is shown and when its URL changes
    private func localModels(_ c: Connection) -> some View {
        let m = s.models[c.baseURL]
        let place = m?.server == "ollama" ? "Ollama" : "the server"
        return VStack(alignment: .leading, spacing: 6) {
            HStack(spacing: 8) {
                Text(verbatim: "ON THIS MACHINE").font(Fonts.mono(10.5, .medium)).foregroundStyle(p.fg2)
                Group {
                    if m == nil || m?.loading == true { Text(verbatim: "asking \(place)…") }
                    else if let e = m?.error { Text(verbatim: e).foregroundStyle(p.warn) }
                    else if let m, m.items.isEmpty { Text(verbatim: m.server == "ollama" ? "Ollama has no models yet: ollama pull <model>" : "the server lists no models") }
                    else if let m { Text(verbatim: "\(m.items.count) model\(m.items.count == 1 ? "" : "s") on \(place) · click one to use it") }
                }
                .font(Fonts.mono(11.5)).foregroundStyle(p.fg2).lineLimit(2).fixedSize(horizontal: false, vertical: true)
                Spacer(minLength: 0)
                Button { s.listModels(c) } label: { Text(verbatim: "↻ refresh").font(Fonts.mono(11.5)).foregroundStyle(p.acc) }
                    .buttonStyle(.plain).disabled(m?.loading == true)
            }
            if let items = m?.items, !items.isEmpty {
                ScrollView {
                    VStack(spacing: 2) { ForEach(items, id: \.self) { modelRow(c, $0) } }
                }
                .frame(maxHeight: min(CGFloat(items.count) * 34, 172))
                .padding(4)
                .background(RoundedRectangle(cornerRadius: 8).fill(p.bg2))
            }
        }
        // a short wait so typing a URL does not ask after every letter
        .task(id: "\(c.id)|\(c.baseURL)") {
            try? await Task.sleep(nanoseconds: 450_000_000)
            if !Task.isCancelled { s.listModels(c) }
        }
    }

    private func modelRow(_ c: Connection, _ it: ConnectionStore.ServerModels.Item) -> some View {
        let on = c.model == it.name
        return Button { bind(c, \.model).wrappedValue = it.name } label: {
            HStack(spacing: 10) {
                Circle().strokeBorder(on ? p.acc : p.fg2.opacity(0.6), lineWidth: 1.5)
                    .background(Circle().fill(on ? p.acc : .clear).padding(3))
                    .frame(width: 13, height: 13)
                Text(verbatim: it.name).font(Fonts.mono(12.5, on ? .semibold : .regular)).foregroundStyle(on ? p.fg : p.tx).lineLimit(1)
                if it.vision == true {
                    Text(verbatim: "vision").font(Fonts.mono(10.5)).foregroundStyle(p.code)
                        .padding(.horizontal, 6).padding(.vertical, 1)
                        .overlay(RoundedRectangle(cornerRadius: 4).strokeBorder(p.code.opacity(0.6), lineWidth: 1))
                }
                Spacer(minLength: 8)
                Text(verbatim: [it.params, it.bytes > 0 ? ByteCountFormatter.string(fromByteCount: Int64(it.bytes), countStyle: .file) : ""].filter { !$0.isEmpty }.joined(separator: " · "))
                    .font(Fonts.mono(11.5)).foregroundStyle(p.fg2)
            }
            .padding(.horizontal, 10).padding(.vertical, 7)
            .background(RoundedRectangle(cornerRadius: 6).fill(on ? p.sel : .clear))
            .contentShape(Rectangle())
        }
        .buttonStyle(.plain)
    }

    private func kindHint(_ t: String) -> String {
        switch t {
        case "streamdense": "The local gateway, with prompt reuse, warm-ups and the watcher."
        case "anthropic": "Anthropic's Messages API, or a server that speaks it."
        case "openai": "Chat completions: Ollama, llama.cpp, LM Studio, MLX, OpenAI, OpenRouter and others."
        default: "A model served by an MCP tool. The reply arrives whole. Without a messages and tools argument it is chat only: no file tools."
        }
    }

    private func privacy(_ c: Connection) -> some View {
        let leaves = c.leaves
        return VStack(alignment: .trailing, spacing: 3) {
            Pixel(leaves == nil ? "On this machine" : "Leaves this machine", size: 9, color: leaves == nil ? p.ok : p.warn)
            if let h = leaves { Text(verbatim: "code Elisa reads goes to \(h)").font(Fonts.mono(11.5)).foregroundStyle(p.fg2).lineLimit(2).multilineTextAlignment(.trailing) }
        }
        .frame(maxWidth: 300, alignment: .trailing)
    }

    @ViewBuilder private func mcpFields(_ c: Connection) -> some View {
        Field(label: "Server URL", hint: "Streamable HTTP. Leave blank to start a local command instead.") { TextField("", text: bind(c, \.url)).modifier(Input()) }
        if c.url.isEmpty {
            Field(label: "Command", hint: "Runs as you, with your files. Only use a server you trust. Stdio; a key is passed as MCP_API_KEY.") {
                TextField("", text: bind(c, \.command)).modifier(Input())
            }
        }
    }

    /// the context window: a size button, Auto (the gateway says), or Custom with a number
    private func contextField(_ c: Connection) -> some View {
        let isPreset = Connection.contextSizes.contains { $0.1 == c.maxCtx }
        let custom = customCtx == c.id || (!c.maxCtx.isEmpty && !isPreset)
        return Field(label: "Context size", hint: contextHint(c.type)) {
            HStack(spacing: 4) {
                if c.type == "streamdense" {
                    Btn(title: "Auto", on: c.maxCtx.isEmpty && !custom) { customCtx = nil; bind(c, \.maxCtx).wrappedValue = "" }
                }
                ForEach(Connection.contextSizes, id: \.1) { label, value in
                    Btn(title: label, on: c.maxCtx == value && !custom) { customCtx = nil; bind(c, \.maxCtx).wrappedValue = value }
                }
                Btn(title: "Custom", on: custom) { customCtx = c.id }
            }
            if custom {
                HStack(spacing: 8) {
                    TextField("", text: Binding(get: { bind(c, \.maxCtx).wrappedValue }, set: { bind(c, \.maxCtx).wrappedValue = $0.filter(\.isNumber) }))
                        .modifier(Input()).frame(width: 140)
                    Text(verbatim: "tokens").font(Fonts.mono(11.5)).foregroundStyle(p.fg2)
                }
                .padding(.top, 3)
            }
        }
    }

    /// can the model see pictures: Auto (Elisa asks / knows), or the user says yes or no
    private func visionField(_ c: Connection) -> some View {
        let auto: String = switch c.type {
        case "anthropic": "Auto: Claude models can see images."
        case "openai": "Auto asks Ollama; other servers count as no. On enables the image button."
        default: "Auto counts as no for this type."
        }
        return Field(label: "Images (vision)", hint: auto) {
            HStack(spacing: 4) {
                Btn(title: "Auto", on: c.vision == nil) { bind(c, \.vision).wrappedValue = nil }
                Btn(title: "On", on: c.vision == true) { bind(c, \.vision).wrappedValue = true }
                Btn(title: "Off", on: c.vision == false) { bind(c, \.vision).wrappedValue = false }
            }
        }
    }

    private func contextHint(_ t: String) -> String {
        switch t {
        case "streamdense": "Auto uses the gateway's size. The gateway sets the real limit: to go higher, restart it with a larger --max-ctx."
        case "anthropic": "Match the model's window."
        default: "Elisa plans around this size but does not change the server's. Match what the model was loaded with."
        }
    }

    private func keyFields(_ c: Connection) -> some View {
        HStack(alignment: .top, spacing: 14) {
            Field(label: "Key from environment variable", hint: keyEnvHint(c)) { TextField("", text: bind(c, \.keyEnv)).modifier(Input()) }
            Field(label: "Or key saved in the file", hint: c.hasKey && !c.removeKey && c.newKey.isEmpty ? "A key is saved. Type to replace it." : "Stored in plain text in a private file (mode 600).") {
                HStack(spacing: 6) {
                    SecureField("", text: bind(c, \.newKey)).modifier(Input())
                    if c.hasKey && !c.removeKey && c.newKey.isEmpty { Btn(title: "Remove", danger: true) { bind(c, \.removeKey).wrappedValue = true } }
                }
            }
        }
    }

    private func keyEnvHint(_ c: Connection) -> String {
        if c.keyEnv.isEmpty { return "Preferred: the key stays out of the file." }
        return c.envSet ? "Set in this app's environment." : "Set it where you start Elisa; apps opened from the Dock do not see your shell's variables."
    }

    private var footer: some View {
        VStack(alignment: .leading, spacing: 10) {
            if let n = s.note {
                Text(verbatim: n.text).font(Fonts.mono(12.5)).foregroundStyle(n.ok ? p.ok : p.err).fixedSize(horizontal: false, vertical: true)
                    .textSelection(.enabled).frame(maxWidth: .infinity, alignment: .leading)
            }
            HStack(spacing: 8) {
                if let c = s.items.first(where: { $0.id == s.selected }) {
                    Btn(title: "Test connection") { s.test(c) }
                    Btn(title: "Delete", danger: true) { s.delete(c.id) }
                }
                Spacer()
                Text(verbatim: pathLabel).font(Fonts.mono(11)).foregroundStyle(p.fg2).lineLimit(1).truncationMode(.head)
                Btn(title: reconnects ? "Save and reconnect" : "Save", on: s.dirty, enabled: s.dirty && !s.busy) {
                    // with no session open, the next one starts on the connection just saved (not this window's last model)
                    s.save { if reconnects { m.reconnect() } else { m.model = "" } }
                }
            }
        }
    }

    private var pathLabel: String { (s.path as NSString).abbreviatingWithTildeInPath }
    /// a session restarts on the new connection and resumes its history
    private var reconnects: Bool { m.folder != nil }
}

@MainActor
final class ConnectionsWindow {
    static let shared = ConnectionsWindow()
    private var window: NSWindow?
    /// the window's store (internal so --selftest can press its buttons through the same functions)
    let store = ConnectionStore()

    private func view(_ m: AppModel) -> some View {
        ConnectionsRoot(s: store, m: m)
    }

    /// openAdd: with the Add connection menu open on this preset (the welcome screen's "Add a local model" → ollama,
    /// "Connect an API" → claude)
    func show(_ m: AppModel, select: String? = nil, openAdd: String? = nil) {
        store.load()
        if let a = openAdd {
            DispatchQueue.main.asyncAfter(deadline: .now() + 0.4) {
                self.store.menuSel = self.store.menuItems.firstIndex { $0.name == a } ?? 0
                self.store.menuOpen = true
            }
        }
        if let select { DispatchQueue.main.asyncAfter(deadline: .now() + 1) { if let c = self.store.items.first(where: { $0.name == select }) { self.store.selected = c.id } } }
        if let w = window { w.contentView = NSHostingView(rootView: view(m)); Front.user(w); return }
        let w = NSWindow(contentRect: NSRect(x: 0, y: 0, width: 880, height: 880), styleMask: [.titled, .closable, .resizable, .miniaturizable, .fullSizeContentView], backing: .buffered, defer: false)
        w.title = "Model Connections"
        w.titlebarAppearsTransparent = true
        w.titleVisibility = .hidden
        w.isReleasedWhenClosed = false
        w.minSize = NSSize(width: 820, height: 840)
        w.contentView = NSHostingView(rootView: view(m))
        if !DebugFrame.place(w) { w.center() }
        Front.user(w)
        window = w
    }

    /// `--connections-shot FILE [--connections-select NAME]`: the window as a PNG (offscreen), then quit.
    func shot(_ m: AppModel, to file: String, select: String?) {
        show(m)
        DispatchQueue.main.asyncAfter(deadline: .now() + 1.5) {
            if let select, let c = self.store.items.first(where: { $0.name == select }) { self.store.selected = c.id }
            if CommandLine.arguments.contains("--connections-add-open") { self.store.menuOpen = true; self.store.menuSel = 1 }
            // long enough for the local-model list (a 0.45 s wait, then the server's answer)
            DispatchQueue.main.asyncAfter(deadline: .now() + 2.5) {
                guard let v = self.window?.contentView, let rep = NSBitmapImageRep(bitmapDataPlanes: nil, pixelsWide: Int(v.bounds.width * 2), pixelsHigh: Int(v.bounds.height * 2), bitsPerSample: 8, samplesPerPixel: 4, hasAlpha: true, isPlanar: false, colorSpaceName: .deviceRGB, bytesPerRow: 0, bitsPerPixel: 0) else { NSApp.terminate(nil); return }
                rep.size = v.bounds.size
                v.cacheDisplay(in: v.bounds, to: rep)
                if let d = rep.representation(using: .png, properties: [:]) { try? d.write(to: URL(fileURLWithPath: file)); print("shot: \(file)") }
                NSApp.terminate(nil)
            }
        }
    }
}
