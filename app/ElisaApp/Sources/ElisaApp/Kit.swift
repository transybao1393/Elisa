// The brand kit, read at launch from elisa/design (v2 overlay over v1, v2 wins): colours, layout numbers, UI icons and
// the character renderer. Nothing from the kit is copied into the app or downloaded; the two OFL fonts it names are
// bundled as SwiftPM resources (Resources/Fonts).
// `Kit.locate` / `file` / `text` / `fontReport` are adapted from app/ElisaCompanion/Sources/ElisaCompanion/Kit.swift.
import AppKit
import CoreText
import SwiftUI

struct Kit {
    /// elisa/design (holds "Elisa Brand Kit" and "Elisa Brand Kit v2 update")
    let design: URL

    static let v1 = "Elisa Brand Kit"
    static let v2 = "Elisa Brand Kit v2 update"

    /// ELISA_KIT_DIR, else $STREAMDENSE_ROOT/elisa/design, else the repo this source file lives in
    static func locate() -> Kit? {
        let env = ProcessInfo.processInfo.environment
        var c: [URL] = []
        if let d = env["ELISA_KIT_DIR"], !d.isEmpty { c.append(URL(fileURLWithPath: d)) }
        if let r = env["STREAMDENSE_ROOT"], !r.isEmpty { c.append(URL(fileURLWithPath: r).appendingPathComponent("elisa/design")) }
        c.append(Kit.repoRoot.appendingPathComponent("elisa/design"))
        let fm = FileManager.default
        return c.first { fm.fileExists(atPath: $0.appendingPathComponent(v1).path) }.map { Kit(design: $0) }
    }

    /// .../app/ElisaApp/Sources/ElisaApp/Kit.swift -> repo root
    static var repoRoot: URL {
        if let r = ProcessInfo.processInfo.environment["STREAMDENSE_ROOT"], !r.isEmpty { return URL(fileURLWithPath: r) }
        var u = URL(fileURLWithPath: #filePath)
        for _ in 0..<5 { u = u.deletingLastPathComponent() }
        return u
    }

    /// a kit file: the v2 overlay's copy if it has one, else v1's
    func file(_ rel: String) -> URL? {
        for k in [Kit.v2, Kit.v1] {
            let u = design.appendingPathComponent(k).appendingPathComponent(rel)
            if FileManager.default.fileExists(atPath: u.path) { return u }
        }
        return nil
    }

    func text(_ rel: String) -> String? { file(rel).flatMap { try? String(contentsOf: $0, encoding: .utf8) } }

    func json(_ rel: String) -> [String: Any] {
        guard let u = file(rel), let d = try? Data(contentsOf: u), let j = try? JSONSerialization.jsonObject(with: d) as? [String: Any] else { return [:] }
        return j
    }

    /// which kit fonts are in use (bundled with the app as OFL files; this app never downloads anything)
    static func fontReport() -> String {
        let r = ["Silkscreen", "Chivo Mono"].map { f in
            Fonts.installed(f) ? "\(f): available" : "\(f): not found, using the system monospace"
        }.joined(separator: "; ")
        return r + (Fonts.registered.isEmpty ? " (no bundled OFL font files found)" : " (registered from the \(Fonts.source): \(Fonts.registered.joined(separator: ", ")))")
    }

    /// a UI icon (06-icons/ui/elisa-icon-<name>-<16|20>.svg) as a template image (tinted by foregroundStyle)
    func icon(_ name: String, _ size: Int) -> NSImage? {
        guard var s = text("06-icons/ui/elisa-icon-\(name)-\(size).svg") else { return nil }
        s = s.replacingOccurrences(of: "currentColor", with: "#000000")
        guard let img = NSImage(data: Data(s.utf8)) else { return nil }
        img.isTemplate = true
        return img
    }
}

// MARK: - fonts: Chivo Mono + Silkscreen (bundled OFL files), else the system monospace

enum Fonts {
    /// a family is usable when the system has it or this process registered it (NSFontManager's family list does
    /// not see process-scope registrations at once, so the registered descriptors' family names count too)
    static func installed(_ f: String) -> Bool { families.contains(f) || NSFontManager.shared.availableFontFamilies.contains(f) }
    nonisolated(unsafe) static var families: Set<String> = []

    /// font files registered at launch (process scope, nothing installed system-wide)
    nonisolated(unsafe) static var registered: [String] = []
    /// where they came from: "app bundle", "kit folder", or "" (none: the system monospace fallback is used)
    nonisolated(unsafe) static var source = ""

    /// Registers Silkscreen + Chivo Mono with CTFontManager (process scope). First the copies bundled with the app
    /// (Resources/Fonts, SIL OFL 1.1, from github.com/google/fonts; see THIRD_PARTY_NOTICES.md), then any .ttf/.otf
    /// under the kit's 03-type. Only from a folder that also holds the OFL text. A family already installed on the
    /// system is left alone. Nothing found -> `mono` / `pixel` fall back to the system monospace.
    @MainActor static func registerFonts(_ kit: Kit?) {
        var dirs: [(URL, String)] = []
        if let b = resourceBundle() { dirs.append((b.appendingPathComponent("Fonts"), "app bundle")) }
        if let kit { for k in [Kit.v2, Kit.v1] { dirs.append((kit.design.appendingPathComponent(k).appendingPathComponent("03-type"), "kit folder")) } }
        let fm = FileManager.default
        for (root, src) in dirs {
            guard let it = fm.enumerator(at: root, includingPropertiesForKeys: nil) else { continue }
            for case let u as URL in it where ["ttf", "otf"].contains(u.pathExtension.lowercased()) {
                guard oflLicensed(u.deletingLastPathComponent()), !registered.contains(u.lastPathComponent) else { continue }
                let fam = u.lastPathComponent.hasPrefix("Silkscreen") ? "Silkscreen" : u.lastPathComponent.hasPrefix("ChivoMono") ? "Chivo Mono" : ""
                if !fam.isEmpty && installed(fam) && !registered.contains(where: { $0.hasPrefix(fam.replacingOccurrences(of: " ", with: "")) }) { continue }
                var err: Unmanaged<CFError>?
                if CTFontManagerRegisterFontsForURL(u as CFURL, .process, &err) {
                    registered.append(u.lastPathComponent)
                    for d in (CTFontManagerCreateFontDescriptorsFromURL(u as CFURL) as? [CTFontDescriptor]) ?? [] {
                        if let n = CTFontDescriptorCopyAttribute(d, kCTFontFamilyNameAttribute) as? String { families.insert(n) }
                    }
                    if source.isEmpty { source = src }
                }
            }
        }
    }

    /// SwiftPM's resource bundle (ElisaApp_ElisaApp.bundle), looked up by hand: `Bundle.module` traps when it is
    /// missing, and a missing bundle must only mean the system-monospace fallback
    static func resourceBundle() -> URL? {
        let name = "ElisaApp_ElisaApp.bundle"
        var c: [URL] = []
        if let r = Bundle.main.resourceURL { c.append(r.appendingPathComponent(name)) }
        if let e = Bundle.main.executableURL?.resolvingSymlinksInPath() { c.append(e.deletingLastPathComponent().appendingPathComponent(name)) }
        c.append(Bundle.main.bundleURL.appendingPathComponent(name))
        for u in c {
            let fonts = u.appendingPathComponent("Fonts")
            if FileManager.default.fileExists(atPath: fonts.path) { return u }
            // a .bundle may keep its files under Contents/Resources
            let alt = u.appendingPathComponent("Contents/Resources")
            if FileManager.default.fileExists(atPath: alt.appendingPathComponent("Fonts").path) { return alt }
        }
        return nil
    }

    private static func oflLicensed(_ dir: URL) -> Bool {
        let names = (try? FileManager.default.contentsOfDirectory(atPath: dir.path)) ?? []
        return names.contains { n in
            let l = n.lowercased()
            guard l.hasPrefix("ofl") || l.hasPrefix("license") || l.hasPrefix("licence") else { return false }
            let t = (try? String(contentsOf: dir.appendingPathComponent(n), encoding: .utf8)) ?? ""
            return t.contains("Open Font License") || t.contains("OFL")
        }
    }
    static let chivo = installed("Chivo Mono")
    static let silk = installed("Silkscreen")
    /// Chivo Mono (prose, code, user turns, status, input)
    static func mono(_ size: CGFloat, _ w: Font.Weight = .regular) -> Font {
        chivo ? noLigatures("Chivo Mono", size, w) : .system(size: size, weight: w, design: .monospaced)
    }
    /// Chivo Mono slanted, for *emphasis* and block quotes. Only the upright face is bundled, and SwiftUI's `.italic()`
    /// finds no italic in it (the text stayed upright), so the upright glyphs are sheared by a font matrix instead
    static func monoItalic(_ size: CGFloat, _ w: Font.Weight = .regular) -> Font {
        chivo ? noLigatures("Chivo Mono", size, w, slant: true) : .system(size: size, weight: w, design: .monospaced).italic()
    }
    /// the shear of `monoItalic`: tan(12°), about a usual italic angle
    static let slant: CGFloat = 0.2126

    /// Chivo Mono ships fi / fl / ff / ffi / ffl ligatures (GSUB liga, on by default): in a monospace face they squeeze
    /// two letters into one cell, so "file" looked broken. Every app font is made here with ligatures (common + rare)
    /// and contextual alternates off. Cached per family/size/weight (fonts are made in view bodies).
    private static var cache: [String: Font] = [:]
    private static func noLigatures(_ family: String, _ size: CGFloat, _ w: Font.Weight, slant: Bool = false) -> Font {
        let key = "\(family)|\(size)|\(w)|\(slant)"
        if let f = cache[key] { return f }
        let weights: [Font.Weight: NSFont.Weight] = [.ultraLight: .ultraLight, .thin: .thin, .light: .light, .regular: .regular,
            .medium: .medium, .semibold: .semibold, .bold: .bold, .heavy: .heavy, .black: .black]
        let off: [[NSFontDescriptor.FeatureKey: Int]] = [
            [.typeIdentifier: kLigaturesType, .selectorIdentifier: kCommonLigaturesOffSelector],
            [.typeIdentifier: kLigaturesType, .selectorIdentifier: kRareLigaturesOffSelector],
            [.typeIdentifier: kContextualAlternatesType, .selectorIdentifier: kContextualAlternatesOffSelector],
        ]
        let d = NSFontDescriptor(fontAttributes: [.family: family, .traits: [NSFontDescriptor.TraitKey.weight: (weights[w] ?? .regular).rawValue]])
            .addingAttributes([.featureSettings: off])
        let f: Font
        if slant {
            var m = CGAffineTransform(a: 1, b: 0, c: Self.slant, d: 1, tx: 0, ty: 0)
            f = Font(CTFontCreateWithFontDescriptor(d as CTFontDescriptor, size, &m))
        } else {
            f = NSFont(descriptor: d, size: size).map { Font($0) } ?? .custom(family, size: size).weight(w)
        }
        cache[key] = f
        return f
    }
    /// the drawn width of one line of `mono(size)` text (sizes horizontally scrolled code / diff blocks to their widest line)
    static func monoWidth(_ s: String, _ size: CGFloat) -> CGFloat {
        let f = (chivo ? NSFont(descriptor: NSFontDescriptor(fontAttributes: [.family: "Chivo Mono"]), size: size) : nil)
            ?? NSFont.monospacedSystemFont(ofSize: size, weight: .regular)
        return ceil((s as NSString).size(withAttributes: [.font: f, .ligature: 0]).width)
    }
    /// Silkscreen pixel caps: display/brand labels only (app name, speaker + panel labels, badges, bubble verbs);
    /// never body text. Fallback: small monospace caps
    static func pixel(_ size: CGFloat) -> Font {
        silk ? noLigatures("Silkscreen", size, .regular) : .system(size: size * 0.92, weight: .semibold, design: .monospaced)
    }
}

/// a Silkscreen-style label: uppercase, +.06em
struct Pixel: View {
    let text: String
    var size: CGFloat = 10
    var color: Color
    init(_ text: String, size: CGFloat = 10, color: Color) { self.text = text; self.size = size; self.color = color }
    var body: some View {
        Text(text.uppercased()).font(Fonts.pixel(size)).tracking(size * 0.06).foregroundStyle(color)
    }
}

// MARK: - brand (designs/: app icon and wordmark, bundled under Resources/Brand)

@MainActor enum Brand {
    private static var cache: [String: NSImage] = [:]
    private static func image(_ file: String) -> NSImage? {
        if let i = cache[file] { return i }
        guard let u = Fonts.resourceBundle()?.appendingPathComponent("Brand/\(file)"), let i = NSImage(contentsOf: u) else { return nil }
        cache[file] = i
        return i
    }
    /// the app icon (Dock, ⌘-Tab, About panel); nil leaves the default
    static var appIcon: NSImage? { image("Elisa.icns") }
    /// the vector wordmark in the theme's text colour
    static func wordmark(dark: Bool) -> NSImage? { image(dark ? "elisa-wordmark-on-dark.pdf" : "elisa-wordmark-on-light.pdf") }
}

/// The Elisa wordmark at a given height; the pixel-font text when the bundled artwork is missing.
struct Wordmark: View {
    @Environment(\.pal) var p
    var height: CGFloat
    var body: some View {
        if let img = Brand.wordmark(dark: p.dark) {
            Image(nsImage: img).resizable().interpolation(.high).aspectRatio(contentMode: .fit).frame(height: height)
                .accessibilityLabel("Elisa")
        } else {
            Pixel("Elisa", size: height, color: p.fg)
        }
    }
}

// MARK: - palette (02-color/colors.json)

struct Palette {
    let dark: Bool
    let bg, bg2, bg3, fg, tx, fg2, acc, code, ok, err, warn, sep, gut, add, del, sel, inputBg: Color
    /// hex of the window background: the status-bar face's sad lid is painted in it (the kit's `bg` option)
    let bgHex: String
    let accHex, warnHex, errHex: String

    static func load(_ kit: Kit?, dark: Bool) -> Palette {
        let all = kit?.json("02-color/colors.json") ?? [:]
        let j = (all[dark ? "dark" : "light"] as? [String: String]) ?? [:]
        let d: [String: String] = dark
            ? ["bg": "#16131C", "bg2": "#201C29", "bg3": "#2A2536", "fg": "#FFFFFF", "text": "#ECE8F2", "textMuted": "#A9A2B6", "accent": "#FF8FB1",
               "code": "#9FE0F0", "ok": "#78C878", "error": "#FF6B7A", "warn": "#F2CC6B", "separator": "#3F3850", "gutter": "#6A6280",
               "diffAdd": "rgba(120,200,120,.13)", "diffDel": "rgba(255,107,122,.13)", "selection": "rgba(255,143,177,.15)"]
            : ["bg": "#F8F6FB", "bg2": "#EFEBF4", "bg3": "#E5E0EC", "fg": "#120E18", "text": "#221C2A", "textMuted": "#5E566C", "accent": "#C2386A",
               "code": "#1D6E84", "ok": "#2C7D35", "error": "#C42B3C", "warn": "#8A6400", "separator": "#DAD3E4", "gutter": "#8C8499",
               "diffAdd": "rgba(44,125,53,.10)", "diffDel": "rgba(196,43,60,.09)", "selection": "rgba(194,56,106,.12)", "inputBg": "#FFFFFF"]
        let v = { (k: String) in j[k] ?? d[k] ?? "#FF00FF" }
        let c = { (k: String) in Color(nsColor: parseColor(v(k))) }
        return Palette(dark: dark, bg: c("bg"), bg2: c("bg2"), bg3: c("bg3"), fg: c("fg"), tx: c("text"), fg2: c("textMuted"), acc: c("accent"),
                       code: c("code"), ok: c("ok"), err: c("error"), warn: c("warn"), sep: c("separator"), gut: c("gutter"),
                       add: c("diffAdd"), del: c("diffDel"), sel: c("selection"), inputBg: (j["inputBg"] ?? d["inputBg"]).map { Color(nsColor: parseColor($0)) } ?? c("bg2"),
                       bgHex: v("bg"), accHex: v("accent"), warnHex: v("warn"), errHex: v("error"))
    }
}

/// "#RRGGBB" or "rgba(r,g,b,a)"
func parseColor(_ s: String) -> NSColor {
    let t = s.trimmingCharacters(in: .whitespaces)
    if t.hasPrefix("#"), let n = UInt32(t.dropFirst(), radix: 16) {
        return NSColor(srgbRed: CGFloat(n >> 16 & 255) / 255, green: CGFloat(n >> 8 & 255) / 255, blue: CGFloat(n & 255) / 255, alpha: 1)
    }
    if t.hasPrefix("rgba(") {
        let p = t.dropFirst(5).dropLast().split(separator: ",").compactMap { Double($0.trimmingCharacters(in: .whitespaces)) }
        if p.count == 4 { return NSColor(srgbRed: p[0] / 255, green: p[1] / 255, blue: p[2] / 255, alpha: p[3]) }
    }
    return .magenta
}

private struct PaletteKey: EnvironmentKey { static let defaultValue = Palette.load(nil, dark: true) }
extension EnvironmentValues {
    var pal: Palette { get { self[PaletteKey.self] } set { self[PaletteKey.self] = newValue } }
}

// MARK: - layout numbers (04-layout/layout.json, v2)

struct Layout {
    var dockSizes: [String: Int] = ["Small": 96, "Medium": 128, "Large": 168]
    var narrowBreakpoint: CGFloat = 1100
    var narrowMax: CGFloat = 96
    var attentionScale: CGFloat = 1.22
    var trayButton: CGFloat = 30, trayRadius: CGFloat = 7, trayGap: CGFloat = 4, trayPadBottom: CGFloat = 10
    var bubblePad: (CGFloat, CGFloat) = (5, 10), bubbleRadius: CGFloat = 8, bubbleGap: CGFloat = 2, bubbleLift: CGFloat = -6
    var replyMarker: CGFloat = 24
    var statusFace: CGFloat = 22

    static func load(_ kit: Kit?) -> Layout {
        var l = Layout()
        guard let j = kit?.json("04-layout/layout.json"), let c = j["companion"] as? [String: Any] else { return l }
        if let s = c["sizes"] as? [String: Int] { l.dockSizes = s }
        if let n = c["narrowBreakpoint"] as? Double { l.narrowBreakpoint = n }
        if let n = c["narrowMax"] as? Double { l.narrowMax = n }
        if let n = c["attentionScale"] as? Double { l.attentionScale = n }
        if let t = c["careTray"] as? [String: Any], let b = t["button"] as? [String: Any] {
            l.trayButton = b["size"] as? Double ?? 30; l.trayRadius = b["radius"] as? Double ?? 7; l.trayGap = b["gap"] as? Double ?? 4
            l.trayPadBottom = t["paddingBottom"] as? Double ?? 10
        }
        if let b = c["bubble"] as? [String: Any] {
            if let p = b["padding"] as? [Double], p.count == 2 { l.bubblePad = (p[0], p[1]) }
            l.bubbleRadius = b["radius"] as? Double ?? 8; l.bubbleGap = b["gap"] as? Double ?? 2; l.bubbleLift = b["liftOnAttention"] as? Double ?? -6
        }
        if let ci = j["characterInline"] as? [String: Any] {
            l.replyMarker = ci["replyMarker"] as? Double ?? 24
            if let s = ci["statusBar"] as? [String: Any] { l.statusFace = s["size"] as? Double ?? 22 }
        }
        return l
    }
}
