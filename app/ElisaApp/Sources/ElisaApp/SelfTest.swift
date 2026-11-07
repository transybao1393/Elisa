// --selftest DIR [--selftest-image PATH]... [--selftest-quit] (debug): drives a real session through the app's own
// send path, against whatever model the session is connected to, and checks each answer. The steps: a text question,
// an image, a follow-up about that image (is it still in the history?), two images at once, an image with no text,
// a file read through a tool, a reply interrupted half way, and a message after the interrupt. Steps that need a
// picture are skipped without --selftest-image (a second --selftest-image gives the two-image step its second one).
// Approvals are answered yes. 2 s into each step and after it the window goes to DIR as a PNG; at the end the
// transcript goes to DIR/transcript.txt and a PASS / FAIL line per step is printed (and written to DIR/results.txt).
// Without --selftest-quit the window stays open afterwards, so the session can go on by hand.
import AppKit

@MainActor
final class SelfTest {
    static let shared = SelfTest()

    private struct Step {
        let name: String
        var text: String
        var images: [String] = []
        /// interrupt (Esc) this many seconds after sending
        var interruptAfter: Double? = nil
        /// the reply must match (case-insensitive regex); nil = any non-empty reply
        var expect: String? = nil
        /// the turn should end cancelled (the interrupt step)
        var cancelled = false
    }

    private weak var m: AppModel?
    private var dir = ""
    private var quit = false
    private var steps: [Step] = []
    private var i = -1
    private var t0 = Date()
    private var errors: [String] = []
    private var results: [String] = []
    private var sawCancel = false
    private var done = false

    func run(into d: String, images: [String], quit: Bool, model: AppModel) {
        (dir, self.quit, m) = (d, quit, model)
        try? FileManager.default.createDirectory(atPath: d, withIntermediateDirectories: true)
        let img1 = images.first, img2 = images.count > 1 ? images[1] : images.first
        steps = [Step(name: "text", text: "Reply in one short sentence: what is 2 + 3?", expect: "5|five")]
        if let a = img1 {
            steps.append(Step(name: "image", text: "Describe this picture in one sentence.", images: [a], expect: "cartoon|character|creature|cat|animal|mascot|figure|ears|eyes"))
            steps.append(Step(name: "image-follow-up", text: "In the picture I sent, what colour are the insides of its ears? One word.", expect: "blue|teal|cyan|turquoise"))
            if let b = img2 { steps.append(Step(name: "two-images", text: "How many images are in this message? Answer with just the number.", images: [a, b], expect: "2|two")) }
            steps.append(Step(name: "image-only", text: "", images: [a]))
        }
        // a file only this run knows, read through a tool
        let word = ["MANGO", "PAPAYA", "LYCHEE", "GUAVA"].randomElement()!
        if let cwd = model.folder {
            try? "The secret word is \(word).\n".write(toFile: (cwd as NSString).appendingPathComponent("selftest-note.txt"), atomically: true, encoding: .utf8)
            steps.append(Step(name: "tool-read", text: "Use your read tool on the file selftest-note.txt and tell me the secret word in it.", expect: word))
        }
        steps.append(Step(name: "interrupt", text: "Write a 400-word story about a fox who learns to sail.", interruptAfter: 4, cancelled: true))
        steps.append(Step(name: "after-interrupt", text: "Reply with just the word OK.", expect: "ok"))
        model.onEventHook = { [weak self] e in self?.on(e) }
        after(900) { self.finish("timed out at step \(self.i + 1) of \(self.steps.count)") }
    }

    private func after(_ s: Double, _ f: @escaping @MainActor () -> Void) {
        DispatchQueue.main.asyncAfter(deadline: .now() + s) { MainActor.assumeIsolated { f() } }
    }

    /// the text of the last reply (prose parts)
    private func lastReply() -> String {
        guard let m, let b = m.blocks.last(where: { if case .reply = $0.kind { return true }; return false }) else { return "" }
        return b.parts.compactMap { if case .prose(let t) = $0.kind { return t }; return nil }.joined().trimmingCharacters(in: .whitespacesAndNewlines)
    }

    private func next() {
        guard let m, !done else { return }
        i += 1
        if i >= steps.count { finish(nil); return }
        let s = steps[i]
        errors = []; sawCancel = false; t0 = Date()
        print("selftest: step \(i + 1)/\(steps.count) \(s.name)")
        if !s.images.isEmpty {
            m.attach(urls: s.images.map { URL(fileURLWithPath: $0) })
            if m.attachments.count != s.images.count {
                record(s, ok: false, "\(m.attachments.count) of \(s.images.count) images attached (\(m.attachBlocked ?? "see the window"))")
                m.attachments = []
                after(0.3) { self.next() }
                return
            }
        }
        m.send(s.text)
        // a second picture mid-step: the reply in progress (reasoning, tool rows) as it looks while it runs
        let n = i
        after(2.0) { if self.i == n, !self.done { Shots.shared.capture(String(format: "%02d-%@-live", n + 1, s.name), into: self.dir) } }
        if let k = s.interruptAfter { after(k) { if self.i < self.steps.count, self.steps[self.i].name == s.name { m.interrupt() } } }
    }

    private func record(_ s: Step, ok: Bool, _ detail: String) {
        let line = String(format: "%@ %-16@ %5.1fs · %@", ok ? "PASS" : "FAIL", s.name as NSString, Date().timeIntervalSince(t0), detail)
        results.append(line)
        print("selftest: " + line)
    }

    private func judge() {
        guard i >= 0, i < steps.count else { return }
        let s = steps[i]
        let r = lastReply()
        var problems = errors
        if s.cancelled {
            if !sawCancel { problems.append("the reply was not cancelled") }
        } else if r.isEmpty {
            problems.append("no reply")
        } else if let e = s.expect, r.range(of: e, options: [.regularExpression, .caseInsensitive]) == nil {
            problems.append("expected /\(e)/")
        }
        let shown = r.replacingOccurrences(of: "\n", with: " ").prefix(150)
        record(s, ok: problems.isEmpty, (problems.isEmpty ? "" : problems.joined(separator: "; ") + " · ") + "“\(shown)”")
        Shots.shared.capture(String(format: "%02d-%@", i + 1, s.name), into: dir)
    }

    private func on(_ e: [String: Any]) {
        guard let m, !done else { return }
        switch e["type"] as? String ?? "" {
        case "banner":
            print("selftest: model \(e["model"] ?? "?") at \(e["base_url"] ?? "?") · vision \(e["vision"] ?? "?") · ctx \(e["max_ctx"] ?? "?")")
        case "ready" where i < 0:
            after(0.5) { self.next() }
        case "approve":
            after(0.2) { m.answer(0) }
        case "pick":
            after(0.2) { m.pick(nil) }
        case "info" where (e["text"] as? String).map({ $0.contains("[cancelled]") || $0.contains("cancelled;") }) == true:
            sawCancel = true
        case "info" where (e["text"] as? String)?.hasPrefix("[stopped") == true:
            errors.append(e["text"] as? String ?? "stopped")
        case "error":
            errors.append("error: \(e["text"] ?? "")")
        case "turn_done" where i >= 0:
            after(0.6) { self.judge(); self.after(0.4) { self.next() } }
        default: break
        }
    }

    private func finish(_ problem: String?) {
        guard !done, let m else { return }
        done = true
        m.onEventHook = { _ in }
        if let problem { results.append("FAIL " + problem) }
        try? m.debugTranscript().write(toFile: (dir as NSString).appendingPathComponent("transcript.txt"), atomically: true, encoding: .utf8)
        let passed = results.filter { $0.hasPrefix("PASS") }.count
        let summary = "selftest: \(passed)/\(results.count) passed"
        try? (results.joined(separator: "\n") + "\n" + summary + "\n").write(toFile: (dir as NSString).appendingPathComponent("results.txt"), atomically: true, encoding: .utf8)
        print(summary)
        print("selftest: transcript, results and pictures in \(dir)")
        if quit { NSApp.terminate(nil) }
    }
}
