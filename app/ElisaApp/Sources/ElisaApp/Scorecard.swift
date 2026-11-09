// The model scorecard: facts about models measured on this Mac by `elisa eval --suite model` (elisa/src/eval.rs,
// README "elisa eval", schema `elisa-eval/1`). Elisa never picks, defaults to or recommends a model: this shows what
// was measured, sorted the way the user asks (by name until then), and the user chooses.
//
// Runs come from ~/.elisa/evals/history.jsonl (one line per run); a run started here also passes `--save`, so its
// whole report sits in ~/.elisa/evals/reports/ and the history line names it (that is where the failed cases'
// reasons come from). A test runs `elisa eval --suite model --model M --provider NAME --json TMP --save` in the
// background in its own process group; Cancel sends SIGTERM to the group (eval then kills its sessions and removes
// its temp folders), then SIGKILL if it is still there after 3 s.
import AppKit
import SwiftUI

/// one model's results from one run
struct EvalRun: Identifiable {
    struct Suite: Hashable { let name: String; let passed: Int; let cases: Int }
    struct Failure: Hashable { let id: String; let reason: String }
    let id = UUID()
    var model = ""
    var connection = ""
    var baseURL = ""
    var version = ""
    var started = Date(timeIntervalSince1970: 0)
    var passed = 0
    var cases = 0
    var suites: [Suite] = []
    var medianTurn: Double?
    var maxTurn: Double?
    var medianTTFT: Double?
    var toolOK = 0
    var toolChecks = 0
    var emptyTurns = 0
    var emptyRate: Double?
    var loopStops = 0
    var failures: [Failure] = []
    /// the saved report has the reasons; an older history line has only the failed cases' ids
    var reasons = false

    var passRate: Double { cases > 0 ? Double(passed) / Double(cases) : 0 }
    var toolRate: Double? { toolChecks > 0 ? Double(toolOK) / Double(toolChecks) : nil }

    /// the model runs in one history line or report (scripted "fake" aggregates are not models)
    static func parse(_ j: [String: Any], report: [String: Any]?) -> [EvalRun] {
        guard (j["schema"] as? String) == "elisa-eval/1", j["case"] == nil || j["case"] is NSNull else { return [] }
        let aggs = (j["aggregates"] as? [[String: Any]] ?? []).filter { $0["kind"] as? String == "model" }
        let m = j["model"] as? [String: Any]
        let cases = (report?["cases"] ?? j["cases"]) as? [[String: Any]] ?? []
        return aggs.compactMap { a in
            guard let name = a["model"] as? String, !name.isEmpty else { return nil }
            var r = EvalRun()
            r.model = name
            r.connection = (m?["name"] as? String == name ? m?["connection"] as? String : nil) ?? ""
            r.baseURL = a["base_url"] as? String ?? ""
            r.version = j["elisa_version"] as? String ?? ""
            r.started = Date(timeIntervalSince1970: (j["started_at"] as? NSNumber)?.doubleValue ?? 0)
            r.passed = (a["passed"] as? NSNumber)?.intValue ?? 0
            r.cases = (a["cases"] as? NSNumber)?.intValue ?? 0
            r.suites = (a["suites"] as? [String: [String: Any]] ?? [:]).map { k, v in
                Suite(name: k, passed: (v["passed"] as? NSNumber)?.intValue ?? 0, cases: (v["cases"] as? NSNumber)?.intValue ?? 0)
            }.sorted { $0.name < $1.name }
            r.medianTurn = (a["median_turn_s"] as? NSNumber)?.doubleValue
            r.maxTurn = (a["max_turn_s"] as? NSNumber)?.doubleValue
            r.medianTTFT = (a["median_ttft_s"] as? NSNumber)?.doubleValue
            let tc = a["tool_call_accuracy"] as? [String: Any]
            r.toolOK = (tc?["passed"] as? NSNumber)?.intValue ?? 0
            r.toolChecks = (tc?["checks"] as? NSNumber)?.intValue ?? 0
            r.emptyTurns = (a["empty_reply_turns"] as? NSNumber)?.intValue ?? 0
            r.emptyRate = (a["empty_reply_rate"] as? NSNumber)?.doubleValue
            r.loopStops = (a["loop_guard_stops"] as? NSNumber)?.intValue ?? 0
            r.reasons = report != nil
            r.failures = cases.filter { c in
                c["status"] as? String == "fail" && ((c["model"] as? String).map { $0 == name } ?? true) && (c["kind"] as? String).map({ $0 == "model" }) ?? true
            }.map { c in
                let note = (c["notes"] as? [String])?.first ?? ""
                return Failure(id: c["id"] as? String ?? "?", reason: note.split(separator: "\n").first.map(String.init) ?? "")
            }
            return r
        }
    }

    static let day: DateFormatter = { let f = DateFormatter(); f.setLocalizedDateFormatFromTemplate("MMMd"); return f }()
    static let dayTime: DateFormatter = { let f = DateFormatter(); f.dateStyle = .medium; f.timeStyle = .short; return f }()

    static func secs(_ x: Double?) -> String {
        guard let x else { return "—" }
        return x < 10 ? String(format: "%.1f s", x) : String(format: "%.0f s", x)
    }

    /// "8/11 · 1.1 s/turn · tested Nov 8"
    var scoreLine: String {
        var parts = ["\(passed)/\(cases)"]
        if let t = medianTurn { parts.append(String(format: t < 10 ? "%.1f s/turn" : "%.0f s/turn", t)) }
        parts.append("tested \(EvalRun.day.string(from: started))")
        return parts.joined(separator: " · ")
    }
}

/// what a test talks to: a saved connection (`--provider`), or Ollama on this Mac without one
struct EvalTarget: Equatable {
    let model: String
    let args: [String]
    /// where the requests go when that is not this Mac (shown before the test runs)
    let leaves: String?

    /// a saved connection, with its model
    static func connection(_ c: Connection) -> EvalTarget {
        EvalTarget(model: c.model, args: ["--provider", c.name, "--model", c.model], leaves: c.leaves)
    }

    /// a model installed in Ollama: through the saved Ollama connection when there is one, else straight to Ollama
    static func ollama(_ model: String, conns: [Connection]) -> EvalTarget {
        if let c = conns.first(where: { $0.type == "openai" && $0.baseURL.contains(":11434") && !$0.name.isEmpty }) {
            return EvalTarget(model: model, args: ["--provider", c.name, "--model", model], leaves: c.leaves)
        }
        return EvalTarget(model: model, args: ["--base-url", "http://127.0.0.1:11434/v1", "--type", "openai", "--max-ctx", "8192", "--model", model], leaves: nil)
    }
}

@MainActor
final class EvalStore: ObservableObject {
    static let shared = EvalStore()

    struct Job {
        let target: EvalTarget
        let started = Date()
        var pid: pid_t = 0
        var total = 0
        var done = 0
        var failed = 0
        var last = ""
        var cancelling = false
    }

    /// every model's runs, newest first
    @Published private(set) var runs: [String: [EvalRun]] = [:]
    @Published private(set) var job: Job?
    /// the model whose Test was pressed and is waiting for the user to confirm
    @Published var confirming: EvalTarget?
    /// how the last test ended, by model: "finished: 8/11", "cancelled", or why it could not run
    @Published var outcome: [String: (text: String, ok: Bool)] = [:]
    /// the model of the test that ended last (its outcome is the scorecard window's status line)
    @Published var lastEnded: String?
    private var errTail: [String] = []
    private var errHandle: FileHandle?
    private var buf = Data()
    private var loaded = false

    static var home: String { ProcessInfo.processInfo.environment["HOME"].flatMap { $0.isEmpty ? nil : $0 } ?? NSHomeDirectory() }
    static var historyPath: String { (home as NSString).appendingPathComponent(".elisa/evals/history.jsonl") }

    func loadOnce() { if !loaded { load() } }

    /// reads the history (and each line's saved report) off the main thread
    func load() {
        loaded = true
        let path = Self.historyPath
        DispatchQueue.global(qos: .userInitiated).async {
            var by: [String: [EvalRun]] = [:]
            let text = (try? String(contentsOfFile: path, encoding: .utf8)) ?? ""
            for line in text.split(separator: "\n") {
                guard let j = (try? JSONSerialization.jsonObject(with: Data(line.utf8))) as? [String: Any] else { continue }
                var rep: [String: Any]?
                if let rp = j["report"] as? String, let d = FileManager.default.contents(atPath: rp) {
                    rep = (try? JSONSerialization.jsonObject(with: d)) as? [String: Any]
                }
                for r in EvalRun.parse(j, report: rep) { by[r.model, default: []].append(r) }
            }
            for k in by.keys { by[k]?.sort { $0.started > $1.started } }
            Task { @MainActor in self.runs = by }
        }
    }

    func latest(_ model: String) -> EvalRun? { runs[model]?.first ?? runs[model + ":latest"]?.first }
    func scoreLine(_ model: String) -> String? { model.isEmpty ? nil : latest(model)?.scoreLine }
    func running(_ model: String) -> Bool { job?.target.model == model }

    // MARK: running a test

    func start(_ t: EvalTarget) {
        confirming = nil
        guard job == nil else { return }
        guard let bin = ElisaProcess.binary() else {
            outcome[t.model] = ("elisa binary not found: build it (cd elisa && cargo build --release) or set ELISA_BIN", false); return
        }
        outcome[t.model] = nil
        let json = (NSTemporaryDirectory() as NSString).appendingPathComponent("elisa-eval-\(ProcessInfo.processInfo.processIdentifier)-\(Int(Date().timeIntervalSince1970)).json")
        let args = ["eval", "--suite", "model", "--json", json, "--save"] + t.args
        var fds: [Int32] = [0, 0]
        guard pipe(&fds) == 0 else { outcome[t.model] = ("cannot start the test: no pipe", false); return }
        let (rd, wr) = (fds[0], fds[1])
        var fa: posix_spawn_file_actions_t?
        var attr: posix_spawnattr_t?
        posix_spawn_file_actions_init(&fa)
        posix_spawnattr_init(&attr)
        defer { posix_spawn_file_actions_destroy(&fa); posix_spawnattr_destroy(&attr) }
        posix_spawn_file_actions_addopen(&fa, 0, "/dev/null", O_RDONLY, 0)
        posix_spawn_file_actions_addopen(&fa, 1, "/dev/null", O_WRONLY, 0)
        posix_spawn_file_actions_adddup2(&fa, wr, 2)
        posix_spawn_file_actions_addchdir_np(&fa, NSTemporaryDirectory())
        // its own process group (Cancel signals the whole group); only 0, 1, 2 reach it
        posix_spawnattr_setflags(&attr, Int16(POSIX_SPAWN_SETPGROUP | POSIX_SPAWN_CLOEXEC_DEFAULT))
        posix_spawnattr_setpgroup(&attr, 0)
        let argv: [UnsafeMutablePointer<CChar>?] = ([bin.path] + args).map { strdup($0) } + [nil]
        defer { argv.forEach { free($0) } }
        var pid: pid_t = 0
        let rc = posix_spawn(&pid, bin.path, &fa, &attr, argv, environ)
        close(wr)
        guard rc == 0 else { close(rd); outcome[t.model] = ("cannot start elisa: \(String(cString: strerror(rc)))", false); return }
        print("eval: started \(t.model) (pid \(pid)): elisa \(args.joined(separator: " "))")
        var j = Job(target: t)
        j.pid = pid
        job = j
        errTail = []
        buf = Data()
        // kept while the test runs: a released handle closes the pipe, and eval's next progress line would fail
        let h = FileHandle(fileDescriptor: rd, closeOnDealloc: true)
        errHandle = h
        h.readabilityHandler = { [weak self] fh in
            let d = fh.availableData
            if d.isEmpty { fh.readabilityHandler = nil }
            Task { @MainActor in self?.feed(d) }
        }
        DispatchQueue.global().async {
            var st: Int32 = 0
            while waitpid(pid, &st, 0) < 0 && errno == EINTR {}
            let code: Int32 = (st & 0x7f) == 0 ? (st >> 8) & 0xff : 128 + (st & 0x7f)
            Task { @MainActor in self.finished(code, json: json) }
        }
    }

    /// eval's stderr: "elisa eval: 11 case(s) …", then one "  PASS id  2.3s  note" line per case
    private func feed(_ d: Data) {
        buf.append(d)
        while let i = buf.firstIndex(of: 0x0A) {
            let line = String(decoding: buf[buf.startIndex..<i], as: UTF8.self)
            buf.removeSubrange(buf.startIndex...i)
            guard !line.trimmingCharacters(in: .whitespaces).isEmpty else { continue }
            errTail = Array((errTail + [line]).suffix(6))
            guard var j = job else { continue }
            if line.hasPrefix("elisa eval: "), let n = Int(line.dropFirst(12).split(separator: " ").first ?? "") { j.total = n }
            let w = line.split(separator: " ", omittingEmptySubsequences: true)
            if line.hasPrefix("  "), w.count >= 2, ["PASS", "FAIL", "SKIP"].contains(String(w[0])) {
                j.done += 1
                if w[0] == "FAIL" { j.failed += 1 }
                j.last = "\(w[1]) \(w[0].lowercased())"
            }
            job = j
        }
    }

    private func finished(_ code: Int32, json: String) {
        let model = job?.target.model ?? ""
        let cancelled = job?.cancelling == true
        job = nil
        errHandle?.readabilityHandler = nil
        errHandle = nil
        print("eval: \(model) ended, exit \(code)\(cancelled ? " (cancelled)" : "")\(code > 1 ? " · " + errTail.joined(separator: " | ") : "")")
        defer { try? FileManager.default.removeItem(atPath: json); lastEnded = model }
        if cancelled { outcome[model] = ("test cancelled", false); return }
        if code == 0 || code == 1, let d = FileManager.default.contents(atPath: json),
           let j = (try? JSONSerialization.jsonObject(with: d)) as? [String: Any], let r = EvalRun.parse(j, report: j).first {
            outcome[model] = ("test finished: \(r.passed)/\(r.cases) passed", true)
        } else {
            let why = errTail.last { !$0.hasPrefix("  ") } ?? "exit \(code)"
            outcome[model] = ("test did not finish: \(why)", false)
        }
        load()
    }

    func cancel() {
        guard var j = job, j.pid > 0, !j.cancelling else { return }
        j.cancelling = true
        job = j
        let pid = j.pid
        killpg(pid, SIGTERM)
        DispatchQueue.global().asyncAfter(deadline: .now() + 3) {
            // still there (no answer to SIGTERM): the whole group goes
            if kill(pid, 0) == 0 { killpg(pid, SIGKILL) }
        }
    }

    /// at app exit: a running test does not outlive the app
    func stopNow() { if let j = job, j.pid > 0 { killpg(j.pid, SIGTERM) } }
}

// MARK: - the inline Test control (local model list, Model Connections)

/// "Test this model": confirm (how long, what it uses, where it sends), then progress with Cancel, then the outcome.
struct EvalControl: View {
    @ObservedObject var store = EvalStore.shared
    @Environment(\.pal) var p
    let target: EvalTarget
    /// why the test cannot run now (e.g. the connection is not saved), shown instead of the button
    var blocked: String?
    var compact = false

    var body: some View {
        VStack(alignment: .leading, spacing: 6) {
            if let j = store.job, j.target.model == target.model { progress(j) }
            else if store.confirming == target { confirm }
            else {
                HStack(spacing: 10) {
                    if let b = blocked {
                        Text(verbatim: b).font(Fonts.mono(11)).foregroundStyle(p.fg2)
                    } else {
                        Button { store.confirming = target } label: {
                            Text(verbatim: "Test this model").font(Fonts.mono(11, .semibold)).foregroundStyle(store.job == nil ? p.acc : p.fg2)
                        }
                        .buttonStyle(.plain).disabled(store.job != nil)
                        .help(store.job == nil ? "Run Elisa's 11 model test cases with \(target.model)" : "Another test is running")
                    }
                    if let o = store.outcome[target.model] {
                        Text(verbatim: o.text).font(Fonts.mono(11)).foregroundStyle(o.ok ? p.ok : p.warn).lineLimit(2)
                    }
                }
            }
        }
    }

    private var confirm: some View {
        VStack(alignment: .leading, spacing: 7) {
            Text(verbatim: "Runs Elisa's 11 model test cases with \(target.model): small chat, reading, editing and approval tasks in a temporary folder. It takes a few minutes and keeps the model busy; your projects are not touched.")
                .font(Fonts.mono(11)).foregroundStyle(p.tx).fixedSize(horizontal: false, vertical: true)
            if let h = target.leaves {
                Text(verbatim: "Sends 11 short test tasks to \(h) (one or a few requests each). A paid API bills these requests.")
                    .font(Fonts.mono(11, .semibold)).foregroundStyle(p.warn).fixedSize(horizontal: false, vertical: true)
            }
            HStack(spacing: 8) {
                SmallBtn(title: "Run the test", primary: true) { store.start(target) }
                SmallBtn(title: "Not now") { store.confirming = nil }
            }
        }
        .padding(10)
        .background(RoundedRectangle(cornerRadius: 6).fill(p.bg2))
        .overlay(RoundedRectangle(cornerRadius: 6).strokeBorder(p.sep, lineWidth: 1))
    }

    private func progress(_ j: EvalStore.Job) -> some View {
        let f = j.total > 0 ? Double(j.done) / Double(j.total) : 0
        return VStack(alignment: .leading, spacing: 5) {
            HStack(spacing: 8) {
                GeometryReader { g in
                    ZStack(alignment: .leading) {
                        Capsule().fill(p.bg3)
                        Capsule().fill(p.acc).frame(width: max(4, g.size.width * f))
                    }
                }
                .frame(height: 5)
                TimelineView(.periodic(from: j.started, by: 1)) { ctx in
                    let s = Int(ctx.date.timeIntervalSince(j.started))
                    Text(verbatim: "\(j.cancelling ? "cancelling" : "testing") · \(j.done)/\(j.total > 0 ? "\(j.total)" : "…") cases · \(s / 60):\(String(format: "%02d", s % 60))")
                        .font(Fonts.mono(10.5)).foregroundStyle(p.fg2).fixedSize()
                }
                SmallBtn(title: "Cancel") { store.cancel() }
            }
            .animation(.easeOut(duration: 0.3), value: f)
            if !j.last.isEmpty && !compact {
                Text(verbatim: "last: \(j.last)").font(Fonts.mono(10.5)).foregroundStyle(p.fg2).lineLimit(1)
            }
        }
    }
}

// MARK: - the scorecard window

struct ScorecardView: View {
    @ObservedObject var store = EvalStore.shared
    @Environment(\.pal) var p
    enum Col: String, CaseIterable { case model = "Model", passed = "Passed", turn = "Median turn", ttft = "First token", tools = "Tool calls", empty = "Empty replies", loops = "Loop stops", date = "Tested", version = "Elisa" }
    @State private var sort: Col = .model
    @State private var ascending = true
    @State private var open: Set<String> = []

    private func width(_ c: Col) -> CGFloat? {
        switch c { case .model: nil; case .passed: 96; case .turn, .ttft: 92; case .tools: 84; case .empty: 96; case .loops: 82; case .date: 92; case .version: 60 }
    }

    private var rows: [(String, EvalRun)] {
        let items = store.runs.compactMap { k, v in v.first.map { (k, $0) } }
        func key(_ r: EvalRun) -> Double? {
            switch sort {
            case .model, .version: nil
            case .passed: r.passRate
            case .turn: r.medianTurn
            case .ttft: r.medianTTFT
            case .tools: r.toolRate
            case .empty: r.emptyRate
            case .loops: Double(r.loopStops)
            case .date: r.started.timeIntervalSince1970
            }
        }
        return items.sorted { a, b in
            let less: Bool
            switch sort {
            case .model: less = a.0.localizedStandardCompare(b.0) == .orderedAscending
            case .version: less = a.1.version.localizedStandardCompare(b.1.version) == .orderedAscending
            default:
                // a missing value sorts last either way
                switch (key(a.1), key(b.1)) {
                case let (x?, y?): if x == y { return a.0 < b.0 }; less = x < y
                case (nil, _?): return false
                case (_?, nil): return true
                default: return a.0 < b.0
                }
            }
            return ascending ? less : !less
        }
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            VStack(alignment: .leading, spacing: 6) {
                Pixel("Model scorecard", size: 11, color: p.acc)
                Text(verbatim: "Results from your Mac on Elisa's test cases. Small models vary between runs.")
                    .font(Fonts.mono(12)).foregroundStyle(p.tx)
                Text(verbatim: "Test a model from the local model list or from Model Connections. Click a column to sort; click a row for its details.")
                    .font(Fonts.mono(11)).foregroundStyle(p.fg2)
            }
            .padding(.horizontal, 22).padding(.top, 30).padding(.bottom, 14)
            if let j = store.job {
                HStack(spacing: 8) {
                    Text(verbatim: "running: \(j.target.model)").font(Fonts.mono(11, .semibold)).foregroundStyle(p.fg)
                    EvalControl(target: j.target, compact: true)
                }
                .padding(.horizontal, 22).padding(.bottom, 10)
            } else if let name = store.lastEnded, let o = store.outcome[name] {
                // how the last test ended ("test cancelled", "test finished: 9/11 passed", why it did not finish), as
                // the model list says it: the progress row is gone, so this says what happened to it
                HStack(spacing: 8) {
                    Text(verbatim: "last test: \(name)").font(Fonts.mono(11, .semibold)).foregroundStyle(p.fg)
                    Text(verbatim: o.text).font(Fonts.mono(11)).foregroundStyle(o.ok ? p.ok : p.warn).lineLimit(2).textSelection(.enabled)
                    Button { store.lastEnded = nil } label: { Text(verbatim: "×").font(Fonts.mono(12)).foregroundStyle(p.fg2) }
                        .buttonStyle(.plain).help("Dismiss")
                }
                .padding(.horizontal, 22).padding(.bottom, 10)
            }
            Rectangle().fill(p.sep).frame(height: 1)
            header.padding(.horizontal, 14).padding(.vertical, 6).background(p.bg2)
            Rectangle().fill(p.sep).frame(height: 1)
            if store.runs.isEmpty {
                Text(verbatim: "No results yet. Nothing has been tested on this Mac.").font(Fonts.mono(12)).foregroundStyle(p.fg2).padding(22)
                Spacer(minLength: 0)
            } else {
                ScrollView {
                    LazyVStack(alignment: .leading, spacing: 0) {
                        ForEach(rows, id: \.0) { name, r in
                            row(name, r)
                            Rectangle().fill(p.sep.opacity(0.6)).frame(height: 1)
                        }
                    }
                }
            }
            Rectangle().fill(p.sep).frame(height: 1)
            HStack {
                Text(verbatim: (EvalStore.historyPath as NSString).abbreviatingWithTildeInPath).font(Fonts.mono(10.5)).foregroundStyle(p.gut).lineLimit(1).truncationMode(.head)
                Spacer()
                Button { store.load() } label: { Text(verbatim: "↻ reload").font(Fonts.mono(11)).foregroundStyle(p.acc) }.buttonStyle(.plain)
            }
            .padding(.horizontal, 22).padding(.vertical, 9)
        }
        .background(p.bg)
        .ignoresSafeArea()
        .frame(minWidth: 900, minHeight: 420)
        .onAppear { store.load() }
        // --scorecard-open (debug): every row's details shown
        .onReceive(store.$runs) { r in if CommandLine.arguments.contains("--scorecard-open") { open = Set(r.keys) } }
    }

    private var header: some View {
        HStack(spacing: 8) {
            Color.clear.frame(width: 12, height: 1)
            ForEach(Col.allCases, id: \.self) { c in
                Button {
                    if sort == c { ascending.toggle() } else { sort = c; ascending = true }
                } label: {
                    HStack(spacing: 3) {
                        Text(verbatim: c.rawValue).font(Fonts.mono(10.5, sort == c ? .semibold : .regular)).foregroundStyle(sort == c ? p.fg : p.fg2).lineLimit(1)
                        if sort == c { Text(verbatim: ascending ? "▲" : "▼").font(.system(size: 7)).foregroundStyle(p.fg2) }
                    }
                    .frame(maxWidth: width(c) == nil ? .infinity : nil, alignment: c == .model ? .leading : .trailing)
                    .frame(width: width(c), alignment: c == .model ? .leading : .trailing)
                    .contentShape(Rectangle())
                }
                .buttonStyle(.plain)
                .help("Sort by \(c.rawValue.lowercased())")
            }
        }
    }

    private func cell(_ s: String, _ c: Col, color: Color? = nil, sub: String? = nil) -> some View {
        VStack(alignment: c == .model ? .leading : .trailing, spacing: 1) {
            Text(verbatim: s).font(Fonts.mono(12, c == .model ? .semibold : .regular)).foregroundStyle(color ?? p.fg).lineLimit(1)
            if let sub { Text(verbatim: sub).font(Fonts.mono(10)).foregroundStyle(p.fg2).lineLimit(1) }
        }
        .frame(maxWidth: width(c) == nil ? .infinity : nil, alignment: c == .model ? .leading : .trailing)
        .frame(width: width(c), alignment: c == .model ? .leading : .trailing)
    }

    private func row(_ name: String, _ r: EvalRun) -> some View {
        let all = store.runs[name] ?? [r]
        let isOpen = open.contains(name)
        let range: String? = all.count > 1 ? {
            let ps = all.map(\.passed)
            let lo = ps.min() ?? 0, hi = ps.max() ?? 0
            return "\(all.count) runs · \(lo == hi ? "\(lo)" : "\(lo)–\(hi)")/\(r.cases)"
        }() : nil
        return VStack(alignment: .leading, spacing: 0) {
            HStack(spacing: 8) {
                Text(verbatim: isOpen ? "▾" : "▸").font(Fonts.mono(11)).foregroundStyle(p.fg2).frame(width: 12)
                cell(name, .model, sub: r.connection.isEmpty ? nil : "via \(r.connection)")
                cell("\(r.passed)/\(r.cases)", .passed, sub: range)
                cell(EvalRun.secs(r.medianTurn), .turn, sub: r.maxTurn.map { "max \(EvalRun.secs($0))" })
                cell(EvalRun.secs(r.medianTTFT), .ttft)
                cell(r.toolChecks > 0 ? "\(r.toolOK)/\(r.toolChecks)" : "—", .tools)
                cell(r.emptyRate.map { String(format: "%.0f%%", $0 * 100) } ?? "—", .empty, sub: "\(r.emptyTurns) turn\(r.emptyTurns == 1 ? "" : "s")")
                cell("\(r.loopStops)", .loops)
                cell(EvalRun.day.string(from: r.started), .date)
                cell(r.version.isEmpty ? "—" : r.version, .version, color: p.fg2)
            }
            .padding(.horizontal, 14).padding(.vertical, 8)
            .contentShape(Rectangle())
            .onTapGesture { if isOpen { open.remove(name) } else { open.insert(name) } }
            .background(isOpen ? p.sel.opacity(0.5) : .clear)
            if isOpen { detail(r, all).frame(maxWidth: .infinity, alignment: .leading).padding(.leading, 34).padding(.trailing, 14).padding(.bottom, 12).padding(.top, 2).background(p.sel.opacity(0.5)) }
        }
    }

    private func detail(_ r: EvalRun, _ all: [EvalRun]) -> some View {
        VStack(alignment: .leading, spacing: 7) {
            Text(verbatim: "run \(EvalRun.dayTime.string(from: r.started)) · Elisa \(r.version)\(r.baseURL.isEmpty ? "" : " · \(r.baseURL)")")
                .font(Fonts.mono(11)).foregroundStyle(p.fg2).textSelection(.enabled)
            Text(verbatim: "by suite: " + r.suites.map { "\($0.name) \($0.passed)/\($0.cases)" }.joined(separator: " · "))
                .font(Fonts.mono(11)).foregroundStyle(p.tx)
            if r.failures.isEmpty {
                Text(verbatim: "no failed cases").font(Fonts.mono(11)).foregroundStyle(p.ok)
            } else {
                Text(verbatim: "failed cases (\(r.failures.count))").font(Fonts.mono(11, .semibold)).foregroundStyle(p.fg)
                ForEach(r.failures, id: \.self) { f in
                    HStack(alignment: .firstTextBaseline, spacing: 8) {
                        Text(verbatim: f.id).font(Fonts.mono(11, .semibold)).foregroundStyle(p.err).frame(width: 230, alignment: .leading)
                        Text(verbatim: f.reason.isEmpty ? (r.reasons ? "—" : "reason not saved for this run") : f.reason)
                            .font(Fonts.mono(11)).foregroundStyle(p.tx).lineLimit(1).truncationMode(.tail).help(f.reason)
                    }
                }
            }
            if all.count > 1 {
                Text(verbatim: "earlier runs: " + all.dropFirst().prefix(8).map { "\(EvalRun.day.string(from: $0.started)) \($0.passed)/\($0.cases)" }.joined(separator: " · "))
                    .font(Fonts.mono(11)).foregroundStyle(p.fg2)
            }
        }
    }
}

private struct ScorecardRoot: View {
    @ObservedObject var m: AppModel
    var body: some View { ScorecardView().environment(\.pal, m.pal).preferredColorScheme(m.dark ? .dark : .light) }
}

@MainActor
final class ScorecardWindow {
    static let shared = ScorecardWindow()
    private var window: NSWindow?

    func show(_ m: AppModel) {
        EvalStore.shared.load()
        if let w = window { w.contentView = NSHostingView(rootView: ScorecardRoot(m: m)); Front.user(w); return }
        let w = NSWindow(contentRect: NSRect(x: 0, y: 0, width: 1060, height: 560), styleMask: [.titled, .closable, .resizable, .miniaturizable, .fullSizeContentView], backing: .buffered, defer: false)
        w.title = "Model Scorecard"
        w.titlebarAppearsTransparent = true
        w.titleVisibility = .hidden
        w.isReleasedWhenClosed = false
        w.minSize = NSSize(width: 900, height: 420)
        w.contentView = NSHostingView(rootView: ScorecardRoot(m: m))
        if !DebugFrame.place(w) { w.center() }
        Front.user(w)
        window = w
    }
}

/// `--frame X,Y,W,H` (debug): every window the app opens goes there (an off-screen frame keeps test runs off the
/// screen), and `--frames-window TITLE` snapshots that window instead of the main one.
@MainActor
enum DebugFrame {
    static let rect: NSRect? = {
        let a = CommandLine.arguments
        guard let i = a.firstIndex(of: "--frame"), i + 1 < a.count else { return nil }
        let f = a[i + 1].split(separator: ",").compactMap { Double($0) }
        return f.count == 4 ? NSRect(x: f[0], y: f[1], width: f[2], height: f[3]) : nil
    }()

    /// moves `w` to the debug frame's origin, keeping its own size; false = no debug frame
    @discardableResult static func place(_ w: NSWindow) -> Bool {
        guard let r = rect else { return false }
        w.setFrame(NSRect(origin: r.origin, size: w.frame.size), display: false)
        return true
    }

    static func snap(title: String, to path: String) {
        guard let v = NSApp.windows.first(where: { $0.title == title && $0.isVisible })?.contentView,
              let rep = NSBitmapImageRep(bitmapDataPlanes: nil, pixelsWide: Int(v.bounds.width * 2), pixelsHigh: Int(v.bounds.height * 2), bitsPerSample: 8, samplesPerPixel: 4, hasAlpha: true, isPlanar: false, colorSpaceName: .deviceRGB, bytesPerRow: 0, bitsPerPixel: 0) else { return }
        rep.size = v.bounds.size
        v.cacheDisplay(in: v.bounds, to: rep)
        if let d = rep.representation(using: .png, properties: [:]) { try? d.write(to: URL(fileURLWithPath: path)) }
    }
}
