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
        /// tools: nil = not checked, [] = none may run (plain chat), else each name pattern (regex) must have run
        var tools: [String]? = nil
        /// restart the core on this conversation (--continue) before sending: the resume check
        var restart = false
        /// documents attached with the message (the paperclip's path: `attach(urls:)`)
        var docs: [String] = []
        /// a file the turn must leave in the project folder
        var fileMade: String? = nil
        /// not a message: "close-chat", "new-chat", "switch-model:NAME", "break-server", "fix-server"
        var action: String? = nil
        /// a slash command: judged on the info lines it prints (it gives no reply)
        var cmd = false
        /// answer the turn's approval "no"
        var deny = false
        /// a file the turn must NOT leave in the project folder
        var fileNot: String? = nil
        /// the reply must NOT match (regex)
        var notExpect: String? = nil
        /// the info lines printed during the step must match (regex)
        var infoExpect: String? = nil
        /// the step should fail visibly (an error or a clear line), not hang or crash
        var expectError = false
    }

    private weak var m: AppModel?
    private var dir = ""
    private var quit = false
    private var steps: [Step] = []
    /// the model this run is for, kept while a step switches to another one
    private var origin: String?

    /// The models Ollama has that can chat (capability "completion"), asked at run time: nothing listed by hand.
    static func chatModels() -> [String] {
        func get(_ path: String, _ body: [String: Any]? = nil) -> [String: Any]? {
            var r = URLRequest(url: URL(string: "http://127.0.0.1:11434" + path)!, timeoutInterval: 5)
            if let b = body { r.httpMethod = "POST"; r.httpBody = try? JSONSerialization.data(withJSONObject: b) }
            var out: [String: Any]?
            let done = DispatchSemaphore(value: 0)
            URLSession.shared.dataTask(with: r) { d, _, _ in
                out = d.flatMap { try? JSONSerialization.jsonObject(with: $0) as? [String: Any] }; done.signal()
            }.resume()
            done.wait()
            return out
        }
        let names = (get("/api/tags")?["models"] as? [[String: Any]] ?? []).compactMap { $0["name"] as? String }
        return names.filter { n in ((get("/api/show", ["model": n])?["capabilities"] as? [String]) ?? []).contains("completion") }
    }
    private var i = -1
    private var t0 = Date()
    private var errors: [String] = []
    private var results: [String] = []
    private var sawCancel = false
    private var done = false
    /// --selftest-chat: the everyday-conversation scenario instead of the feature steps
    private var chat: Bool { CommandLine.arguments.contains("--selftest-chat") }
    private var restarting = false
    /// an action step waiting for the session's "ready"
    private var pending: Step?
    /// blocks in the transcript when the step started (its info lines are the ones after)
    private var blocksAtStart = 0
    private var judged = -1
    /// seconds between steps (--selftest-pace N): time for the person watching to read each reply
    private var pace: Double { Double(CommandLine.arguments.drop(while: { $0 != "--selftest-pace" }).dropFirst().first ?? "") ?? 0 }
    private var more: Bool { CommandLine.arguments.contains("--selftest-more") }

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
        steps.append(Step(name: "interrupt", text: "Write a 400-word story about a fox who learns to sail.", interruptAfter: 1.5, cancelled: true))
        steps.append(Step(name: "after-interrupt", text: "Reply with just the word OK.", expect: "ok"))
        model.onEventHook = { [weak self] e in self?.on(e) }
        // how people really open a chat: a different one each time, not always "hey" (all need no tool)
        var openers = ["hey", "hi Elisa!", "good morning", "chào bạn", "yo, you there?", "hello, can you help me with something?",
                       "xin chào, bạn khỏe không?", "tell me something fun", "what's up?", "hey there, how's it going?",
                       "morning! busy day ahead", "ê, đang rảnh không?"].shuffled()
        let opener = { openers.isEmpty ? "hi" : openers.removeFirst() }
        if chat, let cwd = model.folder {
            try? "Meeting code: ORCHID-42\n".write(toFile: (cwd as NSString).appendingPathComponent("notes.txt"), atomically: true, encoding: .utf8)
            steps = [
                Step(name: "greeting", text: opener(), expect: "\\w", tools: []),
                Step(name: "tell-a-fact", text: "My name is Linh and I live in Hanoi.", expect: "linh|hanoi|nice|got it|noted|hello|hi", tools: []),
                Step(name: "recall-in-chat", text: "What's my name?", expect: "linh", tools: []),
                Step(name: "time", text: "what time is it?", expect: "\\d{1,2}:\\d{2}", tools: []),
                Step(name: "time-follow-up", text: "and in Tokyo?", expect: "\\d{1,2}:\\d{2}", tools: []),
                Step(name: "joke", text: "tell me a short joke", expect: "\\w", tools: []),
                Step(name: "why-follow-up", text: "why is that funny?", expect: "\\w", tools: []),
            ]
            if let a = img1 {
                steps.append(Step(name: "image", text: "what is this?", images: [a], expect: "cartoon|character|creature|cat|animal|mascot|figure", tools: []))
                steps.append(Step(name: "image-follow-up", text: "what colour are the insides of its ears?", expect: "blue|teal|cyan|turquoise", tools: []))
            }
            steps.append(Step(name: "file-intent", text: "read notes.txt and tell me the meeting code", expect: "orchid-42|orchid 42", tools: ["^read$|read_file|^cat$|bash"]))
            steps.append(Step(name: "web-intent", text: "search the web: what is the capital of Australia?", expect: "canberra", tools: ["web"]))
            let kit = "/Users/johnathan/Development/Elisa/elisa/tests/fixtures/docs"
            let pdf = (cwd as NSString).appendingPathComponent("kronos.pdf")
            if !FileManager.default.fileExists(atPath: pdf) {
                try? FileManager.default.copyItem(atPath: "/Users/johnathan/Development/elisa-test-2/.elisa/uploads/1794211488/2508.02739v1.pdf", toPath: pdf)
            }
            if FileManager.default.fileExists(atPath: pdf) {
                steps.append(Step(name: "pdf-upload", text: "what is this paper about?", expect: "kronos|forecast|financial|candlestick|k-line|time series|market", docs: [pdf]))
                steps.append(Step(name: "pdf-follow-up", text: "what kind of data does it work with?", expect: "k-line|candlestick|price|financial|market|stock|ohlc|time series"))
            }
            steps.append(Step(name: "sheet-upload", text: "which region sold the most units?", expect: "north", docs: [kit + "/sales.xlsx"]))
            steps.append(Step(name: "marketing-plan", text: "Create a short marketing plan for a small coffee shop in Hanoi.", expect: "(?s)(audience|customer|target).*(social|instagram|facebook|tiktok|promotion|budget|loyalty)", tools: []))
            steps.append(Step(name: "save-plan", text: "save that plan to marketing-plan.md", expect: "\\w", tools: ["write"], fileMade: "marketing-plan.md"))
            steps.append(Step(name: "resume", text: "What's my name and where do I live?", expect: "(?s)(linh.*hanoi|hanoi.*linh)", tools: [], restart: true))
            steps.append(Step(name: "resume-context", text: "what was the meeting code you found?", expect: "orchid", tools: nil))
            // to another model and back: the steps after it test the model this run is for again
            steps.append(Step(name: "switch-model", text: "", action: "switch-model:@other"))
            steps.append(Step(name: "after-switch", text: "what did I tell you my name was?", expect: "linh"))
            steps.append(Step(name: "switch-back", text: "", action: "switch-model:@back"))
            steps.append(Step(name: "after-switch-back", text: "and where do I live?", expect: "hanoi"))
            steps.append(Step(name: "close-chat", text: "", action: "close-chat"))
            steps.append(Step(name: "new-chat", text: "", action: "new-chat"))
            steps.append(Step(name: "new-chat-hello", text: opener(), expect: "\\w", tools: []))
        }
        if more, let cwd = model.folder {
            let kit = "/Users/johnathan/Development/Elisa/elisa/tests/fixtures/docs"
            let filler = String(repeating: "The quarterly report lists shipping volumes, warehouse counts, staff rotas and supplier names for every region in detail. ", count: 110)
            steps = [
                Step(name: "hey-new-chat", text: opener(), expect: "\\w", tools: []),
                Step(name: "vietnamese", text: "Xin chào, bạn khỏe không?", expect: "[ăâđêôơưạảấầẩẫậắằẳẵặẹẻẽếềểễệỉịọỏốồổỗộớờởỡợụủứừửữựỳỵỷỹ]", tools: []),
                Step(name: "back-to-english-math", text: "Now in English please: what is 15% of 240?", expect: "\\b36\\b", tools: []),
                Step(name: "markdown-table", text: "Show me a markdown table of 3 fruits and their colours.", expect: "\\|.*\\|", tools: []),
                Step(name: "thanks", text: "thanks!", expect: "\\w", tools: []),
                Step(name: "tell-facts", text: "My name is Linh, I live in Hanoi, my favourite colour is green and my meeting code is ORCHID-42.", expect: "\\w", tools: []),
                Step(name: "pin", text: "/pin my favourite colour is green", cmd: true, infoExpect: "(?i)pin"),
                Step(name: "notes-cmd", text: "/notes", cmd: true, infoExpect: "\\w"),
                Step(name: "web-1", text: "search the web: what is the population of Iceland?", expect: "\\d", tools: ["web"]),
                Step(name: "web-2-same", text: "search the web: what is the population of Iceland?", expect: "\\d", tools: ["web"]),
                Step(name: "cache-cmd", text: "/cache", cmd: true, infoExpect: "web_search[^·]*hits [1-9]"),
                Step(name: "deny-write", text: "save the word hello to deny.txt", expect: "\\w", deny: true, fileNot: "deny.txt", notExpect: "(?i)\\b(saved|have written|wrote)\\b"),
                Step(name: "two-docs", text: "what is in these two files?", expect: "(?i)north|south|region|unit|plan|task", docs: [kit + "/sales.xlsx", kit + "/plan.ods"]),
                Step(name: "compact-cmd", text: "/compact", cmd: true, infoExpect: "(?i)compact"),
                Step(name: "after-compact", text: "What's my name and my meeting code?", expect: "(?s)(?i)linh.*orchid|orchid.*linh"),
                Step(name: "fill-1", text: "Keep this in mind, then just reply OK: " + filler, expect: "\\w"),
                Step(name: "fill-2", text: "More of the same, just reply OK: " + filler, expect: "\\w"),
                Step(name: "fill-3", text: "And more, just reply OK: " + filler, expect: "\\w"),
                Step(name: "fill-4", text: "Last one, just reply OK: " + filler, expect: "\\w"),
                Step(name: "after-auto-compact", text: "What's my name, where do I live and what is my meeting code?", expect: "(?s)(?i)(?=.*linh)(?=.*hanoi)(?=.*orchid)"),
                Step(name: "new-chat", text: "", action: "new-chat"),
                Step(name: "continue-last", text: "continue our last conversation", expect: "(?i)linh|hanoi|orchid|green|iceland|report|fruit"),
                Step(name: "memory-colour", text: "What's my favourite colour?", expect: "(?i)green"),
                Step(name: "forget-cmd", text: "/forget", cmd: true, infoExpect: "(?i)forg|delet|unpin|no notes"),
                Step(name: "new-chat-2", text: "", action: "new-chat"),
                Step(name: "after-forget", text: "What's my favourite colour?", expect: "\\w", notExpect: "(?i)\\b(you (like|love)|your favou?rite colou?r is) (green|blue|red|purple|yellow|pink|orange|black|white)\\b|^\\W*(green|blue|red|purple|yellow|pink|orange|black|white)\\W*$"),
                Step(name: "server-down", text: "", action: "break-server"),
                Step(name: "hey-server-down", text: "hey", expectError: true),
                Step(name: "server-back", text: "", action: "fix-server"),
                Step(name: "hey-server-back", text: opener(), expect: "\\w", tools: []),
                Step(name: "news-search", text: "search the web for the latest news about Apple", expect: "\\w", tools: ["web"]),
            ]
            _ = cwd
        }
        after(1800) { self.finish("timed out at step \(self.i + 1) of \(self.steps.count)") }
    }

    private func after(_ s: Double, _ f: @escaping @MainActor () -> Void) {
        DispatchQueue.main.asyncAfter(deadline: .now() + s) { MainActor.assumeIsolated { f() } }
    }

    /// the text of the last reply (prose parts)
    private func lastReply() -> String {
        guard let m, let b = m.blocks.last(where: { if case .reply = $0.kind { return true }; return false }) else { return "" }
        return b.parts.compactMap { if case .prose(let t) = $0.kind { return t }; return nil }.joined().trimmingCharacters(in: .whitespacesAndNewlines)
    }

    /// an action step: close the chat window, start a new chat, or switch the connection's model
    private func act(_ a: String, _ s: Step) {
        guard let m else { return }
        let win = { NSApp.windows.first { $0.title == "Elisa" && $0.isVisible } }
        if a == "close-chat" {
            win()?.performClose(nil)
            after(3) {
                let ok = m.welcome && win() != nil && !m.elisaRunning
                self.record(s, ok: ok, "welcome page \(m.welcome ? "shown" : "NOT shown") · window \(win() != nil ? "visible" : "gone") · session \(m.elisaRunning ? "still running" : "stopped")")
                DebugFrame.capture(String(format: "%02d-%@", self.i + 1, s.name), into: self.dir)
                self.after(0.5) { self.next() }
            }
        } else if a == "new-chat" {
            pending = s
            m.start()
        } else if a == "break-server" || a == "fix-server" {
            let home = ProcessInfo.processInfo.environment["HOME"] ?? NSHomeDirectory()
            let f = (home as NSString).appendingPathComponent(".config/elisa/providers.toml")
            if var t = try? String(contentsOfFile: f, encoding: .utf8) {
                t = a == "break-server" ? t.replacingOccurrences(of: "127.0.0.1:11434", with: "127.0.0.1:9") : t.replacingOccurrences(of: "127.0.0.1:9/", with: "127.0.0.1:11434/")
                try? t.write(toFile: f, atomically: true, encoding: .utf8)
            }
            pending = Step(name: s.name, text: "", action: a)
            m.reconnect()
        } else if a.hasPrefix("switch-model:") {
            var name = String(a.dropFirst("switch-model:".count))
            // @other: a different installed model than the one under test; @back: the one under test again
            // a random other chat model Ollama has (not always the same one: that would bias every run)
            if name == "@other" { origin = m.model; name = SelfTest.chatModels().filter { $0 != m.model }.randomElement() ?? m.model }
            if name == "@back" { name = origin ?? m.model }
            let home = ProcessInfo.processInfo.environment["HOME"] ?? NSHomeDirectory()
            let f = (home as NSString).appendingPathComponent(".config/elisa/providers.toml")
            if var t = try? String(contentsOfFile: f, encoding: .utf8), let r = t.range(of: #"model = "[^"]*""#, options: .regularExpression) {
                t.replaceSubrange(r, with: "model = \"\(name)\"")
                try? t.write(toFile: f, atomically: true, encoding: .utf8)
            }
            pending = Step(name: s.name, text: name, action: a)
            m.reconnect()
        }
    }

    /// an action step's session is ready: judge it
    private func actionReady(_ s: Step) {
        guard let m else { return }
        pending = nil
        if s.action?.hasPrefix("switch-model:") == true {
            let infos = m.blocks.compactMap { if case .info(let t, _) = $0.kind { return t }; return nil }.joined(separator: " ")
            // the LAST capabilities line: after several switches the first one belongs to an earlier model
            let caps = infos.components(separatedBy: "Ollama: native API").last.flatMap { $0.split(separator: "·", maxSplits: 1).last }
                .map { String($0.prefix(120)).trimmingCharacters(in: .whitespaces) } ?? "none shown"
            let ok = m.model == s.text
            record(s, ok: ok, "now \(m.model) · capabilities detected: \(caps)")
        } else if s.action == "break-server" || s.action == "fix-server" {
            record(s, ok: m.phase != .exited || s.action == "break-server", "session on \(m.baseURL)")
        } else {
            record(s, ok: !m.welcome && m.phase == .idle, m.welcome ? "still on the welcome page" : "new chat open on \(m.model)")
        }
        DebugFrame.capture(String(format: "%02d-%@", i + 1, s.name), into: dir)
        after(0.5) { self.next() }
    }

    /// the tools the last reply ran
    private func lastTools() -> [String] {
        guard let m, let b = m.blocks.last(where: { if case .reply = $0.kind { return true }; return false }) else { return [] }
        return b.parts.compactMap { if case .tool(let t) = $0.kind { return t.name }; return nil }
    }

    private func next() {
        guard let m, !done else { return }
        i += 1
        if i >= steps.count { finish(nil); return }
        let s = steps[i]
        errors = []; sawCancel = false; t0 = Date()
        print("selftest: step \(i + 1)/\(steps.count) \(s.name)")
        if s.restart && !restarting {
            // the core restarts on this conversation; the step goes on at its "ready"
            restarting = true
            i -= 1
            m.reconnect()
            return
        }
        restarting = false
        blocksAtStart = m.blocks.count
        if let a = s.action { act(a, s); return }
        if !s.docs.isEmpty {
            m.attach(urls: s.docs.map { URL(fileURLWithPath: $0) })
            if m.documents.count != s.docs.count {
                record(s, ok: false, "\(m.documents.count) of \(s.docs.count) documents attached")
                m.documents = []
                after(0.3) { self.next() }
                return
            }
        }
        if !s.images.isEmpty {
            m.attach(urls: s.images.map { URL(fileURLWithPath: $0) })
            // a model without vision: the right behaviour is a clear refusal, and the follow-up must not invent the picture
            if !m.vision, let why = m.imagesBlocked, m.attachments.isEmpty {
                record(s, ok: true, "refused as it should (no vision): \(why)")
                if i + 1 < steps.count, steps[i + 1].name == "image-follow-up" {
                    steps[i + 1].expect = "can.?t|cannot|no (image|picture|photo)|not (able|see)|unable|didn.?t (get|receive|see)|don.?t see|haven.?t (sent|shared)"
                    steps[i + 1].notExpect = "\\b(blue|teal|cyan|turquoise|pink|red|green|yellow|black|white|purple|orange)\\b"
                }
                after(0.3) { self.next() }
                return
            }
            if m.attachments.count != s.images.count {
                record(s, ok: false, "\(m.attachments.count) of \(s.images.count) images attached (\(m.imagesBlocked ?? "see the window"))")
                m.attachments = []
                after(0.3) { self.next() }
                return
            }
        }
        m.send(s.text)
        // a second picture mid-step: the reply in progress (reasoning, tool rows) as it looks while it runs
        let n = i
        after(2.0) { if self.i == n, !self.done { DebugFrame.capture(String(format: "%02d-%@-live", n + 1, s.name), into: self.dir) } }
        if let k = s.interruptAfter { after(k) { if self.i < self.steps.count, self.steps[self.i].name == s.name { m.interrupt() } } }
    }

    private func record(_ s: Step, ok: Bool, _ detail: String) {
        let line = String(format: "%@ %-16@ %5.1fs · %@", ok ? "PASS" : "FAIL", s.name as NSString, Date().timeIntervalSince(t0), detail)
        results.append(line)
        print("selftest: " + line)
    }

    private func judge() {
        guard i >= 0, i < steps.count, judged != i else { return }
        judged = i
        let s = steps[i]
        let infos = (m?.blocks.dropFirst(blocksAtStart) ?? []).flatMap { b -> [String] in
            var v: [String] = []
            if case .info(let t, _) = b.kind { v.append(t) }
            for p in b.parts { if case .info(let t, _) = p.kind { v.append(t) } }
            return v
        }.joined(separator: " · ")
        if s.cmd || s.expectError {
            var problems: [String] = []
            if s.cmd, let e = s.infoExpect, infos.range(of: e, options: .regularExpression) == nil { problems.append("expected /\(e)/ in what it printed") }
            if s.expectError, errors.isEmpty, infos.range(of: "(?i)connect|refused|reach|server|failed|error", options: .regularExpression) == nil, lastReply().isEmpty == false {
                problems.append("no clear error shown")
            }
            let shown = (errors.first ?? infos).replacingOccurrences(of: "\n", with: " ").prefix(160)
            record(s, ok: problems.isEmpty, (problems.isEmpty ? "" : problems.joined(separator: "; ") + " · ") + "“\(shown)”")
            DebugFrame.capture(String(format: "%02d-%@", i + 1, s.name), into: dir)
            return
        }
        let r = lastReply()
        var problems = errors
        if s.cancelled {
            if !sawCancel { problems.append("the reply was not cancelled") }
        } else if r.isEmpty {
            problems.append("no reply")
        } else if let e = s.expect, r.range(of: e, options: [.regularExpression, .caseInsensitive]) == nil {
            problems.append("expected /\(e)/")
        }
        if let want = s.tools {
            let ran = lastTools()
            if want.isEmpty && !ran.isEmpty { problems.append("plain chat ran tools: \(ran.joined(separator: ", "))") }
            for w in want where !ran.contains(where: { $0.range(of: w, options: .regularExpression) != nil }) {
                problems.append("expected a tool /\(w)/, ran: \(ran.isEmpty ? "none" : ran.joined(separator: ", "))")
            }
        }
        if let n = s.notExpect, r.range(of: n, options: [.regularExpression, .caseInsensitive]) != nil { problems.append("must not say /\(n)/") }
        if let e = s.infoExpect, infos.range(of: e, options: .regularExpression) == nil { problems.append("expected /\(e)/ in the info lines") }
        if let f = s.fileNot, let cwd = m?.folder, FileManager.default.fileExists(atPath: (cwd as NSString).appendingPathComponent(f)) {
            problems.append("\(f) was created although the write was denied")
        }
        if let f = s.fileMade, let cwd = m?.folder, !FileManager.default.fileExists(atPath: (cwd as NSString).appendingPathComponent(f)) {
            problems.append("\(f) was not created")
        }
        // Elisa's own instructions must never show in a reply
        for leak in ["<state>", "</state>", "<=80", "state card", "[state]"] where r.localizedCaseInsensitiveContains(leak) {
            problems.append("leaked \"\(leak)\"")
        }
        let shown = r.replacingOccurrences(of: "\n", with: " ").prefix(150)
        record(s, ok: problems.isEmpty, (problems.isEmpty ? "" : problems.joined(separator: "; ") + " · ") + "“\(shown)”")
        DebugFrame.capture(String(format: "%02d-%@", i + 1, s.name), into: dir)
    }

    private func on(_ e: [String: Any]) {
        guard let m, !done else { return }
        switch e["type"] as? String ?? "" {
        case "banner":
            print("selftest: model \(e["model"] ?? "?") at \(e["base_url"] ?? "?") · vision \(e["vision"] ?? "?") · ctx \(e["max_ctx"] ?? "?")")
        case "ready" where pending != nil:
            if let p = pending { after(1.0) { self.actionReady(p) } }
        case "ready" where i >= 0 && i < steps.count && (steps[i].cmd || steps[i].expectError) && pending == nil && !restarting:
            after(0.8) { guard self.judged != self.i else { return }; self.judge(); self.after(0.4 + self.pace) { self.next() } }
        case "ready" where i < 0 || restarting:
            after(0.5) { self.next() }
        case "approve":
            let deny = i >= 0 && i < steps.count && steps[i].deny
            after(0.2) { if deny { m.answer(word: "no") } else { m.answer(0) } }
        case "pick":
            after(0.2) { m.pick(nil) }
        case "info" where (e["text"] as? String).map({ $0.contains("[cancelled]") || $0.contains("cancelled;") }) == true:
            sawCancel = true
        case "info" where (e["text"] as? String)?.hasPrefix("[stopped") == true:
            errors.append(e["text"] as? String ?? "stopped")
        case "error":
            errors.append("error: \(e["text"] ?? "")")
        case "turn_done" where i >= 0:
            after(0.6) { guard self.judged != self.i else { return }; self.judge(); self.after(0.4 + self.pace) { self.next() } }
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
