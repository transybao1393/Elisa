// The app state, built from Elisa's JSON-lines events (elisa/src/jsonl.rs). Elisa owns the agent and Shade's state
// machine (the same controller as the TUI dock); the app only mirrors them.
import AppKit
import SwiftUI

struct ShadeFrame: Equatable {
    var state = "idle", verb = "", target = "", tone = "accent", pop = false, face = "idle"
    /// a long tool's occasional fidget (elisa `View::fidget`): the dock draws the listening pose, unless motion is reduced
    var fidget = false
    /// when this state started (its animation loop starts here)
    var since = Date()
}

enum ToolStatus { case running, pending, ok, err, denied }

struct ToolRow {
    var name: String, verb: String, target: String
    var status: ToolStatus = .running
    var t0 = Date()
    var elapsed: Double?
    var note = "", red = false
    var preview: [String] = []
    var more = 0
    /// the tool's output as Elisa sent it (capped at 4000 chars by elisa): the ▸ expand view
    var output = ""
    var add: Int?, del: Int?
    var editIndex: Int?
}

enum Tone { case plain, err, warn, ok }

/// one check of the fix loop (`check` {cmd, status, round, rounds, summary, line}, elisa/src/jsonl.rs): status is
/// running | pass | fail (a fix round follows) | timeout | stopped (gave up); round 0 = the first check, n = after fix
/// round n; rounds = the most fix rounds; summary is "" while running; line = Elisa's ready-made transcript text
struct CheckRow {
    var cmd: String
    var status: String
    var round: Int?
    var rounds: Int?
    var summary: String
    var line: String

    var running: Bool { status == "running" }
    var passed: Bool { status == "pass" }
    /// errors went back to the model: fix round round+1 of rounds follows
    var fixing: Bool { status == "fail" }
    var stopped: Bool { status == "stopped" }
    var known: Bool { ["running", "pass", "fail", "timeout", "stopped"].contains(status) }

    init(_ e: [String: Any]) {
        cmd = e["cmd"] as? String ?? ""
        status = (e["status"] as? String ?? "").lowercased()
        summary = e["summary"] as? String ?? ""
        line = e["line"] as? String ?? ""
        round = (e["round"] as? NSNumber)?.intValue
        rounds = (e["rounds"] as? NSNumber)?.intValue
    }
}

struct Part: Identifiable {
    enum Kind {
        case prose(String)
        case tool(ToolRow)
        case check(CheckRow)
        case footer(String)
        case info(String, Tone)
        /// the state card's text and its number (`card` {n}); nil when Elisa does not send one (before n existed)
        case card(String, Int?)
        case box(String, String)
        /// the model's reasoning before its answer (`thinking` events): its text, when it started, and when the answer
        /// (or a tool, or the end of the turn) ended it; nil while it goes on
        case thinking(String, start: Date, end: Date?)
    }
    let id = UUID()
    var kind: Kind
}

struct Block: Identifiable {
    enum Kind { case user(String, cmd: Bool), reply, info(String, Tone) }
    let id = UUID()
    var kind: Kind
    var parts: [Part] = []
    var live = false
    /// a user message's attached pictures, as sent (thumbnails in the transcript)
    var images: [NSImage] = []
    /// a reply's finished steps in plain words (`activity` done events, Activity.swift) and its one summary line
    var acts: [ActivityItem] = []
    var actSummary: String?
    /// a user message's attached documents, as sent (chips in the transcript)
    var docs: [DocAttachment] = []
}

/// A resumed conversation's earlier turns (`history` {items: [{role: user|assistant, text}]}, sent right after the
/// banner or a resume line, elisa/src/context.rs `earlier_turns`): normal user and reply blocks (markdown via Prose)
/// between two divider lines, so a continued conversation never looks empty.
enum EarlierTurns {
    static let before = "— earlier in this conversation —"
    static let after = "— continued —"

    static func blocks(_ e: [String: Any]) -> [Block] {
        let items = (e["items"] as? [[String: Any]]) ?? []
        var out: [Block] = []
        for i in items {
            guard let t = i["text"] as? String, !t.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty else { continue }
            switch i["role"] as? String {
            case "user": out.append(Block(kind: .user(t, cmd: false)))
            case "assistant": out.append(Block(kind: .reply, parts: [Part(kind: .prose(t))]))
            default: continue
            }
        }
        if out.isEmpty { return [] }
        return [Block(kind: .info(before, .plain))] + out + [Block(kind: .info(after, .plain))]
    }
}

struct DiffLine: Identifiable { let id: Int; let k: String; let o: Int?; let n: Int?; let t: String }

struct Edit: Identifiable {
    let id = UUID()
    var path: String, kind: String, add: Int, del: Int, lines: [DiffLine], turn: Int, time: Date
    var failed: String?
}

struct Stat {
    /// nil (or 0) = no decode window (a short or tool-only reply): shown as "—"
    var tokS: Double? = nil
    var ttft = 0.0, ctxUsed = 0, maxCtx = 0, reused = 0, out = 0
    /// the decode rate as the status bar / footer show it: "21.4", or "—" when there is none
    var tokSText: String { tokS.map { $0 > 0 && $0.isFinite ? String(format: "%.1f", $0) : "—" } ?? "—" }
}

struct Approval {
    let tool: String; let summary: String; var sel = 0
    /// a call outside the project folder (Elisa's `approve` with outside: true): read / write / bash and the paths;
    /// its answers are allow once / allow these paths for this session / deny
    var outside = false; var access = ""; var accessWord = ""; var paths: [String] = []
    /// the MCP server the tool comes from (`server` on `approve`; "web" is Elisa's built-in web server);
    /// nil for Elisa's own tools. "always" allows the whole server for this project
    var server: String? = nil
    /// "always allow this pattern" (`rule` on `approve`): the exact pattern answering "rule" saves to the user's
    /// config; nil when the core offers none for this call
    var rule: String? = nil
    /// not a tool call but a yes / no question (`question: true`: a plugin's setup, a project's trust prompt)
    var question = false

    /// the answers the box offers, in order: (label, the word sent back). "no" is always last
    var choices: [(label: String, answer: String)] {
        if question { return [("Yes", "yes"), ("No", "no")] }
        if outside { return [("Yes, once", "yes"), ("Yes, allow these paths for this session", "always"), ("No", "no")] }
        var c: [(String, String)] = [("Yes", "yes"), ("", "always")]
        if rule != nil { c.append(("Yes, and always allow this pattern", "rule")) }
        c.append(("No", "no"))
        return c
    }
}

enum Phase: Equatable { case starting, idle, thinking, streaming, tool, permission, exited }

@MainActor
final class AppModel: ObservableObject {
    let kit: Kit?
    let layout: Layout
    private let elisa = ElisaProcess()

    @Published var dark = true
    @Published var blocks: [Block] = []
    @Published var shade = ShadeFrame()
    @Published var stat: Stat?
    @Published var sumIn = 0
    @Published var sumOut = 0
    @Published var phase: Phase = .starting
    @Published var phaseSince = Date()
    @Published var approval: Approval?
    /// no folder open yet: the window shows the welcome screen (Welcome.swift) instead of the transcript
    @Published var welcome = false
    /// the last folders opened, newest first, that still exist (the welcome screen's Recent list)
    var recentFolders: [String] {
        let d = UserDefaults.standard
        let l = d.stringArray(forKey: "recentFolders") ?? d.string(forKey: "lastFolder").map { [$0] } ?? []
        return l.filter { FileManager.default.fileExists(atPath: $0) }
    }
    @Published var picker: [[String: Any]]?
    @Published var pickSel = 0
    @Published var edits: [Edit] = []
    @Published var editIdx = 0
    @Published var editsOpen = true
    /// the edit panel's width: dragged on its left edge (double-click resets to the max), kept while ctrl-o hides it;
    /// max = the kit's 440, min 264 (60%). Saved per user in UserDefaults ("editsWidth"), clamped into range on load
    @Published var editsWidth: CGFloat = AppModel.loadEditsWidth() {
        didSet { if editsWidth != oldValue { UserDefaults.standard.set(Double(editsWidth), forKey: "editsWidth") } }
    }
    static let editsWidthRange: ClosedRange<CGFloat> = 264...440
    func setEditsWidth(_ v: CGFloat) { editsWidth = min(max(v.rounded(), Self.editsWidthRange.lowerBound), Self.editsWidthRange.upperBound) }
    private static func loadEditsWidth() -> CGFloat {
        let v = (UserDefaults.standard.object(forKey: "editsWidth") as? NSNumber).map { CGFloat($0.doubleValue) } ?? editsWidthRange.upperBound
        return min(max(v, editsWidthRange.lowerBound), editsWidthRange.upperBound)
    }
    /// Shade's size in px, set by dragging the tray's resize handle (double-click resets to Medium) or stepped by
    /// ctrl-s; range = the kit's dock sizes Medium..Large (128..168). Saved per user as a number in UserDefaults
    /// ("shadeSize"); the old S/M/L strings migrate: Small or Medium -> Medium, Large -> Large
    @Published var shadeSize: CGFloat = AppModel.loadShadeSize() {
        didSet { if shadeSize != oldValue { UserDefaults.standard.set(Double(shadeSize), forKey: "shadeSize") } }
    }
    /// Shade hidden (the tray's hide button; the status-bar face brings it back); remembered like "shadeSize"
    @Published var shadeHidden = UserDefaults.standard.bool(forKey: "shadeHidden") {
        didSet { UserDefaults.standard.set(shadeHidden, forKey: "shadeHidden") }
    }
    @Published var queued = 0
    /// --script playback running: its status-bar line ("playback · turn 7/21"); nil = off. playbackStop ends it
    @Published var playbackLine: String?
    var playbackStop: (() -> Void)?
    @Published var input = ""
    /// a long pasted text (PasteText.long), kept once here and not in the input field (a text field lays out, and its
    /// undo stack keeps, every character: a 114 KB paste cost hundreds of MB); shown as a chip, sent after the typed text
    @Published var pasted: String?
    @Published var slashSel = 0
    @Published var model = ""
    /// the server Elisa talks to (from `banner`), for the status bar's model chip
    @Published var baseURL = ""
    /// the model can see pictures (`banner.vision`): the image button and drop target work only then
    @Published var vision = false
    /// pictures waiting to go with the next message
    @Published var attachments: [Attachment] = []
    /// the pictures of the message just sent, shown on its `user` line when Elisa echoes it
    var sentImages: [NSImage] = []
    /// pictures refused at attach time since the last message (the model has no vision): the next `send` says so in
    /// `images_refused`, so Elisa answers a question about them honestly instead of the model making one up (BUGS #61)
    var refusedImages = 0
    /// documents waiting to go with the next message (any model: Elisa reads them, src/attach.rs)
    @Published var documents: [DocAttachment] = []
    /// the documents of the message just sent, shown on its `user` line
    var sentDocs: [DocAttachment] = []
    /// each attached file's intake result (`doc_info`, by the path sent): the chip's one-line result and its panel
    @Published var docInfo: [String: DocInfo] = [:]
    /// the JSONL protocol the running Elisa speaks (`hello.protocol`); documents need 2
    var protocolVersion = 1
    @Published var cwd = ""
    @Published var maxCtx = 0
    @Published var yolo = false
    @Published var watchLine: String?
    @Published var auditLine: String?
    @Published var job: (label: String, frac: Double?, done: String?, at: Date)?
    @Published var commands: [(String, String)] = []
    /// the latest state card's number (`card` {n}), only when Elisa sends one (an older Elisa omits it)
    @Published var cardNo: Int?
    /// tool rows whose output is expanded (▸ expand / ▾ collapse), by part id
    @Published var expanded: Set<UUID> = []
    /// the window is below the kit's 900 px narrow width: the edit panel becomes an overlay
    @Published var narrow = false
    /// narrow only: the edit panel shown as an overlay over the transcript
    @Published var editsSheet = false
    /// scroll hold: the user scrolled up; new output does not move the transcript, the pill counts it
    @Published var scrollHeld = false
    @Published var heldLines = 0
    @Published var heldNew = false
    @Published var scrollTick = 0
    /// the live line (Activity.swift): Elisa's current step, a step waiting out its first 300 ms, app-side work
    @Published var liveAct: LiveActivity?
    var nextAct: LiveActivity?
    @Published var localAct: LiveActivity?
    /// this Elisa reports its steps in words: tool rows move into each reply's step list
    @Published var activitySeen = false
    var folder: String?
    var extraArgs: [String] = []
    private var turn = 0
    private var cur: Int?
    private var typingSent = false
    var onEventHook: ([String: Any]) -> Void = { _ in }

    init(kit: Kit?) {
        self.kit = kit
        layout = Layout.load(kit)
        elisa.onEvent = { [weak self] e in self?.event(e) }
        elisa.onExit = { [weak self] c in self?.exited(c) }
        DispatchQueue.main.async { [weak self] in if let self { DevReload.shared.watch(self) } }
    }

    private lazy var palDark = Palette.load(kit, dark: true)
    private lazy var palLight = Palette.load(kit, dark: false)
    var pal: Palette { dark ? palDark : palLight }
    /// the newest reply: its mini Shade keeps following the live state (older replies hold idle)
    var lastReplyID: UUID? { blocks.last(where: { if case .reply = $0.kind { return true }; return false })?.id }
    var busy: Bool { phase != .idle && phase != .exited && phase != .starting }
    var title: String {
        let full = cwd.isEmpty ? (folder ?? "") : cwd
        let t = (full as NSString).abbreviatingWithTildeInPath
        if t.hasPrefix("~") || t.count <= 40 { return t }
        // temp or system paths: the last two components are what matters
        let c = (full as NSString).pathComponents.filter { $0 != "/" }
        return c.count > 2 ? "…/" + c.suffix(2).joined(separator: "/") : t
    }

    // MARK: - lifecycle

    func start() {
        welcome = false
        attachments = []; sentImages = []; vision = false; documents = []; sentDocs = []
        blocks = []; edits = []; editIdx = 0; stat = nil; sumIn = 0; sumOut = 0; turn = 0; cur = nil; queued = 0
        approval = nil; picker = nil; liveAct = nil; nextAct = nil; watchLine = nil; auditLine = nil; job = nil; compactLine = nil; cardNo = nil; expanded = []
        scrollHeld = false; heldLines = 0; heldNew = false
        PluginStore.shared.sessionEnded()
        setPhase(.starting)
        do { try elisa.start(cwd: folder, extra: extraArgs + ["--theme", dark ? "dark" : "light"]) }
        catch { blocks.append(Block(kind: .info(error.localizedDescription, .err))); setPhase(.exited) }
        // a launch's --continue is for the first session only: a later new session (another folder, a restart after
        // exit) starts clean, as the person expects; reconnect() adds it back for its own restart
        extraArgs.removeAll { $0 == "--continue" || $0 == "--continue-open" }
    }

    func stop() { elisa.stopNow() }
    /// the core process is alive (CloseTest)
    var elisaRunning: Bool { elisa.running }

    /// a saved Model Connections change: Elisa reads its connection at start, so restart it on this project's session
    func reconnect() {
        // on the welcome screen there is no session yet: the next folder opened uses the new connection
        guard !welcome else { return }
        let extra = extraArgs
        // the conversation open now, whatever was said in it (BUGS #63: a short chat was skipped as not worth continuing)
        extraArgs.removeAll { $0 == "--continue" }
        extraArgs.append("--continue-open")
        start()
        extraArgs = extra
    }

    func open(folder f: String) {
        folder = f
        UserDefaults.standard.set(f, forKey: "lastFolder")
        UserDefaults.standard.set(Array(([f] + recentFolders.filter { $0 != f }).prefix(5)), forKey: "recentFolders")
        start()
    }

    private func exited(_ code: Int32) {
        if phase == .exited { return }
        approval = nil
        PluginStore.shared.sessionEnded()
        blocks.append(Block(kind: .info("[elisa exited (code \(code)) · File > Open Folder… starts a new session]", code == 0 ? .plain : .err)))
        setPhase(.exited)
    }

    private func setPhase(_ p: Phase) { if p != phase { phase = p; phaseSince = Date() } }

    // MARK: - user actions

    /// text pasted into the input box (⌘V, or the --script `@paste` line): a long one becomes the pasted-text chip
    func paste(_ s: String) {
        if PasteText.long(s) { pasted = pasted.map { $0 + "\n" + s } ?? s } else { input += s }
    }

    func send(_ raw: String) {
        var t = raw.trimmingCharacters(in: .whitespacesAndNewlines)
        // the pasted-text chip goes with this message, after what was typed
        if let p = pasted, phase != .exited, !t.hasPrefix("/") {
            t = t.isEmpty ? p : t + "\n\n" + p
            pasted = nil
        }
        let imgs = attachments
        let docs = documents
        guard !t.isEmpty || !imgs.isEmpty || !docs.isEmpty || refusedImages > 0, phase != .exited else { return }
        if t == "/exit" || t == "/quit" { NSApp.terminate(nil); return }
        if t == "/model" { ConnectionsWindow.shared.show(self); return }
        var f: [String: Any] = [:]
        if !imgs.isEmpty && !t.hasPrefix("/") {
            // a picture on its own still needs a question
            if t.isEmpty { t = imgs.count == 1 ? "What is in this image?" : "What is in these images?" }
            f["images"] = imgs.map { ["media_type": $0.mediaType, "data": $0.data] }
            sentImages = imgs.map(\.thumb)
            attachments = []
        }
        if !docs.isEmpty && !t.hasPrefix("/") {
            // the path only: Elisa copies the file into the project's upload area and reads it there
            if t.isEmpty { t = docs.count == 1 ? "Summarise this document." : "Summarise these documents." }
            f["documents"] = docs.map { ["path": $0.url.path] }
            sentDocs = docs
            documents = []
        }
        if refusedImages > 0 && !t.hasPrefix("/") {
            // the refused picture with no words: the question it would have carried
            if t.isEmpty { t = refusedImages == 1 ? "What is in this image?" : "What is in these images?" }
            f["images_refused"] = refusedImages
            refusedImages = 0
        }
        f["text"] = t
        Speak.shared.stop()
        elisa.send("send", f)
        typingSent = false
    }

    /// an attached document: let the core read it into its cache now, before ⏎
    func prefetchDoc(_ path: String) { elisa.send("prefetch", ["documents": [["path": path]]]) }

    func typing(_ nonempty: Bool) {
        if nonempty { Speak.shared.stop() }
        guard phase == .idle, nonempty != typingSent else { return }
        typingSent = nonempty
        elisa.send("typing", ["nonempty": nonempty])
    }

    func answer(_ i: Int) {
        answer(word: ["yes", "always", "no"][max(0, min(2, i))])
    }

    /// answers the open approval with Elisa's word: yes | always | rule | no
    func answer(word: String) {
        guard let a = approval else { return }
        elisa.send("approve", ["answer": word])
        approval = nil
        // a yes / no question (plugin setup, project trust) runs no tool
        setPhase(a.question ? .thinking : word == "no" ? .thinking : .tool)
    }

    /// a session is running in a folder (the Plugins window sends its actions to it)
    var sessionOpen: Bool { elisa.running && !welcome && phase != .exited }

    /// `{"type":"plugin","op":…}`: the same as typing `/plugin OP ARG` (queued like a message)
    func sendPlugin(_ op: String, _ arg: String = "") {
        var f: [String: Any] = ["op": op]
        if op == "add" { f["path"] = arg } else if !arg.isEmpty { f["name"] = arg }
        elisa.send("plugin", f)
    }

    /// `{"type":"sandbox","op":…}`: the same as typing `/sandbox OP NAME` (a pull asks in the chat first)
    func sendSandbox(_ op: String, _ name: String = "") {
        var f: [String: Any] = ["op": op]
        if !name.isEmpty { f["name"] = name }
        elisa.send("sandbox", f)
    }

    func pick(_ i: Int?) {
        guard picker != nil else { return }
        elisa.send("pick", ["index": i.map { $0 as Any } ?? NSNull()])
        picker = nil
    }

    /// Esc: deny a pending permission (the kit: esc = no), else interrupt the turn
    func interrupt() {
        Speak.shared.interrupted()
        if approval != nil { answer(word: "no"); return }
        if picker != nil { pick(nil); return }
        elisa.send("interrupt")
    }

    func care(_ a: String) { elisa.send("care", ["action": a]) }

    /// the resize range, from the kit's dock sizes (Medium .. Large)
    var shadeRange: ClosedRange<CGFloat> {
        let lo = CGFloat(layout.dockSizes["Medium"] ?? 128), hi = CGFloat(layout.dockSizes["Large"] ?? 168)
        return lo...max(lo, hi)
    }
    func setShadeSize(_ v: CGFloat) { shadeSize = min(max(v.rounded(), shadeRange.lowerBound), shadeRange.upperBound) }
    /// ctrl-s: one 10 px step up; past Large it wraps back to Medium
    func stepShadeSize() { setShadeSize(shadeSize >= shadeRange.upperBound ? shadeRange.lowerBound : shadeSize + 10) }
    private static func loadShadeSize() -> CGFloat {
        let d = UserDefaults.standard
        switch d.object(forKey: "shadeSize") {
        case let n as NSNumber: return CGFloat(n.doubleValue)
        case let t as String:
            let v: CGFloat = t == "Large" ? 168 : 128
            d.set(Double(v), forKey: "shadeSize")
            return v
        default: return 128
        }
    }

    func toggleTheme() {
        dark.toggle()
        elisa.send("theme", ["theme": dark ? "dark" : "light"])
    }

    /// the slash menu: open while the input is a bare `/word`
    var slashItems: [(String, String)] {
        guard input.hasPrefix("/"), !input.contains(where: { $0.isWhitespace }) else { return [] }
        return commands.filter { $0.0.hasPrefix(input) }
    }

    func runSlash(_ i: Int) {
        let it = slashItems
        guard i < it.count else { return }
        input = ""
        slashSel = 0
        send(it[i].0)
    }

    /// ctrl-o: the side panel when wide, the overlay when narrow
    func toggleEdits() { if narrow { editsSheet.toggle() } else { editsOpen.toggle() } }
    var editsShown: Bool { narrow ? editsSheet : editsOpen }

    func toggleExpand(_ id: UUID) { if expanded.contains(id) { expanded.remove(id) } else { expanded.insert(id) } }

    /// the transcript's scroll position changed (distance from the bottom, points)
    func scrolled(distance d: CGFloat) {
        let h = d >= ScrollHold.holdAt
        if h != scrollHeld { scrollHeld = h; if !h { heldNew = false } }
        let n = h ? max(1, Int((d / 20).rounded())) : 0
        if n != heldLines { heldLines = n }
    }

    /// --inject: feed a synthetic event line through the same path as Elisa's
    func inject(_ e: [String: Any]) { apply(e) }

    func editPrev() { editIdx = max(0, editIdx - 1) }
    func editNext() { editIdx = min(max(0, edits.count - 1), editIdx + 1) }

    // MARK: - events

    private func event(_ e: [String: Any]) {
        apply(e)
        onEventHook(e)
    }

    private func s(_ e: [String: Any], _ k: String) -> String { e[k] as? String ?? "" }
    private func n(_ e: [String: Any], _ k: String) -> Int { (e[k] as? NSNumber)?.intValue ?? 0 }
    private func d(_ e: [String: Any], _ k: String) -> Double { (e[k] as? NSNumber)?.doubleValue ?? 0 }

    /// the reply block of the current turn (created on first use)
    private func reply() -> Int {
        if let c = cur, c < blocks.count { return c }
        blocks.append(Block(kind: .reply, live: true))
        cur = blocks.count - 1
        return cur!
    }

    private func add(_ k: Part.Kind) { let r = reply(); blocks[r].parts.append(Part(kind: k)) }
    /// the current turn's reply block (created on first use) / nil outside a turn (Activity.swift)
    func currentReply() -> Int { reply() }
    var currentBlock: Int? { cur.flatMap { $0 < blocks.count ? $0 : nil } }

    /// the transcript line of a fold in progress ("compacting the conversation…"), by its block / part id, so it can
    /// be updated in place and become the result line when the fold ends
    private var compactLine: UUID?

    /// a progress label that is a fold (the history being compacted), not other background work (memory indexing)
    static func isFold(_ label: String) -> Bool { label.hasPrefix("summarizing") || label.hasPrefix("folding") }

    /// show `t` on the fold's line: updates it when it is still there, else adds it
    private func setCompactLine(_ t: String, _ tone: Tone = .plain) {
        if let id = compactLine {
            if let b = blocks.firstIndex(where: { $0.id == id }) { blocks[b].kind = .info(t, tone); return }
            for b in blocks.indices.reversed() {
                if let j = blocks[b].parts.firstIndex(where: { $0.id == id }) { blocks[b].parts[j].kind = .info(t, tone); return }
            }
        }
        // outside a reply it is its own line, busy or not: after turn_done (the fold at the turn's end) a reply block
        // made for it was an empty extra "reply" under the real one
        if cur == nil {
            let b = Block(kind: .info(t, tone)); blocks.append(b); compactLine = b.id
        } else {
            let r = reply(); let part = Part(kind: .info(t, tone)); blocks[r].parts.append(part); compactLine = part.id
        }
    }

    /// info outside a turn is its own line; inside, it belongs to the reply
    private func note(_ t: String, _ tone: Tone) {
        if cur == nil && !busy { blocks.append(Block(kind: .info(t, tone))) } else { add(.info(t, tone)) }
    }

    private var lastTool: (Int, Int)? {
        guard let r = cur, r < blocks.count, let j = blocks[r].parts.lastIndex(where: { if case .tool = $0.kind { return true }; return false }) else { return nil }
        return (r, j)
    }

    private func updateTool(_ f: (inout ToolRow) -> Void) {
        guard let (r, j) = lastTool, case .tool(var t) = blocks[r].parts[j].kind else { return }
        f(&t)
        blocks[r].parts[j].kind = .tool(t)
    }

    /// reasoning not shown yet: it reaches the transcript at most every `thinkingEvery` seconds (a few words per
    /// event, ~100 events a second, made the preview flicker), and before any other event
    private var thinkingPending = ""
    private var thinkingShown = Date.distantPast
    private let thinkingEvery = 0.5

    private func flushThinking() {
        guard !thinkingPending.isEmpty else { return }
        let t = thinkingPending
        thinkingPending = ""; thinkingShown = Date()
        let r = reply()
        if let last = blocks[r].parts.last, case .thinking(let p, let st, nil) = last.kind {
            blocks[r].parts[blocks[r].parts.count - 1].kind = .thinking(p + t, start: st, end: nil)
        } else {
            blocks[r].parts.append(Part(kind: .thinking(t, start: requestSince ?? thinkingStarted ?? Date(), end: nil)))
            requestSince = nil
        }
        thinkingStarted = nil
    }

    /// when the first reasoning of a stretch arrived (it may wait for the throttle before it shows)
    private var thinkingStarted: Date?
    /// when the running request started (`waiting`): its first reasoning counts from then
    private var requestSince: Date?

    /// the reasoning in progress is over (the answer, a tool or the turn's end came): it shows how long it took
    private func closeThinking() {
        guard let r = cur, r < blocks.count, let j = blocks[r].parts.lastIndex(where: { if case .thinking(_, _, nil) = $0.kind { return true }; return false }),
              case .thinking(let t, let st, nil) = blocks[r].parts[j].kind else { return }
        blocks[r].parts[j].kind = .thinking(t, start: st, end: Date())
    }

    private func apply(_ e: [String: Any]) {
        if s(e, "type") != "thinking" { flushThinking() }
        if ["delta", "tool_start", "reply_done", "turn_done", "error"].contains(s(e, "type")) { closeThinking() }
        switch s(e, "type") {
        case "hello":
            commands = (e["commands"] as? [[String]] ?? []).compactMap { $0.count == 2 ? ($0[0], $0[1]) : nil }
            protocolVersion = max(1, n(e, "protocol"))
        case "banner":
            model = s(e, "model"); baseURL = s(e, "base_url"); vision = (e["vision"] as? Bool) ?? false; cwd = s(e, "cwd"); maxCtx = n(e, "max_ctx")
            var t = "[\(model) · \(title) · ctx \(k(maxCtx))"
            if let r = e["resumed"] as? NSNumber { t += " · resumed \(r.intValue) messages" }
            if let notes = e["notes"] as? String, !notes.isEmpty { t += " · notes loaded (/notes)" }
            blocks.append(Block(kind: .info(t + "]", .plain)))
        case "history":
            if let c = cur, c < blocks.count { blocks[c].live = false }
            cur = nil
            blocks.append(contentsOf: EarlierTurns.blocks(e))
        case "ready":
            setPhase(.idle)
        case "queued":
            queued = n(e, "n")
        case "user":
            let t = s(e, "text")
            if let c = cur, c < blocks.count { blocks[c].live = false }
            cur = nil
            scrollHeld = false; heldNew = false
            let cmd = t.hasPrefix("/")
            Speak.shared.newTurn()
            var ub = Block(kind: .user(t, cmd: cmd))
            if !sentImages.isEmpty { ub.images = sentImages; sentImages = [] }
            if !sentDocs.isEmpty { ub.docs = sentDocs; sentDocs = [] }
            blocks.append(ub)
            if !cmd { turn += 1 }
            setPhase(.thinking)
        case "waiting":
            _ = reply()
            setPhase(.thinking)
            // the request's reasoning counts from here, as the status bar does (an image is read before the first token)
            requestSince = Date()
        case "thinking":
            // one reasoning section per stretch of reasoning: it grows until the answer (or a tool) starts
            if thinkingPending.isEmpty && thinkingStarted == nil { thinkingStarted = Date() }
            thinkingPending += s(e, "text")
            if Date().timeIntervalSince(thinkingShown) >= thinkingEvery { flushThinking() }
        case "delta":
            let t = s(e, "text")
            let r = reply()
            if let last = blocks[r].parts.last, case .prose(let p) = last.kind {
                blocks[r].parts[blocks[r].parts.count - 1].kind = .prose(p + t)
            } else {
                blocks[r].parts.append(Part(kind: .prose(t)))
            }
            setPhase(.streaming)
        case "reply_done":
            let st = Stat(tokS: (e["tok_s"] as? NSNumber)?.doubleValue, ttft: d(e, "ttft_s"), ctxUsed: n(e, "ctx_used"), maxCtx: n(e, "max_ctx"), reused: n(e, "reused"), out: n(e, "output_tokens"))
            stat = st
            let inT = max(0, st.ctxUsed - st.out)
            sumIn += inT; sumOut += st.out
            let r = reply()
            blocks[r].parts.removeAll { if case .footer = $0.kind { return true }; return false }
            add(.footer("in \(k(inT)) (reused \(k(st.reused))) · out \(k(st.out)) · \(st.tokSText) tok/s · ttft \(dur(st.ttft))"))
            setPhase(.thinking)
        case "tool_start":
            let name = s(e, "name")
            let summary = s(e, "summary")
            let target = summary.split(separator: " ", maxSplits: 1).dropFirst().first.map(String.init) ?? ""
            let server = (e["server"] as? String).flatMap { $0.isEmpty ? nil : $0 }
            add(.tool(ToolRow(name: name, verb: toolLabel(name, server: server), target: target)))
            setPhase(.tool)
        case "tool_detail":
            let tgt = e["target"] as? String
            var idx: Int?
            if let det = e["detail"] as? [String: Any], let kind = det["kind"] as? String, ["edit", "rewrite", "new file"].contains(kind) {
                let lines = (det["lines"] as? [[String: Any]] ?? []).enumerated().map { i, l in
                    DiffLine(id: i, k: l["k"] as? String ?? "ctx", o: (l["o"] as? NSNumber)?.intValue, n: (l["n"] as? NSNumber)?.intValue, t: l["t"] as? String ?? "")
                }
                edits.append(Edit(path: det["path"] as? String ?? "", kind: kind, add: n(det, "add"), del: n(det, "del"), lines: lines, turn: turn, time: Date()))
                if edits.count > 50 { edits.removeFirst() }
                editIdx = edits.count - 1
                idx = editIdx
            }
            updateTool { t in
                if let tgt, !tgt.isEmpty { t.target = tgt }
                if let i = idx, i < edits.count { t.add = edits[i].add; t.del = edits[i].del; t.editIndex = i }
            }
        case "approve":
            approval = Approval(tool: s(e, "tool"), summary: s(e, "summary"), outside: (e["outside"] as? Bool) ?? false,
                                access: s(e, "access"), accessWord: s(e, "access_word"), paths: (e["paths"] as? [String]) ?? [],
                                server: (e["server"] as? String).flatMap { $0.isEmpty ? nil : $0 },
                                rule: (e["rule"] as? String).flatMap { $0.isEmpty ? nil : $0 }, question: (e["question"] as? Bool) ?? false)
            if approval?.question == true {
                // a plugin's setup or a project's trust prompt: its window (Plugins.swift) takes the yes / no too
                PluginStore.shared.question(self)
                setPhase(.permission)
                break
            }
            // the approval names the tool's MCP server: an older core's tool_start does not, so relabel the row
            let tool = s(e, "tool")
            updateTool { t in
                t.status = .pending
                if let srv = approval?.server, t.name == tool { t.verb = toolLabel(tool, server: srv) }
            }
            setPhase(.permission)
        case "tool_done":
            let err = (e["is_error"] as? Bool) ?? false
            let note = s(e, "note")
            var editIndex: Int?
            updateTool { t in
                t.elapsed = Date().timeIntervalSince(t.t0)
                t.status = note == "denied" ? .denied : (e["red"] as? Bool ?? err) ? .err : .ok
                t.note = note; t.red = (e["red"] as? Bool) ?? err
                t.preview = e["preview"] as? [String] ?? []
                t.more = n(e, "more")
                t.output = e["output"] as? String ?? ""
                editIndex = t.editIndex
            }
            // a denied or failed edit never reached the file: marked so (Edits pane), and the pane goes back to the
            // newest edit that did land, so the real diff stays in view
            if let i = editIndex, i < edits.count, err || note == "denied" || (e["red"] as? Bool ?? false) {
                edits[i].failed = note.isEmpty ? "failed" : note
                if editIdx == i, let j = edits.lastIndex(where: { $0.failed == nil }) { editIdx = j }
            }
            approval = nil
            setPhase(.thinking)
        case "info":
            let t = s(e, "text")
            if t.contains("approve-all ON") { yolo = true }
            if t.contains("approve-all OFF") { yolo = false }
            note(t, .plain)
        case "error":
            note(s(e, "text"), .err)
        case "notes":
            add(.box("NOTES", s(e, "text")))
        case "compacted":
            // the fold's own line becomes the result (it said "compacting…" while the fold ran)
            setCompactLine("✓ compacted the conversation: \(k(n(e, "from"))) → \(k(n(e, "to"))) tokens · earlier turns are in the notes (/notes)")
            compactLine = nil
            // the context meter follows the fold / /clear (the next reply_done sets it again)
            if e["to"] is NSNumber {
                var st = stat ?? Stat(maxCtx: maxCtx)
                st.ctxUsed = n(e, "to")
                stat = st
            }
        case "card":
            // {text, n?}: n is the card's number (shown as "state #N"); an older Elisa omits it and none is shown
            let no = (e["n"] as? NSNumber)?.intValue
            if let no { cardNo = no }
            add(.card(s(e, "text"), no))
        case "check":
            // the fix loop: a new round (or a new command) is a new row; the same round updates its row in place
            let c = CheckRow(e)
            if let r = cur, r < blocks.count, let j = blocks[r].parts.lastIndex(where: { if case .check = $0.kind { return true }; return false }),
               case .check(let old) = blocks[r].parts[j].kind, old.cmd == c.cmd, old.round == c.round, old.running {
                blocks[r].parts[j].kind = .check(c)
            } else {
                add(.check(c))
            }
        case "recalled":
            // memories from earlier sessions that the request carries: say what they are, not just "↺ <text>"
            for h in e["hits"] as? [String] ?? [] { add(.info("↺ recalled from memory: \(h)", .plain)) }
        case "full":
            add(.box("FULL MODEL · \(s(e, "title"))", s(e, "text")))
        case "watch":
            if let dmn = e["domain"] as? String, !dmn.isEmpty { watchLine = "watcher: \(dmn) \(String(format: "%.2f", d(e, "confidence")))" }
            if let a = e["audit"] as? String { auditLine = a }
            if let l = e["line"] as? String { note(l, .plain) }
        case "progress":
            if e["clear"] as? Bool == true { job = nil } else {
                let label = s(e, "label"), done = e["done"] as? String
                // keep the start time while the same job runs: the status bar counts the seconds from it
                let at = job.map { $0.label == label && $0.done == nil ? $0.at : Date() } ?? Date()
                job = (label, (e["frac"] as? NSNumber)?.doubleValue, done, at)
                if Self.isFold(label) {
                    if let done {
                        // a fold that ended without compacting (cancelled, nothing to fold) says so on its line
                        if compactLine != nil { setCompactLine(done.hasPrefix("summary failed") || done.contains("cancelled") ? "⚠︎ compacting: \(done)" : "✓ \(done)") }
                    } else if compactLine == nil {
                        setCompactLine("⟳ compacting the conversation (\(label))… the reply waits for it")
                    }
                }
            }
        case "pick":
            picker = e["sessions"] as? [[String: Any]] ?? []
            pickSel = 0
        case "shade":
            var f = ShadeFrame(state: s(e, "state"), verb: s(e, "verb"), target: s(e, "target"), tone: s(e, "tone"), pop: (e["pop"] as? Bool) ?? false, face: s(e, "face"),
                               fidget: (e["fidget"] as? Bool) ?? false)
            f.since = f.state == shade.state ? shade.since : Date()
            shade = f
        case "activity":
            activityEvent(e)
        case "doc_info":
            let i = DocInfo(e)
            if !i.path.isEmpty { docInfo[i.path] = i }
        case "reply_retracted":
            // the reply said it could not read the attached document; Elisa asks once more and the new reply follows
            if let c = cur, c < blocks.count {
                blocks[c].parts.removeAll { if case .prose = $0.kind { return true }; if case .footer = $0.kind { return true }; return false }
            }
        case "turn_done":
            activityTurnDone()
            // Speak Replies (Speak.swift): the turn's final reply, now that it is complete
            if let c = cur, c < blocks.count { Speak.shared.turnDone(blocks[c].parts) { [weak self] t in self?.blocks.append(Block(kind: .info(t, .warn))) } }
            if let c = cur, c < blocks.count { blocks[c].live = false }
            cur = nil
            approval = nil
        case "exit":
            exited(Int32(n(e, "code")))
        case "plugins", "plugin_preview", "plugin_result", "project_trust":
            PluginStore.shared.event(s(e, "type"), e, self)
        case "sandbox_images", "image_pull":
            SandboxStore.shared.event(s(e, "type"), e, self)
        default:
            break
        }
        if blocks.count > 400 { blocks.removeFirst(blocks.count - 360); cur = nil }
        if scrollHeld && !heldNew && ["delta", "tool_start", "tool_done", "info", "error", "card", "check", "notes", "full", "recalled", "compacted"].contains(s(e, "type")) { heldNew = true }
        scrollTick &+= 1
    }
}

/// A duration for display: under a minute `4.7s` (`places` decimals, 0 gives `5s`); from 60 s `1m 0s` / `12m 5s`;
/// from an hour `1h 2m`. Never `60s`. Same rule as the Rust `ui::dur`; raw seconds stay in machine outputs.
func dur(_ s: Double, _ places: Int = 1) -> String {
    let s = s.isFinite ? max(0, s) : 0
    let shown = String(format: "%.\(places)f", s)
    if (Double(shown) ?? s) < 60 { return shown + "s" }
    let t = max(60, Int(s.rounded(.down)))
    return t >= 3600 ? "\(t / 3600)h \(t % 3600 / 60)m" : "\(t / 60)m \(t % 60)s"
}

/// 1234 -> "1.2K"
/// Elisa's own tools (and its built-in servers' tools): their row labels
let toolLabels: [String: String] = [
    "read": "Read", "write": "Write", "edit": "Edit", "bash": "Bash", "glob": "Glob", "grep": "Grep",
    "outline": "Outline", "read_symbol": "Read symbol", "edit_symbol": "Edit symbol",
    "web_search": "Web search", "web_fetch": "Web page",
    "think": "Thinking", "steps": "Steps", "step_done": "Step done", "calc": "Calc",
    "run_code": "Run code", "docs_pdf": "PDF", "docs_sheet": "Spreadsheet",
    "sec_secrets": "Secret scan", "sec_deps": "Dependency audit", "sec_binary": "Binary check",
    "sec_jwt": "JWT decode",
]

/// a tool row's label: a known Elisa tool by its friendly name; an MCP tool as "server · tool" with the tool part
/// humanized ("time · convert time"). The core exposes an MCP tool as `server_tool`; `server` (when the core sends
/// it) says where the server name ends, else the part before the first `_` is taken as the server
func toolLabel(_ name: String, server: String? = nil) -> String {
    if let l = toolLabels[name] { return l }
    let human = { (t: Substring) in t.replacingOccurrences(of: "_", with: " ").replacingOccurrences(of: "-", with: " ") }
    if let srv = server {
        let rest = name.hasPrefix(srv + "_") ? name.dropFirst(srv.count + 1) : Substring(name)
        return "\(srv) · \(human(rest.isEmpty ? Substring(name) : rest))"
    }
    if let i = name.firstIndex(of: "_"), i != name.startIndex, name.index(after: i) != name.endIndex {
        return "\(name[..<i]) · \(human(name[name.index(after: i)...]))"
    }
    return name.prefix(1).uppercased() + name.dropFirst()
}

func k(_ n: Int) -> String { n < 1000 ? "\(n)" : String(format: "%.1fK", Double(n) / 1000) }
