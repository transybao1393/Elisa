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
        /// no approval may be asked during the step (after an "always" answer, or with /yolo on)
        var noAsk = false
        /// scroll the transcript up this many seconds after sending (output then arrives while it is held)
        var scrollUpAfter: Double? = nil
    }

    private weak var m: AppModel?
    private var dir = ""
    private var quit = false
    private var steps: [Step] = []
    /// elisa/tests/fixtures/docs, found from this source file's place (repo/app/ElisaApp/Sources/ElisaApp/SelfTest.swift)
    static let fixtures: String = {
        var u = URL(fileURLWithPath: #filePath)
        for _ in 0..<5 { u.deleteLastPathComponent() }
        return u.appendingPathComponent("elisa/tests/fixtures/docs").path
    }()

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
    /// the transcripts of chats closed during the run (close-chat), kept so transcript.txt holds every chat, not only the last
    private var closedTranscripts = ""
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
        // the settings the app-control steps change go back at the end
        for k in Self.uiDefaults { savedDefaults[k] = UserDefaults.standard.object(forKey: k) ?? NSNull() }
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
            let kit = SelfTest.fixtures
            let pdf = (cwd as NSString).appendingPathComponent("kronos.pdf")
            if !FileManager.default.fileExists(atPath: pdf) {
                if let src = ProcessInfo.processInfo.environment["ELISA_SELFTEST_PDF"] { try? FileManager.default.copyItem(atPath: src, toPath: pdf) }
            }
            if FileManager.default.fileExists(atPath: pdf) {
                steps.append(Step(name: "pdf-upload", text: "what is this paper about?", expect: "kronos|forecast|financial|candlestick|k-line|time series|market", docs: [pdf]))
                steps.append(Step(name: "pdf-follow-up", text: "what kind of data does it work with?", expect: "k-line|candlestick|price|financial|market|stock|ohlc|time series|exchange|asset|trading"))
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
            let kit = SelfTest.fixtures
            let filler = SelfTest.fillMark
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
                // after /forget the honest answer is "I don't know" / "you haven't told me", never a new colour
                Step(name: "after-forget", text: "What's my favourite colour?", expect: "(?i)\\b(don.?t|do not|can.?t|cannot) (know|remember|recall|have)\\b|\\b(haven.?t|have not|didn.?t|did not) (told|tell|said|say|shared|share|mentioned|mention|heard|hear|been told)\\b|\\bnot sure\\b|\\bno idea\\b", notExpect: "(?i)\\b(you (like|love)|your favou?rite colou?r is) (green|blue|red|purple|yellow|pink|orange|black|white)\\b|^\\W*(green|blue|red|purple|yellow|pink|orange|black|white)\\W*$"),
                Step(name: "server-down", text: "", action: "break-server"),
                Step(name: "hey-server-down", text: "hey", expectError: true),
                Step(name: "server-back", text: "", action: "fix-server"),
                Step(name: "hey-server-back", text: opener(), expect: "\\w", tools: []),
                // the headlines found, not where to read them ("…can be found at various sources like…")
                Step(name: "news-search", text: "search the web for the latest news about Apple", expect: "\\w", tools: ["web"], notExpect: "(?i)can be found (at|on|in)|sources like"),
            ]
            _ = cwd
        }
        // --selftest-steps FILE: a conversation from a JSON list instead of the built-in steps; --selftest-files DIR:
        // files copied into the project folder first (what the conversation reads)
        if let file = Self.arg("--selftest-steps") {
            if let src = Self.arg("--selftest-files"), let cwd = model.folder,
               let names = try? FileManager.default.contentsOfDirectory(atPath: src) {
                for n in names { try? FileManager.default.copyItem(atPath: (src as NSString).appendingPathComponent(n), toPath: (cwd as NSString).appendingPathComponent(n)) }
            }
            guard let data = FileManager.default.contents(atPath: file),
                  let list = (try? JSONSerialization.jsonObject(with: data)) as? [[String: Any]], !list.isEmpty else {
                finish("could not read the steps in \(file)"); return
            }
            steps = list.enumerated().map { Self.step($0.element, $0.offset) }
        }
        // --selftest-model NAME: the run first picks its model the way the user does (`/model`), in the one real config
        if let name = Self.arg("--selftest-model") { steps.insert(Step(name: "use-model", text: "", action: "use-model:\(name)"), at: 0) }
        after(max(1800, Double(steps.count) * 180)) { self.finish("timed out at step \(self.i + 1) of \(self.steps.count)") }
    }

    private static func arg(_ name: String) -> String? {
        CommandLine.arguments.drop(while: { $0 != name }).dropFirst().first
    }

    /// one step of a --selftest-steps file: {"name", "text", "images", "docs", "expect", "not_expect", "tools",
    /// "action", "cmd", "deny", "file_made", "file_not", "info_expect", "interrupt_after", "cancelled", "restart",
    /// "expect_error"}; only "text" (or "action") is needed
    private static func step(_ o: [String: Any], _ n: Int) -> Step {
        var s = Step(name: o["name"] as? String ?? "step-\(n + 1)", text: o["text"] as? String ?? "")
        s.images = o["images"] as? [String] ?? []
        s.docs = o["docs"] as? [String] ?? []
        s.expect = o["expect"] as? String
        s.notExpect = o["not_expect"] as? String
        s.tools = o["tools"] as? [String]
        s.action = o["action"] as? String
        s.cmd = o["cmd"] as? Bool ?? s.text.hasPrefix("/")
        s.deny = o["deny"] as? Bool ?? false
        s.fileMade = o["file_made"] as? String
        s.fileNot = o["file_not"] as? String
        s.infoExpect = o["info_expect"] as? String
        s.interruptAfter = o["interrupt_after"] as? Double
        s.cancelled = o["cancelled"] as? Bool ?? false
        s.restart = o["restart"] as? Bool ?? false
        s.expectError = o["expect_error"] as? Bool ?? false
        s.noAsk = o["no_ask"] as? Bool ?? false
        s.scrollUpAfter = o["scroll_up_after"] as? Double
        return s
    }

    private func after(_ s: Double, _ f: @escaping @MainActor () -> Void) {
        DispatchQueue.main.asyncAfter(deadline: .now() + s) { MainActor.assumeIsolated { f() } }
    }

    /// the prose of a reply block
    private func prose(_ b: Block) -> String {
        b.parts.compactMap { if case .prose(let t) = $0.kind { return t }; return nil }.joined().trimmingCharacters(in: .whitespacesAndNewlines)
    }

    /// the reply blocks of this step's turn (since it was sent), newest last; all of them before any step was sent
    private func stepReplies() -> [Block] {
        guard let m else { return [] }
        let from = blocksAtStart <= m.blocks.count ? blocksAtStart : 0
        return m.blocks.dropFirst(from).filter { if case .reply = $0.kind { return true }; return false }
    }

    /// the text of this turn's last assistant reply (prose parts): the newest reply block of the step that has words,
    /// so a block that comes after it in the same turn (an auto-compaction or digest line, a retried round's empty
    /// block) never hides the reply that was shown
    private func lastReply() -> String {
        stepReplies().last(where: { !prose($0).isEmpty }).map(prose) ?? ""
    }

    /// an action step: close the chat window, start a new chat, or switch the connection's model
    private func act(_ a: String, _ s: Step) {
        guard let m else { return }
        if uiAction(a, s) { return }
        let win = { NSApp.windows.first { $0.title == "Elisa" && $0.isVisible } }
        if a == "close-chat" {
            closedTranscripts += m.debugTranscript() + "\n\n=== chat closed (close-chat) ===\n\n"
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
            // this window's session only: a --base-url override on its own restart (the connection's kind and model
            // stay); the shared providers.toml is never written, so another window can't pick up the dead address
            if let k = m.extraArgs.firstIndex(of: "--base-url") { m.extraArgs.removeSubrange(k...min(k + 1, m.extraArgs.count - 1)) }
            if a == "break-server" {
                // the same path on a port nothing listens on (9, discard)
                var u = URLComponents(string: m.baseURL) ?? URLComponents()
                u.scheme = "http"; u.host = "127.0.0.1"; u.port = 9
                m.extraArgs += ["--base-url", u.string ?? "http://127.0.0.1:9/v1"]
            }
            pending = Step(name: s.name, text: "", action: a)
            // on this window's own model: another window saving the shared config meanwhile can't swap it
            m.reconnect(to: m.model)
        } else if a.hasPrefix("use-model:") {
            // the user's own way: `/model NAME` (Model Connections' file change, then the restart on this conversation)
            let name = String(a.dropFirst("use-model:".count))
            if m.model == name { record(s, ok: true, "already on \(name)"); after(0.5) { self.next() }; return }
            pending = Step(name: s.name, text: name, action: "switch-model:\(name)")
            m.useModel(name)
        } else if a.hasPrefix("switch-model:") {
            var name = String(a.dropFirst("switch-model:".count))
            // @other: a different installed model than the one under test; @back: the one under test again
            // a random other chat model Ollama has (not always the same one: that would bias every run)
            if name == "@other" { origin = m.model; name = SelfTest.chatModels().filter { $0 != m.model }.randomElement() ?? m.model }
            if name == "@back" { name = origin ?? m.model }
            if m.model == name { record(s, ok: true, "already on \(name)"); after(0.5) { self.next() }; return }
            // the window's own switch (as /model NAME does), so another window saving the shared config can't pick it
            pending = Step(name: s.name, text: name, action: a)
            m.useModel(name)
        }
        // a switch that never brings a new session up is a failure with the window's own last line, not a hang
        if pending != nil {
            let at = i
            after(60) { [weak self] in
                guard let self, let m = self.m, !self.done, self.i == at, let p = self.pending else { return }
                self.pending = nil
                let last = m.blocks.reversed().compactMap { if case .info(let t, _) = $0.kind { return t }; return nil }.first ?? "nothing shown"
                self.record(p, ok: false, "no new session within 60s on \(m.model) · last line: \(last.prefix(200))")
                DebugFrame.capture(String(format: "%02d-%@", at + 1, p.name), into: self.dir)
                self.after(0.5) { self.next() }
            }
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
        } else if s.action == "conn-save" {
            record(s, ok: !m.welcome && m.phase == .idle, "\(conns.note?.text ?? "saved") · reconnected: \(m.model) at \(m.baseURL) · ctx \(m.maxCtx) · vision \(m.vision) · \(fileMode(conns.path))")
        } else {
            record(s, ok: !m.welcome && m.phase == .idle, m.welcome ? "still on the welcome page" : "new chat open on \(m.model)")
        }
        DebugFrame.capture(String(format: "%02d-%@", i + 1, s.name), into: dir)
        after(0.5) { self.next() }
    }

    /// the tools the last reply ran
    private func lastTools() -> [String] {
        stepReplies().flatMap { b in b.parts.compactMap { if case .tool(let t) = $0.kind { return t.name }; return nil } }
    }

    /// stands in a step's text for the filler, sized when the step is sent (the model's window is known by then)
    static let fillMark = "<<FILL>>"
    /// about 14% of the model's window per message (110 sentences ≈ 2.3K tokens on a 16K window), so four of them
    /// pass the fold point on any window size and auto-compact runs; never more than one message can hold
    static func filler(window: Int) -> String {
        let count = window > 0 ? max(6, window * 110 / 16384) : 110
        return String(repeating: "The quarterly report lists shipping volumes, warehouse counts, staff rotas and supplier names for every region in detail. ", count: count)
    }

    private func next() {
        guard let m, !done else { return }
        i += 1
        if i >= steps.count { finish(nil); return }
        let s = steps[i]
        errors = []; sawCancel = false; t0 = Date()
        uiStep = false; asked = 0; answerNotes = []
        print("selftest: step \(i + 1)/\(steps.count) \(s.name)")
        if s.restart && !restarting {
            // the core restarts on this conversation; the step goes on at its "ready"
            restarting = true
            i -= 1
            // on this window's own model: another window saving the shared config meanwhile can't swap it
            m.reconnect(to: m.model)
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
        m.send(s.text.replacingOccurrences(of: SelfTest.fillMark, with: SelfTest.filler(window: m.maxCtx)))
        // a second picture mid-step: the reply in progress (reasoning, tool rows) as it looks while it runs
        let n = i
        after(2.0) { if self.i == n, !self.done { DebugFrame.capture(String(format: "%02d-%@-live", n + 1, s.name), into: self.dir) } }
        if let k = s.interruptAfter { after(k) { if self.i < self.steps.count, self.steps[self.i].name == s.name { m.interrupt() } } }
        // a command that prints but never says "ready" (Shade's care commands) is judged on what it printed by 25 s
        if s.cmd { after(25) { guard !self.done, self.i == n, self.judged != n else { return }; self.answerNotes.append("no ready within 25 s"); self.judge(); self.after(0.4 + self.pace) { self.next() } } }
        // as a wheel does it (the playback's scroll): small steps, so the hold wins against the streaming reply
        if let k = s.scrollUpAfter { for j in 0..<8 { after(k + Double(j) * 0.08) { if self.i == n, !self.done { ScrollHold.scrollUp(80) } } } }
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
            record(s, ok: problems.isEmpty, (problems.isEmpty ? "" : problems.joined(separator: "; ") + " · ") + (answerNotes.isEmpty ? "" : answerNotes.joined(separator: " · ") + " · ") + "“\(shown)”")
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
        if let w = armed { problems.append("no approval came to answer \(w)"); armed = nil }
        if s.noAsk && asked > 0 { problems.append("asked for approval \(asked)× although it should not ask") }
        problems += answerNotes.filter { $0.hasPrefix("no rule") }
        if s.scrollUpAfter != nil { answerNotes.append("transcript held \(m?.scrollHeld == true) · new output \(m?.heldNew == true)") }
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
        let notes = answerNotes.filter { !$0.hasPrefix("no rule") }
        record(s, ok: problems.isEmpty, (problems.isEmpty ? "" : problems.joined(separator: "; ") + " · ") + (notes.isEmpty ? "" : notes.joined(separator: " · ") + " · ") + "“\(shown)”")
        DebugFrame.capture(String(format: "%02d-%@", i + 1, s.name), into: dir)
    }

    private func on(_ e: [String: Any]) {
        guard let m, !done else { return }
        switch e["type"] as? String ?? "" {
        case "banner":
            print("selftest: model \(e["model"] ?? "?") at \(e["base_url"] ?? "?") · vision \(e["vision"] ?? "?") · ctx \(e["max_ctx"] ?? "?")")
        case "ready" where pending != nil:
            if let p = pending { after(1.0) { self.actionReady(p) } }
        case "ready" where i >= 0 && i < steps.count && (steps[i].cmd || steps[i].expectError) && pending == nil && !restarting && !(uiStep && !steps[i].cmd):
            after(0.8) { guard self.judged != self.i else { return }; self.judge(); self.after(0.4 + self.pace) { self.next() } }
        case "ready" where i < 0 || restarting:
            after(0.5) { self.next() }
        case "approve":
            // a plugin's setup question waits for the plugin-setup step (the setup window's buttons)
            if e["question"] as? Bool == true, i >= 0, i < steps.count, steps[i].action?.hasPrefix("plugin") == true { break }
            let deny = i >= 0 && i < steps.count && steps[i].deny
            asked += 1
            let word = armed
            armed = nil
            after(0.2) {
                if deny { m.answer(word: "no") }
                else if let w = word {
                    // "rule" is offered only when the core names a pattern; without one the box has no such button
                    if w == "hold" {
                        let n = self.i, name = self.i < self.steps.count ? self.steps[self.i].name : "approval"
                        self.after(1.3) {
                            DebugFrame.capture(String(format: "%02d-%@-approval", n + 1, name), into: self.dir)
                            self.answerNotes.append("approval box pictured (\(m.approval?.summary.count ?? 0) chars), then answered yes")
                            m.answer(word: "yes")
                        }
                    } else if w == "rule" && m.approval?.rule == nil { self.answerNotes.append("no rule offered for \(m.approval?.tool ?? "?"); answered yes"); m.answer(word: "yes") }
                    else { self.answerNotes.append("approval \(m.approval?.tool ?? "?") answered \(w)\(m.approval?.rule.map { " (pattern \($0))" } ?? "")"); m.answer(word: w) }
                } else { m.answer(0) }
            }
        case "pick":
            let k = pickIndex
            pickIndex = nil
            if let k { answerNotes.append("picker offered \((e["sessions"] as? [Any])?.count ?? 0) sessions, picked \(k + 1)") }
            after(0.2) { m.pick(k) }
        case "info" where (e["text"] as? String).map({ $0.hasPrefix("Stopped.") || $0.contains("[cancelled]") || $0.contains("cancelled;") }) == true:
            sawCancel = true
        case "info" where (e["text"] as? String)?.hasPrefix("[stopped") == true:
            errors.append(e["text"] as? String ?? "stopped")
        case "error":
            errors.append("error: \(e["text"] ?? "")")
        case "turn_done" where i >= 0 && !(i < steps.count && uiStep && !steps[i].cmd):
            after(0.6) { guard self.judged != self.i else { return }; self.judge(); self.after(0.4 + self.pace) { self.next() } }
        default: break
        }
    }

    // MARK: - app controls (the u* step files)
    //
    // Each action calls the function the real button, menu item or field calls (named beside it); no mouse or key
    // events are made. Each records PASS / FAIL with what it saw (a state value, a window, a file), and pictures the
    // main window before and after. Only the system panels (Open Folder, Attach Files, Add plugin's folder chooser) are
    // skipped: the step hands over the path those panels would return.

    /// the UserDefaults the control steps change: saved at the start of a run, put back at its end
    private static let uiDefaults = ["shadeHidden", "shadeSize", "editsWidth", Speak.key, "voiceLang"]
    private var savedDefaults: [String: Any] = [:]
    /// the clipboard's text before the first copy step (put back at the end)
    private var savedPasteboard: String?
    private var pasteboardTaken = false
    /// an app-control step is running (a turn_done or ready meanwhile is not its result)
    private var uiStep = false
    /// `answer:WORD`: the answer for the next approval; what the approvals of this step were answered
    private var armed: String?
    private var answerNotes: [String] = []
    /// approvals asked during this step
    private var asked = 0
    /// `resume:N`: the session the picker gets (0-based)
    private var pickIndex: Int?

    private var conns: ConnectionStore { ConnectionsWindow.shared.store }

    private static let windowTitles = ["connections": "Model Connections", "scorecard": "Model Scorecard", "plugins": "Plugins",
                                       "sandbox": "Sandbox Images", "setup": "Plugin Setup", "main": "Elisa", "elisa": "Elisa"]
    private func window(_ name: String) -> NSWindow? {
        let t = Self.windowTitles[name.lowercased()] ?? name
        return NSApp.windows.first { $0.title == t && $0.isVisible }
    }

    /// ports a test never talks to: the user's own llama-server (8080), the MLX server (8081) and 8000
    private static func offLimits(_ url: String) -> Bool {
        guard let p = URLComponents(string: url)?.port else { return false }
        return [8080, 8081, 8000].contains(p)
    }

    private static func expand(_ p: String) -> String { (p as NSString).expandingTildeInPath }

    private func fileMode(_ path: String) -> String {
        let a = try? FileManager.default.attributesOfItem(atPath: Self.expand(path))
        return (a?[.posixPermissions] as? NSNumber).map { "mode " + String($0.intValue, radix: 8) } ?? "no file"
    }

    /// records an app-control step, pictures the main window, then the next step
    private func uiDone(_ s: Step, _ ok: Bool, _ detail: String) {
        guard !done, i < steps.count, steps[i].name == s.name else { return }
        record(s, ok: ok, detail)
        DebugFrame.capture(String(format: "%02d-%@", i + 1, s.name), into: dir)
        after(0.4 + min(pace, 1.5)) { self.next() }
    }

    /// after `d` seconds, judge with `f` (unless the run moved on)
    private func later(_ s: Step, _ d: Double, _ f: @escaping @MainActor () -> (Bool, String)) {
        let at = i
        after(d) { guard !self.done, self.i == at else { return }; let (ok, why) = f(); self.uiDone(s, ok, why) }
    }

    /// checks `cond` every 0.3 s until it holds or `timeout` passes, then `then(held)`
    private func poll(_ timeout: Double, _ cond: @escaping @MainActor () -> Bool, then: @escaping @MainActor (Bool) -> Void) {
        let at = i, t0 = Date()
        func tick() {
            guard !done, i == at else { return }
            if cond() { then(true); return }
            if Date().timeIntervalSince(t0) >= timeout { then(false); return }
            after(0.3) { tick() }
        }
        tick()
    }

    /// a slash command started by a control (the slash menu, /resume): judged like a typed command on its "ready",
    /// or after 25 s when none comes
    private func asCommand(_ s: Step, expect: String) {
        steps[i].cmd = true
        if steps[i].infoExpect == nil { steps[i].infoExpect = expect }
        let at = i
        after(25) {
            guard !self.done, self.i == at, self.judged != at else { return }
            self.judge()
            self.after(0.4 + self.pace) { self.next() }
        }
    }

    /// the connection selected in Model Connections, changed the way its field bindings change it
    private func connEdit(_ f: (inout Connection) -> Void) -> Connection? {
        let st = conns
        guard let k = st.items.firstIndex(where: { $0.id == st.selected }) else { return nil }
        let wasActive = st.active == st.items[k].name
        f(&st.items[k])
        if wasActive { st.active = st.items[k].name }
        st.changed()
        return st.items[k]
    }

    private var connSelected: Connection? { conns.items.first { $0.id == conns.selected } }

    /// the text of a value the `state:` and `conn-check:` steps compare
    private func stateValue(_ k: String) -> String? {
        guard let m else { return nil }
        switch k {
        case "model": return m.model
        case "base_url": return m.baseURL
        case "max_ctx": return "\(m.maxCtx)"
        case "vision": return "\(m.vision)"
        case "yolo": return "\(m.yolo)"
        case "dark": return "\(m.dark)"
        case "phase": return "\(m.phase)"
        case "edits": return "\(m.editsShown)"
        case "edit_idx": return "\(m.editIdx)"
        case "shade_hidden": return "\(m.shadeHidden)"
        case "shade_size": return "\(Int(m.shadeSize))"
        case "scroll_held": return "\(m.scrollHeld)"
        case "pasted": return "\(m.pasted != nil)"
        case "attachments": return "\(m.attachments.count + m.documents.count)"
        case "input": return m.input
        case "voice_locale": return m.voiceLocale
        case "speak": return "\(Speak.shared.enabled)"
        case "narrow": return "\(m.narrow)"
        default: return nil
        }
    }

    /// true when `a` is an app-control action (and it is now running)
    private func uiAction(_ a: String, _ s: Step) -> Bool {
        guard let m else { return false }
        let c = a.firstIndex(of: ":")
        let verb = c.map { String(a[..<$0]) } ?? a
        let arg = c.map { String(a[a.index(after: $0)...]) } ?? ""
        let known: Set<String> = ["toggle-theme", "edits", "edit-prev", "edit-next", "shade-hide", "shade-show", "shade-size", "care",
                                  "expand-last", "scroll-up", "jump-new", "paste", "remove-paste", "attach", "remove-attachment", "type",
                                  "slash", "copy-message", "copy-transcript", "answer", "resume", "open", "close", "snap", "frame", "wait",
                                  "conn-add", "conn-set", "conn-select", "conn-active", "conn-test", "conn-save", "conn-delete", "conn-check",
                                  "plugin-add", "plugin", "plugin-setup", "speak", "voice-lang", "eval", "state", "file", "copy"]
        guard known.contains(verb) else { return false }
        uiStep = true
        DebugFrame.capture(String(format: "%02d-%@-before", i + 1, s.name), into: dir)
        let fail = { (why: String) in self.uiDone(s, false, why) }
        switch verb {

        // MARK: main window
        case "toggle-theme":
            // the status bar's theme chip / Elisa > Toggle Theme (⌘⇧L)
            let was = m.dark
            m.toggleTheme()
            later(s, 0.8) { (m.dark != was, "theme \(was ? "dark" : "light") → \(m.dark ? "dark" : "light")") }
        case "edits":
            // the status bar's edits chip / Elisa > Show Edits (⌃O)
            let was = m.editsShown
            // edits:open / edits:close leave it as it is when it already is
            if (arg == "open" && was) || (arg == "close" && !was) { later(s, 0.3) { (true, "edits already \(was ? "shown" : "hidden")") }; break }
            m.toggleEdits()
            later(s, 0.8) { (m.editsShown != was, "edits \(m.editsShown ? "shown" : "hidden") as \(m.narrow ? "an overlay (narrow window)" : "the side panel") · \(m.edits.count) edit(s)") }
        case "edit-prev", "edit-next":
            // the edit panel's ‹ › buttons / Elisa > Previous Edit, Next Edit
            let was = m.editIdx, n = m.edits.count
            if verb == "edit-prev" { m.editPrev() } else { m.editNext() }
            let want = verb == "edit-prev" ? max(0, was - 1) : min(max(0, n - 1), was + 1)
            later(s, 0.6) { (n > 0 && m.editIdx == want, n == 0 ? "no edits to step through" : "edit \(was + 1) → \(m.editIdx + 1) of \(n): \(m.edits[min(m.editIdx, n - 1)].path)") }
        case "shade-hide", "shade-show":
            // the tray's hide button / the status bar's Shade face (and Elisa > Hide Shade, Show Shade)
            let hide = verb == "shade-hide"
            if hide { m.shadeHidden = true } else if m.shadeHidden { m.shadeHidden = false }
            later(s, 0.8) { (m.shadeHidden == hide, "Shade \(m.shadeHidden ? "hidden" : "shown") · saved setting \(UserDefaults.standard.bool(forKey: "shadeHidden"))") }
        case "shade-size":
            // the tray's resize handle (drag / its accessibility steps) or `step`: Elisa > Shade Size (+10) (⌃S)
            let r = m.shadeRange, was = m.shadeSize
            let want: CGFloat
            if arg == "step" { want = was >= r.upperBound ? r.lowerBound : min(was + 10, r.upperBound); m.stepShadeSize() }
            else if let v = Double(arg) { want = min(max(CGFloat(v).rounded(), r.lowerBound), r.upperBound); m.setShadeSize(CGFloat(v)) }
            else { fail("shade-size:N or shade-size:step"); return true }
            later(s, 0.8) { (m.shadeSize == want, "Shade \(Int(was)) → \(Int(m.shadeSize)) px (range \(Int(r.lowerBound))–\(Int(r.upperBound)))") }
        case "care":
            // the tray's care buttons and a click on Shade (poke) / Elisa > Give Water, Give a Snack, Give a Toy, Poke
            guard ["water", "food", "toy", "poke"].contains(arg) else { fail("care:water|food|toy|poke"); return true }
            let before = m.shade
            m.care(arg)
            poll(5, { m.shade != before }) { ok in
                self.uiDone(s, ok, ok ? "Shade \(before.state) → \(m.shade.state)\(m.shade.verb.isEmpty ? "" : " · \(m.shade.verb)")\(m.shade.pop ? " · pop" : "")" : "no Shade change within 5 s (still \(m.shade.state))")
            }
        case "expand-last":
            // the newest reply's step list (its summary line), else the newest tool row's ▸ expand
            var id: UUID?, what = ""
            if let b = m.blocks.last(where: { !$0.acts.isEmpty }) { id = b.id; what = "step list “\(b.actSummary ?? "")”" }
            else {
                for b in m.blocks.reversed() {
                    if let p = b.parts.last(where: { if case .tool = $0.kind { return true }; return false }) { id = p.id; what = "tool row"; break }
                }
            }
            guard let id else { fail("nothing to expand (no tool row or step list yet)"); return true }
            let was = m.expanded.contains(id)
            m.toggleExpand(id)
            later(s, 0.8) { (m.expanded.contains(id) != was, "\(what) \(m.expanded.contains(id) ? "expanded" : "collapsed")") }
        case "scroll-up":
            // a scroll wheel's movement on the transcript (the playback's own scroll)
            guard ScrollHold.view != nil, let pt = Double(arg.isEmpty ? "600" : arg) else { fail("no transcript scroll view"); return true }
            ScrollHold.scrollUp(CGFloat(pt))
            later(s, 0.8) { (m.scrollHeld, "transcript held \(m.scrollHeld) · \(m.heldLines) line(s) below · pill \(m.scrollHeld ? "shown" : "not shown")") }
        case "jump-new":
            // the "↓ new output" pill
            let held = m.scrollHeld, fresh = m.heldNew
            guard held, let jump = ScrollHold.jump else { fail("the pill is not showing (the transcript is not held)"); return true }
            jump()
            later(s, 0.8) { (!m.scrollHeld && fresh, "pill said \(fresh ? "new output" : "only lines below (no new output while held)") · after the jump held \(m.scrollHeld)") }
        case "paste":
            // ⌘V in the input box: a long text becomes the pasted-text chip (m.paste)
            guard let t = try? String(contentsOfFile: Self.expand(arg), encoding: .utf8) else { fail("cannot read \(arg)"); return true }
            let long = PasteText.long(t)
            m.paste(t)
            later(s, 0.8) { (long ? m.pasted != nil : m.input.contains(t), "\(PasteText.size(t)), \(PasteText.lineCount(t)) lines → \(m.pasted != nil ? "pasted-text chip" : "the input field")") }
        case "remove-paste":
            // the chip's ×
            guard m.pasted != nil else { fail("no pasted-text chip"); return true }
            m.pasted = nil
            later(s, 0.6) { (m.pasted == nil, "chip \(m.pasted == nil ? "removed" : "still there")") }
        case "attach":
            // Attach Files… (⌘⇧A) after its panel: the chosen file goes to attach(urls:)
            let p = Self.expand(arg)
            guard FileManager.default.fileExists(atPath: p) else { fail("no file \(arg)"); return true }
            let n = m.attachments.count + m.documents.count
            m.attach(urls: [URL(fileURLWithPath: p)])
            later(s, 1.0) { (m.attachments.count + m.documents.count == n + 1, "\(m.attachments.count) picture(s), \(m.documents.count) document(s) waiting\(m.imagesBlocked.map { " · \($0)" } ?? "")") }
        case "remove-attachment":
            // the attachment chip's ×
            guard let id = m.documents.last?.id ?? m.attachments.last?.id else { fail("nothing attached"); return true }
            let n = m.attachments.count + m.documents.count
            m.removeAttachment(id)
            later(s, 0.6) { (m.attachments.count + m.documents.count == n - 1, "\(n) → \(m.attachments.count + m.documents.count) attached") }
        case "type":
            // what the input field's binding sets as the person types (the slash menu opens on a bare /word)
            m.input = arg
            m.slashSel = 0
            m.typing(!arg.isEmpty)
            later(s, 0.6) { (m.input == arg, "input “\(m.input)” · slash menu \(m.slashItems.isEmpty ? "closed" : "open with \(m.slashItems.count) item(s): \(m.slashItems.prefix(6).map(\.0).joined(separator: " "))")") }
        case "slash":
            // a slash menu row (click, or ⏎ on the highlighted one): runSlash
            let items = m.slashItems
            guard let n = Int(arg), n >= 0, n < items.count else { fail("the slash menu has \(items.count) item(s) for “\(m.input)”"); return true }
            let cmd = items[n].0
            guard !["/exit", "/quit", "/model"].contains(cmd) else { fail("\(cmd) is not run by a test (it quits or opens a window)"); return true }
            steps[i].text = cmd
            answerNotes.append("menu row \(n + 1): \(cmd)")
            asCommand(s, expect: "\\w")
            m.runSlash(n)
        case "copy-message", "copy-transcript":
            // right-click → Copy Message on the newest reply / Edit > Copy Transcript for Debugging (⌥⌘C)
            if !pasteboardTaken { savedPasteboard = NSPasteboard.general.string(forType: .string); pasteboardTaken = true }
            let want: String
            if verb == "copy-transcript" { m.copyTranscript(); want = "" } else {
                guard let b = m.blocks.last(where: { if case .reply = $0.kind { return !$0.parts.isEmpty }; return false }) else { fail("no reply to copy"); return true }
                want = m.plainText(b)
                Clipboard.put(want)
            }
            // (the newest reply with words in the whole chat: a copy step sends no message of its own)
            let reply = m.blocks.last(where: { if case .reply = $0.kind { return !prose($0).isEmpty }; return false }).map(prose) ?? ""
            later(s, 0.5) {
                let got = NSPasteboard.general.string(forType: .string) ?? ""
                let ok = verb == "copy-transcript" ? got.hasPrefix("Elisa transcript") && got.contains("model: \(m.model)") : got == want && got.contains(String(reply.prefix(40)))
                return (ok && !got.isEmpty, "clipboard: \(got.count) chars “\(got.prefix(90).replacingOccurrences(of: "\n", with: " "))”")
            }

        // MARK: input
        case "answer":
            // the approval box's answers (Yes / always / Yes, and always allow this pattern / No): the next approval
            // hold: the box stays up 1.5 s for a picture, then yes
            guard ["yes", "always", "rule", "no", "hold"].contains(arg) else { fail("answer:yes|always|rule|no|hold"); return true }
            armed = arg
            later(s, 0.2) { (true, "the next approval is answered \(arg) (judged in the next step)") }
        case "resume":
            // /resume, then the session picker's row N
            pickIndex = max(0, (Int(arg) ?? 1) - 1)
            steps[i].text = "/resume"
            asCommand(s, expect: "(?i)resumed")
            m.send("/resume")

        // MARK: windows
        case "open":
            // Elisa > Model Connections… (⌘,) / Model Scorecard… / Plugins… / Sandbox Images…
            switch arg {
            case "connections": ConnectionsWindow.shared.show(m)
            case "scorecard": ScorecardWindow.shared.show(m)
            case "plugins": PluginsWindow.shared.show(m)
            case "sandbox": SandboxWindow.shared.show(m)
            default: fail("open:connections|scorecard|plugins|sandbox"); return true
            }
            let file = (dir as NSString).appendingPathComponent(String(format: "%02d-%@-window.png", i + 1, s.name))
            later(s, 2.5) {
                let w = self.window(arg)
                if w != nil { DebugFrame.snap(title: w!.title, to: file) }
                return (w != nil, w.map { "“\($0.title)” open, \(Int($0.frame.width))×\(Int($0.frame.height))" + (arg == "connections" ? " · \(self.conns.items.count) connection(s), active “\(self.conns.active)”" : "") } ?? "no window")
            }
        case "close":
            // the window's close button
            guard let w = window(arg) else { fail("no open window “\(arg)”"); return true }
            guard w.title != "Elisa" else { fail("the chat window is closed with close-chat"); return true }
            w.performClose(nil)
            later(s, 0.8) { (self.window(arg) == nil, "“\(w.title)” \(self.window(arg) == nil ? "closed" : "still open")") }
        case "snap":
            let t = Self.windowTitles[arg.lowercased()] ?? arg
            let name = String(format: "%02d-%@-%@", i + 1, s.name, t.replacingOccurrences(of: " ", with: "_"))
            let file = (dir as NSString).appendingPathComponent(name + ".png")
            if t == "Elisa" { DebugFrame.capture(name, into: dir) } else { DebugFrame.snap(title: t, to: file) }
            later(s, 0.3) {
                let n = (try? FileManager.default.attributesOfItem(atPath: file))?[.size] as? NSNumber
                return ((n?.intValue ?? 0) > 0, n.map { "\(name).png, \($0.intValue / 1024) KB" } ?? "no picture (is “\(t)” open?)")
            }
        case "frame":
            // the main window resized (as dragging its corner does) for layout pictures: W×H is the content size
            let wh = arg.split(separator: ",").compactMap { Double($0) }
            guard wh.count == 2, let w = window("main") else { fail("frame:W,H with the chat window open"); return true }
            let f = w.frame
            let nf = w.frameRect(forContentRect: NSRect(x: 0, y: 0, width: wh[0], height: wh[1]))
            w.setFrame(NSRect(x: f.minX, y: f.maxY - nf.height, width: nf.width, height: nf.height), display: true)
            later(s, 1.2) {
                let c = w.contentRect(forFrameRect: w.frame).size
                // asked below the window's minimum: it stops there (said in the detail), as a drag would
                let floor = c.height > wh[1] + 1 && c.height - wh[1] < 40
                return (abs(c.width - wh[0]) < 2 && (abs(c.height - wh[1]) < 2 || floor), "content \(Int(c.width))×\(Int(c.height)) (asked \(Int(wh[0]))×\(Int(wh[1]))\(floor ? "; the window's minimum height is \(Int(c.height))" : "")) · narrow \(m.narrow)")
            }
        case "wait":
            later(s, Double(arg) ?? 1) { (true, "waited \(arg) s") }

        // MARK: Model Connections (the window's own store; open:connections first)
        case "conn-add":
            // + Add connection → a preset row, then the Server URL typed before its 0.45 s model probe
            guard window("connections") != nil else { fail("open:connections first"); return true }
            let c2 = arg.firstIndex(of: ":")
            let name = c2.map { String(arg[..<$0]) } ?? arg
            let url = c2.map { String(arg[arg.index(after: $0)...]) } ?? ""
            guard name != "mlx" else { fail("the MLX preset is never added in a test (it probes 127.0.0.1:8081)"); return true }
            guard let pr = conns.menuItems.first(where: { $0.name == name }) else { fail("no preset \(name)"); return true }
            let target = url.isEmpty ? pr.base : url
            guard !Self.offLimits(target) else { fail("\(target) is off limits for tests"); return true }
            conns.menuOpen = false
            conns.add(pr)
            if !url.isEmpty { _ = connEdit { $0.baseURL = url } }
            later(s, 1.0) {
                let c = self.connSelected
                return (c?.baseURL == target, c.map { "added “\($0.name)” at \($0.baseURL) · unsaved \(self.conns.dirty)" } ?? "nothing selected")
            }
        case "conn-select":
            // a connection row in the sidebar
            guard let c = conns.items.first(where: { $0.name == arg }) else { fail("no connection \(arg)"); return true }
            guard !Self.offLimits(c.baseURL) else { fail("\(c.baseURL) is off limits for tests (selecting it asks its server)"); return true }
            conns.selected = c.id
            conns.note = nil
            later(s, 0.6) { (self.connSelected?.name == arg, "selected \(self.connSelected?.name ?? "none")") }
        case "conn-set":
            // the detail pane's fields and buttons: Name, Server URL, Model, Context size, Images, key fields, Remove
            let kv = arg.split(separator: "=", maxSplits: 1).map(String.init)
            let k = kv.first ?? "", v = kv.count > 1 ? kv[1] : ""
            var shown = v
            let r: Connection?
            switch k {
            case "name": r = connEdit { $0.name = v }
            case "model": r = connEdit { $0.model = v }
            case "base_url":
                guard !Self.offLimits(v) else { fail("\(v) is off limits for tests"); return true }
                r = connEdit { $0.baseURL = v }
            case "max_ctx":
                let n = Connection.contextSizes.first { $0.0.lowercased() == v.lowercased() }?.1 ?? v.filter(\.isNumber)
                shown = n
                r = connEdit { $0.maxCtx = n }
            case "vision": r = connEdit { $0.vision = v == "on" ? true : v == "off" ? false : nil }
            case "api_key_env": r = connEdit { $0.keyEnv = v }
            case "api_key": shown = "(\(v.count) characters)"; r = connEdit { $0.newKey = v }
            case "remove_key": shown = "Remove"; r = connEdit { $0.removeKey = true }
            default: fail("conn-set: name|model|base_url|max_ctx|vision|api_key_env|api_key|remove_key"); return true
            }
            guard let c = r else { fail("no connection selected"); return true }
            later(s, 0.6) { (self.conns.dirty, "\(c.name): \(k) = \(shown) · unsaved \(self.conns.dirty)") }
        case "conn-active":
            // the dot beside a connection (config.toml's row for none)
            let name = arg == "config.toml" ? "" : arg
            guard name.isEmpty || conns.items.contains(where: { $0.name == name }) else { fail("no connection \(arg)"); return true }
            conns.active = name
            conns.changed()
            later(s, 0.6) { (self.conns.active == name, "Elisa uses “\(self.conns.active.isEmpty ? "config.toml" : self.conns.active)” · unsaved \(self.conns.dirty)") }
        case "conn-test":
            // Test connection
            guard let c = connSelected else { fail("no connection selected"); return true }
            conns.test(c)
            poll(95, { !self.conns.busy && self.conns.note?.text.hasPrefix("testing") == false }) { ended in
                let n = self.conns.note
                let ok = ended && (n?.ok ?? false) != s.expectError
                self.uiDone(s, ok, "\(c.name): \(n?.ok == true ? "ok" : "failed") · \(n?.text.replacingOccurrences(of: "\n", with: " ").prefix(200) ?? "no answer")")
            }
        case "conn-save":
            // Save and reconnect: the file is written, then the session restarts on the saved connection
            guard conns.dirty, !conns.busy else { fail("Save is disabled: nothing changed"); return true }
            let st = conns
            st.save {
                if m.folder != nil {
                    self.pending = Step(name: s.name, text: "", action: "conn-save")
                    m.reconnect()
                } else { m.model = "" }
            }
            let at = i
            poll(30, { !st.busy && st.note != nil }) { _ in
                if st.note?.ok != true { self.uiDone(s, false, "not saved: \(st.note?.text ?? "no answer")"); return }
                self.after(60) {
                    guard !self.done, self.i == at, let p = self.pending, p.action == "conn-save" else { return }
                    self.pending = nil
                    self.uiDone(s, false, "saved, but no new session within 60 s on \(m.model)")
                }
            }
        case "conn-delete":
            // select the row, then Delete
            guard let c = conns.items.first(where: { $0.name == arg }) else { fail("no connection \(arg)"); return true }
            conns.selected = c.id
            conns.delete(c.id)
            later(s, 0.6) { (!self.conns.items.contains { $0.name == arg }, "\(arg) \(self.conns.items.contains { $0.name == arg } ? "still listed" : "deleted") · unsaved \(self.conns.dirty)") }
        case "conn-check":
            // what the window shows after a save (it reads the file back with --providers-json)
            let kv = arg.split(separator: "=", maxSplits: 1).map(String.init)
            let k = kv.first ?? "", v = kv.count > 1 ? kv[1] : ""
            let c = connSelected
            switch k {
            case "has_key": later(s, 0.3) { ("\(c?.hasKey ?? false)" == v, "\(c?.name ?? "none"): has_key \(c?.hasKey ?? false) · \(self.fileMode(self.conns.path))") }
            case "name": later(s, 0.3) { (self.conns.items.contains { $0.name == v }, "connections: \(self.conns.items.map(\.name).joined(separator: ", "))") }
            case "max_ctx": later(s, 0.3) { (c?.maxCtx == v, "\(c?.name ?? "none"): max_ctx \(c?.maxCtx ?? "")") }
            case "model": later(s, 0.3) { (c?.model == v, "\(c?.name ?? "none"): model \(c?.model ?? "")") }
            case "vision": later(s, 0.3) { (c.map { $0.vision == nil ? "auto" : $0.vision! ? "on" : "off" } == v, "\(c?.name ?? "none"): vision \(c?.vision.map { "\($0)" } ?? "auto")") }
            case "active": later(s, 0.3) { (self.conns.active == v, "active “\(self.conns.active)”") }
            case "selected": later(s, 0.3) { (c?.name == v, "selected “\(c?.name ?? "none")” (list: \(self.conns.items.map(\.name).joined(separator: ", ")))") }
            case "dirty": later(s, 0.3) { ("\(self.conns.dirty)" == v, "unsaved changes \(self.conns.dirty) · Save \(self.conns.dirty ? "enabled" : "disabled")") }
            case "mode": later(s, 0.3) { let fm = self.fileMode(self.conns.path); return (fm == "mode " + v, "\(self.conns.path): \(fm)") }
            case "key_hidden":
                // the key is never read back: not in --providers-json, not in the window's state, not in the chat
                guard let bin = ElisaProcess.binary() else { fail("no elisa binary"); return true }
                let at = i
                DispatchQueue.global(qos: .userInitiated).async {
                    let j = ConnectionStore.call(bin, ["--providers-json"], nil)
                    let text = (try? JSONSerialization.data(withJSONObject: j)).map { String(decoding: $0, as: UTF8.self) } ?? ""
                    DispatchQueue.main.async {
                        MainActor.assumeIsolated {
                            guard self.i == at else { return }
                            let inJSON = text.contains(v), inState = self.conns.items.contains { $0.newKey == v }, inChat = m.debugTranscript().contains(v)
                            self.uiDone(s, !v.isEmpty && !inJSON && !inState && !inChat, "key in --providers-json \(inJSON) · in the window's fields \(inState) · in the chat \(inChat)")
                        }
                    }
                }
            default: fail("conn-check: has_key|name|max_ctx|model|vision|active|selected|dirty|mode|key_hidden")
            }

        // MARK: plugins (the session's plugin ops, as the Plugins and Plugin Setup windows send them)
        case "plugin-add":
            // Add plugin…: the folder its open panel returns goes to add(path:)
            let p = Self.expand(arg)
            guard FileManager.default.fileExists(atPath: (p as NSString).appendingPathComponent("plugin.toml")) else { fail("no plugin.toml in \(arg)"); return true }
            let st = PluginStore.shared
            st.add(path: p, m)
            poll(60, { st.preview != nil || m.approval?.question == true || (st.status.map { !$0.text.hasPrefix("sent") } ?? false) }) { _ in
                let pv = st.preview
                let servers = pv?.servers.map { "\($0.name) (\($0.runs))" }.joined(separator: ", ") ?? ""
                self.uiDone(s, pv != nil && pv!.problems.isEmpty, pv.map { "setup screen: \($0.op) \($0.name) \($0.version) · servers \(servers)" + ($0.problems.isEmpty ? "" : " · problems: \($0.problems.joined(separator: "; "))") + " · setup window \(self.window("setup") != nil ? "open" : "not open") · question \(m.approval?.question == true)" } ?? (st.status?.text ?? "no setup screen within 60 s"))
            }
        case "plugin-setup":
            // the Plugin Setup window's buttons (Install / Trust · Not now)
            let yes = arg == "yes", st = PluginStore.shared
            poll(30, { m.approval?.question == true }) { open in
                guard open else { self.uiDone(s, false, "no setup question open"); return }
                let name = st.preview?.name ?? st.plugins.last?.name ?? ""
                st.answer(yes, m)
                let busyWords = ["installing", "trusting", "updating", "sent"]
                self.poll(120, { st.status.map { t in !busyWords.contains { t.text.hasPrefix($0) } } ?? false }) { ended in
                    self.after(1.5) {
                        let pl = st.plugins.first { $0.name == name }
                        let ok = ended && (yes ? st.status?.ok == true && pl != nil : true)
                        self.uiDone(s, ok, "\(st.status?.text ?? "no result") · \(pl.map { "\($0.name) \($0.enabled ? "on" : "off"), \($0.trusted ? "trusted" : "not trusted"), servers \($0.servers.map { "\($0.name) \($0.state ?? ($0.failed ? "failed" : "ok"))" }.joined(separator: ", "))" } ?? "\(name) not in the list") · setup window \(self.window("setup") == nil ? "closed" : "still open")")
                    }
                }
            }
        case "plugin":
            // the Plugins window's row: the on/off switch, Trust…, Remove (then its confirm)
            let parts = arg.split(separator: ":", maxSplits: 1).map(String.init)
            guard parts.count == 2, ["enable", "disable", "remove", "trust"].contains(parts[0]) else { fail("plugin:enable|disable|remove|trust:NAME"); return true }
            let (op, name) = (parts[0], parts[1]), st = PluginStore.shared
            if op == "remove" { st.confirmRemove = name }
            st.act(op, name, m)
            let cond: @MainActor () -> Bool = {
                let pl = st.plugins.first { $0.name == name }
                switch op {
                case "enable": return pl?.enabled == true
                case "disable": return pl?.enabled == false
                case "remove": return pl == nil
                default: return m.approval?.question == true || st.preview != nil
                }
            }
            poll(30, cond) { ok in
                let pl = st.plugins.first { $0.name == name }
                self.uiDone(s, ok, "\(st.status?.text ?? "") · \(pl.map { "\($0.name) \($0.enabled ? "on" : "off"), \($0.trusted ? "trusted" : "not trusted")" } ?? "\(name) not listed")\(op == "trust" ? " · setup question \(m.approval?.question == true)" : "")")
            }

        // MARK: settings
        case "speak":
            // Elisa > Speak Replies (its @AppStorage, off stops speaking at once)
            let on = arg == "on"
            UserDefaults.standard.set(on, forKey: Speak.key)
            if !on { Speak.shared.stop() }
            later(s, 0.5) { (Speak.shared.enabled == on, "Speak Replies \(Speak.shared.enabled ? "on" : "off")") }
        case "voice-lang":
            // the mic's language tag / its right-click picker (@AppStorage "voiceLang")
            guard VoiceLang(rawValue: arg) != nil else { fail("voice-lang:system|auto|en|vi"); return true }
            UserDefaults.standard.set(arg, forKey: "voiceLang")
            later(s, 0.5) { (UserDefaults.standard.string(forKey: "voiceLang") == arg, "voice language \(arg) → whisper is told “\(m.voiceLocale)”") }

        // MARK: the scorecard's test
        case "eval":
            // Test this model → Run the test, then Cancel after S seconds (eval:MODEL:S, MODEL @current = this window's)
            let parts = arg.split(separator: ":").map(String.init)
            let model = parts.first == "@current" || parts.first == nil ? m.model : parts[0]
            let secs = parts.count > 1 ? Double(parts[1]) ?? 15 : 15
            let ev = EvalStore.shared
            guard ev.job == nil else { fail("another test is running"); return true }
            let cs = conns
            cs.load()
            after(1.5) {
                guard self.i < self.steps.count, self.steps[self.i].name == s.name else { return }
                let t = EvalTarget.ollama(model, conns: cs.items)
                ev.confirming = t
                self.after(1.0) {
                    ev.start(t)
                    guard let pid = ev.job?.pid, pid > 0 else { self.uiDone(s, false, "did not start: \(ev.outcome[model]?.text ?? "?")"); return }
                    self.after(secs) {
                        let progress = ev.job.map { "\($0.done)/\($0.total) cases" } ?? "already ended"
                        ev.cancel()
                        self.poll(15, { ev.job == nil }) { ended in
                            let gone = kill(pid, 0) != 0
                            let o = ev.outcome[model]?.text ?? "no outcome"
                            self.uiDone(s, ended && gone && o == "test cancelled", "\(model) via \(t.args.joined(separator: " ")) · \(progress) at \(Int(secs)) s · Cancel → \(o) · process \(gone ? "gone" : "STILL RUNNING")")
                        }
                    }
                }
            }

        // MARK: checks
        case "state":
            // state:FIELD~REGEX — a value the window shows (model chip, theme, Shade …)
            let kv = arg.split(separator: "~", maxSplits: 1).map(String.init)
            guard kv.count == 2, stateValue(kv[0]) != nil else { fail("state:FIELD~REGEX"); return true }
            later(s, 0.3) { let v = self.stateValue(kv[0]) ?? ""; return (v.range(of: kv[1], options: .regularExpression) != nil, "\(kv[0]) = \(v)") }
        case "file":
            // file:PATH~REGEX or file:PATH!~REGEX (PATH relative to the project folder, or ~/…, or absolute)
            // the separator is the first ~ that does not start the path (~/…)
            guard let sep = arg.indices.first(where: { $0 > arg.startIndex && arg[$0] == "~" }) else { fail("file:PATH~REGEX"); return true }
            let neg = arg[arg.index(before: sep)] == "!"
            let path = String(arg[..<(neg ? arg.index(before: sep) : sep)]), re = String(arg[arg.index(after: sep)...])
            var p = Self.expand(path)
            if !p.hasPrefix("/"), let f = m.folder { p = (f as NSString).appendingPathComponent(p) }
            later(s, 0.3) {
                let t = (try? String(contentsOfFile: p, encoding: .utf8)) ?? ""
                let hit = t.range(of: re, options: .regularExpression) != nil
                return (FileManager.default.fileExists(atPath: p) && hit != neg, "\(p): \(hit ? "matches" : "does not match") /\(re)/")
            }
        case "copy":
            // a fixture copied into the project folder (as the person would put it there)
            guard let f = m.folder else { fail("no project folder"); return true }
            let src = Self.expand(arg), dst = (f as NSString).appendingPathComponent((src as NSString).lastPathComponent)
            try? FileManager.default.removeItem(atPath: dst)
            let ok = (try? FileManager.default.copyItem(atPath: src, toPath: dst)) != nil
            later(s, 0.2) { (ok, "\(ok ? "copied" : "could not copy") \(src) → project") }
        default:
            return false
        }
        return true
    }

    /// the end of a run: the settings and the clipboard the control steps changed go back
    private func restoreUI() {
        for (k, v) in savedDefaults {
            if v is NSNull { UserDefaults.standard.removeObject(forKey: k) } else { UserDefaults.standard.set(v, forKey: k) }
        }
        if pasteboardTaken {
            if let t = savedPasteboard { Clipboard.put(t) } else { NSPasteboard.general.clearContents() }
        }
        EvalStore.shared.stopNow()
    }

    private func finish(_ problem: String?) {
        guard !done, let m else { return }
        done = true
        m.onEventHook = { _ in }
        restoreUI()
        if let problem { results.append("FAIL " + problem) }
        try? (closedTranscripts + m.debugTranscript()).write(toFile: (dir as NSString).appendingPathComponent("transcript.txt"), atomically: true, encoding: .utf8)
        let passed = results.filter { $0.hasPrefix("PASS") }.count
        let summary = "selftest: \(passed)/\(results.count) passed"
        try? (results.joined(separator: "\n") + "\n" + summary + "\n").write(toFile: (dir as NSString).appendingPathComponent("results.txt"), atomically: true, encoding: .utf8)
        print(summary)
        print("selftest: transcript, results and pictures in \(dir)")
        if quit { NSApp.terminate(nil) }
    }
}
