// --closetest FILE (debug): with the session's real model, checks that closing the chat window brings the welcome
// page back instead of quitting, that a new session from there works, and that closing the welcome page then quits.
// Steps: a real message and reply; close the chat (performClose, the window's own close path, no synthetic input);
// welcome page shown, session stopped, app alive; a new session in the same folder with a real message and reply;
// close the chat again; close the welcome page, the app quits. A PASS / FAIL line per check goes to FILE.
import AppKit

@MainActor
enum CloseTest {
    private static var file: String?
    private static var lines: [String] = []
    private static weak var m: AppModel?
    /// --closetest-keep: pauses so a person can watch, and leaves the window open at the end (no quit step)
    private static var keep: Bool { CommandLine.arguments.contains("--closetest-keep") }
    private static var pause: Double { keep ? 3 : 0 }

    private static func log(_ s: String) {
        lines.append(s)
        guard let f = file else { return }
        try? (lines.joined(separator: "\n") + "\n").write(toFile: f, atomically: true, encoding: .utf8)
    }

    /// the window as a PNG next to the report (FILE without .txt, then -name.png), so each step can be looked at
    private static func snap(_ name: String) {
        guard let f = file else { return }
        let png = (f.hasSuffix(".txt") ? String(f.dropLast(4)) : f) + "-\(name).png"
        DebugFrame.snap(title: "Elisa", to: png)
        log("     picture: \(png)")
    }

    private static func check(_ name: String, _ ok: Bool) { log("\(ok ? "PASS" : "FAIL") \(name)") }

    private static var mainWindow: NSWindow? { NSApp.windows.first { $0.title == "Elisa" && $0.isVisible } }

    /// the last reply's prose
    private static var lastReply: String {
        guard let m, let b = m.blocks.last(where: { if case .reply = $0.kind { return true } else { return false } }) else { return "" }
        return b.parts.compactMap { if case .prose(let t) = $0.kind { return t } else { return nil } }.joined()
    }

    /// polls every 0.5 s until `cond` holds or `timeout` passes, then calls `next` with whether it held
    private static func wait(_ timeout: Double, _ cond: @escaping () -> Bool, _ next: @escaping (Bool) -> Void) {
        let t0 = Date()
        func tick() {
            if cond() { next(true); return }
            if Date().timeIntervalSince(t0) > timeout { next(false); return }
            DispatchQueue.main.asyncAfter(deadline: .now() + 0.5) { tick() }
        }
        tick()
    }

    /// sends `text` once the session is ready and checks the real model's reply against `expect` (regex, any case)
    private static func turn(_ label: String, _ text: String, expect: String, _ next: @escaping () -> Void) {
        guard let m else { return }
        wait(60, { m.phase == .idle }) { ready in
            check("\(label): session ready", ready)
            guard ready else { finish(); return }
            check("\(label): real model (\(m.model))", !m.model.isEmpty)
            let before = m.sumOut
            m.send(text)
            wait(180, { m.sumOut > before && m.phase == .idle && !lastReply.isEmpty }) { ok in
                let r = lastReply
                log("     reply: \(r.prefix(160).replacingOccurrences(of: "\n", with: " "))")
                check("\(label): reply to \"\(text)\"", ok && r.range(of: expect, options: [.regularExpression, .caseInsensitive]) != nil)
                snap(label.replacingOccurrences(of: " ", with: "-") + "-reply")
                next()
            }
        }
    }

    /// closes the chat window and checks the welcome page comes back with the session stopped
    private static func closeChat(_ label: String, _ next: @escaping () -> Void) {
        guard let m else { return }
        check("\(label): chat window open", mainWindow != nil && !m.welcome)
        DispatchQueue.main.asyncAfter(deadline: .now() + pause) { mainWindow?.performClose(nil) }
        wait(5 + pause, { m.welcome && mainWindow != nil && !m.elisaRunning }) { _ in
            check("\(label): app still running", true)
            check("\(label): welcome page shown", m.welcome)
            check("\(label): window visible again", mainWindow != nil)
            check("\(label): session stopped", !m.elisaRunning)
            snap(label.replacingOccurrences(of: " ", with: "-") + "-welcome")
            DispatchQueue.main.asyncAfter(deadline: .now() + pause) { next() }
        }
    }

    private static func finish() {
        if keep { log("done · the window stays open on the welcome page"); return }
        log("closing the welcome page (the app should quit)")
        mainWindow?.performClose(nil)
        // still here 5 s later: the app did not quit
        DispatchQueue.main.asyncAfter(deadline: .now() + 5) {
            check("app quit after closing the welcome page", false)
            exit(1)
        }
    }

    static func run(report: String, model: AppModel) {
        file = report
        m = model
        let folder = model.folder
        turn("session 1", "hey", expect: "\\w") {
            closeChat("close 1") {
                // a new session from the welcome page, in the same folder (not via open(folder:), which would add the
                // test folder to the person's recent folders)
                model.folder = folder
                model.start()
                turn("session 2", "What is 2+2? Answer with the number only.", expect: "\\b4\\b|four") {
                    closeChat("close 2") { finish() }
                }
            }
        }
    }

    /// applicationWillTerminate: the quit came from closing the welcome page
    static func terminating() {
        guard file != nil else { return }
        check("app quit after closing the welcome page", Boot.model.welcome)
    }
}
