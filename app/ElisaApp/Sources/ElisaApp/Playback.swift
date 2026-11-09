// --script FILE: scripted playback of a person working in this window (default off). Each prompt is typed into the
// real input box at a human-ish pace, sent with Enter, and the turn is waited out; permission boxes are answered
// after a short pause; between turns there are reading pauses (longer for longer
// replies), and the script can scroll the transcript up and back or open the EDITS panel to look at diffs.
// Everything goes through the same model state the keyboard uses, so the window shows it exactly as if typed.
// It stops only from an explicit control: the status bar's "■ stop playback" button or ctrl-. (the status bar shows
// "playback · turn 7/21"). Focus, clicks, scrolling and window moves do not stop it; while it runs the input box and
// the permission box is disabled ("automated test running"), so stray typing cannot take over.
//
//   --script FILE   the script (format below)       --seed N        RNG seed (default 1): same seed, same pacing
//   --script-delay S  wait before the first prompt (default 3)       --script-cap S  time cap in seconds (default 2700)
//   --script-from-final   start at the script's final block (with --continue: recall on a resumed session)
//   --record          recording a video: the playback's status-bar line, stop button and badge are hidden (ctrl-. still stops)
// At the time cap the current turn finishes, the rest of the middle turns are skipped and the final block is played,
// then the playback stops (a hard stop follows at cap + 1200 s). The final block starts at a line `@final`, else at
// the slash commands just before the first prompt starting "what was the goal".
//
// Script lines (blank lines and lines starting with # are skipped):
//   any other line        a prompt or a slash command, typed and sent (\n in the text = shift-enter newline)
//   @scroll               scroll the transcript up a few screens, read, then back to the bottom
//   @edits                open the EDITS panel, step back through the diffs, close it again
//   @wait S               an extra idle pause of S seconds
//   @outside once|session|deny   the answer to the next outside-the-project permission box of the prompt above
//                         (allow once / allow these paths for this session / deny). An outside box with no line left
//                         is denied and logged as `playback: … FINDING: outside-folder request …`
//   @answer yes|always|no   the answer to the next plain permission box of the prompt above (repeat the line for
//                         later boxes, in order); boxes with no line left are answered yes, as before
//   @esc S                press Esc S seconds after the prompt above is sent (once; cancels the request / tool, and
//                         an open permission box is answered no, as the key does)
//   @attach FILE          attach a picture or document as the paperclip does; the next prompt line is sent with it
//   @paste FILE           paste FILE's text into the input box at once (as ⌘V does: a long text becomes a "pasted
//                         text" chip) and send it
//
// JSON lines are accepted too (one object per line, mixed freely with the lines above):
//   {"prompt": "TEXT"}  {"cmd": "/stats"}   typed and sent (also {"type": "prompt"|"cmd", "text": "TEXT"})
//   {"outside": "once"|"session"|"deny"}   the next outside permission box's answer (as @outside)
//   {"answer": "yes"|"always"|"no"}   the next plain permission box's answer (as @answer)   {"esc": S}   (as @esc)
//   {"pause": S}  {"scroll": true}  {"edits": true}   (also {"type": "pause"|"scroll"|"edits", ...};
//   pause takes "s" or "seconds" too)
// Every action is logged to stdout as `playback: +T.Ts <what>` (T = seconds since the playback started). A sent prompt
// is logged by its length and its first 80 characters with secrets redacted (LogRedact, as ELISA_APP_LOG does): a
// typed API key never reaches stdout.
// While a script plays the app holds a user-initiated activity (no App Nap, no idle sleep), so timers keep running
// when the display idles or the window is in the background; it ends when the playback stops.
import AppKit
import SwiftUI

/// SplitMix64: a tiny seeded RNG, so a run repeats exactly (same seed -> same pauses and typing pace)
struct SplitMix: RandomNumberGenerator {
    var s: UInt64
    init(seed: UInt64) { s = seed &+ 0x9E37_79B9_7F4A_7C15 }
    mutating func next() -> UInt64 {
        s &+= 0x9E37_79B9_7F4A_7C15
        var z = s
        z = (z ^ (z >> 30)) &* 0xBF58_476D_1CE4_E5B9
        z = (z ^ (z >> 27)) &* 0x94D0_49BB_1331_11EB
        return z ^ (z >> 31)
    }
}

@MainActor
final class Playback {
    static let shared = Playback()
    /// --record (with --script): recording a video (README GIF); the playback's own status-bar line, stop button and
    /// "automated test running" badge are not drawn. The input still takes no keys while it plays.
    static let recording = CommandLine.arguments.contains("--record")

    enum Step {
        case say(String)
        /// a file's text, pasted and sent
        case paste(String)
        /// a picture or document attached as the paperclip does; the next prompt is sent with it
        case attach(String)
        case scroll, edits, wait(Double)
    }

    private weak var m: AppModel?
    private var rng = SplitMix(seed: 1)
    private var steps: [Step] = []
    private var stopped = false
    private var t0 = Date()
    private var cap = 2700.0
    private var task: Task<Void, Never>?
    private var turns = 0
    private var turnNo = 0

    private var fromFinal = false
    private var inFinal = false
    private var final: Int?

    func run(file: String, seed: UInt64, delay: Double, cap: Double, fromFinal: Bool = false, model: AppModel) {
        guard let text = try? String(contentsOfFile: file, encoding: .utf8) else { print("playback: cannot read \(file)"); return }
        steps = Self.parse(text)
        outs = Self.outsideAnswers
        answers = Self.plainAnswers
        escs = Self.escAfter
        final = Self.finalStart(steps, marks: Self.finalMarks)
        self.fromFinal = fromFinal
        rng = SplitMix(seed: seed)
        t0 = Date()
        m = model
        self.cap = cap
        log("loaded \(steps.count) steps from \(file) · seed \(seed)")
        // only an explicit stop ends it (the status bar's ■ stop playback, or ctrl-.): focus, clicks, scrolling and
        // window moves do not; the input box and the permission box are disabled while it runs
        model.playbackStop = { [weak self] in self?.stop("stopped by you (■ stop playback / ctrl-.)") }
        turns = steps.filter { switch $0 { case .say, .paste: return true; default: return false } }.count
        model.playbackLine = "playback · starting"
        activity = ProcessInfo.processInfo.beginActivity(options: [.userInitiated, .idleSystemSleepDisabled], reason: "Elisa --script playback")
        task = Task { @MainActor in
            await self.pause(delay)
            await self.play()
        }
    }

    nonisolated(unsafe) static var finalMarks: [Int] = []
    /// step index of a prompt -> its scripted answers to outside-the-project permission boxes, in order
    nonisolated(unsafe) static var outsideAnswers: [Int: [String]] = [:]
    private var outs: [Int: [String]] = [:]
    /// step index of a prompt -> its scripted answers to plain permission boxes ("yes" | "always" | "no"), in order
    nonisolated(unsafe) static var plainAnswers: [Int: [String]] = [:]
    private var answers: [Int: [String]] = [:]
    /// step index of a prompt -> seconds after sending it to press Esc
    nonisolated(unsafe) static var escAfter: [Int: Double] = [:]
    private var escs: [Int: Double] = [:]
    /// the App Nap / idle sleep assertion held while the script plays
    private var activity: NSObjectProtocol?
    /// outside-the-project requests the script had no answer for (denied), counted for the end line
    private var findings = 0

    /// where the final block starts: `@final`, else the slash commands just before "what was the goal…"
    static func finalStart(_ steps: [Step], marks: [Int]) -> Int? {
        if let f = marks.last { return f }
        guard var i = steps.firstIndex(where: { if case .say(let t) = $0 { return t.lowercased().hasPrefix("what was the goal") }; return false }) else { return nil }
        while i > 0, case .say(let t) = steps[i - 1], t.hasPrefix("/") { i -= 1 }
        return i
    }

    static func parse(_ text: String) -> [Step] {
        finalMarks = []
        outsideAnswers = [:]
        plainAnswers = [:]
        escAfter = [:]
        var out: [Step] = []
        func answer(_ w: String) {
            let w = w.trimmingCharacters(in: .whitespaces).lowercased()
            let a = w.hasPrefix("a") || w.hasPrefix("s") ? "always" : w.hasPrefix("n") || w.hasPrefix("d") ? "no" : "yes"
            guard case .say? = out.last else { print("playback: @answer with no prompt above: skipped"); return }
            plainAnswers[out.count - 1, default: []].append(a)
        }
        func esc(_ secs: Double?) {
            guard case .say? = out.last else { print("playback: @esc with no prompt above: skipped"); return }
            escAfter[out.count - 1] = max(0, secs ?? 2)
        }
        func outside(_ w: String) {
            let w = w.trimmingCharacters(in: .whitespaces).lowercased()
            let a = w.hasPrefix("s") || w.hasPrefix("a") ? "session" : w.hasPrefix("o") || w.hasPrefix("y") ? "once" : "deny"
            guard case .say? = out.last else { print("playback: @outside with no prompt above: skipped"); return }
            outsideAnswers[out.count - 1, default: []].append(a)
        }
        for raw in text.components(separatedBy: "\n") {
            let l = raw.trimmingCharacters(in: .whitespaces)
            if l.isEmpty || l.hasPrefix("#") { continue }
            if l.hasPrefix("{"), let d = l.data(using: .utf8), let j = (try? JSONSerialization.jsonObject(with: d)) as? [String: Any] {
                let type = j["type"] as? String ?? ["prompt", "cmd", "outside", "answer", "esc", "pause", "scroll", "edits"].first { j[$0] != nil } ?? ""
                let str = { (k: String) in j[k] as? String }
                let num = { (k: String) in (j[k] as? NSNumber)?.doubleValue }
                switch type {
                case "prompt", "cmd":
                    if let t = str(type) ?? str("text") { out.append(.say(t)) }
                case "outside": outside(str("outside") ?? str("answer") ?? "deny")
                case "answer": answer(str("answer") ?? str("text") ?? "yes")
                case "esc": esc(num("esc") ?? num("s") ?? num("seconds"))
                case "pause": out.append(.wait(num("pause") ?? num("s") ?? num("seconds") ?? 5))
                case "scroll": out.append(.scroll)
                case "edits": out.append(.edits)
                default: print("playback: skipped line \(Self.shown(l, 60))")
                }
                continue
            }
            if l.hasPrefix("@plan") || l.hasPrefix("@test") { print("playback: planning is gone, skipped: \(Self.shown(l, 60))"); continue }
            if l.hasPrefix("@paste ") { out.append(.paste(String(l.dropFirst(7)).trimmingCharacters(in: .whitespaces))); continue }
            if l.hasPrefix("@attach ") { out.append(.attach(String(l.dropFirst(8)).trimmingCharacters(in: .whitespaces))); continue }
            if l.hasPrefix("@outside") { outside(String(l.dropFirst(8))) }
            else if l.hasPrefix("@answer") { answer(String(l.dropFirst(7))) }
            else if l == "@esc" || l.hasPrefix("@esc ") { esc(Double(l.dropFirst(4).trimmingCharacters(in: .whitespaces))) }
            else if l == "@scroll" { out.append(.scroll) }
            else if l == "@edits" { out.append(.edits) }
            else if l == "@final" { out.append(.wait(0)); finalMarks.append(out.count - 1) }
            else if l.hasPrefix("@wait") { out.append(.wait(Double(l.dropFirst(5).trimmingCharacters(in: .whitespaces)) ?? 5)) }
            else { out.append(.say(l.replacingOccurrences(of: "\\n", with: "\n"))) }
        }
        return out
    }

    private func stop(_ why: String) {
        guard !stopped else { return }
        stopped = true
        task?.cancel()
        if let a = activity { ProcessInfo.processInfo.endActivity(a); activity = nil }
        log("stopped: \(why)" + (findings > 0 ? " · \(findings) outside-folder finding(s)" : ""))
        m?.playbackLine = nil
        m?.playbackStop = nil
        m?.blocks.append(Block(kind: .info("[playback \(why)]", .plain)))
    }

    /// a prompt as the log shows it: secrets redacted (the whole text first, so a key is never cut into a shape the
    /// redactor misses), then at most `n` characters, and the length
    static func shown(_ t: String, _ n: Int) -> String {
        let r = LogRedact.redact(t).replacingOccurrences(of: "\n", with: "⏎")
        return "(\(t.count) chars) " + (r.count > n ? r.prefix(n) + "…" : r)
    }

    private func log(_ s: String) { print(String(format: "playback: +%.1fs ", Date().timeIntervalSince(t0)) + s) }

    private func r(_ lo: Double, _ hi: Double) -> Double { Double.random(in: lo...hi, using: &rng) }

    private func pause(_ s: Double) async {
        let end = Date().addingTimeInterval(max(0, s))
        while !stopped && Date() < end { try? await Task.sleep(nanoseconds: 100_000_000) }
    }

    private var live: Bool {
        if stopped { return false }
        if Date().timeIntervalSince(t0) > cap + (final == nil ? 0 : 1200) { stop("stopped: hard time cap reached"); return false }
        return true
    }

    private func play() async {
        guard let m else { return }
        // wait for elisa to be ready, with the edits panel closed (it is opened on purpose by @edits)
        while live && m.phase == .starting { await pause(0.25) }
        m.editsOpen = false
        var i = fromFinal ? (final ?? 0) : 0
        inFinal = fromFinal
        if fromFinal { log("starting at the final block (step \(i + 1))") }
        while i < steps.count {
            guard live else { return }
            if !inFinal, let f = final, Date().timeIntervalSince(t0) > cap {
                inFinal = true
                if f > i { log("time cap \(Int(cap)) s: skipping steps \(i + 1)…\(f), playing the final block"); i = f }
                m.blocks.append(Block(kind: .info("[playback: time cap reached · skipping to the final block (recall, /stats)]", .plain)))
            }
            let s = steps[i]
            let stepOuts = outs[i] ?? []
            let stepAnswers = answers[i] ?? []
            let stepEsc = escs[i]
            i += 1
            switch s {
            case .wait(let d): log("wait \(d)s"); await pause(d)
            case .attach(let f):
                m.attach(urls: [URL(fileURLWithPath: f)])
                log("attached \((f as NSString).lastPathComponent)")
                await pause(r(0.6, 1.2))
            case .scroll: await scroll()
            case .edits: await edits()
            case .paste(let f):
                turnNo += 1
                m.playbackLine = "playback · turn \(turnNo)/\(turns)"
                guard let text = try? String(contentsOfFile: f, encoding: .utf8) else { log("paste: cannot read \(f)"); continue }
                m.paste(text)
                await pause(r(0.5, 1.2))
                guard live else { return }
                enter(m.input)
                log("pasted and sent [\(i)/\(steps.count)] \(text.count) chars from \((f as NSString).lastPathComponent)")
                await waitTurn()
                guard live else { return }
                await read()
            case .say(let t):
                turnNo += 1
                m.playbackLine = "playback · turn \(turnNo)/\(turns)"
                await type(t, into: { m.input = $0 }, current: { m.input })
                guard live else { return }
                await pause(r(0.3, 0.9))   // a look at what was typed before Enter
                guard live else { return }
                enter(t)
                log("sent [\(i)/\(steps.count)] \(Self.shown(t, 80))")
                await waitTurn(outs: stepOuts, answers: stepAnswers, esc: stepEsc)
                guard live else { return }
                await read()
            }
        }
        if live { stop("finished: the script ran to its end") }
    }

    /// types t character by character: 35–110 ms per key, a little longer after spaces and punctuation, now and then
    /// a short think
    private func type(_ t: String, into set: (String) -> Void, current: () -> String) async {
        var s = current()
        for ch in t {
            guard live else { return }
            s.append(ch)
            set(s)
            var d = r(0.035, 0.11)
            if ch == " " { d += r(0.0, 0.08) }
            if ",.?:;".contains(ch) { d += r(0.1, 0.35) }
            if r(0, 1) < 0.02 { d += r(0.4, 1.2) }
            await pause(d)
        }
    }

    /// Enter in the input box: the same as InputBox's return key (a bare /word runs the slash menu's match)
    private func enter(_ t: String) {
        guard let m else { return }
        let menu = m.slashItems
        if !menu.isEmpty, let i = menu.firstIndex(where: { $0.0 == t }) {
            m.runSlash(i)
            return
        }
        m.input = ""
        m.send(t)
    }

    /// until the turn is over (elisa idle again, nothing queued); permission boxes are answered on the way
    private func waitTurn(outs: [String] = [], answers: [String] = [], esc: Double? = nil) async {
        guard let m else { return }
        var outs = outs
        var answers = answers
        var esc = esc
        var sawBusy = false
        var idleSince: Date?
        let sent = Date()
        while live {
            if let e = esc, Date().timeIntervalSince(sent) >= e {
                esc = nil
                log(String(format: "esc after %.1fs", Date().timeIntervalSince(sent)))
                m.interrupt()
                sawBusy = true; idleSince = nil
            }
            if m.approval != nil {
                let a = m.approval!
                await pause(r(1.2, 3.5))
                guard live, m.approval != nil else { continue }
                if a.outside {
                    // outside the project folder: only the script's own answer allows it; none = deny + finding
                    let where_ = "\(a.tool) \(a.access) \(a.paths.joined(separator: ", "))"
                    if outs.isEmpty {
                        findings += 1
                        log("FINDING: outside-folder request with no @outside line: \(where_) -> deny")
                        m.blocks.append(Block(kind: .info("[playback finding: outside-folder request denied (no @outside line): \(where_)]", .plain)))
                        m.answer(2)
                    } else {
                        let ans = outs.removeFirst()
                        log("permission outside \(where_) -> \(ans)")
                        m.answer(ans == "once" ? 0 : ans == "session" ? 1 : 2)
                    }
                    sawBusy = true; idleSince = nil
                    continue
                }
                let ans = answers.isEmpty ? "yes" : answers.removeFirst()
                log("permission \(a.summary.prefix(60)) -> \(ans)")
                m.answer(ans == "always" ? 1 : ans == "no" ? 2 : 0)
                sawBusy = true; idleSince = nil
                continue
            }
            if m.busy {
                sawBusy = true; idleSince = nil
            } else if m.phase == .idle && m.queued == 0 {
                if idleSince == nil { idleSince = Date() }
                // a slash command may never leave idle: give it a few seconds to print
                let need = sawBusy ? 1.2 : 4.0
                if Date().timeIntervalSince(idleSince!) >= need {
                    log(String(format: "turn done in %.1fs", Date().timeIntervalSince(sent)))
                    return
                }
            }
            if m.phase == .exited { stop("stopped: elisa exited"); return }
            await pause(0.25)
        }
    }

    /// reading the reply just shown: ~2 s plus its length at 30–50 chars/s, at most 25 s
    private func read() async {
        let n = replySize()
        let d = min(25, r(1.5, 3.5) + Double(n) / r(30, 50))
        log(String(format: "reading %d chars: %.1fs", n, d))
        await pause(d)
    }

    /// the text of the newest turn: every reply block after the newest user block (a turn can end in a reply block
    /// that holds only a footer or an info line, made by an event after the prose, so the last reply alone is often
    /// empty), counting prose, cards and boxes; a slash command's info lines count when nothing else
    /// does; with nothing at all, the newest reply that has text
    private func replySize() -> Int {
        guard let m else { return 0 }
        func size(_ b: Block) -> Int {
            var n = 0
            for p in b.parts {
                switch p.kind {
                case .prose(let t): n += t.count
                case .box(_, let t), .card(let t, _): n += t.count
                default: break
                }
            }
            return n
        }
        var n = 0, info = 0
        for b in m.blocks.reversed() {
            switch b.kind {
            case .user: return n > 0 ? n : info > 0 ? info : m.blocks.reversed().lazy.filter { if case .reply = $0.kind { return true }; return false }.map(size).first { $0 > 0 } ?? 0
            case .reply:
                n += size(b)
                for p in b.parts { if case .info(let t, _) = p.kind { info += t.count } }
            case .info(let t, _): info += t.count
            }
        }
        return n > 0 ? n : info
    }

    /// scroll up a few screens in steps (as a wheel would), read, then back down to the newest line
    private func scroll() async {
        guard let sv = ScrollHold.view else { log("scroll: no transcript view"); return }
        let screens = Int(r(2, 4.99))
        log("scroll up \(screens) screens")
        for _ in 0..<screens * 6 {
            guard live else { return }
            ScrollHold.scrollUp(sv.contentView.bounds.height / 6)
            await pause(r(0.05, 0.15))
        }
        await pause(r(3, 7))
        for _ in 0..<screens * 6 + 2 {
            guard live else { return }
            ScrollHold.scrollUp(-sv.contentView.bounds.height / 6)
            await pause(r(0.04, 0.1))
        }
        if let doc = sv.documentView {
            // the last bit: exactly to the end, so the transcript sticks to new output again
            ScrollHold.watcher?.markUserInput()
            let clip = sv.contentView
            let y = doc.isFlipped ? max(doc.bounds.minY, doc.bounds.maxY - clip.bounds.height) : doc.bounds.minY
            clip.scroll(to: NSPoint(x: clip.bounds.origin.x, y: y))
            sv.reflectScrolledClipView(clip)
        }
        log("scroll back to the bottom")
    }

    /// open the EDITS panel, look at the newest diff, step back through two older ones, then close it
    private func edits() async {
        guard let m else { return }
        log("edits panel: \(m.edits.count) edits")
        if m.narrow { m.editsSheet = true } else { m.editsOpen = true }
        m.editIdx = max(0, m.edits.count - 1)
        await pause(r(4, 8))
        for _ in 0..<min(2, max(0, m.edits.count - 1)) {
            guard live else { return }
            m.editPrev()
            await pause(r(2.5, 5))
        }
        guard live else { return }
        m.editIdx = max(0, m.edits.count - 1)
        await pause(r(1, 2))
        if m.narrow { m.editsSheet = false } else { m.editsOpen = false }
        log("edits panel closed")
    }
}
