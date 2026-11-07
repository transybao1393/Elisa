// The brand kit, read at launch: the renderer scripts and the colour / layout data, v2 overlay over v1 (v2 wins).
// Nothing is copied or downloaded: the page is built from the kit folder as it is on disk.
import AppKit
import Foundation

enum KitError: Error, CustomStringConvertible {
    case missing(String)
    var description: String { switch self { case .missing(let s): return s } }
}

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
        // .../app/ElisaCompanion/Sources/ElisaCompanion/Kit.swift -> repo root
        let src = URL(fileURLWithPath: #filePath)
        c.append(src.deletingLastPathComponent().deletingLastPathComponent().deletingLastPathComponent()
            .deletingLastPathComponent().deletingLastPathComponent().appendingPathComponent("elisa/design"))
        // the binary at app/ElisaCompanion/.build/<config>/ElisaCompanion -> repo root
        let exe = URL(fileURLWithPath: CommandLine.arguments[0]).resolvingSymlinksInPath()
        c.append(exe.deletingLastPathComponent().deletingLastPathComponent().deletingLastPathComponent()
            .deletingLastPathComponent().deletingLastPathComponent().appendingPathComponent("elisa/design"))
        let fm = FileManager.default
        return c.first { fm.fileExists(atPath: $0.appendingPathComponent(v1).path) }.map { Kit(design: $0) }
    }

    /// a kit file: the v2 overlay's copy if it has one, else v1's
    func file(_ rel: String) throws -> URL {
        for k in [Kit.v2, Kit.v1] {
            let u = design.appendingPathComponent(k).appendingPathComponent(rel)
            if FileManager.default.fileExists(atPath: u.path) { return u }
        }
        throw KitError.missing("brand kit file not found: \(rel) (in \(design.path))")
    }

    func text(_ rel: String) throws -> String { try String(contentsOf: file(rel), encoding: .utf8) }

    /// the page: companion.html with the kit's renderer (elisa-character.js is v1 only; elisa-shade.js is v2) and
    /// colors.json + layout.json inlined, plus the dock size (layout.json companion.sizes)
    func page(template: String, size: Int) throws -> String {
        let character = try text("05-character/renderer/elisa-character.js")
        let shade = try text("05-character/renderer/elisa-shade.js")
        let colors = try text("02-color/colors.json")
        let layout = try text("04-layout/layout.json")
        let data = "{\"colors\": \(colors), \"layout\": \(layout), \"size\": \(size)}"
        let safe = { (s: String) in s.replacingOccurrences(of: "</script", with: "<\\/script") }
        return template
            .replacingOccurrences(of: "/*@KIT_CHARACTER@*/", with: safe(character))
            .replacingOccurrences(of: "/*@KIT_SHADE@*/", with: safe(shade))
            .replacingOccurrences(of: "/*@KIT_DATA@*/", with: safe(data))
    }

    /// layout.json companion.sizes (Small 96, Medium 128, Large 168)
    func dockSizes() -> [String: Int] {
        guard let d = try? Data(contentsOf: file("04-layout/layout.json")),
              let j = try? JSONSerialization.jsonObject(with: d) as? [String: Any],
              let c = j["companion"] as? [String: Any], let s = c["sizes"] as? [String: Int] else {
            return ["Small": 96, "Medium": 128, "Large": 168]
        }
        return s
    }

    /// which kit fonts are installed (the kit loads them from Google Fonts; this app never downloads anything)
    static func fontReport() -> String {
        let fams = Set(NSFontManager.shared.availableFontFamilies)
        return ["Silkscreen", "Chivo Mono"].map { f in
            fams.contains(f) ? "\(f): installed" : "\(f): not installed, using the system monospace"
        }.joined(separator: "; ")
    }
}
