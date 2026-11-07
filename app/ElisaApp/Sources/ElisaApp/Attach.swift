// Pictures for a model with vision: the 📎 button, drag and drop, and paste put them on the next message. Each is
// scaled to at most 1024 px a side (what vision models read anyway, and it keeps the request and the session small),
// then sent as base64: PNG when it had transparency (flattened onto white, sharp edges kept), else JPEG.
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
    /// why pictures can't be attached right now (nil: they can)
    var attachBlocked: String? {
        if phase == .exited { return "Elisa is not running" }
        if demo { return "the demo gateway has no model, so it can't see images" }
        if !vision { return "\(model.isEmpty ? "this model" : model) can't see images. Choose a model with vision in Model Connections (⌘,)" }
        return nil
    }

    func attach(_ images: [(NSImage, String)]) {
        if let why = attachBlocked { note(why); return }
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

    func attach(urls: [URL]) {
        var got: [(NSImage, String)] = []
        var bad: [String] = []
        for u in urls {
            if let img = NSImage(contentsOf: u) { got.append((img, u.lastPathComponent)) } else { bad.append(u.lastPathComponent) }
        }
        if !bad.isEmpty { note("not an image: " + bad.joined(separator: ", ")) }
        if !got.isEmpty { attach(got) }
    }

    func pickImages() {
        if let why = attachBlocked { note(why); return }
        let panel = NSOpenPanel()
        panel.allowsMultipleSelection = true
        panel.canChooseDirectories = false
        panel.allowedContentTypes = [.png, .jpeg, .gif, .webP, .heic, .tiff, .bmp]
        panel.message = "Images for \(model) (at most \(ImageEncoder.maxCount))"
        if panel.runModal() == .OK { attach(urls: panel.urls) }
    }

    /// ⌘V with a picture (or image files) on the clipboard; false: nothing to attach, so the text field pastes
    func pasteImages() -> Bool {
        let pb = NSPasteboard.general
        if let urls = pb.readObjects(forClasses: [NSURL.self], options: [.urlReadingFileURLsOnly: true, .urlReadingContentsConformToTypes: [UTType.image.identifier]]) as? [URL], !urls.isEmpty {
            attach(urls: urls); return true
        }
        // a copied picture, but not when text is there too (copying from a web page puts both)
        guard pb.string(forType: .string) == nil, let imgs = pb.readObjects(forClasses: [NSImage.self]) as? [NSImage], !imgs.isEmpty else { return false }
        attach(imgs.map { ($0, "pasted image") })
        return true
    }

    func removeAttachment(_ id: UUID) { attachments.removeAll { $0.id == id } }

    private func note(_ s: String) { blocks.append(Block(kind: .info("[image: \(s)]", .warn))) }
}

/// the pictures waiting to be sent, above the text: a thumbnail each, × removes it
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
                Text("\(m.attachments.count) image\(m.attachments.count == 1 ? "" : "s") · sends with your message")
                    .font(Fonts.mono(11)).foregroundStyle(p.fg2)
            }
            .padding(.top, 6).padding(.trailing, 6)
        }
    }
}

/// the 📎 button: opens a file picker; greyed out, with the reason on hover, when the model can't see
struct AttachButton: View {
    @EnvironmentObject var m: AppModel
    @Environment(\.pal) var p
    @State private var hover = false
    var body: some View {
        let why = m.attachBlocked
        Button { m.pickImages() } label: {
            Image(systemName: "paperclip").font(.system(size: 13, weight: .medium))
                .foregroundStyle(why == nil ? (hover ? p.acc : p.fg) : p.fg2.opacity(0.45))
                .frame(width: 22, height: 22)
        }
        .buttonStyle(.plain)
        .disabled(why != nil)
        .onHover { hover = $0 }
        .help(why.map { "Images are off: \($0)" } ?? "Attach images (or drop / paste them here) · \(m.model) can see them")
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
