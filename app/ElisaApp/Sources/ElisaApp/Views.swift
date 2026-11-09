// The main window (brand kit 08-source "Elisa Main Window v2" + the v2 "Elisa Prototype"): title bar 38, the
// bottom-anchored transcript with Shade docked at its bottom-right, the edit panel, and the
// bottom stack.
import AppKit
import SwiftUI

struct MainWindow: View {
    @EnvironmentObject var m: AppModel

    var body: some View {
        let p = m.pal
        if m.welcome {
            WelcomeView()
                .ignoresSafeArea()
                .environment(\.pal, p)
                .preferredColorScheme(m.dark ? .dark : .light)
        } else {
            session(p)
        }
    }

    private func session(_ p: Palette) -> some View {
        GeometryReader { g in
            // kit "BELOW 900 PX": the edit panel leaves the row (title-bar toggle, overlay sheet), padding 28 -> 20
            let narrow = g.size.width < 900
            VStack(spacing: 0) {
                TitleBar()
                HStack(spacing: 0) {
                    TranscriptColumn(winWidth: g.size.width)
                    if !narrow && m.editsOpen {
                        EditsDivider(winWidth: g.size.width)
                        EditsPane().frame(width: EditsDivider.width(m.editsWidth, g.size.width))
                    }
                }
                BottomStack()
            }
            .onAppear { m.narrow = narrow }
            .onChange(of: narrow) { _, v in m.narrow = v }
        }
        .background(p.bg)
        .ignoresSafeArea()
        .environment(\.pal, p)
        .preferredColorScheme(m.dark ? .dark : .light)
    }
}

struct TitleBar: View {
    @EnvironmentObject var m: AppModel
    @Environment(\.pal) var p
    var body: some View {
        ZStack {
            HStack(spacing: 0) {
                Pixel("ELISA", size: 11, color: p.fg)
                Text("  " + m.title).font(Fonts.mono(11.5)).foregroundStyle(p.fg2).lineLimit(1).truncationMode(.head)
            }
            .padding(.horizontal, 160)
            HStack(spacing: 8) {
                Spacer()
                chip(m.dark ? "dark ◐" : "light ◐") { m.toggleTheme() }.help("Toggle the theme (⌘⇧L)")
                if m.narrow {
                    chip(m.editsSheet ? "edits \(m.edits.count) ▸" : "edits \(m.edits.count) ◂") { m.toggleEdits() }.help("Show / hide the edits overlay (⌃O)")
                } else {
                    chip(m.editsOpen ? "edits ▸" : "◂ edits") { m.toggleEdits() }.help("Show / hide the edit panel (⌃O)")
                }
            }
            .padding(.horizontal, 14)
        }
        .frame(height: 38)
        .overlay(alignment: .bottom) { Rectangle().fill(p.sep).frame(height: 1) }
    }

    private func chip(_ t: String, _ a: @escaping () -> Void) -> some View {
        Button(action: a) {
            Text(t).font(Fonts.mono(11)).foregroundStyle(p.fg).padding(.horizontal, 8).padding(.vertical, 2)
                .overlay(RoundedRectangle(cornerRadius: 4).strokeBorder(p.sep, lineWidth: 1))
        }
        .buttonStyle(.plain)
    }
}

// MARK: - transcript

struct TranscriptColumn: View {
    @EnvironmentObject var m: AppModel
    @Environment(\.pal) var p
    let winWidth: CGFloat

    var body: some View {
        GeometryReader { geo in
            // Shade's size: the user's choice (dragged, kit dock sizes Medium..Large); below the 1100 px window breakpoint
            // the kit caps the character at narrowMax (scaled by the same size/Medium factor)
            let medium = CGFloat(m.layout.dockSizes["Medium"] ?? 128)
            let full = min(max(m.shadeSize, m.shadeRange.lowerBound), m.shadeRange.upperBound)
            let size = winWidth < m.layout.narrowBreakpoint ? min(full, m.layout.narrowMax * full / medium) : full
            let k = size / medium
            // the dock column (character + care tray + gaps): kept clear by the text, the pill and the edits overlay
            let dockW = m.shadeHidden ? 28 : size + 28 + m.layout.trayButton * k + 6
            let side: CGFloat = m.narrow ? 20 : 28
            ZStack(alignment: .bottomTrailing) {
                ScrollViewReader { proxy in
                    ScrollView {
                        VStack(alignment: .leading, spacing: 16) {
                            ForEach(m.blocks) { b in BlockView(b: b).copyMenu("Copy Message") { m.plainText(b) } }
                            // what Elisa is doing right now, in words (Activity.swift)
                            if let a = m.liveLine { LiveActivityLine(a: a) }
                            Color.clear.frame(height: 1).id("end")
                        }
                        .padding(.top, 20).padding(.leading, side).padding(.trailing, dockW).padding(.bottom, 6)
                        .frame(maxWidth: .infinity, minHeight: geo.size.height, alignment: .bottomLeading)
                        .background(ScrollWatcher { d in m.scrolled(distance: d) })
                    }
                    .defaultScrollAnchor(.bottom)
                    .scrollIndicators(.automatic)
                    .onChange(of: m.scrollTick) { if !m.scrollHeld { proxy.scrollTo("end", anchor: .bottom) } }
                    // a resize keeps a stuck transcript at its newest line
                    .onChange(of: geo.size) { if !m.scrollHeld { proxy.scrollTo("end", anchor: .bottom) } }
                    .overlay(alignment: .bottomTrailing) {
                        if m.scrollHeld {
                            NewOutputPill {
                                m.scrollHeld = false; m.heldNew = false; m.heldLines = 0
                                proxy.scrollTo("end", anchor: .bottom)
                            }
                            .padding(.trailing, dockW).padding(.bottom, 8)
                        }
                    }
                }
                if m.narrow && m.editsSheet {
                    // narrow: the edit panel as an overlay sheet over the transcript; the dock stays visible beside it
                    EditsPane()
                        .clipShape(RoundedRectangle(cornerRadius: 8))
                        .overlay(RoundedRectangle(cornerRadius: 8).strokeBorder(p.sep, lineWidth: 1))
                        .shadow(color: .black.opacity(p.dark ? 0.5 : 0.18), radius: 18, y: 10)
                        .padding(EdgeInsets(top: 10, leading: 12, bottom: 10, trailing: dockW - 4))
                        .frame(maxWidth: .infinity, maxHeight: .infinity)
                }
                // the bubble wraps inside the dock column (less the dock's trailing 16): full text, never truncated
                if !m.shadeHidden { Dock(size: size, scale: k, room: dockW - 16).padding(.trailing, 16) }
                if !m.slashItems.isEmpty { SlashMenu().padding(.leading, 24).frame(maxWidth: .infinity, alignment: .bottomLeading) }
            }
        }
    }
}

struct BlockView: View {
    @EnvironmentObject var m: AppModel
    @Environment(\.pal) var p
    let b: Block

    var body: some View {
        switch b.kind {
        case .user(let t, let cmd):
            VStack(alignment: .leading, spacing: 2) {
                Pixel("YOU", color: p.fg2)
                UserText(id: b.id, text: t, font: cmd ? Fonts.mono(13.5, .medium) : Fonts.mono(13, .semibold))
                if !b.images.isEmpty { SentImages(images: b.images) }
                if !b.docs.isEmpty { SentDocs(docs: b.docs) }
            }
        case .info(let t, let tone):
            InfoLine(text: t, tone: tone)
        case .reply:
            ZStack(alignment: .topLeading) {
                ReplyMarker(live: b.live || b.id == m.lastReplyID).offset(y: -3)
                VStack(alignment: .leading, spacing: 8) {
                    Pixel("ELISA", color: p.acc)
                    // steps reported in words: one quiet line that opens to them and to the tool rows (Activity.swift)
                    if b.actSummary != nil { ActivitySummaryView(b: b) }
                    ForEach(m.activitySeen ? b.parts.filter { !$0.isTool } : b.parts) { part in PartView(part: part) }
                }
                .padding(.leading, 36)
            }
        }
    }
}

/// Long texts (a big paste): what counts as long, and the short preview shown in its place.
enum PasteText {
    /// a paste this long goes to the pasted-text chip, and a message this long is shown collapsed
    static let minChars = 4000
    static let minLines = 60
    /// the collapsed preview: at most this many lines and characters
    static let previewLines = 12
    static let previewChars = 1200

    static func long(_ s: String) -> Bool {
        if s.utf16.count >= minChars { return true }
        var n = 0
        for c in s.utf8 where c == 10 { n += 1; if n >= minLines { return true } }
        return false
    }

    /// lines as a person counts them (a final line break does not start another)
    static func lineCount(_ s: String) -> Int { s.utf8.reduce(1) { $1 == 10 ? $0 + 1 : $0 } - (s.hasSuffix("\n") ? 1 : 0) }

    /// the first lines (at most previewLines and previewChars), and whether anything was left out
    static func preview(_ s: String) -> (text: String, cut: Bool) {
        var out = Substring()
        var lines = 0
        var end = s.startIndex
        while end < s.endIndex, lines < previewLines {
            let nl = s[end...].firstIndex(of: "\n") ?? s.endIndex
            end = nl < s.endIndex ? s.index(after: nl) : nl
            lines += 1
        }
        out = s[s.startIndex..<end]
        var cut = end < s.endIndex
        if out.count > previewChars { out = out.prefix(previewChars); cut = true }
        return (String(out).trimmingCharacters(in: .newlines), cut)
    }

    static func size(_ s: String) -> String {
        let b = s.utf8.count
        return b >= 1024 ? String(format: "%.0f KB", Double(b) / 1024) : "\(b) bytes"
    }
}

/// a sent message: in full, or, when long (a big paste), collapsed to its first lines with "… N more lines" and a
/// button to read it all. Laying out a 114 KB message in one Text took ~550 MB that stayed; the full text is now laid
/// out only when asked for, in pieces, in a lazy stack inside a bounded scroll area. The text is kept once (the block).
struct UserText: View {
    @EnvironmentObject var m: AppModel
    @Environment(\.pal) var p
    let id: UUID
    let text: String
    let font: Font

    var body: some View {
        if !PasteText.long(text) {
            Text(text).font(font).foregroundStyle(p.fg).lineSpacing(5).textSelection(.enabled)
        } else {
            let open = m.expanded.contains(id)
            let lines = PasteText.lineCount(text)
            VStack(alignment: .leading, spacing: 6) {
                if open {
                    ScrollView {
                        LazyVStack(alignment: .leading, spacing: 0) {
                            ForEach(Array(UserText.pieces(text).enumerated()), id: \.offset) { _, piece in
                                Text(piece).font(font).foregroundStyle(p.fg).lineSpacing(5).textSelection(.enabled)
                                    .frame(maxWidth: .infinity, alignment: .leading)
                            }
                        }
                    }
                    .frame(maxHeight: 420)
                } else {
                    let pv = PasteText.preview(text)
                    Text(pv.text).font(font).foregroundStyle(p.fg).lineSpacing(5).textSelection(.enabled)
                }
                HStack(spacing: 10) {
                    let pv = PasteText.preview(text)
                    let shown = PasteText.lineCount(pv.text)
                    Text(open ? "\(lines) line\(lines == 1 ? "" : "s") · \(PasteText.size(text))"
                         : "… " + (lines > shown ? "\(lines - shown) more line\(lines - shown == 1 ? "" : "s")" : "more") + " · \(PasteText.size(text)) in all")
                        .font(Fonts.mono(11.5)).foregroundStyle(p.fg2)
                    Button { m.toggleExpand(id) } label: {
                        Text(open ? "collapse" : "show all").font(Fonts.mono(11.5)).foregroundStyle(p.acc)
                    }
                    .buttonStyle(.plain)
                    Button { NSPasteboard.general.clearContents(); NSPasteboard.general.setString(text, forType: .string) } label: {
                        Text("copy").font(Fonts.mono(11.5)).foregroundStyle(p.acc)
                    }
                    .buttonStyle(.plain)
                    .help("Copy the whole message")
                }
            }
        }
    }

    /// the text in pieces of about 2000 characters, cut at a line break where there is one
    static func pieces(_ s: String) -> [String] {
        var out: [String] = []
        var i = s.startIndex
        while i < s.endIndex {
            let hard = s.index(i, offsetBy: 2000, limitedBy: s.endIndex) ?? s.endIndex
            var j = hard
            if hard < s.endIndex, let nl = s[i..<hard].lastIndex(of: "\n"), s.distance(from: i, to: nl) > 500 { j = s.index(after: nl) }
            out.append(String(s[i..<j]))
            i = j
        }
        return out
    }
}

/// the pasted-text chip above the input: size, lines and the first line; × drops it
struct PastedChip: View {
    @EnvironmentObject var m: AppModel
    @Environment(\.pal) var p
    let text: String
    var body: some View {
        let first = text.prefix(200).split(separator: "\n", omittingEmptySubsequences: true).first.map(String.init) ?? ""
        HStack(spacing: 8) {
            Text("pasted text").font(Fonts.mono(11.5, .semibold)).foregroundStyle(p.acc)
            Text("\(PasteText.size(text)) · \(PasteText.lineCount(text)) lines").font(Fonts.mono(11.5)).foregroundStyle(p.fg2)
            Text(first).font(Fonts.mono(11.5)).foregroundStyle(p.tx).lineLimit(1).truncationMode(.tail)
            Spacer(minLength: 4)
            Button { m.pasted = nil } label: { Text("×").font(Fonts.mono(13)).foregroundStyle(p.fg2) }
                .buttonStyle(.plain).help("Remove the pasted text")
        }
        .padding(.horizontal, 8).padding(.vertical, 4)
        .background(RoundedRectangle(cornerRadius: 6).fill(p.bg2))
        .overlay(RoundedRectangle(cornerRadius: 6).strokeBorder(p.sep, lineWidth: 1))
    }
}

struct InfoLine: View {
    @Environment(\.pal) var p
    let text: String
    let tone: Tone
    var body: some View {
        let c = tone == .err ? p.err : tone == .warn ? p.warn : tone == .ok ? p.ok : p.fg2
        if let rows = Self.commandRows(text) {
            // a command list (/help): two columns, so a wrapped description stays under its own start
            let w = min(rows.compactMap(\.cmd).map { Fonts.monoWidth($0, 12) }.max() ?? 0, Fonts.monoWidth(String(repeating: "m", count: 14), 12))
            VStack(alignment: .leading, spacing: 4) {
                ForEach(Array(rows.enumerated()), id: \.offset) { _, r in
                    if let cmd = r.cmd {
                        HStack(alignment: .firstTextBaseline, spacing: 12) {
                            Text(cmd).foregroundStyle(p.code).frame(width: w, alignment: .leading)
                            Text(r.text).fixedSize(horizontal: false, vertical: true)
                        }
                    } else {
                        Text(r.text).fixedSize(horizontal: false, vertical: true)
                    }
                }
            }
            .font(Fonts.mono(12)).foregroundStyle(c).textSelection(.enabled)
        } else {
            Text(text).font(Fonts.mono(12)).lineSpacing(4).foregroundStyle(c).textSelection(.enabled)
        }
    }

    /// `/name   what it does` lines (at least three): each split into the command and its description; other lines
    /// (a heading, a closing note) keep the whole width. nil: not a command list
    static func commandRows(_ s: String) -> [(cmd: String?, text: String)]? {
        let lines = s.components(separatedBy: "\n")
        guard lines.filter({ $0.hasPrefix("/") }).count >= 3 else { return nil }
        return lines.map { l in
            guard l.hasPrefix("/"), let sp = l.firstIndex(where: { $0 == " " }) else { return (nil, l) }
            return (String(l[..<sp]), l[sp...].trimmingCharacters(in: .whitespaces))
        }
    }
}

struct PartView: View {
    @Environment(\.pal) var p
    let part: Part
    var body: some View {
        switch part.kind {
        case .prose(let t): Prose(text: t)
        case .tool(let row): ToolRowView(row: row, id: part.id).copyMenu("Copy Tool Call and Output") { AppModel.plainText(part) }
        case .check(let c): CheckRowView(row: c).copyMenu("Copy Check") { AppModel.plainText(part) }
        case .footer(let t): Text(t).font(Fonts.mono(11.5)).foregroundStyle(p.fg2).textSelection(.enabled)
        case .info(let t, let tone): InfoLine(text: t, tone: tone)
        case .card(let t, let n): CardBox(text: t, n: n)
        case .box(let title, let t): TextBox(title: title, text: t)
        case .thinking(let t, let st, let en): ThinkingView(text: t, id: part.id, start: st, end: en).copyMenu("Copy Reasoning") { AppModel.plainText(part) }
        }
    }
}

/// the model's reasoning, as a person thinking: while it thinks, a thought bubble, "thinking" with a band of light
/// passing over it and its seconds, and its newest sentences running upward in a 3-row window (under "…");
/// when it is done, one quiet line ("thought for 12s · 263 words") that opens to the whole text.
struct ThinkingView: View {
    /// the reasoning without the `<think>` tags some models send inside it
    static func clean(_ text: String) -> String {
        text.replacingOccurrences(of: "<think>", with: "").replacingOccurrences(of: "</think>", with: "").trimmingCharacters(in: .whitespacesAndNewlines)
    }

    @EnvironmentObject var m: AppModel
    @Environment(\.pal) var p
    let text: String
    let id: UUID
    let start: Date
    let end: Date?
    /// the preview's width, for how many characters make 3 rows
    @State private var width: CGFloat = 700

    private static let thoughtFont = Font.system(size: 12.5, design: .rounded).italic()
    private var live: Bool { end == nil }

    var body: some View {
        let open = m.expanded.contains(id)
        VStack(alignment: .leading, spacing: 8) {
            Button {
                withAnimation(.spring(response: 0.38, dampingFraction: 0.86)) { m.toggleExpand(id) }
            } label: { header(open) }
            .buttonStyle(.plain)
            if open {
                full.transition(.opacity.combined(with: .move(edge: .top)))
            } else if live && !Self.clean(text).isEmpty {
                preview.transition(.opacity)
            }
        }
        .frame(maxWidth: 860, alignment: .leading)
        // finishing settles the preview away softly instead of cutting it
        .animation(.easeInOut(duration: 0.45), value: live)
    }

    private func header(_ open: Bool) -> some View {
        HStack(spacing: 8) {
            if live {
                TimelineView(.animation) { tl in
                    let t = tl.date.timeIntervalSinceReferenceDate
                    HStack(spacing: 8) {
                        ThoughtBubble(t: t, color: p.acc)
                        Shimmer(text: "thinking", t: t, base: p.fg2, light: p.fg)
                        Text(dur(tl.date.timeIntervalSince(start))).foregroundStyle(p.fg2.opacity(0.7)).monospacedDigit()
                    }
                }
            } else {
                Image(systemName: "chevron.right")
                    .font(.system(size: 9, weight: .semibold))
                    .rotationEffect(.degrees(open ? 90 : 0))
                Text("thought for \(dur((end ?? Date()).timeIntervalSince(start)))")
                Text("· \(words) words").foregroundStyle(p.fg2.opacity(0.7))
            }
        }
        .font(Fonts.mono(11.5)).foregroundStyle(p.fg2)
        .contentShape(Rectangle())
    }

    private var words: Int { Self.clean(text).split(whereSeparator: \.isWhitespace).count }

    /// the reasoning in sentences (after . ! ? or a line break), each with its index so it keeps its identity as more
    /// text comes; the last one may still be growing
    static func sentences(_ text: String) -> [String] {
        var out: [String] = [], cur = ""
        let chars = Array(clean(text))
        for (i, c) in chars.enumerated() {
            if c == "\n" { if !cur.trimmingCharacters(in: .whitespaces).isEmpty { out.append(cur.trimmingCharacters(in: .whitespaces)) }; cur = ""; continue }
            cur.append(c)
            if ".!?".contains(c), i + 1 < chars.count, chars[i + 1] == " " || chars[i + 1] == "\n" {
                out.append(cur.trimmingCharacters(in: .whitespaces)); cur = ""
            }
        }
        if !cur.trimmingCharacters(in: .whitespaces).isEmpty { out.append(cur.trimmingCharacters(in: .whitespaces)) }
        return out
    }

    /// one row of the preview in 12.5-pt rounded text with its line spacing
    private static let row: CGFloat = 19

    /// the newest sentences running upward in a 3-row window: a new one slides in from below, the older ones drift up
    /// and fade out at the top under "…"
    private var preview: some View {
        let all = Self.sentences(text)
        let shown = Array(all.enumerated().suffix(6))
        // more than the window holds: the "…" above, and the top row fades
        let more = all.count > 3 || Self.clean(text).count > Int(width / 6.4) * 3
        return VStack(alignment: .leading, spacing: 1) {
            if more { Text("…").foregroundStyle(p.fg2.opacity(0.5)) }
            VStack(alignment: .leading, spacing: 3) {
                ForEach(shown, id: \.offset) { _, line in
                    Text(line)
                        .frame(maxWidth: .infinity, alignment: .leading)
                        .fixedSize(horizontal: false, vertical: true)
                        .transition(.asymmetric(insertion: .move(edge: .bottom).combined(with: .opacity), removal: .opacity))
                }
            }
            // bottom-aligned and clipped to 3 rows: the newest words stay in view, the oldest leave over the top
            .frame(height: Self.row * 3, alignment: .bottom)
            .clipped()
            .mask(LinearGradient(stops: [.init(color: .black.opacity(more ? 0 : 1), location: 0), .init(color: .black, location: 0.55)], startPoint: .top, endPoint: .bottom))
            .animation(.spring(response: 0.55, dampingFraction: 0.9), value: all.count)
            .background(GeometryReader { g in Color.clear.onAppear { width = g.size.width }.onChange(of: g.size.width) { _, w in width = w } })
        }
        .font(Self.thoughtFont).foregroundStyle(p.fg2).lineSpacing(3)
        .padding(.horizontal, 14).padding(.vertical, 10)
        .background(RoundedRectangle(cornerRadius: 12, style: .continuous).fill(p.bg2.opacity(0.7)))
        .onTapGesture { withAnimation(.spring(response: 0.38, dampingFraction: 0.86)) { m.toggleExpand(id) } }
    }

    /// the whole reasoning, scrollable
    private var full: some View {
        ScrollView {
            Text(Self.clean(text))
                .font(Self.thoughtFont).foregroundStyle(p.fg2).lineSpacing(4)
                .textSelection(.enabled)
                .frame(maxWidth: .infinity, alignment: .leading)
                .padding(.horizontal, 14).padding(.vertical, 10)
        }
        .frame(maxHeight: 280)
        .background(RoundedRectangle(cornerRadius: 12, style: .continuous).fill(p.bg2.opacity(0.7)))
    }

    /// "0.8s" under 10 s (a short thought is not "0s"), "12s", "1m 5s"
    private func dur(_ s: Double) -> String {
        s < 10 ? String(format: "%.1fs", max(0, s)) : s < 60 ? "\(Int(s))s" : "\(Int(s) / 60)m \(Int(s) % 60)s"
    }
}

/// a thought bubble: three circles rising from small to big (○ o O), each floating up and filling in after the one
/// before, like the bubbles above someone lost in thought
struct ThoughtBubble: View {
    let t: Double
    let color: Color
    private static let sizes: [CGFloat] = [3.5, 5, 7]
    var body: some View {
        HStack(alignment: .bottom, spacing: 2.5) {
            ForEach(0..<3, id: \.self) { i in
                // a 2.4 s cycle, each circle 0.3 s behind the smaller one
                let v = 0.5 + 0.5 * sin((t / 2.4 - Double(i) * 0.125) * 2 * .pi)
                Circle()
                    .fill(color.opacity(0.18 + 0.5 * v))
                    .overlay(Circle().strokeBorder(color.opacity(0.45 + 0.45 * v), lineWidth: 1))
                    .frame(width: Self.sizes[i], height: Self.sizes[i])
                    // rising to the right, each bubble a little higher, and floating with its breath
                    .offset(y: -CGFloat(i) * 2.5 - 1.5 * v)
            }
        }
        .frame(height: 12, alignment: .bottom)
    }
}

/// text with a soft band of light passing over it, left to right, every 2.2 s
struct Shimmer: View {
    let text: String
    let t: Double
    let base: Color
    let light: Color
    var body: some View {
        // the band's centre runs from before the first letter to past the last
        let x = (t.truncatingRemainder(dividingBy: 2.2) / 2.2) * 1.6 - 0.3
        Text(text).foregroundStyle(base)
            .overlay(
                LinearGradient(stops: [
                    .init(color: light.opacity(0), location: max(0, x - 0.2)),
                    .init(color: light.opacity(0.9), location: min(1, max(0, x))),
                    .init(color: light.opacity(0), location: min(1, x + 0.2)),
                ], startPoint: .leading, endPoint: .trailing)
                .mask(Text(text))
            )
    }
}

/// assistant prose: inline markdown (code spans in the code colour) and fenced code blocks in a box
struct Prose: View {
    @Environment(\.pal) var p
    let text: String

    var body: some View {
        VStack(alignment: .leading, spacing: 10) {
            ForEach(Array(Prose.split(text).enumerated()), id: \.offset) { _, seg in
                if seg.code {
                    CodeBox(lang: seg.lang, code: seg.text)
                } else if !seg.text.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty {
                    MarkdownBlocks(text: seg.text.trimmingCharacters(in: .newlines), inline: inline)
                }
            }
        }
    }

    struct Seg { var text: String; var code: Bool; var lang: String }

    /// ``` fences; an unclosed fence (still streaming) is a code block too
    static func split(_ s: String) -> [Seg] {
        var out: [Seg] = []
        var cur = Seg(text: "", code: false, lang: "")
        for line in s.components(separatedBy: "\n") {
            if line.trimmingCharacters(in: .whitespaces).hasPrefix("```") {
                out.append(cur)
                cur = cur.code ? Seg(text: "", code: false, lang: "") : Seg(text: "", code: true, lang: String(line.trimmingCharacters(in: .whitespaces).dropFirst(3)))
                continue
            }
            cur.text += (cur.text.isEmpty ? "" : "\n") + line
        }
        out.append(cur)
        return out.filter { $0.code || !$0.text.isEmpty }
    }

    private func inline(_ s: String) -> AttributedString {
        var a = (try? AttributedString(markdown: s, options: .init(interpretedSyntax: .inlineOnlyPreservingWhitespace))) ?? AttributedString(s)
        a.font = Fonts.mono(13)
        a.foregroundColor = p.tx
        for r in a.runs {
            guard let i = r.inlinePresentationIntent else { continue }
            if i.contains(.code) { a[r.range].foregroundColor = p.code }
            if i.contains(.stronglyEmphasized) { a[r.range].font = Fonts.mono(13, .semibold); a[r.range].foregroundColor = p.fg }
            // slanted by a font matrix: the bundled face has no italic, so `.italic()` drew it upright
            if i.contains(.emphasized) { a[r.range].font = Fonts.monoItalic(13, i.contains(.stronglyEmphasized) ? .semibold : .regular) }
            if i.contains(.strikethrough) { a[r.range].strikethroughStyle = .single; a[r.range].foregroundColor = p.fg2 }
        }
        // links: clickable, in the accent colour
        for r in a.runs where r.link != nil { a[r.range].foregroundColor = p.acc; a[r.range].underlineStyle = .single }
        return a
    }
}

struct CodeBox: View {
    @Environment(\.pal) var p
    let lang: String
    let code: String
    var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            HStack {
                Text(lang.isEmpty ? "code" : lang)
                Spacer()
                Button("/copy") {
                    NSPasteboard.general.clearContents()
                    NSPasteboard.general.setString(code, forType: .string)
                }
                .buttonStyle(.plain)
            }
            .font(Fonts.mono(11)).foregroundStyle(p.fg2).padding(.horizontal, 12).padding(.vertical, 4)
            Rectangle().fill(p.sep).frame(height: 1)
            ScrollView(.horizontal, showsIndicators: false) {
                Group {
                    if let a = Highlight.attributed(code, lang: lang, pal: p, font: Fonts.mono(12.5)) {
                        Text(a)
                    } else {
                        Text(code).font(Fonts.mono(12.5)).foregroundStyle(p.tx)
                    }
                }
                .lineSpacing(5).textSelection(.enabled).fixedSize()
                .padding(.horizontal, 12).padding(.vertical, 9)
            }
        }
        .frame(maxWidth: 680, alignment: .leading)
        .background(RoundedRectangle(cornerRadius: 6).fill(p.bg2))
        .overlay(RoundedRectangle(cornerRadius: 6).strokeBorder(p.sep, lineWidth: 1))
    }
}

/// a tool row (kit B): pixel badge · verb · target · ± · elapsed, then the ⎿ preview
struct ToolRowView: View {
    @EnvironmentObject var m: AppModel
    @Environment(\.pal) var p
    let row: ToolRow
    let id: UUID

    /// the output's lines as the expanded view shows them (no trailing blank or `[exit N]` line, at most 300)
    private var outLines: [String] {
        var ls = row.output.components(separatedBy: "\n").map { $0.replacingOccurrences(of: "\t", with: "    ") }
        while let l = ls.last, l.trimmingCharacters(in: .whitespaces).isEmpty || (l.hasPrefix("[exit ") && l.hasSuffix("]")) { ls.removeLast() }
        if nextHint != nil { ls.removeLast() }
        return Array(ls.prefix(300))
    }

    /// cut output: its last line when that is a paging hint (`next: …`, e.g. docs' `[output cut at the size limit;
    /// next: pages="2-11"]`, which elisa keeps after the cut): shown even when collapsed, and last when expanded
    private var nextHint: String? {
        guard row.output.contains("more chars]") else { return nil }
        let last = row.output.trimmingCharacters(in: .whitespacesAndNewlines).components(separatedBy: "\n").last ?? ""
        let t = last.trimmingCharacters(in: .whitespaces)
        return t.hasPrefix("next:") || t.contains("; next:") || t.hasPrefix("[next:") ? t : nil
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            HStack(alignment: .firstTextBaseline, spacing: 8) {
                badge
                Text(row.verb).font(Fonts.mono(12.5, .semibold)).foregroundStyle(p.fg)
                Text(row.target).font(Fonts.mono(12.5)).foregroundStyle(p.code).lineLimit(1).truncationMode(.middle)
                if row.status == .ok, let a = row.add {
                    Text("+\(a)").font(Fonts.mono(12.5)).foregroundStyle(p.ok)
                    Text("−\(row.del ?? 0)").font(Fonts.mono(12.5)).foregroundStyle(p.err)
                }
                Spacer(minLength: 8)
                elapsed
            }
            .frame(minHeight: 20)
            if !row.note.isEmpty && row.status != .denied && !(row.status == .ok && row.add != nil) {
                let lines = outLines
                let open = m.expanded.contains(id)
                // collapsed: a red row shows its 3 preview lines + "… +N lines ▸ expand"; any other row with more output
                // than its one-line note gets "▸ expand" beside the note (kit: ⎿ preview collapses to 3 lines)
                let shown = row.red ? max(1, row.preview.count) : 1
                let canExpand = lines.count > shown || row.more > 0
                HStack(alignment: .firstTextBaseline, spacing: 10) {
                    Text("⎿ " + row.note).foregroundStyle(row.red ? p.err : p.fg2).lineLimit(2)
                    if canExpand && !row.red && !open { toggle(false) }
                }
                .font(Fonts.mono(12.5)).padding(.leading, 34).frame(minHeight: 20)
                if !open, let h = nextHint { hintLine(h) }
                if open {
                    ForEach(Array(lines.enumerated()), id: \.offset) { _, l in line(l) }
                    if row.output.contains("more chars]") || lines.count == 300 { line("… (output cut)") }
                    if let h = nextHint { hintLine(h) }
                    toggle(true).padding(.leading, 34 + 24).frame(minHeight: 20)
                } else if row.red {
                    ForEach(Array(row.preview.enumerated()), id: \.offset) { _, l in line(l) }
                    if row.more > 0 || canExpand {
                        HStack(spacing: 8) {
                            if row.more > 0 { Text("… +\(row.more) lines").foregroundStyle(p.fg2) }
                            if canExpand { toggle(false) }
                        }
                        .font(Fonts.mono(12.5)).padding(.leading, 34 + 24).frame(minHeight: 20)
                    }
                }
            }
        }
    }

    /// the paging hint of cut output, in the code colour so it reads as the way on
    private func hintLine(_ l: String) -> some View {
        Text(l).font(Fonts.mono(12.5)).foregroundStyle(p.code).lineLimit(2).truncationMode(.middle)
            .padding(.leading, 34 + 24).frame(minHeight: 20, alignment: .leading)
            .textSelection(.enabled)
    }

    private func line(_ l: String) -> some View {
        Text(l.isEmpty ? " " : l).font(Fonts.mono(12.5)).foregroundStyle(p.fg2).lineLimit(1).truncationMode(.tail)
            .padding(.leading, 34 + 24).frame(minHeight: 20, alignment: .leading)
            .textSelection(.enabled)
    }

    /// "▸ expand" / "▾ collapse" (kit: in text colour)
    private func toggle(_ open: Bool) -> some View {
        Button { m.toggleExpand(id) } label: {
            Text(open ? "▾ collapse" : "▸ expand").font(Fonts.mono(12.5)).foregroundStyle(p.tx)
        }
        .buttonStyle(.plain)
        .help(open ? "Show the 3-line preview" : "Show the whole output")
    }

    private var badge: some View {
        let (t, c): (String, Color) = switch row.status {
        case .ok: ("OK", p.ok)
        case .err: ("ERR", p.err)
        case .running: ("RUN", p.acc)
        case .pending: ("ASK", p.warn)
        case .denied: ("NO", p.err)
        }
        return Text(t).font(Fonts.pixel(10)).tracking(0.6).foregroundStyle(p.bg).padding(.horizontal, 5).frame(height: 18).background(c)
    }

    @ViewBuilder private var elapsed: some View {
        switch row.status {
        case .running:
            TimelineView(.periodic(from: .now, by: 0.1)) { tl in
                Text(dur(tl.date.timeIntervalSince(row.t0))).font(Fonts.mono(12.5)).foregroundStyle(p.fg)
            }
        case .pending: Text("waiting for permission").font(Fonts.mono(12.5)).foregroundStyle(p.warn)
        case .denied: Text("denied").font(Fonts.mono(12.5)).foregroundStyle(p.err)
        default: Text(dur(row.elapsed ?? 0)).font(Fonts.mono(12.5)).foregroundStyle(p.fg2)
        }
    }
}

/// a fix-loop check as a tool-style row: `FIX check cargo clippy ✗ 3 errors → fixing (1/2)`; the running row is
/// replaced by the result row for the same cmd and round (Model), as the TUI does
struct CheckRowView: View {
    @Environment(\.pal) var p
    let row: CheckRow
    @State private var t0 = Date()

    var body: some View {
        let (badge, bc): (String, Color) = row.running ? ("RUN", p.acc) : row.passed ? ("OK", p.ok) : row.fixing ? ("FIX", p.warn) : ("ERR", p.err)
        HStack(alignment: .firstTextBaseline, spacing: 8) {
            // badge and "check" never give up width (a long "→ stopped" outcome squeezed them to one letter per line)
            Text(badge).font(Fonts.pixel(10)).tracking(0.6).foregroundStyle(p.bg).lineLimit(1).fixedSize()
                .padding(.horizontal, 5).frame(height: 18).background(bc)
            if !row.known || row.cmd.isEmpty {
                // a status this app does not know: Elisa's own line
                Text(row.line.isEmpty ? "check" : row.line).font(Fonts.mono(12.5)).foregroundStyle(p.tx).lineLimit(1)
            } else {
                // one text that wraps under itself in a narrow row: nothing is squeezed out (the outcome vanished,
                // or "check" went one letter per line, when the three pieces fought for one line)
                (Text("check ").font(Fonts.mono(12.5, .semibold)).foregroundStyle(p.fg)
                    + Text(row.cmd).font(Fonts.mono(12.5)).foregroundStyle(p.code)
                    + Text(" ") + outcome.font(Fonts.mono(12.5)))
                    .fixedSize(horizontal: false, vertical: true).layoutPriority(1)
            }
            Spacer(minLength: 8)
            if row.running {
                TimelineView(.periodic(from: .now, by: 0.1)) { tl in
                    Text(dur(tl.date.timeIntervalSince(t0))).font(Fonts.mono(12.5)).foregroundStyle(p.fg)
                }
            } else if let r = row.round, r > 0 {
                Text("after fix \(r)").font(Fonts.mono(12.5)).foregroundStyle(p.fg2).lineLimit(1).fixedSize()
            }
        }
        .frame(minHeight: 20)
    }

    private var outcome: Text {
        let sum = row.summary.isEmpty ? Text("") : Text(" " + row.summary).foregroundStyle(p.tx)
        if row.running { return Text("…").foregroundStyle(p.fg2) }
        if row.passed { return Text("✓").foregroundStyle(p.ok) + sum }
        let mark = Text("✗").foregroundStyle(p.err) + sum
        if row.stopped { return mark + Text(" → stopped").foregroundStyle(p.err) }
        if row.fixing {
            let n = row.rounds.map { " (\((row.round ?? 0) + 1)/\($0))" } ?? ""
            return mark + Text(" → fixing" + n).foregroundStyle(p.warn)
        }
        return mark
    }
}

/// the state card (what Elisa carries forward): `key: value` lines in a two-column grid
struct CardBox: View {
    @EnvironmentObject var m: AppModel
    @Environment(\.pal) var p
    let text: String
    var n: Int? = nil
    var body: some View {
        let rows: [(String, String)] = text.components(separatedBy: "\n").compactMap { l in
            let t = l.trimmingCharacters(in: .whitespaces).trimmingCharacters(in: CharacterSet(charactersIn: "-* "))
            guard !t.isEmpty else { return nil }
            if let i = t.firstIndex(of: ":"), t.distance(from: t.startIndex, to: i) < 16 {
                return (String(t[..<i]), t[t.index(after: i)...].trimmingCharacters(in: .whitespaces))
            }
            return ("", t)
        }
        VStack(alignment: .leading, spacing: 10) {
            HStack(spacing: 10) {
                Pixel("STATE", color: p.fg2)
                if let n { Text("state #\(n)").font(Fonts.mono(11.5, .semibold)).foregroundStyle(p.fg) }
                Text("carried forward").font(Fonts.mono(11.5)).foregroundStyle(p.fg2)
                Spacer()
                Text("/state").font(Fonts.mono(11.5)).foregroundStyle(p.fg2)
            }
            Grid(alignment: .leading, horizontalSpacing: 16, verticalSpacing: 4) {
                ForEach(Array(rows.enumerated()), id: \.offset) { _, r in
                    GridRow {
                        Text(r.0).foregroundStyle(p.fg2).frame(width: 80, alignment: .leading)
                        Text(r.1).foregroundStyle(p.tx)
                    }
                }
            }
            .font(Fonts.mono(12))
        }
        .padding(EdgeInsets(top: 12, leading: 16, bottom: 14, trailing: 16))
        .frame(maxWidth: 860, alignment: .leading)
        .overlay(RoundedRectangle(cornerRadius: 8).strokeBorder(p.sep, lineWidth: 1))
    }
}

struct TextBox: View {
    @Environment(\.pal) var p
    let title: String
    let text: String
    var body: some View {
        VStack(alignment: .leading, spacing: 10) {
            Pixel(title, color: p.fg2)
            Text(text).font(Fonts.mono(12)).foregroundStyle(p.tx).lineSpacing(5).textSelection(.enabled)
        }
        .padding(EdgeInsets(top: 12, leading: 16, bottom: 14, trailing: 16))
        .frame(maxWidth: 860, alignment: .leading)
        .overlay(RoundedRectangle(cornerRadius: 8).strokeBorder(p.sep, lineWidth: 1))
    }
}

/// the slash menu: commands from Elisa's hello line, filtered by what is typed
struct SlashMenu: View {
    @EnvironmentObject var m: AppModel
    @Environment(\.pal) var p
    var body: some View {
        let items = m.slashItems
        VStack(alignment: .leading, spacing: 0) {
            ForEach(Array(items.enumerated()), id: \.offset) { i, c in
                HStack(spacing: 14) {
                    Text(c.0).font(Fonts.mono(12.5, .medium)).foregroundStyle(i == m.slashSel ? p.acc : p.fg).frame(width: 84, alignment: .leading)
                    Text(c.1).font(Fonts.mono(12.5)).foregroundStyle(p.tx).lineLimit(1)
                    Spacer(minLength: 0)
                }
                .padding(.horizontal, 8).padding(.vertical, 3)
                .background(RoundedRectangle(cornerRadius: 4).fill(i == m.slashSel ? p.sel : .clear))
                .contentShape(Rectangle())
                .onTapGesture { m.runSlash(i) }
            }
            Rectangle().fill(p.sep).frame(height: 1).padding(.top, 4)
            Text("↑↓ move · tab complete · enter run · esc close").font(Fonts.mono(11)).foregroundStyle(p.fg2).padding(.horizontal, 8).padding(.top, 4)
        }
        .padding(6)
        .frame(width: 470)
        .background(RoundedRectangle(cornerRadius: 8).fill(p.bg2).shadow(color: .black.opacity(0.4), radius: 16, y: 12))
        .overlay(RoundedRectangle(cornerRadius: 8).strokeBorder(p.sep, lineWidth: 1))
    }
}
