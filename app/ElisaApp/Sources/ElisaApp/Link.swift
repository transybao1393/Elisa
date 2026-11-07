// The Elisa process: `elisa --serve-jsonl --cwd DIR [--demo-states]`, JSON lines both ways (elisa/src/jsonl.rs).
import Foundation

@MainActor
final class ElisaProcess {
    private var proc: Process?
    private var input: FileHandle?
    private var buf = Data()
    var onEvent: ([String: Any]) -> Void = { _ in }
    var onExit: (Int32) -> Void = { _ in }

    /// $ELISA_BIN, else elisa/target/release/elisa, else elisa/target/debug/elisa (repo root from STREAMDENSE_ROOT or
    /// this source file)
    static func binary() -> URL? {
        let fm = FileManager.default
        if let b = ProcessInfo.processInfo.environment["ELISA_BIN"], !b.isEmpty, fm.isExecutableFile(atPath: b) { return URL(fileURLWithPath: b) }
        for c in ["elisa/target/release/elisa", "elisa/target/debug/elisa"] {
            let u = Kit.repoRoot.appendingPathComponent(c)
            if fm.isExecutableFile(atPath: u.path) { return u }
        }
        return nil
    }

    func start(cwd: String?, demo: Bool, extra: [String]) throws {
        stop()
        guard let bin = ElisaProcess.binary() else {
            throw NSError(domain: "elisa", code: 1, userInfo: [NSLocalizedDescriptionKey: "elisa binary not found: build it (cd elisa && cargo build --release) or set ELISA_BIN"])
        }
        let p = Process()
        p.executableURL = bin
        var args = ["--serve-jsonl"]
        if demo { args.append("--demo-states") }
        if let cwd { args += ["--cwd", cwd] }
        p.arguments = args + extra
        let out = Pipe(), inp = Pipe()
        p.standardOutput = out
        p.standardInput = inp
        p.standardError = FileHandle.standardError
        out.fileHandleForReading.readabilityHandler = { [weak self] h in
            let d = h.availableData
            if d.isEmpty { h.readabilityHandler = nil }
            Task { @MainActor in self?.feed(d) }
        }
        p.terminationHandler = { [weak self] pr in
            let code = pr.terminationStatus
            Task { @MainActor in self?.onExit(code) }
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
        let ts = ISO8601DateFormatter.string(from: Date(), timeZone: .current, formatOptions: [.withTime, .withColonSeparatorInTime, .withFractionalSeconds])
        try? h.write(contentsOf: Data("\(ts) \(dir) \(s)\n".utf8))
    }

    private func feed(_ d: Data) {
        buf.append(d)
        while let i = buf.firstIndex(of: 0x0A) {
            let line = buf[buf.startIndex..<i]
            buf.removeSubrange(buf.startIndex...i)
            Self.log("<-", Data(line))
            guard !line.isEmpty, let j = try? JSONSerialization.jsonObject(with: line) as? [String: Any] else { continue }
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
        send("quit")
        try? input?.close()
        input = nil
        proc = nil
        DispatchQueue.global().asyncAfter(deadline: .now() + 2) { if p.isRunning { p.terminate() } }
    }

    /// at app exit: blocking, bounded
    func stopNow() {
        guard let p = proc else { return }
        send("quit")
        try? input?.close()
        let t0 = Date()
        while p.isRunning && Date().timeIntervalSince(t0) < 1.5 { usleep(20_000) }
        if p.isRunning { p.terminate() }
        proc = nil
    }
}
