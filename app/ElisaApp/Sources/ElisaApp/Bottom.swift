// The bottom stack (kit D): never scrolls. A 20 px blank row, the 22 px status bar led by Shade's face, the input
// (or the permission box / session picker in its slot), then the right-aligned meta rows.
import AppKit
import SwiftUI

struct BottomStack: View {
    @EnvironmentObject var m: AppModel
    @Environment(\.pal) var p
    var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            Color.clear.frame(height: 20)
            StatusBar().frame(height: 22)
            Group {
                if m.approval != nil { PermissionBox() }
                else if m.picker != nil { PickerBox() }
                else { InputBox() }
            }
            .padding(.top, 8)
            // --script playback: the boxes take no input until it is stopped (status bar ■ stop playback, ctrl-.)
            .disabled(m.playbackLine != nil)
            .overlay(alignment: .topTrailing) {
                if m.playbackLine != nil {
                    Text("automated test running").font(Fonts.mono(11)).foregroundStyle(p.warn)
                        .padding(.horizontal, 6).padding(.vertical, 2)
                        .background(RoundedRectangle(cornerRadius: 4).fill(p.inputBg))
                        .overlay(RoundedRectangle(cornerRadius: 4).strokeBorder(p.warn, lineWidth: 1))
                        .padding(.top, 2).padding(.trailing, 10)
                }
            }
            MetaRows().padding(.top, 8)
        }
        .padding(.horizontal, m.narrow ? 20 : 24)
        .padding(.bottom, 12)
    }
}

struct StatusBar: View {
    @EnvironmentObject var m: AppModel
    @Environment(\.pal) var p
    var body: some View {
        HStack(spacing: 10) {
            StatusFace()
            TimelineView(.periodic(from: .now, by: 1)) { tl in
                Text(main(tl.date)).foregroundStyle(p.fg) + Text(hint).foregroundStyle(p.fg2)
            }
            if !m.model.isEmpty { ModelChip() }
            ForEach(items, id: \.self) { t in
                HStack(spacing: 6) { Text("│").foregroundStyle(p.gut); Text(t) }
            }
            Spacer(minLength: 8)
            if let pl = m.playbackLine {
                Text(pl).foregroundStyle(p.fg)
                Button { m.playbackStop?() } label: {
                    Text("■ stop playback").foregroundStyle(p.warn).padding(.horizontal, 6)
                        .overlay(RoundedRectangle(cornerRadius: 4).strokeBorder(p.warn, lineWidth: 1))
                }
                .buttonStyle(.plain)
                .help("Stop the scripted playback (ctrl-.)")
            }
            if m.yolo {
                Text("approve-all · /yolo").foregroundStyle(p.warn).padding(.horizontal, 6)
                    .overlay(RoundedRectangle(cornerRadius: 4).strokeBorder(p.warn, lineWidth: 1))
            }
            // narrow (kit "BELOW 900 PX"): the key hints leave the status bar
            if !m.narrow { keys }
        }
        .font(Fonts.mono(12))
        .foregroundStyle(p.fg2)
        .lineLimit(1)
    }

    private func main(_ now: Date) -> String {
        let s = Int(now.timeIntervalSince(m.phaseSince))
        // a fold blocks the next request: say so, not "thinking…", while it runs
        if let j = m.job, j.done == nil, AppModel.isFold(j.label), m.phase != .exited, m.phase != .permission {
            return "compacting the conversation… \(dur(now.timeIntervalSince(j.at), 0))"
        }
        switch m.phase {
        case .starting: return "starting elisa…"
        case .idle: return m.job.map { $0.done == nil ? "working: \($0.label)" : "ready" } ?? "ready"
        case .thinking: return "thinking… \(dur(Double(s), 0))"
        case .streaming: return "streaming…"
        case .tool: return "running tool… \(dur(Double(s), 0))"
        case .permission: return "waiting for permission"
        case .exited: return "elisa exited"
        }
    }
    private var hint: String { m.phase == .thinking || m.phase == .tool || m.phase == .streaming ? "  (esc to interrupt)" : "" }

    private var items: [String] {
        var v: [String] = []
        if m.queued > 0 { v.append("\(m.queued) queued") }
        // the kit's "state #15": the latest card's number, only when Elisa sends it (an older Elisa omits `n`)
        if let n = m.cardNo { v.append("state #\(n)") }
        return v
    }

    @ViewBuilder private var keys: some View {
        let k = { (a: String, b: String) in Text(a).foregroundStyle(p.fg) + Text(" " + b) }
        if m.approval != nil {
            k("1–3", "choose") + Text(" · ") + k("esc", "= no")
        } else {
            k("⏎", "send") + Text(" · ") + k("⇧⏎", "newline") + Text(" · ") + k("/", "commands") + Text(" · ") + k("ctrl-o", "edits")
        }
    }
}

struct InputBox: View {
    @EnvironmentObject var m: AppModel
    @Environment(\.pal) var p
    @FocusState private var focus: Bool

    @State private var dropping = false

    var body: some View {
        VStack(alignment: .leading, spacing: 4) {
            if !m.attachments.isEmpty { AttachmentStrip() }
            row
        }
        .padding(.vertical, 11).padding(.horizontal, 14)
        .background(RoundedRectangle(cornerRadius: 8).fill(p.inputBg))
        .overlay(RoundedRectangle(cornerRadius: 8).strokeBorder(dropping ? p.ok : p.acc, lineWidth: dropping ? 2 : 1))
        .contentShape(Rectangle())
        .onTapGesture { focus = true }
        .onAppear { focus = true }
        // dropped image files (a model without vision gets a note saying why, from `attach`)
        .onDrop(of: [.fileURL, .image], isTargeted: $dropping) { providers in drop(providers) }
    }

    private var row: some View {
        HStack(alignment: .top, spacing: 10) {
            Text("›").font(Fonts.mono(14)).foregroundStyle(p.acc).frame(height: 22)
            ZStack(alignment: .topLeading) {
                if m.input.isEmpty {
                    Text(m.phase == .exited ? "Elisa is not running · File > Open Folder…"
                         : m.attachments.isEmpty ? "Message Elisa · / for commands" : "Ask about the image · ⏎ sends")
                        .font(Fonts.mono(13)).foregroundStyle(p.fg2).frame(height: 22).allowsHitTesting(false)
                }
                TextField("", text: $m.input, axis: .vertical)
                    .textFieldStyle(.plain)
                    .font(Fonts.mono(13))
                    .foregroundStyle(p.fg)
                    .lineLimit(1...8)
                    .focused($focus)
                    .padding(.top, 3)
                    .onKeyPress(phases: .down) { k in key(k) }
                    .onChange(of: m.input) { _, v in m.typing(!v.isEmpty); if m.slashSel >= max(1, m.slashItems.count) { m.slashSel = 0 } }
            }
            if m.busy && (!m.input.isEmpty || m.queued > 0) {
                Text(m.queued > 0 ? "\(m.queued) queued · sends after this turn" : "queued · sends after this turn")
                    .font(Fonts.mono(11)).foregroundStyle(p.fg2).frame(height: 22, alignment: .bottom)
            }
            AttachButton()
        }
    }

    private func drop(_ providers: [NSItemProvider]) -> Bool {
        var handled = false
        for pr in providers {
            if pr.canLoadObject(ofClass: URL.self) {
                handled = true
                _ = pr.loadObject(ofClass: URL.self) { u, _ in
                    guard let u else { return }
                    Task { @MainActor in m.attach(urls: [u]) }
                }
            } else if pr.canLoadObject(ofClass: NSImage.self) {
                handled = true
                _ = pr.loadObject(ofClass: NSImage.self) { img, _ in
                    guard let img = img as? NSImage else { return }
                    let i = img
                    Task { @MainActor in m.attach([(i, "dropped image")]) }
                }
            }
        }
        return handled
    }

    private func key(_ k: KeyPress) -> KeyPress.Result {
        let menu = m.slashItems
        // ⌘V with a picture on the clipboard attaches it; text pastes as usual
        if k.key == "v" && k.modifiers == .command && m.pasteImages() { return .handled }
        switch k.key {
        case .return:
            if k.modifiers.contains(.shift) || k.modifiers.contains(.option) { m.input += "\n"; return .handled }
            if !menu.isEmpty { m.runSlash(min(m.slashSel, menu.count - 1)); return .handled }
            let t = m.input
            m.input = ""
            m.send(t)
            return .handled
        case .escape:
            if !menu.isEmpty { m.input = ""; return .handled }
            m.interrupt()
            return .handled
        case .downArrow where !menu.isEmpty: m.slashSel = (m.slashSel + 1) % menu.count; return .handled
        case .upArrow where !menu.isEmpty: m.slashSel = (m.slashSel + menu.count - 1) % menu.count; return .handled
        case .tab where !menu.isEmpty: m.input = menu[min(m.slashSel, menu.count - 1)].0; return .handled
        default: return .ignored
        }
    }
}

/// the permission box (kit 03): takes the input's slot; ↑↓ / 1–3 / enter / esc
struct PermissionBox: View {
    @EnvironmentObject var m: AppModel
    @Environment(\.pal) var p
    @FocusState private var focus: Bool

    var body: some View {
        let a = m.approval ?? Approval(tool: "", summary: "")
        let word = ["bash": "Run", "edit": "Edit", "write": "Write"][a.tool] ?? a.tool.capitalized
        let what = a.summary.hasPrefix(a.tool + " ") ? String(a.summary.dropFirst(a.tool.count + 1)) : a.summary
        let tool = a.tool == "bash" ? "Bash" : a.tool.capitalized
        let opts = a.outside ? ["Yes, once", "Yes, allow these paths for this session", "No"]
            : ["Yes", "Yes, always allow \(tool) in this project", "No"]
        VStack(alignment: .leading, spacing: 10) {
            HStack(alignment: .firstTextBaseline, spacing: 10) {
                (Text("Allow \(word) ").font(Fonts.mono(15, .semibold)).foregroundStyle(p.fg)
                 + Text(what).font(Fonts.mono(14, .medium)).foregroundStyle(p.code)
                 + Text("?").font(Fonts.mono(15, .semibold)).foregroundStyle(p.fg))
                    .lineLimit(2)
                Spacer()
                Text(a.outside ? "\(tool) · outside \(m.title)" : "\(tool) · in \(m.title)").font(Fonts.mono(11.5))
                    .foregroundStyle(a.outside ? p.warn : p.fg2).lineLimit(1)
            }
            if a.outside {
                // where it reaches: read / write and every exact path, in the warning colour
                VStack(alignment: .leading, spacing: 3) {
                    (Text("⚠ outside the project folder · ").foregroundStyle(p.warn)
                     + Text(a.accessWord.isEmpty ? a.access : a.accessWord).foregroundStyle(p.warn).bold())
                        .font(Fonts.mono(13, .semibold))
                    ForEach(Array(a.paths.enumerated()), id: \.offset) { _, path in
                        Text(path).font(Fonts.mono(12.5, .medium)).foregroundStyle(p.fg).textSelection(.enabled)
                            .lineLimit(2).truncationMode(.middle)
                    }
                }
                .padding(.horizontal, 10).padding(.vertical, 6)
                .frame(maxWidth: .infinity, alignment: .leading)
                .background(RoundedRectangle(cornerRadius: 6).fill(p.warn.opacity(0.12)))
                .overlay(RoundedRectangle(cornerRadius: 6).strokeBorder(p.warn.opacity(0.6), lineWidth: 1))
            }
            VStack(alignment: .leading, spacing: 2) {
                ForEach(0..<3, id: \.self) { i in
                    HStack(spacing: 8) {
                        Text(i == a.sel ? "›" : " ").foregroundStyle(p.acc)
                        Text("\(i + 1). ").foregroundStyle(p.fg) + optText(opts[i], tool: tool)
                        Spacer()
                    }
                    .font(Fonts.mono(13))
                    .padding(.horizontal, 8).padding(.vertical, 4)
                    .background(RoundedRectangle(cornerRadius: 4).fill(i == a.sel ? p.sel : .clear))
                    .contentShape(Rectangle())
                    .onTapGesture { m.answer(i) }
                }
            }
            (Text("↑↓").foregroundStyle(p.fg) + Text(" select · ") + Text("enter").foregroundStyle(p.fg) + Text(" confirm · ")
             + Text("1–3").foregroundStyle(p.fg) + Text(" pick · ") + Text("esc").foregroundStyle(p.fg) + Text(" deny"))
                .font(Fonts.mono(11.5)).foregroundStyle(p.fg2)
        }
        .padding(EdgeInsets(top: 14, leading: 16, bottom: 12, trailing: 16))
        .background(RoundedRectangle(cornerRadius: 8).fill(p.inputBg))
        .overlay(RoundedRectangle(cornerRadius: 8).strokeBorder(a.outside ? p.warn : p.acc, lineWidth: 1))
        .focusable()
        .focusEffectDisabled()
        .focused($focus)
        .onAppear { focus = true }
        .onKeyPress(phases: .down) { k in
            guard var ap = m.approval else { return .ignored }
            switch k.key {
            case .upArrow: ap.sel = (ap.sel + 2) % 3; m.approval = ap
            case .downArrow: ap.sel = (ap.sel + 1) % 3; m.approval = ap
            case .return: m.answer(ap.sel)
            case .escape: m.answer(2)
            default:
                if let c = k.characters.first, let n = Int(String(c)), (1...3).contains(n) { m.answer(n - 1) } else { return .ignored }
            }
            return .handled
        }
    }

    private func optText(_ s: String, tool: String) -> Text {
        guard let r = s.range(of: " \(tool) ") else { return Text(s).foregroundStyle(p.tx) }
        return Text(s[..<r.lowerBound] + " ").foregroundStyle(p.tx) + Text(tool).foregroundStyle(p.code) + Text(" " + s[r.upperBound...]).foregroundStyle(p.tx)
    }
}

/// /resume: pick a saved session (same slot as the permission box)
struct PickerBox: View {
    @EnvironmentObject var m: AppModel
    @Environment(\.pal) var p
    @FocusState private var focus: Bool
    var body: some View {
        let list = m.picker ?? []
        VStack(alignment: .leading, spacing: 10) {
            HStack(alignment: .firstTextBaseline) {
                Text("Resume a session").font(Fonts.mono(15, .semibold)).foregroundStyle(p.fg)
                Text("\(list.count) saved · .elisa/sessions").font(Fonts.mono(11.5)).foregroundStyle(p.fg2)
            }
            ScrollView {
                VStack(alignment: .leading, spacing: 2) {
                    ForEach(Array(list.enumerated()), id: \.offset) { i, s in
                        HStack(spacing: 10) {
                            Text(i == m.pickSel ? "›" : " ").foregroundStyle(p.acc)
                            Text(s["title"] as? String ?? "").foregroundStyle(p.tx).lineLimit(1)
                            Spacer()
                            Text("\((s["n_msgs"] as? NSNumber)?.intValue ?? 0) msgs · \(ts(s["ts"]))").foregroundStyle(p.fg2)
                        }
                        .font(Fonts.mono(13)).padding(.horizontal, 8).padding(.vertical, 3)
                        .background(RoundedRectangle(cornerRadius: 4).fill(i == m.pickSel ? p.sel : .clear))
                        .contentShape(Rectangle()).onTapGesture { m.pick(i) }
                    }
                }
            }
            .frame(maxHeight: 180)
            Text("↑↓ select · enter resume · esc cancel").font(Fonts.mono(11.5)).foregroundStyle(p.fg2)
        }
        .padding(EdgeInsets(top: 14, leading: 16, bottom: 12, trailing: 16))
        .background(RoundedRectangle(cornerRadius: 8).fill(p.inputBg))
        .overlay(RoundedRectangle(cornerRadius: 8).strokeBorder(p.acc, lineWidth: 1))
        .focusable().focusEffectDisabled().focused($focus).onAppear { focus = true }
        .onKeyPress(phases: .down) { k in
            switch k.key {
            case .upArrow: m.pickSel = max(0, m.pickSel - 1)
            case .downArrow: m.pickSel = min(list.count - 1, m.pickSel + 1)
            case .return: m.pick(m.pickSel)
            case .escape: m.pick(nil)
            default: return .ignored
            }
            return .handled
        }
    }

    private func ts(_ v: Any?) -> String {
        guard let n = (v as? NSNumber)?.doubleValue else { return "" }
        let f = DateFormatter()
        f.dateFormat = "MMM d HH:mm"
        return f.string(from: Date(timeIntervalSince1970: n))
    }
}

/// the model Elisa is talking to, always in view: a dot (green = this machine, amber = online), the model name, and
/// where it runs. Clicking it opens Model Connections to switch.
struct ModelChip: View {
    @EnvironmentObject var m: AppModel
    @Environment(\.pal) var p
    @State private var hover = false

    private var local: Bool {
        guard let h = URL(string: m.baseURL)?.host?.lowercased() else { return true }
        return h == "localhost" || h == "::1" || h.hasPrefix("127.") || h.hasSuffix(".local")
    }
    private var place: String {
        if m.demo { return "scripted demo, no model" }
        let port = URL(string: m.baseURL)?.port.map { ":\($0)" } ?? ""
        return local ? "this machine\(port)" : (URL(string: m.baseURL)?.host ?? "online")
    }

    var body: some View {
        Button { ConnectionsWindow.shared.show(m) } label: {
            HStack(spacing: 6) {
                Circle().fill(m.demo ? p.fg2 : local ? p.ok : p.warn).frame(width: 7, height: 7)
                Text(m.demo ? "demo gateway" : m.model).foregroundStyle(p.tx)
                Text(place).foregroundStyle(p.fg2)
            }
            .padding(.horizontal, 8).padding(.vertical, 2)
            .background(RoundedRectangle(cornerRadius: 5).fill(hover ? p.bg3 : p.bg2))
            .overlay(RoundedRectangle(cornerRadius: 5).strokeBorder(p.gut, lineWidth: 1))
        }
        .buttonStyle(.plain)
        .onHover { hover = $0 }
        .help("Model: \(m.model)\nServer: \(m.baseURL)\nClick to change (Model Connections, ⌘,)")
    }
}

/// right-aligned meta rows: context bar, speed, watcher, audit, background job (no system stats: Activity Monitor has those)
struct MetaRows: View {
    @EnvironmentObject var m: AppModel
    @Environment(\.pal) var p
    var body: some View {
        VStack(alignment: .trailing, spacing: 1) {
            if let s = m.stat {
                let maxC = max(1, s.maxCtx > 0 ? s.maxCtx : m.maxCtx)
                let pct = min(100, s.ctxUsed * 100 / maxC)
                let c = pct < 70 ? p.ok : pct <= 85 ? p.warn : p.err
                let filled = min(10, Int((Double(pct) / 10).rounded()))
                (Text("ctx ") + Text("\(k(s.ctxUsed))/\(k(maxC)) ").foregroundStyle(p.tx)
                 + Text(String(repeating: "█", count: filled)).foregroundStyle(c) + Text(String(repeating: "░", count: 10 - filled)).foregroundStyle(p.gut)
                 + Text(" \(pct)%").foregroundStyle(c))
                (Text(String(format: "%.1f", s.tokS)).foregroundStyle(p.tx) + Text(" tok/s · ttft ") + Text(dur(s.ttft)).foregroundStyle(p.tx)
                 + Text(" · reused ") + Text(k(s.reused)).foregroundStyle(p.tx) + Text(" · Σ in \(k(m.sumIn)) · out \(k(m.sumOut))"))
            } else if m.maxCtx > 0 {
                Text("ctx –/\(k(m.maxCtx)) · no reply yet")
            }
            if m.narrow, let w = m.watchLine, let a = m.auditLine {
                // narrow: the watcher folds to one row
                Text(w + " · ") + Text(a).foregroundStyle(auditColor(a))
            } else {
                if let w = m.watchLine { Text(w) }
                if let a = m.auditLine { Text(a).foregroundStyle(auditColor(a)) }
            }
            if let j = m.job {
                let bar = j.frac.map { f in String(repeating: "█", count: Int(f * 5)) + String(repeating: "░", count: 5 - Int(f * 5)) } ?? ""
                Text(j.done ?? "\(j.label) \(bar)")
            }
        }
        .font(Fonts.mono(11.5))
        .foregroundStyle(p.fg2)
        .lineLimit(1)
        .textSelection(.enabled)
        .frame(maxWidth: .infinity, alignment: .trailing)
    }

    private func auditColor(_ a: String) -> Color { a.contains("✓") ? p.ok : a.contains("✗") ? p.warn : p.fg2 }
}
