// Attachments: the 📎 button, drag and drop, and paste put pictures and documents on the next message.
// Pictures (a model with vision only) are scaled to at most 1024 px a side (what vision models read anyway, and it
// keeps the request and the session small), then sent as base64: PNG when it had transparency (flattened onto white,
// sharp edges kept), else JPEG.
// Documents (any model: PDF, Word .docx, spreadsheets, text and code files) are sent as their path only
// (`send.documents`, protocol v2); Elisa copies each into the project's upload area and reads it there
// (elisa/src/attach.rs): a small one goes into the message whole, a large one as an overview or the pages a question
// needs. Each is checked in the background as it is attached (`prefetch` → `doc_info`): the chip shows the result,
// a click opens the details and the readable parts.
import AppKit
import SwiftUI
import UniformTypeIdentifiers

struct ImageError: Error { let why: String }

struct Attachment: Identifiable {
    let id = UUID()
    let name: String
    let thumb: NSImage
    let mediaType: String
    let data: String
}

/// a document waiting to go with the next message (or sent with one): its path, name and size
struct DocAttachment: Identifiable {
    let id = UUID()
    let url: URL
    let name: String
    let size: Int64

    /// the engine's limits (elisa/src/attach.rs)
    static let maxCount = 8
    static let maxBytes: Int64 = 50 * 1024 * 1024
    static let sheetExts: Set<String> = ["xlsx", "xlsm", "xlsb", "xls", "ods", "csv", "tsv", "numbers"]
    static let textExts: Set<String> = ["txt", "md", "markdown", "rst", "log", "rtf", "tex", "json", "jsonl", "yaml", "yml", "toml", "xml",
                                        "html", "htm", "css", "scss", "ini", "cfg", "conf", "properties", "srt", "vtt"]
    static let codeExts: Set<String> = ["swift", "rs", "py", "js", "mjs", "cjs", "ts", "tsx", "jsx", "java", "kt", "kts", "go", "rb", "php", "c", "h",
                                        "cc", "cpp", "hpp", "m", "mm", "cs", "sh", "bash", "zsh", "fish", "sql", "r", "lua", "dart", "scala",
                                        "pl", "ex", "exs", "erl", "hs", "clj", "vue", "svelte", "gradle", "cmake", "dockerfile", "makefile"]
    /// what Elisa can read (numbers is listed for a clear "save it as xlsx" message from the engine, not offered)
    static func readable(_ u: URL) -> Bool {
        let e = u.pathExtension.lowercased()
        let bare = u.lastPathComponent.lowercased()
        if e == "numbers" { return false }
        return e == "pdf" || e == "docx" || sheetExts.contains(e) || textExts.contains(e) || codeExts.contains(e)
            || ["makefile", "dockerfile", "readme", "license"].contains(bare)
    }
    var icon: String {
        let e = url.pathExtension.lowercased()
        if e == "pdf" { return "doc.richtext" }
        if Self.sheetExts.contains(e) { return "tablecells" }
        if Self.codeExts.contains(e) { return "chevron.left.forwardslash.chevron.right" }
        return "doc.text"
    }
    var sizeText: String { ByteCountFormatter.string(fromByteCount: size, countStyle: .file) }
}

enum ImageEncoder {
    static let maxSide: CGFloat = 1024
    /// the engine's limits (elisa/src/types.rs)
    static let maxCount = 4
    static let maxB64 = 5_000_000

    /// the picture scaled and encoded, or why it can't be
    static func encode(_ img: NSImage, name: String) -> Result<Attachment, ImageError> {
        guard let src = img.cgImage(forProposedRect: nil, context: nil, hints: nil) else { return .failure(ImageError(why: "\(name): not an image this app can read")) }
        let (w, h) = (CGFloat(src.width), CGFloat(src.height))
        guard w > 0, h > 0 else { return .failure(ImageError(why: "\(name): empty image")) }
        let k = min(1, maxSide / max(w, h))
        let (tw, th) = (Int((w * k).rounded()), Int((h * k).rounded()))
        guard let rep = NSBitmapImageRep(bitmapDataPlanes: nil, pixelsWide: tw, pixelsHigh: th, bitsPerSample: 8, samplesPerPixel: 4,
                                         hasAlpha: true, isPlanar: false, colorSpaceName: .deviceRGB, bytesPerRow: 0, bitsPerPixel: 0),
              let ctx = NSGraphicsContext(bitmapImageRep: rep) else { return .failure(ImageError(why: "\(name): cannot scale it")) }
        ctx.imageInterpolation = .high
        let alpha = src.alphaInfo != .none && src.alphaInfo != .noneSkipFirst && src.alphaInfo != .noneSkipLast
        // vision models drop transparency, and transparent pixels are stored as black: a dark subject would vanish
        // into a black background. On white it looks as it does on a page, and nothing is lost.
        if alpha { ctx.cgContext.setFillColor(NSColor.white.cgColor); ctx.cgContext.fill(CGRect(x: 0, y: 0, width: tw, height: th)) }
        ctx.cgContext.draw(src, in: CGRect(x: 0, y: 0, width: tw, height: th))
        let (type, out): (String, Data?) = alpha
            ? ("image/png", rep.representation(using: .png, properties: [:]))
            : ("image/jpeg", rep.representation(using: .jpeg, properties: [.compressionFactor: 0.85]))
        guard let d = out else { return .failure(ImageError(why: "\(name): cannot encode it")) }
        let b64 = d.base64EncodedString()
        guard b64.count <= maxB64 else { return .failure(ImageError(why: "\(name): still too large after scaling")) }
        let thumb = NSImage(size: NSSize(width: tw, height: th))
        thumb.addRepresentation(rep)
        return .success(Attachment(name: name, thumb: thumb, mediaType: type, data: b64))
    }
}

extension AppModel {
    /// why nothing can be attached right now (nil: documents can, pictures when `imagesBlocked` is nil too)
    var attachBlocked: String? {
        if phase == .exited { return "Elisa is not running" }
        return nil
    }

    /// why pictures can't be attached right now (nil: they can); documents need no vision
    var imagesBlocked: String? {
        if let why = attachBlocked { return why }
        if !vision { return "\(model.isEmpty ? "this model" : model) can't see images. Choose a model with vision in Model Connections (⌘,)" }
        return nil
    }

    /// why documents can't be attached right now (nil: they can)
    var docsBlocked: String? {
        if let why = attachBlocked { return why }
        if protocolVersion < 2 { return "this Elisa engine is too old to read documents; update Elisa" }
        return nil
    }

    func attach(_ images: [(NSImage, String)]) {
        if let why = imagesBlocked {
            // no vision: the core is told with the next message (`send.images_refused`)
            if attachBlocked == nil { refusedImages += images.count }
            note(why); return
        }
        var errors: [String] = []
        for (img, name) in images {
            if attachments.count >= ImageEncoder.maxCount { errors.append("at most \(ImageEncoder.maxCount) images per message"); break }
            switch ImageEncoder.encode(img, name: name) {
            case .success(let a): attachments.append(a)
            case .failure(let e): errors.append(e.why)
            }
        }
        if !errors.isEmpty { note(errors.joined(separator: " · ")) }
    }

    /// files from the picker, a drop or the clipboard: pictures to `attach(_:)`, documents to `attachDocs`
    func attach(urls: [URL]) {
        var got: [(NSImage, String)] = []
        var docs: [URL] = []
        var bad: [String] = []
        for u in urls {
            let isImage = (UTType(filenameExtension: u.pathExtension)?.conforms(to: .image) ?? false) && u.pathExtension.lowercased() != "pdf"
            if isImage, let img = NSImage(contentsOf: u) { got.append((img, u.lastPathComponent)) }
            else if DocAttachment.readable(u) { docs.append(u) }
            else { bad.append(u.lastPathComponent) }
        }
        if !bad.isEmpty { note("can't read " + bad.joined(separator: ", ") + ": Elisa reads PDF, Word (.docx), spreadsheets (xlsx, xls, ods, csv), text and code files, and images") }
        if !docs.isEmpty { attachDocs(docs) }
        if !got.isEmpty { attach(got) }
    }

    func attachDocs(_ urls: [URL]) {
        if let why = docsBlocked { note(why); return }
        var errors: [String] = []
        for u in urls {
            if documents.count >= DocAttachment.maxCount { errors.append("at most \(DocAttachment.maxCount) documents per message"); break }
            if documents.contains(where: { $0.url.standardizedFileURL == u.standardizedFileURL }) { continue }
            let vals = try? u.resourceValues(forKeys: [.fileSizeKey, .isDirectoryKey])
            if vals?.isDirectory == true { errors.append("\(u.lastPathComponent) is a folder"); continue }
            let size = Int64(vals?.fileSize ?? 0)
            if size > DocAttachment.maxBytes {
                errors.append("\(u.lastPathComponent) is too large (\(ByteCountFormatter.string(fromByteCount: size, countStyle: .file)); the limit is 50 MB per file)")
                continue
            }
            documents.append(DocAttachment(url: u, name: u.lastPathComponent, size: size))
            // read it in the background now, so the text is cached before ⏎
            prefetchDoc(u.path)
        }
        if !errors.isEmpty { note(errors.joined(separator: " · ")) }
    }

    /// the 📎 picker: documents always, pictures too when the model can see
    func pickFiles() {
        if let why = attachBlocked { note(why); return }
        let panel = NSOpenPanel()
        panel.allowsMultipleSelection = true
        panel.canChooseDirectories = false
        var types: [UTType] = [.pdf, .plainText, .commaSeparatedText, .tabSeparatedText, .json, .xml, .html, .sourceCode]
        let exts = ["docx", "xlsx", "xlsm", "xlsb", "xls", "ods", "md", "markdown", "yaml", "yml", "toml", "log", "rtf"]
            + Array(DocAttachment.codeExts)
        types += exts.compactMap { UTType(filenameExtension: $0) }
        if imagesBlocked == nil { types += [.png, .jpeg, .gif, .webP, .heic, .tiff, .bmp] }
        panel.allowedContentTypes = types
        panel.message = imagesBlocked == nil
            ? "Documents or images for \(model)"
            : "Documents for \(model) (PDF, Word, spreadsheets, text, code)"
        if panel.runModal() == .OK { attach(urls: panel.urls) }
    }

    func pickImages() { pickFiles() }

    /// ⌘V with a picture (or image files) on the clipboard; false: nothing to attach, so the text field pastes
    func pasteImages() -> Bool {
        let pb = NSPasteboard.general
        if let urls = pb.readObjects(forClasses: [NSURL.self], options: [.urlReadingFileURLsOnly: true]) as? [URL],
           !urls.isEmpty, urls.allSatisfy({ DocAttachment.readable($0) || (UTType(filenameExtension: $0.pathExtension)?.conforms(to: .image) ?? false) }) {
            attach(urls: urls); return true
        }
        // a copied picture, but not when text is there too (copying from a web page puts both)
        guard pb.string(forType: .string) == nil, let imgs = pb.readObjects(forClasses: [NSImage.self]) as? [NSImage], !imgs.isEmpty else { return false }
        attach(imgs.map { ($0, "pasted image") })
        return true
    }

    func removeAttachment(_ id: UUID) { attachments.removeAll { $0.id == id }; documents.removeAll { $0.id == id } }

    private func note(_ s: String) { blocks.append(Block(kind: .info("[attach: \(s)]", .warn))) }
}

/// an attached file's intake (`doc_info` from elisa/src/attach.rs): what it is, whether it fits, its readable parts
struct DocInfo {
    struct Span: Identifiable { let id: Int; let title: String; let from: Int; let to: Int }
    let path: String, name: String, kind: String, line: String, category: String, unit: String, lang: String, plan: String
    let pages: Int?, words: Int, tokens: Int, budget: Int, tables: Int
    let textLayer: Bool, outline: Bool, refused: Bool
    let sheets: [String]
    let sections: [Span], parts: [Span]
    let warnings: [String]

    init(_ e: [String: Any]) {
        func str(_ k: String) -> String { e[k] as? String ?? "" }
        func int(_ v: Any?) -> Int { (v as? NSNumber)?.intValue ?? 0 }
        func spans(_ k: String) -> [Span] {
            ((e[k] as? [[String: Any]]) ?? []).enumerated().map { i, s in
                let pg = s["pages"] as? [Any] ?? []
                return Span(id: i, title: s["title"] as? String ?? "", from: int(pg.first), to: int(pg.last))
            }
        }
        let fit = e["fit"] as? [String: Any] ?? [:]
        path = str("path"); name = str("name"); kind = str("kind"); line = str("line"); unit = str("unit"); lang = str("lang")
        category = str("category_word").isEmpty ? str("category") : str("category_word")
        plan = fit["plan"] as? String ?? ""
        pages = (e["pages"] as? NSNumber)?.intValue
        words = int(e["words"]); tokens = int(fit["doc_tokens"] ?? e["tokens"]); budget = int(fit["budget"]); tables = int(e["tables"])
        textLayer = e["text_layer"] as? Bool ?? true; outline = e["outline"] as? Bool ?? false; refused = e["refused"] as? Bool ?? false
        sheets = ((e["sheets"] as? [[String: Any]]) ?? []).map { "\($0["name"] as? String ?? "sheet") · \(int($0["rows"])) rows" }
        sections = spans("sections"); parts = spans("parts")
        warnings = e["warnings"] as? [String] ?? []
    }

    /// "pages 9–14" / "page 3" (a PDF), "lines …" is not tracked: other files say "part"
    func range(_ s: Span) -> String {
        let u = unit == "page" ? "page" : "part"
        return s.from == s.to ? "\(u) \(s.from)" : "\(u)s \(s.from)–\(s.to)"
    }
    /// what a click on part `i` (1-based) sends
    func ask(_ i: Int, _ s: Span) -> String { "Summarise part \(i) (\(range(s))) of \(name)" }
}

/// a document's chip: its icon, name and the intake's one-line result (its size until then); a click opens the panel
struct DocChip: View {
    @EnvironmentObject var m: AppModel
    @Environment(\.pal) var p
    let doc: DocAttachment
    @State private var open = false
    var body: some View {
        let info = m.docInfo[doc.url.path]
        let warn = info.map { $0.refused || !$0.warnings.isEmpty } ?? false
        Button { if info != nil { open.toggle() } } label: {
            HStack(spacing: 6) {
                Image(systemName: doc.icon).font(.system(size: 15)).foregroundStyle(warn ? p.warn : p.acc)
                VStack(alignment: .leading, spacing: 1) {
                    Text(doc.name).font(Fonts.mono(11.5, .medium)).foregroundStyle(p.fg).lineLimit(1).truncationMode(.middle)
                    Text(info.map { $0.line.isEmpty ? doc.sizeText : $0.line } ?? "\(doc.sizeText) · checking…")
                        .font(Fonts.mono(10)).foregroundStyle(warn ? p.warn : p.fg2).lineLimit(1).truncationMode(.tail)
                }
            }
            .padding(.horizontal, 8).frame(maxWidth: 300, minHeight: 40, alignment: .leading)
            .background(RoundedRectangle(cornerRadius: 6).fill(p.bg3.opacity(0.5)))
            .overlay(RoundedRectangle(cornerRadius: 6).strokeBorder(warn ? p.warn.opacity(0.6) : p.gut, lineWidth: 1))
            .contentShape(Rectangle())
        }
        .buttonStyle(.plain)
        .help(info == nil ? doc.url.path : "\(doc.url.path)\nClick for details and parts")
        .popover(isPresented: $open, arrowEdge: .top) {
            if let i = info { DocPanel(info: i, size: doc.sizeText) { open = false }.environmentObject(m).environment(\.pal, p) }
        }
    }
}

/// the chip's panel: what the file is, how it fits the window, its warnings, and its readable parts (a click asks
/// for a summary of that part)
struct DocPanel: View {
    @EnvironmentObject var m: AppModel
    @Environment(\.pal) var p
    let info: DocInfo
    let size: String
    let close: () -> Void
    var body: some View {
        VStack(alignment: .leading, spacing: 8) {
            Text(info.name).font(Fonts.mono(13, .semibold)).foregroundStyle(p.fg).lineLimit(2)
            Text(info.line).font(Fonts.mono(11)).foregroundStyle(info.warnings.isEmpty && !info.refused ? p.fg2 : p.warn)
            if !info.refused {
                VStack(alignment: .leading, spacing: 3) {
                    row("type", [info.kind.uppercased(), size].filter { !$0.isEmpty }.joined(separator: " · "))
                    if let n = info.pages { row("pages", "\(n)") }
                    if !info.sheets.isEmpty { row("sheets", info.sheets.joined(separator: ", ")) }
                    row("kind", info.category)
                    row("length", "\(info.words) words · ~\(info.tokens) tokens")
                    if !info.lang.isEmpty { row("language", info.lang == "vi" ? "Vietnamese" : info.lang == "en" ? "English" : info.lang) }
                    if info.kind == "pdf" { row("text", info.textLayer ? "has a text layer" : "no text layer (scanned?)") }
                    if !info.sections.isEmpty { row("sections", "\(info.sections.count)\(info.outline ? " (from its outline)" : "")") }
                    if info.tables > 0 { row("tables", "\(info.tables)") }
                    row("fits", fitText)
                }
            }
            if !info.warnings.isEmpty {
                VStack(alignment: .leading, spacing: 2) {
                    ForEach(info.warnings, id: \.self) { Text("⚠ \($0)").font(Fonts.mono(10.5)).foregroundStyle(p.warn).fixedSize(horizontal: false, vertical: true) }
                }
            }
            if !info.parts.isEmpty {
                Divider()
                Text("Parts · click one for a summary").font(Fonts.mono(10.5, .medium)).foregroundStyle(p.fg2)
                ScrollView {
                    VStack(alignment: .leading, spacing: 2) {
                        ForEach(info.parts) { s in
                            Button { close(); m.send(info.ask(s.id + 1, s)) } label: {
                                HStack(alignment: .firstTextBaseline, spacing: 6) {
                                    Text("\(s.id + 1)").font(Fonts.mono(10.5, .semibold)).foregroundStyle(p.acc).frame(width: 18, alignment: .trailing)
                                    Text(s.title.isEmpty ? "Part \(s.id + 1)" : s.title).font(Fonts.mono(11)).foregroundStyle(p.fg).lineLimit(2)
                                    Spacer(minLength: 8)
                                    Text(info.range(s)).font(Fonts.mono(10)).foregroundStyle(p.fg2)
                                }
                                .padding(.vertical, 3).padding(.horizontal, 4).contentShape(Rectangle())
                            }
                            .buttonStyle(.plain)
                            .help(info.ask(s.id + 1, s))
                        }
                    }
                }
                .frame(maxHeight: 260)
            }
        }
        .padding(12).frame(width: 380, alignment: .leading)
        .background(p.bg)
    }

    private var fitText: String {
        switch info.plan {
        case "whole": return "whole (~\(info.tokens) of \(info.budget) tokens for documents)"
        case "overview": return "as an overview; ask about any part for detail (budget \(info.budget) tokens)"
        case "selection": return "the parts each question needs (budget \(info.budget) tokens)"
        default: return info.plan
        }
    }

    private func row(_ k: String, _ v: String) -> some View {
        HStack(alignment: .firstTextBaseline, spacing: 8) {
            Text(k).font(Fonts.mono(10.5)).foregroundStyle(p.fg2).frame(width: 64, alignment: .leading)
            Text(v).font(Fonts.mono(10.5)).foregroundStyle(p.fg).fixedSize(horizontal: false, vertical: true)
        }
    }
}

/// the pictures and documents waiting to be sent, above the text: a thumbnail or chip each, × removes it
struct AttachmentStrip: View {
    @EnvironmentObject var m: AppModel
    @Environment(\.pal) var p
    var body: some View {
        ScrollView(.horizontal, showsIndicators: false) {
            HStack(spacing: 8) {
                ForEach(m.attachments) { a in
                    ZStack(alignment: .topTrailing) {
                        Image(nsImage: a.thumb).resizable().aspectRatio(contentMode: .fill)
                            .frame(width: 56, height: 56).clipShape(RoundedRectangle(cornerRadius: 6))
                            .overlay(RoundedRectangle(cornerRadius: 6).strokeBorder(p.gut, lineWidth: 1))
                            .help(a.name)
                        Button { m.removeAttachment(a.id) } label: {
                            Text("×").font(Fonts.mono(12, .semibold)).foregroundStyle(p.fg)
                                .frame(width: 18, height: 18).background(Circle().fill(p.bg3))
                        }
                        .buttonStyle(.plain).offset(x: 6, y: -6).help("Remove \(a.name)")
                    }
                }
                ForEach(m.documents) { d in
                    ZStack(alignment: .topTrailing) {
                        DocChip(doc: d)
                        Button { m.removeAttachment(d.id) } label: {
                            Text("×").font(Fonts.mono(12, .semibold)).foregroundStyle(p.fg)
                                .frame(width: 18, height: 18).background(Circle().fill(p.bg3))
                        }
                        .buttonStyle(.plain).offset(x: 6, y: -6).help("Remove \(d.name)")
                    }
                }
                Text(caption).font(Fonts.mono(11)).foregroundStyle(p.fg2)
            }
            .padding(.top, 6).padding(.trailing, 6)
        }
    }

    private var caption: String {
        var parts: [String] = []
        if !m.attachments.isEmpty { parts.append("\(m.attachments.count) image\(m.attachments.count == 1 ? "" : "s")") }
        if !m.documents.isEmpty { parts.append("\(m.documents.count) document\(m.documents.count == 1 ? "" : "s")") }
        return parts.joined(separator: " · ") + " · sends with your message"
    }
}

/// the 📎 button: opens a file picker (documents, and pictures when the model can see); greyed out, with the reason on
/// hover, only when nothing can be attached
struct AttachButton: View {
    @EnvironmentObject var m: AppModel
    @Environment(\.pal) var p
    @State private var hover = false
    var body: some View {
        let why = m.attachBlocked
        Button { m.pickFiles() } label: {
            Image(systemName: "paperclip").font(.system(size: 13, weight: .medium))
                .foregroundStyle(why == nil ? (hover ? p.acc : p.fg) : p.fg2.opacity(0.45))
                .frame(width: 22, height: 22)
        }
        .buttonStyle(.plain)
        .disabled(why != nil)
        .onHover { hover = $0 }
        .help(why.map { "Attachments are off: \($0)" }
              ?? (m.imagesBlocked == nil ? "Attach documents or images (or drop / paste them here)"
                                         : "Attach documents: PDF, Word, spreadsheets, text, code (or drop them here) · \(m.model) can't see images"))
    }
}

/// thumbnails of the pictures a sent message carried
struct SentImages: View {
    @Environment(\.pal) var p
    let images: [NSImage]
    var body: some View {
        HStack(spacing: 8) {
            ForEach(images.indices, id: \.self) { i in
                Image(nsImage: images[i]).resizable().aspectRatio(contentMode: .fit)
                    .frame(maxWidth: 160, maxHeight: 120).clipShape(RoundedRectangle(cornerRadius: 6))
                    .overlay(RoundedRectangle(cornerRadius: 6).strokeBorder(p.gut, lineWidth: 1))
            }
        }
        .padding(.top, 4)
    }
}

/// chips of the documents a sent message carried
struct SentDocs: View {
    let docs: [DocAttachment]
    var body: some View {
        HStack(spacing: 8) { ForEach(docs) { DocChip(doc: $0) } }.padding(.top, 4)
    }
}
