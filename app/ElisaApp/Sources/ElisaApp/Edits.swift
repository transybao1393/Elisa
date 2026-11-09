// The edit panel (kit C): the newest edit as a diff (old / new line numbers, ± column, rows tinted 13%), the history
// pinned at the bottom; [ prev / ] next step through it, ctrl-o hides it. Built from Elisa's tool_detail lines (the
// TUI's own edit model, computed before the tool runs).
import AppKit
import SwiftUI

struct EditsPane: View {
    @EnvironmentObject var m: AppModel
    @Environment(\.pal) var p

    var body: some View {
        VStack(spacing: 0) {
            HStack(spacing: 6) {
                Pixel("EDITS", color: p.fg2)
                Text(m.edits.isEmpty ? "0" : "\(m.editIdx + 1) / \(m.edits.count)").font(Fonts.mono(12)).foregroundStyle(p.fg).padding(.leading, 4)
                    .lineLimit(1).fixedSize()
                Spacer(minLength: 4)
                // a narrowed panel drops the button words, keeping the keys
                ViewThatFits(in: .horizontal) {
                    buttons(prev: "[ prev", next: "] next", hide: "ctrl-o hide")
                    buttons(prev: "[", next: "]", hide: "ctrl-o")
                }
            }
            .padding(.leading, 16).padding(.trailing, 10)
            .frame(height: 40)
            .overlay(alignment: .bottom) { Rectangle().fill(p.sep).frame(height: 1) }
            if m.edits.indices.contains(m.editIdx) {
                diff(m.edits[m.editIdx])
                history
            } else {
                VStack(spacing: 6) {
                    Text("edits will show here").foregroundStyle(p.tx)
                    Text("file changes from this session appear as diffs").foregroundStyle(p.fg2)
                }
                .font(Fonts.mono(12)).multilineTextAlignment(.center).padding(20)
                .frame(maxWidth: .infinity, maxHeight: .infinity)
            }
        }
        .background(p.bg)
    }

    private func btn(_ t: String, _ a: @escaping () -> Void) -> some View {
        Button(action: a) { Text(t).font(Fonts.mono(11)).foregroundStyle(p.fg2).lineLimit(1).fixedSize().padding(.horizontal, 4).padding(.vertical, 2) }.buttonStyle(.plain)
    }

    private func buttons(prev: String, next: String, hide: String) -> some View {
        HStack(spacing: 6) {
            if !m.edits.isEmpty {
                btn(prev) { m.editPrev() }.help("Previous edit ([)")
                btn(next) { m.editNext() }.help("Next edit (])")
            }
            btn(hide) { m.toggleEdits() }.help("Hide the edit panel (⌃O)")
        }
    }

    private func hunk(_ e: Edit) -> String {
        let o = e.lines.compactMap { $0.k == "add" ? nil : $0.o }
        let n = e.lines.compactMap { $0.k == "del" ? nil : $0.n }
        guard let o0 = o.first ?? n.first, let n0 = n.first ?? o.first else { return e.kind }
        return "@@ -\(o0),\(o.count) +\(n0),\(n.count) @@  \(e.kind)"
    }

    private func diff(_ e: Edit) -> some View {
        VStack(spacing: 0) {
            HStack(spacing: 10) {
                Text(e.path).foregroundStyle(p.code).lineLimit(1).truncationMode(.head)
                Text("+\(e.add)").foregroundStyle(p.ok)
                Text("−\(e.del)").foregroundStyle(p.err)
                Spacer()
                Text("turn \(e.turn) · \(Self.time.string(from: e.time))").font(Fonts.mono(11)).foregroundStyle(p.fg2).lineLimit(1).fixedSize()
            }
            .font(Fonts.mono(12))
            .padding(EdgeInsets(top: 10, leading: 16, bottom: 8, trailing: 16))
            Text(hunk(e)).font(Fonts.mono(12)).foregroundStyle(p.fg2).lineLimit(1)
                .frame(maxWidth: .infinity, alignment: .leading).padding(.horizontal, 16).padding(.vertical, 2)
                .background(p.bg2)
                .overlay(alignment: .top) { Rectangle().fill(p.sep).frame(height: 1) }
                .overlay(alignment: .bottom) { Rectangle().fill(p.sep).frame(height: 1) }
            if let f = e.failed {
                Text("⎿ not applied: \(f)").font(Fonts.mono(12)).foregroundStyle(p.err).frame(maxWidth: .infinity, alignment: .leading).padding(.horizontal, 16).padding(.vertical, 4)
            }
            // one 2-axis scroll view: lines never wrap or clip, the gutters scroll with their line so they stay aligned;
            // vertical wheel / swipe scrolls down, horizontal swipe and shift+wheel scroll sideways
            GeometryReader { g in
                ScrollView([.horizontal, .vertical]) {
                    let w = max(g.size.width, Self.gutter + (e.lines.map { Fonts.monoWidth($0.t, Self.fs) }.max() ?? 0) + 16)
                    LazyVStack(alignment: .leading, spacing: 0) {
                        ForEach(e.lines) { l in row(l).frame(width: w, alignment: .leading) }
                    }
                    .padding(.vertical, 2)
                    .frame(minWidth: g.size.width, minHeight: g.size.height, alignment: .topLeading)
                }
            }
            .frame(maxHeight: .infinity)
        }
    }

    /// diff content text: 10% under the 12 pt header text
    static let fs: CGFloat = 10.8
    /// old no. + new no. + ± columns
    static let gutter: CGFloat = 36 + 31 + 27

    private func row(_ l: DiffLine) -> some View {
        let (sign, sc, bg): (String, Color, Color) = switch l.k {
        case "add": ("+", p.ok, p.add)
        case "del": ("−", p.err, p.del)
        default: (" ", p.fg2, .clear)
        }
        return HStack(spacing: 0) {
            if l.k == "gap" || l.k == "note" {
                Text(l.t).foregroundStyle(p.fg2).fixedSize().padding(.leading, 16)
            } else {
                Text(l.o.map(String.init) ?? "").foregroundStyle(p.gut).frame(width: 36, alignment: .trailing)
                Text(l.n.map(String.init) ?? "").foregroundStyle(p.gut).frame(width: 31, alignment: .trailing)
                Text(sign).foregroundStyle(sc).frame(width: 27, alignment: .center)
                Text(l.t).foregroundStyle(p.tx).lineLimit(1).fixedSize()
            }
            Spacer(minLength: 0)
        }
        .font(Fonts.mono(Self.fs))
        .frame(minHeight: 17, alignment: .leading)
        .background(bg)
    }

    private var history: some View {
        VStack(alignment: .leading, spacing: 0) {
            Pixel("HISTORY", color: p.fg2).padding(.bottom, 4)
            ScrollView {
                VStack(spacing: 0) {
                    ForEach(Array(m.edits.enumerated()), id: \.element.id) { i, e in
                        // a denied / failed edit is struck through and says so: it never reached the file
                        let off = e.failed != nil
                        HStack(spacing: 8) {
                            Text("\(i + 1)").frame(width: 14, alignment: .leading)
                            Text(e.kind == "edit" ? "Edit" : "Write").foregroundStyle(off ? p.fg2 : p.fg).strikethrough(off)
                            Text(e.path).foregroundStyle(off ? p.fg2 : p.code).strikethrough(off).lineLimit(1).truncationMode(.head)
                            if off {
                                Text(e.failed == "denied" ? "denied" : "not applied").foregroundStyle(p.err).lineLimit(1).fixedSize()
                            } else {
                                if e.add > 0 { Text("+\(e.add)").foregroundStyle(p.ok) }
                                if e.del > 0 { Text("−\(e.del)").foregroundStyle(p.err) }
                            }
                            Spacer()
                            Text("t\(e.turn)")
                        }
                        .font(Fonts.mono(11.5)).foregroundStyle(p.fg2).frame(height: 20)
                        .padding(.horizontal, 6)
                        .background(RoundedRectangle(cornerRadius: 4).fill(i == m.editIdx ? p.sel : .clear))
                        .contentShape(Rectangle())
                        .onTapGesture { m.editIdx = i }
                    }
                }
            }
            .frame(maxHeight: 120)
            .fixedSize(horizontal: false, vertical: true)
        }
        .padding(EdgeInsets(top: 10, leading: 10, bottom: 12, trailing: 10))
        .overlay(alignment: .top) { Rectangle().fill(p.sep).frame(height: 1) }
    }

    static let time: DateFormatter = { let f = DateFormatter(); f.dateFormat = "HH:mm"; return f }()
}

/// The divider on the edit panel's left edge: drag it to resize the panel (the transcript takes the freed space),
/// left-right resize cursor on hover, double-click resets to the max. The window clamps the width so the transcript
/// keeps at least `transcriptMin` (the saved width is untouched, so a wider window brings it back).
struct EditsDivider: View {
    @EnvironmentObject var m: AppModel
    @Environment(\.pal) var p
    let winWidth: CGFloat
    static let transcriptMin: CGFloat = 459
    @State private var start: CGFloat?
    @State private var hover = false
    @State private var cursorOn = false

    /// the width the layout uses: the saved width, clamped to what the window leaves after the transcript's minimum
    static func width(_ saved: CGFloat, _ win: CGFloat) -> CGFloat {
        let r = AppModel.editsWidthRange
        return max(r.lowerBound, min(saved, win - 1 - transcriptMin))
    }

    var body: some View {
        Rectangle().fill(p.sep).frame(width: 1)
            .overlay(RoundedRectangle(cornerRadius: 3).fill(start != nil || hover ? p.fg2 : p.gut).frame(width: 5, height: 28))
            .overlay(
                Color.clear.frame(width: 9).contentShape(Rectangle())
                    .onHover { hover = $0; cursor(hover || start != nil) }
                    .gesture(DragGesture(minimumDistance: 1, coordinateSpace: .global)
                        .onChanged { g in
                            let s = start ?? Self.width(m.editsWidth, winWidth)
                            if start == nil { start = s }
                            m.setEditsWidth(min(s - g.translation.width, winWidth - 1 - Self.transcriptMin))
                        }
                        .onEnded { _ in start = nil; cursor(hover) })
                    .simultaneousGesture(TapGesture(count: 2).onEnded { m.setEditsWidth(AppModel.editsWidthRange.upperBound) })
                    .help("Drag to resize · double-click to reset")
            )
            .zIndex(1)
            .onDisappear { cursor(false) }
            .accessibilityElement()
            .accessibilityLabel("Edit panel width")
            .accessibilityValue("\(Int(m.editsWidth)) points")
            .accessibilityAdjustableAction { d in
                m.setEditsWidth(m.editsWidth + (d == .increment ? 20 : -20))
            }
    }

    private func cursor(_ on: Bool) {
        guard on != cursorOn else { return }
        cursorOn = on
        if on { NSCursor.resizeLeftRight.push() } else { NSCursor.pop() }
    }
}
