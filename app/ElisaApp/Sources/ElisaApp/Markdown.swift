import SwiftUI

/// Block markdown for assistant prose (the parts between ``` fences): headings, bullet / numbered / task lists with
/// nesting, block quotes, horizontal rules, tables, paragraphs. Inline markup (bold, italic, `code`, links,
/// strikethrough) goes through AttributedString. Tolerant of a reply still streaming: a half-typed table or list is
/// just shown as far as it goes.
enum MD {
    enum Block: Equatable {
        case heading(Int, String)
        case item(indent: Int, marker: String, text: String)   // marker: "•", "3.", "☐", "☑"
        case quote(String)
        case rule
        case table(header: [String], rows: [[String]], align: [TextAlignment])
        case para(String)
    }

    static func blocks(_ s: String) -> [Block] {
        var out: [Block] = []
        var para: [String] = []
        var quote: [String] = []
        func flushPara() { if !para.isEmpty { out.append(.para(para.joined(separator: "\n"))); para = [] } }
        func flushQuote() { if !quote.isEmpty { out.append(.quote(quote.joined(separator: "\n"))); quote = [] } }
        let lines = s.components(separatedBy: "\n")
        var i = 0
        while i < lines.count {
            let raw = lines[i]
            let line = raw.trimmingCharacters(in: .whitespaces)
            let indent = raw.prefix(while: { $0 == " " || $0 == "\t" }).reduce(0) { $0 + ($1 == "\t" ? 4 : 1) } / 2
            if line.isEmpty { flushPara(); flushQuote(); i += 1; continue }
            // table: a | row followed by a |---| rule
            if line.hasPrefix("|") || (line.contains("|") && i + 1 < lines.count && isTableRule(lines[i + 1])) {
                if i + 1 < lines.count, isTableRule(lines[i + 1]) {
                    flushPara(); flushQuote()
                    let header = cells(line)
                    let align = cells(lines[i + 1]).map { c -> TextAlignment in
                        let l = c.hasPrefix(":"), r = c.hasSuffix(":")
                        return l && r ? .center : r ? .trailing : .leading
                    }
                    var rows: [[String]] = []
                    i += 2
                    while i < lines.count, lines[i].contains("|"), !lines[i].trimmingCharacters(in: .whitespaces).isEmpty {
                        rows.append(cells(lines[i])); i += 1
                    }
                    out.append(.table(header: header, rows: rows, align: align))
                    continue
                }
            }
            if let h = heading(line) { flushPara(); flushQuote(); out.append(h); i += 1; continue }
            if isRule(line) { flushPara(); flushQuote(); out.append(.rule); i += 1; continue }
            if line.hasPrefix(">") {
                flushPara()
                quote.append(String(line.dropFirst()).trimmingCharacters(in: .whitespaces)); i += 1; continue
            }
            if let it = item(line, indent: indent) { flushPara(); flushQuote(); out.append(it); i += 1; continue }
            flushQuote()
            para.append(line)
            i += 1
        }
        flushPara(); flushQuote()
        return out
    }

    private static func heading(_ l: String) -> Block? {
        let hashes = l.prefix(while: { $0 == "#" }).count
        guard (1...6).contains(hashes), l.count > hashes, l[l.index(l.startIndex, offsetBy: hashes)] == " " else { return nil }
        return .heading(hashes, String(l.dropFirst(hashes)).trimmingCharacters(in: .whitespaces).trimmingCharacters(in: CharacterSet(charactersIn: "#")).trimmingCharacters(in: .whitespaces))
    }

    private static func isRule(_ l: String) -> Bool {
        let c = l.replacingOccurrences(of: " ", with: "")
        return c.count >= 3 && (Set(c) == ["-"] || Set(c) == ["*"] || Set(c) == ["_"])
    }

    private static func isTableRule(_ l: String) -> Bool {
        let t = l.trimmingCharacters(in: .whitespaces)
        guard t.contains("-"), t.contains("|") || t.hasPrefix(":") || t.hasPrefix("-") else { return false }
        return t.allSatisfy { "|-: ".contains($0) }
    }

    private static func cells(_ l: String) -> [String] {
        var t = l.trimmingCharacters(in: .whitespaces)
        if t.hasPrefix("|") { t.removeFirst() }
        if t.hasSuffix("|") { t.removeLast() }
        return t.components(separatedBy: "|").map { $0.trimmingCharacters(in: .whitespaces) }
    }

    private static func item(_ l: String, indent: Int) -> Block? {
        for b in ["- [ ] ", "* [ ] "] where l.hasPrefix(b) { return .item(indent: indent, marker: "☐", text: String(l.dropFirst(b.count))) }
        for b in ["- [x] ", "- [X] ", "* [x] ", "* [X] "] where l.hasPrefix(b) { return .item(indent: indent, marker: "☑", text: String(l.dropFirst(b.count))) }
        for b in ["- ", "* ", "+ ", "• "] where l.hasPrefix(b) { return .item(indent: indent, marker: "•", text: String(l.dropFirst(b.count))) }
        let digits = l.prefix(while: { $0.isNumber })
        if !digits.isEmpty, digits.count <= 3 {
            let rest = l.dropFirst(digits.count)
            if rest.hasPrefix(". ") || rest.hasPrefix(") ") { return .item(indent: indent, marker: "\(digits).", text: String(rest.dropFirst(2))) }
        }
        return nil
    }
}

/// renders the blocks of one prose segment
struct MarkdownBlocks: View {
    @Environment(\.pal) var p
    let text: String
    let inline: (String) -> AttributedString

    var body: some View {
        VStack(alignment: .leading, spacing: 8) {
            ForEach(Array(MD.blocks(text).enumerated()), id: \.offset) { _, b in block(b) }
        }
    }

    @ViewBuilder private func block(_ b: MD.Block) -> some View {
        switch b {
        case .heading(let level, let t):
            Text(styled(t, size: level == 1 ? 17 : level == 2 ? 15 : 14, weight: .bold))
                .padding(.top, level <= 2 ? 4 : 2)
                .textSelection(.enabled).fixedSize(horizontal: false, vertical: true)
        case .item(let indent, let marker, let t):
            HStack(alignment: .firstTextBaseline, spacing: 6) {
                Text(marker).font(Fonts.mono(13)).foregroundStyle(marker == "•" ? p.acc : p.fg2)
                    .frame(minWidth: marker.count > 1 ? 22 : 10, alignment: .trailing)
                Text(inline(t)).lineSpacing(4).textSelection(.enabled).fixedSize(horizontal: false, vertical: true)
            }
            .padding(.leading, CGFloat(indent) * 16)
        case .quote(let t):
            HStack(alignment: .top, spacing: 8) {
                Rectangle().fill(p.acc.opacity(0.6)).frame(width: 3)
                Text(slanted(t)).foregroundStyle(p.fg2).lineSpacing(4)
                    .textSelection(.enabled).fixedSize(horizontal: false, vertical: true)
            }
            .fixedSize(horizontal: false, vertical: true)
        case .rule:
            Rectangle().fill(p.fg2.opacity(0.3)).frame(height: 1).padding(.vertical, 4)
        case .table(let header, let rows, let align):
            MDTable(header: header, rows: rows, align: align, inline: inline)
        case .para(let t):
            Text(inline(t)).lineSpacing(5).textSelection(.enabled).fixedSize(horizontal: false, vertical: true)
        }
    }

    /// a block quote: every run slanted (Fonts.monoItalic), bold kept bold
    private func slanted(_ s: String) -> AttributedString {
        var a = inline(s)
        for r in a.runs {
            a[r.range].font = Fonts.monoItalic(13, r.inlinePresentationIntent?.contains(.stronglyEmphasized) == true ? .semibold : .regular)
        }
        return a
    }

    private func styled(_ s: String, size: CGFloat, weight: Font.Weight) -> AttributedString {
        var a = inline(s)
        a.font = Fonts.mono(size, weight)
        a.foregroundColor = p.fg
        return a
    }
}

struct MDTable: View {
    @Environment(\.pal) var p
    let header: [String]
    let rows: [[String]]
    let align: [TextAlignment]
    let inline: (String) -> AttributedString

    var body: some View {
        let n = max(header.count, rows.map(\.count).max() ?? 0)
        ScrollView(.horizontal, showsIndicators: false) {
            Grid(alignment: .leading, horizontalSpacing: 14, verticalSpacing: 6) {
                GridRow {
                    ForEach(0..<n, id: \.self) { c in
                        cell(c < header.count ? header[c] : "", c, bold: true)
                    }
                }
                Divider().gridCellUnsizedAxes(.horizontal)
                ForEach(Array(rows.enumerated()), id: \.offset) { _, r in
                    GridRow {
                        ForEach(0..<n, id: \.self) { c in cell(c < r.count ? r[c] : "", c, bold: false) }
                    }
                }
            }
            .padding(10)
            .background(RoundedRectangle(cornerRadius: 6).fill(p.fg2.opacity(0.06)))
            .overlay(RoundedRectangle(cornerRadius: 6).stroke(p.fg2.opacity(0.2), lineWidth: 1))
        }
    }

    @ViewBuilder private func cell(_ s: String, _ c: Int, bold: Bool) -> some View {
        let a = c < align.count ? align[c] : .leading
        var t = inline(s)
        if bold { let _ = { t.font = Fonts.mono(13, .semibold); t.foregroundColor = p.fg }() }
        Text(t).multilineTextAlignment(a).textSelection(.enabled)
            .gridColumnAlignment(a == .trailing ? .trailing : a == .center ? .center : .leading)
    }
}
