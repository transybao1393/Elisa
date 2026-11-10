// Copying the window's text for debugging: one message (right-click → Copy) or the whole session with a header naming
// the model, server, folder and context (Edit → Copy Transcript for Debugging, ⌥⌘C). Plain text, no styling.
import AppKit
import SwiftUI

extension AppModel {
    /// one message as plain text, prefixed with who said it
    func plainText(_ b: Block) -> String {
        switch b.kind {
        case .user(let t, _): return "YOU: \(t)"
        case .info(let t, _): return t
        case .reply: return "ELISA:\n" + b.parts.map(Self.plainText).filter { !$0.isEmpty }.joined(separator: "\n")
        }
    }

    nonisolated static func plainText(_ p: Part) -> String {
        switch p.kind {
        case .prose(let s), .footer(let s): return s
        case .info(let s, _): return s
        case .card(let s, let n): return n.map { "[state #\($0)] " } .map { $0 + s } ?? s
        case .box(let title, let body): return "[\(title)]\n\(body)"
        case .thinking(let s, _, _): return "[thinking]\n\(s.trimmingCharacters(in: .whitespacesAndNewlines))"
        case .check(let c): return "[check] \(c.cmd) · \(c.status)" + (c.summary.isEmpty ? "" : " · \(c.summary)") + (c.line.isEmpty ? "" : "\n\(c.line)")
        case .tool(let t):
            var s = "[tool] \(t.name) \(t.target) · \(t.status)"
            if let e = t.elapsed { s += String(format: " · %.1fs", e) }
            if !t.note.isEmpty { s += " · \(t.note)" }
            if !t.output.isEmpty { s += "\n" + t.output }
            return s
        }
    }

    /// the whole session, with what a problem report needs at the top
    func debugTranscript() -> String {
        var head = ["Elisa transcript · \(Date().formatted(date: .abbreviated, time: .standard))"]
        head.append("model: \(model)")
        head.append("server: \(baseURL.isEmpty ? "?" : baseURL)")
        head.append("folder: \(cwd)")
        head.append("phase: \(phase)")
        if let s = stat {
            head.append(String(format: "last reply: ctx %d/%d · %@ tok/s · ttft %.2fs · reused %d · out %d", s.ctxUsed, s.maxCtx > 0 ? s.maxCtx : maxCtx, s.tokSText, s.ttft, s.reused, s.out))
        } else {
            head.append("context: \(maxCtx) · no reply yet")
        }
        head.append("total: in \(sumIn) · out \(sumOut)")
        return head.joined(separator: "\n") + "\n\n" + blocks.map(plainText).joined(separator: "\n\n") + "\n"
    }

    func copyTranscript() { Clipboard.put(debugTranscript()) }
}

enum Clipboard {
    static func put(_ s: String) {
        NSPasteboard.general.clearContents()
        NSPasteboard.general.setString(s, forType: .string)
    }
}

/// right-click → Copy on any view, with the text worked out only when the menu opens
struct CopyMenu: ViewModifier {
    let label: String
    let text: () -> String
    func body(content: Content) -> some View {
        content.contextMenu { Button(label) { Clipboard.put(text()) } }
    }
}

extension View {
    func copyMenu(_ label: String = "Copy", _ text: @escaping () -> String) -> some View { modifier(CopyMenu(label: label, text: text)) }
}
