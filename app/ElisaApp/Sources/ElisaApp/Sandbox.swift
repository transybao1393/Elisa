// Sandbox images (docs/reference/mcp-and-plugins.md "Sandbox images", events in elisa/src/jsonl.rs): the Sandbox Images window
// (Elisa › Sandbox Images…) lists Elisa's pinned images (src/images.rs) with present / missing, stable and "this
// project", and downloads missing ones. Every download is the user's yes: with a session open the session asks its
// yes / no question in the chat (or in the Plugin Setup window); with none, the window's own confirm row is the yes and
// `elisa sandbox pull IMAGE --yes --json` runs (its JSON lines drive the progress; Cancel sends it SIGINT, which ends
// the pull's whole process group).
import AppKit
import SwiftUI

struct SandboxImage: Identifiable, Hashable {
    var image = "", lang = "", version = "", label = "", purpose = ""
    var stable = false, project = false, pinned = false, isDefault = false
    /// nil: unknown (no Colima)
    var present: Bool?
    var sizeMB = 0
    var diskMB: Int?
    var id: String { image }

    init(_ j: [String: Any]) {
        image = j["image"] as? String ?? ""
        lang = j["lang"] as? String ?? ""
        version = j["version"] as? String ?? ""
        label = j["label"] as? String ?? ""
        purpose = j["purpose"] as? String ?? ""
        stable = j["stable"] as? Bool ?? false
        project = j["project"] as? Bool ?? false
        isDefault = j["default"] as? Bool ?? false
        pinned = j["pinned"] as? Bool ?? false
        present = j["present"] as? Bool
        sizeMB = (j["size_mb"] as? NSNumber)?.intValue ?? 0
        diskMB = (j["disk_mb"] as? NSNumber)?.intValue
    }
}

@MainActor
final class SandboxStore: ObservableObject {
    static let shared = SandboxStore()

    @Published private(set) var images: [SandboxImage] = []
    @Published private(set) var loaded = false
    /// why the list could not be read (no Colima: says `colima start`)
    @Published private(set) var listError: String?
    /// downloads in progress: image → "3/7 layers · 12 s"
    @Published private(set) var pulling: [String: String] = [:]
    /// the last outcome, one line
    @Published var status: (text: String, ok: Bool)?
    /// images waiting for the window's own confirm (no session: the click is the yes)
    @Published var confirm: [String]?
    /// after this image is downloaded, run the plugin operation again (its preview then has no missing image)
    private var afterPull: (image: String, op: String, arg: String)?
    private var cliProc: Process?

    func size(_ image: String) -> Int? { images.first { $0.image == image }.map(\.sizeMB) }

    // MARK: events (from the session, or the CLI's JSON lines)

    func event(_ type: String, _ e: [String: Any], _ m: AppModel) {
        switch type {
        case "sandbox_images":
            let list = (e["images"] as? [[String: Any]] ?? []).map(SandboxImage.init)
            if !list.isEmpty || images.isEmpty { images = list }
            listError = (e["error"] as? String).flatMap { $0.isEmpty ? nil : $0 }
            loaded = true
        case "image_pull":
            let img = e["image"] as? String ?? "", state = e["state"] as? String ?? ""
            if state == "pulling" {
                let n = (e["layers"] as? NSNumber)?.intValue ?? 0, d = (e["done"] as? NSNumber)?.intValue ?? 0
                let s = (e["elapsed_s"] as? NSNumber)?.doubleValue ?? 0
                pulling[img] = (n > 0 ? "\(d)/\(n) layers · " : "starting · ") + String(format: "%.0f s", s)
            } else {
                pulling[img] = nil
                status = (e["text"] as? String ?? state, state == "done")
                if state == "done", let a = afterPull, a.image == img, m.sessionOpen {
                    afterPull = nil
                    m.sendPlugin(a.op, a.arg)
                    PluginStore.shared.status = ("image downloaded · checking \(a.arg) again…", true)
                } else if state != "pulling", afterPull?.image == img { afterPull = nil }
            }
        default: break
        }
    }

    // MARK: actions

    /// the list from `elisa sandbox list --json` (no session needed), in the session's folder so "this project" shows
    func refresh(_ m: AppModel) {
        // the project folder (open, or about to be) so its version files mark "this project"
        let cwd = !m.cwd.isEmpty ? m.cwd : (m.folder ?? "/")
        PluginStore.cli(["sandbox", "list", "--json"], cwd: cwd) { code, out, err in
            guard let d = out.data(using: .utf8), let j = (try? JSONSerialization.jsonObject(with: d)) as? [String: Any] else {
                self.listError = "could not read the image list: " + (err.split(separator: "\n").last.map(String.init) ?? "exit \(code)")
                self.loaded = true
                return
            }
            self.event("sandbox_images", j, m)
        }
    }

    /// Download: through the session (it asks in the chat) or, with none, after the window's confirm row
    func pull(_ names: [String], _ m: AppModel) {
        guard !names.isEmpty else { return }
        if m.sessionOpen {
            m.sendSandbox("pull", names.joined(separator: " "))
            status = ("asked in the chat: download \(names.joined(separator: ", "))?", true)
        } else {
            confirm = names
        }
    }

    /// the confirm row's Download (no session): `elisa sandbox pull … --yes --json`, its lines as events
    func confirmed(_ m: AppModel) {
        guard let names = confirm, cliProc == nil else { return }
        confirm = nil
        guard let bin = ElisaProcess.binary() else { status = ("elisa binary not found", false); return }
        let p = Process()
        p.executableURL = bin
        p.arguments = ["sandbox", "pull"] + names + ["--yes", "--json"]
        p.currentDirectoryURL = URL(fileURLWithPath: !m.cwd.isEmpty ? m.cwd : (m.folder ?? "/"))
        p.standardInput = FileHandle.nullDevice
        let out = Pipe(), err = Pipe()
        p.standardOutput = out
        p.standardError = err
        var buf = Data()
        out.fileHandleForReading.readabilityHandler = { h in
            let d = h.availableData
            if d.isEmpty { h.readabilityHandler = nil; return }
            DispatchQueue.main.async {
                MainActor.assumeIsolated {
                    buf.append(d)
                    while let nl = buf.firstIndex(of: 0x0A) {
                        let line = buf[buf.startIndex..<nl]
                        buf.removeSubrange(buf.startIndex...nl)
                        if let j = (try? JSONSerialization.jsonObject(with: Data(line))) as? [String: Any], let t = j["type"] as? String { self.event(t, j, m) }
                    }
                }
            }
        }
        p.terminationHandler = { proc in
            let e = String(decoding: err.fileHandleForReading.readDataToEndOfFile(), as: UTF8.self)
            DispatchQueue.main.async {
                MainActor.assumeIsolated {
                    self.cliProc = nil
                    self.pulling = [:]
                    if proc.terminationStatus != 0, self.status?.ok != false, let l = e.split(separator: "\n").last { self.status = (String(l), false) }
                    self.refresh(m)
                }
            }
        }
        do { try p.run(); cliProc = p; for n in names { pulling[n] = "starting" } } catch { status = ("cannot run elisa: \(error.localizedDescription)", false) }
    }

    /// Cancel: the CLI gets SIGINT (it ends the pull's process group); in a session, Esc (interrupt)
    func cancel(_ m: AppModel) {
        if let p = cliProc { p.interrupt(); return }
        if m.sessionOpen { m.interrupt() }
    }

    /// the Plugin Setup window's "Download image": answers the session's pending download question, or asks for the
    /// download and checks the plugin again after it
    func downloadForPreview(_ image: String, _ pv: PluginPreview, _ m: AppModel) {
        if let a = m.approval, a.question, a.tool.hasPrefix("sandbox pull"), a.tool.contains(image) {
            m.answer(word: "yes")
            PluginStore.shared.status = ("downloading \(image)…", true)
            return
        }
        afterPull = (image, pv.op, pv.op == "add" ? pv.source : pv.name)
        pull([image], m)
    }
}

// MARK: - the Sandbox Images window

struct SandboxView: View {
    @ObservedObject var store = SandboxStore.shared
    @ObservedObject var m: AppModel
    @Environment(\.pal) var p

    private var missing: [SandboxImage] { store.images.filter { $0.present == false } }

    var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            VStack(alignment: .leading, spacing: 6) {
                Pixel("Sandbox images", size: 11, color: p.acc)
                Text(verbatim: "The container images Elisa runs code and plugins in. Only these can be downloaded, and only when you say yes.")
                    .font(Fonts.mono(12)).foregroundStyle(p.tx).fixedSize(horizontal: false, vertical: true)
                Text(verbatim: "Downloads go into Colima's VM; nothing is installed on this Mac.")
                    .font(Fonts.mono(11)).foregroundStyle(p.fg2)
            }
            .padding(.horizontal, 22).padding(.top, 30).padding(.bottom, 14)
            Rectangle().fill(p.sep).frame(height: 1)
            if let e = store.listError {
                Text(verbatim: e).font(Fonts.mono(12)).foregroundStyle(p.err).padding(.horizontal, 22).padding(.top, 12).textSelection(.enabled)
            }
            if !store.loaded {
                Text(verbatim: "reading…").font(Fonts.mono(12)).foregroundStyle(p.fg2).padding(22)
                Spacer(minLength: 0)
            } else {
                ScrollView {
                    LazyVStack(alignment: .leading, spacing: 0) {
                        ForEach(store.images) { i in
                            row(i)
                            Rectangle().fill(p.sep.opacity(0.6)).frame(height: 1)
                        }
                    }
                }
            }
            if let c = store.confirm {
                let mb = c.compactMap { store.size($0) }.reduce(0, +)
                HStack(spacing: 8) {
                    Text(verbatim: "Download \(c.joined(separator: ", ")) (~\(mb) MB) into Colima's VM? Nothing is installed on this Mac.")
                        .font(Fonts.mono(11)).foregroundStyle(p.tx).fixedSize(horizontal: false, vertical: true)
                    Spacer(minLength: 6)
                    SmallBtn(title: "Not now") { store.confirm = nil }
                    SmallBtn(title: "Download", primary: true) { store.confirmed(m) }
                }
                .padding(.horizontal, 22).padding(.vertical, 8)
                .background(p.bg2)
            }
            Rectangle().fill(p.sep).frame(height: 1)
            HStack(spacing: 10) {
                let stableMissing = missing.filter(\.stable)
                SmallBtn(title: "Download stable (\(stableMissing.count))", primary: !stableMissing.isEmpty) { store.pull(m.sessionOpen ? ["stable"] : stableMissing.map(\.image), m) }
                    .disabled(stableMissing.isEmpty || !store.pulling.isEmpty).opacity(stableMissing.isEmpty ? 0.5 : 1)
                    .help("The stable line of every language; until it is here, runs with no project version use the newest line on this Mac")
                if !store.pulling.isEmpty { SmallBtn(title: "Cancel") { store.cancel(m) } }
                if let s = store.status {
                    Text(verbatim: s.text).font(Fonts.mono(11)).foregroundStyle(s.ok ? p.ok : p.warn).lineLimit(2).textSelection(.enabled)
                } else if !missing.isEmpty {
                    Text(verbatim: "\(missing.count) missing · ~\(missing.map(\.sizeMB).reduce(0, +)) MB").font(Fonts.mono(11)).foregroundStyle(p.fg2)
                }
                Spacer()
                Button { store.refresh(m) } label: { Text(verbatim: "↻ reload").font(Fonts.mono(11)).foregroundStyle(p.acc) }.buttonStyle(.plain)
            }
            .padding(.horizontal, 22).padding(.vertical, 9)
        }
        .background(p.bg)
        .ignoresSafeArea()
        .frame(minWidth: 700, minHeight: 420)
        .onAppear { store.refresh(m) }
    }

    private func row(_ i: SandboxImage) -> some View {
        HStack(alignment: .firstTextBaseline, spacing: 10) {
            Text(verbatim: i.image).font(Fonts.mono(12.5, .semibold)).foregroundStyle(p.code).frame(width: 190, alignment: .leading)
            VStack(alignment: .leading, spacing: 3) {
                HStack(spacing: 6) {
                    Text(verbatim: i.purpose).font(Fonts.mono(11.5)).foregroundStyle(p.tx)
                    if i.stable { tag("stable", p.ok) }
                    if i.project { tag("this project", p.acc) }
                    // the line runs (with no project version) and plugin installers use, because it is here
                    if i.isDefault && i.present == true && !i.stable { tag("on this Mac", p.code) }
                }
                if let pr = store.pulling[i.image] {
                    Text(verbatim: "downloading · \(pr)").font(Fonts.mono(11)).foregroundStyle(p.acc)
                }
            }
            Spacer(minLength: 8)
            switch i.present {
            case .some(true):
                Text(verbatim: "present" + (i.diskMB.map { " · \($0) MB on disk" } ?? "")).font(Fonts.mono(11)).foregroundStyle(p.ok)
            case .some(false):
                Text(verbatim: "missing · ~\(i.sizeMB) MB").font(Fonts.mono(11)).foregroundStyle(p.warn)
                SmallBtn(title: "Download") { store.pull([i.image], m) }.disabled(store.pulling[i.image] != nil || store.confirm != nil)
            case .none:
                Text(verbatim: "unknown").font(Fonts.mono(11)).foregroundStyle(p.fg2)
            }
        }
        .padding(.horizontal, 22).padding(.vertical, 9)
    }

    private func tag(_ t: String, _ c: Color) -> some View {
        Text(verbatim: t).font(Fonts.mono(10.5)).foregroundStyle(c)
            .padding(.horizontal, 5).padding(.vertical, 1)
            .overlay(RoundedRectangle(cornerRadius: 4).strokeBorder(c.opacity(0.6), lineWidth: 1))
    }
}

private struct SandboxRoot: View {
    @ObservedObject var m: AppModel
    var body: some View { SandboxView(m: m).environment(\.pal, m.pal).preferredColorScheme(m.dark ? .dark : .light) }
}

@MainActor
final class SandboxWindow {
    static let shared = SandboxWindow()
    private var window: NSWindow?

    func show(_ m: AppModel) {
        if let w = window { w.contentView = NSHostingView(rootView: SandboxRoot(m: m)); Front.user(w); return }
        let w = NSWindow(contentRect: NSRect(x: 0, y: 0, width: 820, height: 560), styleMask: [.titled, .closable, .resizable, .miniaturizable, .fullSizeContentView], backing: .buffered, defer: false)
        w.title = "Sandbox Images"
        w.titlebarAppearsTransparent = true
        w.titleVisibility = .hidden
        w.isReleasedWhenClosed = false
        w.minSize = NSSize(width: 700, height: 420)
        w.contentView = NSHostingView(rootView: SandboxRoot(m: m))
        if !DebugFrame.place(w) { w.center() }
        Front.user(w)
        window = w
    }
}
