// The Elisa process: `elisa --serve-jsonl --cwd DIR`, JSON lines both ways (elisa/src/jsonl.rs).
import Foundation

@MainActor
final class ElisaProcess {
    private var proc: Process?
    private var input: FileHandle?
    private var buf = Data()
    var onEvent: ([String: Any]) -> Void = { _ in }
    var onExit: (Int32) -> Void = { _ in }

    /// $ELISA_BIN, else the core inside Elisa.app (Contents/MacOS/elisa, scripts/package.sh), else
    /// elisa/target/release/elisa, else elisa/target/debug/elisa (repo root from STREAMDENSE_ROOT or this source file)
    static func binary() -> URL? {
        let fm = FileManager.default
        if let b = ProcessInfo.processInfo.environment["ELISA_BIN"], !b.isEmpty, fm.isExecutableFile(atPath: b) { return URL(fileURLWithPath: b) }
        if let b = bundled() { return b }
        for c in ["elisa/target/release/elisa", "elisa/target/debug/elisa"] {
            let u = Kit.repoRoot.appendingPathComponent(c)
            if fm.isExecutableFile(atPath: u.path) { return u }
        }
        return nil
    }

    /// the core next to the app's own executable when it runs from an .app bundle (nil for `swift run` / run.sh)
    static func bundled() -> URL? {
        guard Bundle.main.bundleURL.pathExtension == "app",
              let dir = Bundle.main.executableURL?.resolvingSymlinksInPath().deletingLastPathComponent() else { return nil }
        let u = dir.appendingPathComponent("elisa")
        return FileManager.default.isExecutableFile(atPath: u.path) ? u : nil
    }

    func start(cwd: String?, extra: [String]) throws {
        stop()
        guard let bin = ElisaProcess.binary() else {
            throw NSError(domain: "elisa", code: 1, userInfo: [NSLocalizedDescriptionKey: "elisa binary not found: build it (cd elisa && cargo build --release) or set ELISA_BIN"])
        }
        let p = Process()
        p.executableURL = bin
        var args = ["--serve-jsonl"]
        if let cwd { args += ["--cwd", cwd] }
        p.arguments = args + extra
        let out = Pipe(), inp = Pipe()
        p.standardOutput = out
        p.standardInput = inp
        p.standardError = FileHandle.standardError
        // events and the exit of a core that was replaced (a model switch restarts it: BUGS #64, the old core's exit
        // arrived after the new one started and the window said "elisa exited" while the new core ran) are dropped:
        // only the current process speaks
        out.fileHandleForReading.readabilityHandler = { [weak self] h in
            let d = h.availableData
            if d.isEmpty { h.readabilityHandler = nil }
            Task { @MainActor in if let self, self.proc === p { self.feed(d) } }
        }
        p.terminationHandler = { [weak self] pr in
            let code = pr.terminationStatus
            Task { @MainActor in if let self, self.proc === pr { self.flushReplyLog(); self.onExit(code) } }
        }
        try p.run()
        proc = p
        input = inp.fileHandleForWriting
        buf.removeAll()
    }

    /// ELISA_APP_LOG=path: every line to and from Elisa, timestamped (image data shortened), for debugging
    private static let logHandle: FileHandle? = {
        guard let p = ProcessInfo.processInfo.environment["ELISA_APP_LOG"], !p.isEmpty else { return nil }
        if !FileManager.default.fileExists(atPath: p) { FileManager.default.createFile(atPath: p, contents: nil) }
        let h = FileHandle(forWritingAtPath: p)
        h?.seekToEndOfFile()
        return h
    }()

    static func log(_ dir: String, _ line: Data) {
        guard let h = logHandle else { return }
        var s = String(decoding: line, as: UTF8.self)
        // base64 pictures would swamp the log: keep their length only
        if let re = try? NSRegularExpression(pattern: #""data":"([A-Za-z0-9+/=\\]{64})[A-Za-z0-9+/=\\]*""#) {
            s = re.stringByReplacingMatches(in: s, range: NSRange(s.startIndex..., in: s), withTemplate: #""data":"$1…""#)
        }
        s = LogRedact.redact(s)
        let ts = ISO8601DateFormatter.string(from: Date(), timeZone: .current, formatOptions: [.withTime, .withColonSeparatorInTime, .withFractionalSeconds])
        try? h.write(contentsOf: Data("\(ts) \(dir) \(s)\n".utf8))
    }

    /// the reply streamed so far, for the log only: a secret split across token-sized `delta` events (or restated by the
    /// model) is invisible to a per-line redactor, so deltas are logged as lengths and the joined reply is logged once,
    /// redacted as a whole, when it ends (reply_done / turn_done / error / Elisa exits)
    private var logReply = ""
    private var logDeltas = 0

    private func logIncoming(_ line: Data, _ j: [String: Any]?) {
        guard Self.logHandle != nil else { return }
        let type = j?["type"] as? String
        if type == "delta", let t = j?["text"] as? String {
            logReply += t
            logDeltas += 1
            Self.log("<-", Data(#"{"v":1,"type":"delta","chars":\#(t.count)}"#.utf8))
            return
        }
        if type == "reply_done" || type == "turn_done" || type == "error" { flushReplyLog() }
        Self.log("<-", line)
    }

    /// the joined reply as one log line: `{"log":"reply_text","deltas":N,"text":"..."}` (not a protocol event)
    private func flushReplyLog() {
        guard !logReply.isEmpty else { return }
        let o: [String: Any] = ["log": "reply_text", "deltas": logDeltas, "text": logReply]
        if let d = try? JSONSerialization.data(withJSONObject: o, options: [.sortedKeys, .withoutEscapingSlashes]) { Self.log("<-", d) }
        logReply = ""
        logDeltas = 0
    }

    private func feed(_ d: Data) {
        buf.append(d)
        while let i = buf.firstIndex(of: 0x0A) {
            let line = buf[buf.startIndex..<i]
            buf.removeSubrange(buf.startIndex...i)
            let j = line.isEmpty ? nil : (try? JSONSerialization.jsonObject(with: line)) as? [String: Any]
            logIncoming(Data(line), j)
            guard let j else { continue }
            onEvent(j)
        }
        if buf.count > 8 << 20 { buf.removeAll() }
    }

    func send(_ type: String, _ fields: [String: Any] = [:]) {
        guard let input else { return }
        var o = fields
        o["v"] = 1
        o["type"] = type
        guard var d = try? JSONSerialization.data(withJSONObject: o) else { return }
        Self.log("->", d)
        d.append(0x0A)
        do { try input.write(contentsOf: d) } catch { self.input = nil }
    }

    var running: Bool { proc?.isRunning ?? false }

    /// quit politely, then terminate if Elisa does not exit within 2 s
    func stop() {
        guard let p = proc else { return }
        flushReplyLog()
        send("quit")
        try? input?.close()
        input = nil
        proc = nil
        DispatchQueue.global().asyncAfter(deadline: .now() + 2) { if p.isRunning { p.terminate() } }
    }

    /// at app exit: blocking, bounded
    func stopNow() {
        guard let p = proc else { return }
        flushReplyLog()
        send("quit")
        try? input?.close()
        let t0 = Date()
        while p.isRunning && Date().timeIntervalSince(t0) < 1.5 { usleep(20_000) }
        if p.isRunning { p.terminate() }
        proc = nil
    }
}

/// Secrets out of the ELISA_APP_LOG lines (the log only: what is sent to Elisa is never changed). The rules follow
/// elisa/src/secrets.rs: token shapes (GitHub, Anthropic / OpenAI, AWS key ids, Slack, Google, Stripe, JWT, PEM private
/// keys) and `KEY=value` / `"key": "value"` where the key names a password / secret / token / api key. A log line is
/// JSON, so a value's newlines are `\n` escapes: a token right after one still counts as starting a word, and a
/// value stops at a backslash. Matches become `[redacted:<rule>]`, as in the core.
enum LogRedact {
    enum Check { case none, random }
    /// word start, or right after a JSON `\n` / `\r` / `\t` escape
    private static let b = #"(?:\b|(?<=\\[nrt]))"#
    private static let rules: [(name: String, re: NSRegularExpression, group: Int, check: Check)] = {
        let keys = #"(?:password|passwd|pwd|secret|token|api[_\-]?key|apikey|access[_\-]?key|auth[_\-]?key|private[_\-]?key)"#
        let list: [(String, String, Int, Check)] = [
            ("private-key", #"-----BEGIN[A-Z0-9 ]{0,40}PRIVATE KEY(?: BLOCK)?-----(?s:.*?)(?:-----END[A-Z0-9 ]{0,40}PRIVATE KEY(?: BLOCK)?-----|\z)"#, 0, .none),
            ("aws-access-key", b + #"(?:AKIA|ASIA|ABIA|ACCA|AGPA|AIDA|AROA|ANPA|ANVA|AIPA)[0-9A-Z]{16}\b"#, 0, .none),
            ("github-token", b + #"(?:gh[pousr]_[A-Za-z0-9]{36,255}|github_pat_[A-Za-z0-9_]{22,255})\b"#, 0, .none),
            ("anthropic-key", b + #"sk-ant-[A-Za-z0-9_\-]{20,}"#, 0, .none),
            ("openai-key", b + #"sk-(?:proj-|svcacct-|admin-)?[A-Za-z0-9_\-]{20,}"#, 0, .random),
            ("slack-webhook", #"https://hooks\.slack\.com/services/T[A-Za-z0-9_]+/B[A-Za-z0-9_]+/[A-Za-z0-9_]+"#, 0, .none),
            ("slack-token", b + #"xox[abposr]-[A-Za-z0-9\-]{10,}"#, 0, .none),
            ("google-api-key", b + #"AIza[0-9A-Za-z_\-]{35}"#, 0, .none),
            ("stripe-key", b + #"(?:sk|rk)_live_[0-9A-Za-z]{16,}"#, 0, .none),
            ("jwt", b + #"eyJ[A-Za-z0-9_\-]{8,}\.eyJ[A-Za-z0-9_\-]{8,}\.[A-Za-z0-9_\-]{8,}"#, 0, .none),
            // KEY=value, KEY: value, "key": "value" (also JSON-escaped \"key\": \"value\")
            ("generic-secret", #"(?i)[a-z0-9_.\-]*"# + keys + #"[a-z0-9_.\-]*(?:\\?["'])?[ \t]*(?::=|=>|=|:)[ \t]*(?:\\?["'`])?([^\s"'`#,;\\]{8,200})"#, 1, .random),
        ]
        return list.compactMap { n, pat, g, c in (try? NSRegularExpression(pattern: pat)).map { (n, $0, g, c) } }
    }()

    /// secrets.rs `looks_random`: a real random credential, not a word, placeholder or reference
    static func looksRandom(_ v: String) -> Bool {
        guard v.count >= 8 else { return false }
        let lower = v.lowercased()
        let placeholders = ["xxxx", "****", "....", "changeme", "change_me", "example", "your_", "your-", "<", ">", "${", "{{", "$(", "%(",
                            "redacted", "placeholder", "dummy", "sample", "process.env", "os.environ", "getenv"]
        if v.hasPrefix("$") || placeholders.contains(where: { lower.contains($0) }) || v.contains("(") || v.contains("[") { return false }
        let u = Array(v.utf8)
        let classes = [u.contains { (97...122).contains($0) }, u.contains { (65...90).contains($0) }, u.contains { (48...57).contains($0) },
                       u.contains { !(97...122).contains($0) && !(65...90).contains($0) && !(48...57).contains($0) && !"_-.".utf8.contains($0) }]
        guard classes.filter({ $0 }).count >= 2 else { return false }
        var counts = [Int](repeating: 0, count: 256)
        for c in u { counts[Int(c)] += 1 }
        let n = Double(u.count)
        let h = counts.filter { $0 > 0 }.reduce(0.0) { let p = Double($1) / n; return $0 - p * log2(p) }
        return h >= 3.0
    }

    static func redact(_ s: String) -> String {
        let ns = s as NSString
        var spans: [(NSRange, String)] = []
        for r in rules {
            for m in r.re.matches(in: s, range: NSRange(location: 0, length: ns.length)) {
                let g = m.range(at: r.group)
                guard g.location != NSNotFound, g.length > 0 else { continue }
                if r.check == .random && !looksRandom(ns.substring(with: g)) { continue }
                spans.append((g, r.name))
            }
        }
        guard !spans.isEmpty else { return s }
        // overlaps merged, the first (longest) wins
        spans.sort { $0.0.location != $1.0.location ? $0.0.location < $1.0.location : $0.0.length > $1.0.length }
        var merged: [(NSRange, String)] = []
        for sp in spans {
            if let last = merged.last, sp.0.location < NSMaxRange(last.0) {
                merged[merged.count - 1].0 = NSUnionRange(last.0, sp.0)
            } else { merged.append(sp) }
        }
        let out = NSMutableString(string: s)
        for (r, name) in merged.reversed() { out.replaceCharacters(in: r, with: "[redacted:\(name)]") }
        return out as String
    }
}
