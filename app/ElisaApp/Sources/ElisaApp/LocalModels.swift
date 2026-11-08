// The welcome screen's local model list: models Ollama can download that call tools (Elisa needs tools), with their
// download size, what they are good at and whether they fit this Mac's memory, plus what is installed already.
// Download = Ollama's own `POST /api/pull` (streamed progress, cancellable); Use = an Ollama connection with that
// model, saved and made active (the same file Model Connections writes).
import AppKit
import SwiftUI

/// one model in the list; `gb` = download size (registry manifests, 2026-11)
struct LocalModel: Identifiable, Hashable {
    let name: String
    let gb: Double
    let note: String
    var tags: [String] = []
    var id: String { name }

    /// tool-calling models in Ollama's library, smallest first
    static let catalog: [LocalModel] = [
        LocalModel(name: "qwen3:1.7b", gb: 1.36, note: "Smallest. Quick answers, light edits.", tags: ["thinking"]),
        LocalModel(name: "qwen3-vl:2b-instruct", gb: 1.93, note: "Small and fast, sees images.", tags: ["vision"]),
        LocalModel(name: "llama3.2:3b", gb: 2.02, note: "Meta's small model. Fast, plain answers."),
        LocalModel(name: "qwen3:4b", gb: 2.5, note: "Light all-rounder; runs on 8–16 GB Macs.", tags: ["thinking"]),
        LocalModel(name: "qwen3-vl:4b", gb: 3.34, note: "Reads screenshots and diagrams.", tags: ["vision"]),
        LocalModel(name: "qwen2.5-coder:7b", gb: 4.68, note: "Tuned for code.", tags: ["coding"]),
        LocalModel(name: "qwen3:8b", gb: 5.23, note: "Solid all-rounder on 16 GB+.", tags: ["thinking"]),
        LocalModel(name: "qwen3-vl:8b", gb: 6.19, note: "Stronger vision model.", tags: ["vision"]),
        LocalModel(name: "qwen3:14b", gb: 9.28, note: "Noticeably smarter; wants 24 GB+.", tags: ["thinking"]),
        LocalModel(name: "gpt-oss:20b", gb: 13.79, note: "OpenAI's open model; strong reasoning, 32 GB+.", tags: ["thinking"]),
        LocalModel(name: "qwen3-coder:30b", gb: 18.56, note: "Larger coding model; 32 GB+.", tags: ["coding"]),
    ]

    /// this Mac's memory in GB
    static let ramGB = Double(ProcessInfo.processInfo.physicalMemory) / 1_073_741_824
    /// runs comfortably: weights plus room for the context and the rest of the Mac
    var fits: Bool { gb * 1.3 + 6 <= LocalModel.ramGB }
}

@MainActor
final class OllamaHub: ObservableObject {
    struct Pull { var done: Int64 = 0; var total: Int64 = 0; var status = "starting…"; var error: String? }
    /// nil = not checked yet
    @Published var reachable: Bool?
    @Published var installed: [String] = []
    @Published var pulls: [String: Pull] = [:]
    var base = URL(string: "http://127.0.0.1:11434")!
    private var tasks: [String: Task<Void, Never>] = [:]

    static let appURL = URL(fileURLWithPath: "/Applications/Ollama.app")
    var appInstalled: Bool { FileManager.default.fileExists(atPath: Self.appURL.path) }

    func refresh() {
        var r = URLRequest(url: base.appendingPathComponent("api/tags")); r.timeoutInterval = 3
        Task {
            do {
                let (d, _) = try await URLSession.shared.data(for: r)
                let j = try JSONSerialization.jsonObject(with: d) as? [String: Any]
                installed = (j?["models"] as? [[String: Any]] ?? []).compactMap { $0["name"] as? String }
                reachable = true
            } catch { reachable = false }
        }
    }

    func has(_ name: String) -> Bool {
        installed.contains(name) || (!name.contains(":") && installed.contains(name + ":latest"))
    }

    /// `POST /api/pull`, streamed: one JSON line per update, per layer (digest) its total / completed
    func pull(_ name: String) {
        guard tasks[name] == nil else { return }
        pulls[name] = Pull()
        tasks[name] = Task {
            var layers: [String: (Int64, Int64)] = [:]
            do {
                var r = URLRequest(url: base.appendingPathComponent("api/pull"))
                r.httpMethod = "POST"; r.timeoutInterval = 3600
                r.setValue("application/json", forHTTPHeaderField: "Content-Type")
                r.httpBody = try JSONSerialization.data(withJSONObject: ["model": name, "stream": true])
                let (bytes, resp) = try await URLSession.shared.bytes(for: r)
                if let h = resp as? HTTPURLResponse, h.statusCode != 200 { throw NSError(domain: "ollama", code: h.statusCode, userInfo: [NSLocalizedDescriptionKey: "Ollama answered \(h.statusCode)"]) }
                for try await line in bytes.lines {
                    guard let j = (try? JSONSerialization.jsonObject(with: Data(line.utf8))) as? [String: Any] else { continue }
                    if let e = j["error"] as? String { throw NSError(domain: "ollama", code: 1, userInfo: [NSLocalizedDescriptionKey: e]) }
                    let st = j["status"] as? String ?? ""
                    if let dg = j["digest"] as? String, let t = (j["total"] as? NSNumber)?.int64Value {
                        layers[dg] = ((j["completed"] as? NSNumber)?.int64Value ?? layers[dg]?.0 ?? 0, t)
                    }
                    var p = pulls[name] ?? Pull()
                    p.done = layers.values.reduce(0) { $0 + $1.0 }; p.total = layers.values.reduce(0) { $0 + $1.1 }
                    p.status = st.hasPrefix("pulling") ? "downloading" : st
                    pulls[name] = p
                    if st == "success" { break }
                }
                pulls[name] = nil
                refresh()
            } catch is CancellationError {
                pulls[name] = nil
            } catch {
                if Task.isCancelled { pulls[name] = nil } else { pulls[name]?.error = error.localizedDescription }
            }
            tasks[name] = nil
        }
    }

    func cancel(_ name: String) { tasks[name]?.cancel(); tasks[name] = nil; pulls[name] = nil }
    func dismissError(_ name: String) { pulls[name] = nil }
}

/// the list itself: Ollama's state on top, then installed models outside the catalog, then the catalog
struct LocalModelPicker: View {
    @ObservedObject var hub: OllamaHub
    /// the model the active connection uses ("" = none)
    let current: String
    let use: (String) -> Void
    let otherServer: () -> Void
    @Environment(\.pal) var p
    @State private var all = false

    var body: some View {
        VStack(alignment: .leading, spacing: 10) {
            HStack(spacing: 8) {
                Image(systemName: "arrow.down.circle").font(.system(size: 13, weight: .medium)).foregroundStyle(p.acc)
                Text("Pick a model to run on this Mac").font(Fonts.mono(13, .semibold)).foregroundStyle(p.fg)
                Spacer()
                Text(String(format: "this Mac: %.0f GB memory", LocalModel.ramGB)).font(Fonts.mono(11)).foregroundStyle(p.fg2)
            }
            switch hub.reachable {
            case nil:
                Text("looking for Ollama…").font(Fonts.mono(11.5)).foregroundStyle(p.fg2)
            case false?:
                offline
            case true?:
                list
            }
            HStack(spacing: 4) {
                Text("Using LM Studio, llama.cpp or MLX instead?").font(Fonts.mono(11)).foregroundStyle(p.fg2)
                Button(action: otherServer) { Text("Add that server ›").font(Fonts.mono(11, .semibold)).foregroundStyle(p.acc) }.buttonStyle(.plain)
            }
            .padding(.top, 2)
        }
        .padding(14)
        .frame(maxWidth: .infinity, alignment: .leading)
        .background(RoundedRectangle(cornerRadius: 8).fill(p.bg))
        .overlay(RoundedRectangle(cornerRadius: 8).strokeBorder(p.sep, lineWidth: 1))
        .onAppear { hub.refresh() }
    }

    private var offline: some View {
        VStack(alignment: .leading, spacing: 8) {
            Text(hub.appInstalled ? "Ollama is installed but not running. Open it, then check again."
                                  : "Elisa downloads and runs local models through Ollama, a free app. Install it, open it, then check again.")
                .font(Fonts.mono(11.5)).foregroundStyle(p.tx).fixedSize(horizontal: false, vertical: true)
            HStack(spacing: 8) {
                if hub.appInstalled {
                    SmallBtn(title: "Open Ollama", primary: true) {
                        NSWorkspace.shared.openApplication(at: OllamaHub.appURL, configuration: .init()) { _, _ in }
                        DispatchQueue.main.asyncAfter(deadline: .now() + 3) { hub.refresh() }
                    }
                } else {
                    SmallBtn(title: "Get Ollama ↗", primary: true) { NSWorkspace.shared.open(URL(string: "https://ollama.com/download")!) }
                }
                SmallBtn(title: "Check again") { hub.reachable = nil; hub.refresh() }
            }
        }
    }

    private var list: some View {
        let extra = hub.installed.filter { n in !LocalModel.catalog.contains { $0.name == n } && !n.contains("embed") }
            .map { LocalModel(name: $0, gb: 0, note: "Installed on this Mac.") }
        // installed first, then by size, smallest first (the user chooses; nothing is picked for them)
        let cat = LocalModel.catalog.sorted { a, b in
            let k = { (x: LocalModel) in hub.has(x.name) ? 0 : 1 }
            let ka = k(a), kb = k(b)
            return ka != kb ? ka < kb : a.gb < b.gb
        }
        let rows = extra + cat
        let shown = all ? rows : Array(rows.prefix(max(5, extra.count + 3)))
        return VStack(alignment: .leading, spacing: 2) {
            ForEach(shown) { row($0) }
            if rows.count > shown.count || all {
                Button(action: { withAnimation(.easeOut(duration: 0.2)) { all.toggle() } }) {
                    Text(all ? "Show fewer" : "Show all \(rows.count) models").font(Fonts.mono(11, .semibold)).foregroundStyle(p.acc)
                        .padding(.horizontal, 8).padding(.vertical, 4)
                }
                .buttonStyle(.plain)
            }
        }
    }

    @ViewBuilder private func row(_ mdl: LocalModel) -> some View {
        let have = hub.has(mdl.name)
        let inUse = !current.isEmpty && (current == mdl.name || current == mdl.name + ":latest")
        let pull = hub.pulls[mdl.name]
        VStack(alignment: .leading, spacing: 5) {
            HStack(alignment: .center, spacing: 8) {
                VStack(alignment: .leading, spacing: 3) {
                    HStack(spacing: 6) {
                        Text(mdl.name).font(Fonts.mono(12.5, .semibold)).foregroundStyle(p.fg)
                        ForEach(mdl.tags, id: \.self) { badge($0, p.code) }
                        if mdl.gb > 0 && !mdl.fits { badge("needs more memory", p.warn) }
                    }
                    Text(mdl.note).font(Fonts.mono(11)).foregroundStyle(p.fg2)
                }
                Spacer(minLength: 8)
                if mdl.gb > 0 { Text(String(format: "%.1f GB", mdl.gb)).font(Fonts.mono(11)).foregroundStyle(p.gut).fixedSize() }
                if inUse {
                    Text("in use ✓").font(Fonts.mono(11.5, .semibold)).foregroundStyle(p.ok).frame(minWidth: 92)
                } else if have {
                    SmallBtn(title: "Use", primary: true) { use(mdl.name) }.frame(minWidth: 92, alignment: .trailing)
                } else if let pull, pull.error == nil {
                    SmallBtn(title: "Cancel") { hub.cancel(mdl.name) }.frame(minWidth: 92, alignment: .trailing)
                } else {
                    SmallBtn(title: "Download") { hub.pull(mdl.name) }.frame(minWidth: 92, alignment: .trailing)
                }
            }
            if let pull {
                if let e = pull.error {
                    HStack(spacing: 6) {
                        Text("download failed: \(e)").font(Fonts.mono(11)).foregroundStyle(p.err).lineLimit(2)
                        Button(action: { hub.dismissError(mdl.name) }) { Text("dismiss").font(Fonts.mono(11)).foregroundStyle(p.fg2) }.buttonStyle(.plain)
                    }
                } else {
                    let f = pull.total > 0 ? Double(pull.done) / Double(pull.total) : 0
                    HStack(spacing: 8) {
                        GeometryReader { g in
                            ZStack(alignment: .leading) {
                                Capsule().fill(p.bg3)
                                Capsule().fill(p.acc).frame(width: max(4, g.size.width * f))
                            }
                        }
                        .frame(height: 5)
                        Text(pull.total > 0 ? String(format: "%.0f%% · %.2f / %.2f GB", f * 100, Double(pull.done) / 1e9, Double(pull.total) / 1e9) : pull.status)
                            .font(Fonts.mono(10.5)).foregroundStyle(p.fg2).fixedSize()
                    }
                    .animation(.easeOut(duration: 0.3), value: f)
                }
            }
        }
        .padding(.horizontal, 8).padding(.vertical, 7)
        .background(RoundedRectangle(cornerRadius: 6).fill(inUse ? p.sel : .clear))
    }

    private func badge(_ t: String, _ c: Color) -> some View {
        Text(t).font(Fonts.mono(9.5, .semibold)).foregroundStyle(c)
            .padding(.horizontal, 5).padding(.vertical, 1)
            .overlay(Capsule().strokeBorder(c.opacity(0.5), lineWidth: 1))
            .fixedSize()
    }
}

struct SmallBtn: View {
    @Environment(\.pal) var p
    let title: String
    var primary = false
    let action: () -> Void
    @State private var hover = false
    var body: some View {
        Button(action: action) {
            Text(verbatim: title).font(Fonts.mono(11.5, .semibold))
                .foregroundStyle(primary ? p.bg : (hover ? p.fg : p.tx))
                .padding(.horizontal, 10).padding(.vertical, 4)
                .background(RoundedRectangle(cornerRadius: 5).fill(primary ? p.acc.opacity(hover ? 0.88 : 1) : (hover ? p.sel : .clear)))
                .overlay(RoundedRectangle(cornerRadius: 5).strokeBorder(primary ? .clear : (hover ? p.acc : p.sep), lineWidth: 1))
                .contentShape(Rectangle())
        }
        .buttonStyle(.plain)
        .onHover { hover = $0 }
    }
}
