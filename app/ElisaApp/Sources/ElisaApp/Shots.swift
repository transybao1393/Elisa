// --shots DIR (debug): plays the scripted demo through the real Elisa loop and writes PNGs of the window at chosen
// moments. The window is rendered in-process (NSView cacheDisplay at the window's backing scale), so no Screen
// Recording permission is needed. While a picture is taken, Elisa's events are held, so the scene does not move on.
import AppKit
import SwiftUI

@MainActor
final class Shots {
    static let shared = Shots()
    /// shots are always taken at 2x (as on a Retina screen), whatever screen the window landed on; the small Shades
    /// are drawn at this scale too, so the crops show their real 2x pixels
    static var scale: CGFloat?
    private var rects: [String: CGRect] = [:]
    private var dir = ""
    private weak var m: AppModel?
    private var stage = 0
    /// the fallback window when SwiftUI did not open the scene's
    private var ownWindow: NSWindow?

    /// frames of the small Shades in window coordinates (from the views' anchor preferences)
    func record(_ r: [String: CGRect]) { rects = r }

    func run(into d: String, model: AppModel) {
        dir = d
        Shots.scale = 2
        m = model
        model.persistPrefs = false
        model.shadeSize = model.shadeRange.lowerBound
        model.shadeHidden = false
        try? FileManager.default.createDirectory(atPath: d, withIntermediateDirectories: true)
        model.onEventHook = { [weak self] e in self?.on(e) }
        // SwiftUI restores the last window size: shots always start at the default 1280 x 820
        after(0.5) {
            // SwiftUI may not open the scene's window (e.g. launched from a background shell, or a restored closed
            // state): then the same MainWindow is hosted in a plain window, so the shots still render
            if self.window?.title != "Elisa" {
                let w = NSWindow(contentRect: NSRect(x: 80, y: 80, width: 1280, height: 820), styleMask: [.titled, .closable, .resizable, .fullSizeContentView],
                                 backing: .buffered, defer: false)
                w.title = "Elisa"
                w.titlebarAppearsTransparent = true
                w.titleVisibility = .hidden
                w.contentView = NSHostingView(rootView: MainWindow().environmentObject(model).frame(minWidth: 640, minHeight: 560))
                w.isReleasedWhenClosed = false
                w.orderFrontRegardless()
                self.ownWindow = w
                print("shots: no scene window; hosting MainWindow in its own window")
            }
            self.window?.setContentSize(NSSize(width: 1280, height: 820))
        }
        after(150) { print("shots: timed out at stage \(self.stage)"); NSApp.terminate(nil) }
    }

    private func after(_ s: Double, _ f: @escaping @MainActor () -> Void) {
        DispatchQueue.main.asyncAfter(deadline: .now() + s) { MainActor.assumeIsolated { f() } }
    }

    private func replyText(_ m: AppModel) -> Int {
        guard let b = m.blocks.last(where: { if case .reply = $0.kind { return $0.live }; return false }) else { return 0 }
        return b.parts.reduce(0) { n, p in if case .prose(let t) = p.kind { return n + t.count }; return n }
    }

    private func on(_ e: [String: Any]) {
        guard let m else { return }
        let type = e["type"] as? String ?? ""
        // the fix loop may ask again (another edit / command in a fix round): yes, so the demo runs to its end
        if stage == 5 && type == "approve" { after(0.3) { m.answer(0) }; return }
        switch stage {
        case 0 where type == "shade" && e["state"] as? String == "idle" && m.phase == .idle:
            stage = 1
            after(1.2) {
                self.shot("01-idle")
                m.send("fix the split test")
            }
        case 1 where type == "delta" && m.shade.state == "streaming" && replyText(m) > 120:
            stage = 2
            m.hold = true
            after(0.5) { self.shot("02-streaming"); m.release() }
        case 2 where type == "tool_detail" && e["name"] as? String == "edit":
            stage = 3
            m.hold = true
            after(0.8) { self.shot("03-tool-editing"); m.release() }
        case 3 where type == "approve" && e["tool"] as? String == "edit":
            stage = 4
            after(0.4) { m.answer(1) }
        case 4 where type == "approve" && e["tool"] as? String == "bash":
            stage = 5
            after(1.4) {
                if let rep = self.shot("04-permission") { self.crops(rep, "08-small-shade-2x") }
                m.answer(0)
            }
        // the turn ends celebrating, or (since check-before-done) after the fix loop's check rows
        case 5 where type == "shade" && e["state"] as? String == "celebrating" || type == "turn_done":
            stage = 6
            after(0.8) {
                self.shot("05-turn-end")
                m.toggleTheme()
                self.after(1.2) {
                    self.shot("06-light")
                    m.toggleTheme()
                    m.care("food")
                    self.after(1.6) {
                        if let rep = self.shot("07-care") { self.crops(rep, "09-small-shade-2x-eating") }
                        self.after(0.3) { self.showcase() }
                    }
                }
            }
        default:
            break
        }
    }

    /// a 1:1 crop (backing scale) of one recorded view frame (`rects[key]`, window coordinates) with a small margin
    private func cardCrop(_ rep: NSBitmapImageRep, _ key: String, _ name: String) {
        guard let r = rects[key], let v = view, let cg = rep.cgImage else { print("shots: no frame for \(key)"); return }
        let sc = CGFloat(rep.pixelsWide) / v.bounds.width
        let c = CGRect(x: (r.minX - 12) * sc, y: (r.minY - 12) * sc, width: (r.width + 24) * sc, height: (r.height + 24) * sc).integral
        guard let img = cg.cropping(to: c.intersection(CGRect(x: 0, y: 0, width: cg.width, height: cg.height))) else { return }
        let out = NSBitmapImageRep(cgImage: img)
        write(out, name)
    }

    // MARK: - showcase: synthetic event lines (the same path as Elisa's) for what the scripted demo never sends

    private func turn(_ user: String, _ evs: [[String: Any]]) {
        guard let m else { return }
        m.inject(["type": "user", "text": user])
        m.inject(["type": "waiting"])
        for e in evs { m.inject(e) }
        m.inject(["type": "reply_done", "tok_s": 21.4, "ttft_s": 0.6, "ctx_used": 3400, "max_ctx": 8192, "reused": 2900, "output_tokens": 212])
        m.inject(["type": "turn_done"])
        m.inject(["type": "ready"])
    }

    static let codeA = """
    Same boundary walk in three languages:

    ```rust
    /// split on word boundaries, never inside a char
    pub fn split_words(input: &str) -> Vec<&str> {
        let mut out = Vec::with_capacity(8);
        for (i, c) in input.char_indices() {
            if is_boundary(c) { out.push(&input[i..i + c.len_utf8()]); }
        }
        out // no 'static needed
    }
    ```

    ```python
    def split_words(text: str) -> list[str]:
        \"\"\"Split on spaces; keeps é intact.\"\"\"
        return [w for w in text.split(" ") if w]  # no bytes()
    print(split_words("ok é 42"), 0x2A, 3.14, None)
    ```

    ```swift
    func splitWords(_ s: String) -> [Substring] {
        // Character-aware: never cuts inside a grapheme
        let parts = s.split(separator: " ", maxSplits: 16)
        return parts.filter { !$0.isEmpty }
    }
    ```
    """

    static let codeB = """
    The other four:

    ```ts
    export async function fetchAll(urls: string[]): Promise<number> {
      const res = await Promise.all(urls.map((u) => fetch(u)));
      return res.filter((r) => r.ok).length; // 200s only
    }
    ```

    ```sql
    -- sessions by domain, newest first
    SELECT domain, COUNT(*) AS n FROM sessions
    WHERE started_at > '2026-11-01' AND tokens >= 1024
    GROUP BY domain ORDER BY n DESC LIMIT 10;
    ```

    ```bash
    set -euo pipefail
    for f in crates/*/Cargo.toml; do
      echo "checking ${f%/*}" && cargo clippy --manifest-path "$f" -q  # quiet
    done
    ```

    ```json
    { "model": "elisa-27b-q4", "max_ctx": 8192, "temperature": 0.2,
      "tools": ["read", "edit", "bash"], "yolo": false, "watch": null }
    ```
    """

    static let clippy = """
        Checking ferrous-core v0.3.1 (crates/core)
    error: this loop could be written as a `for` loop
      --> crates/core/src/split.rs:14:5
       |
    14 |     while let Some((i, c)) = it.next() {
       |     ^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^ help: try: `for (i, c) in it`
       |
    error: redundant clone
      --> crates/core/src/split.rs:22:20
       |
    22 |         out.push(word.clone());
       |                  ^^^^^^^^^^^^ help: remove this
    error: unneeded `return` statement
      --> crates/core/src/lib.rs:9:5
       |
    9  |     return words;
    error: could not compile `ferrous-core` (lib) due to 3 previous errors
    [exit 101]
    """

    private func showcase() {
        guard let m else { return }
        m.hold = true
        turn("show the boundary walk in a few languages", [["type": "delta", "text": Shots.codeA]])
        after(1.0) {
            self.shot("10-code-highlight-dark")
            self.turn("and in TS, SQL, bash and JSON", [["type": "delta", "text": Shots.codeB]])
            self.after(1.0) {
                self.shot("11-code-highlight-more")
                m.dark = false
                self.after(0.8) {
                    self.shot("12-code-highlight-light")
                    m.dark = true
                    self.after(0.4) { self.showcaseTools() }
                }
            }
        }
    }

    private func showcaseTools() {
        guard let m else { return }
        let ls = Shots.clippy.components(separatedBy: "\n").filter { !$0.isEmpty }
        let body = ls.dropLast()
        turn("run clippy and fix what it finds", [
            ["type": "delta", "text": "Running clippy on the workspace."],
            ["type": "tool_start", "name": "bash", "summary": "bash cargo clippy --all-targets -- -D warnings"],
            ["type": "tool_done", "is_error": false, "red": true, "note": "error: could not compile `ferrous-core` (lib) due to 3 previous errors · exit 101",
             "preview": Array(body.suffix(3)), "more": body.count - 3, "output": Shots.clippy],
            ["type": "check", "cmd": "cargo clippy", "status": "running", "round": 0, "rounds": 2, "summary": "", "line": "check: cargo clippy …"],
            ["type": "check", "cmd": "cargo clippy", "status": "fail", "round": 0, "rounds": 2, "summary": "3 errors", "line": "check: cargo clippy ✗ 3 errors → fixing (1/2)"],
            ["type": "a_future_event", "text": "unknown types are ignored"],
            ["type": "delta", "text": "Fixing the three lints: a `for` loop, no clone, no `return`."],
            ["type": "check", "cmd": "cargo clippy", "status": "running", "round": 1, "rounds": 2, "summary": "", "line": "check: cargo clippy …"],
            ["type": "check", "cmd": "cargo clippy", "status": "pass", "round": 1, "rounds": 2, "summary": "", "line": "check: cargo clippy ✓"],
            ["type": "check", "cmd": "cargo test", "status": "running", "round": 1, "rounds": 2, "summary": "", "line": "check: cargo test …"],
            ["type": "delta", "text": "Clippy is clean and all 51 tests pass."],
        ])
        after(1.0) {
            self.shot("13-tool-preview-collapsed")
            let tool = m.blocks.last(where: { if case .reply = $0.kind { return true }; return false })?
                .parts.first(where: { if case .tool = $0.kind { return true }; return false })
            if let t = tool { m.toggleExpand(t.id) }
            self.after(0.8) {
                self.shot("14-tool-preview-expanded")
                self.showcaseScroll()
            }
        }
    }

    private func showcaseScroll() {
        guard let m else { return }
        ScrollHold.scrollUp(360)
        after(0.5) {
            m.inject(["type": "waiting"])
            for i in 1...4 { m.inject(["type": "delta", "text": (i == 1 ? "" : " ") + "More streamed output, sentence \(i), arriving while the transcript is held."]) }
            self.after(0.8) {
                print("shots: scroll held=\(m.scrollHeld) lines=\(m.heldLines) new=\(m.heldNew)")
                self.shot("15-scroll-held-pill")
                // back to the bottom: the hold lifts and new output sticks again
                if let sv = ScrollHold.view, let doc = sv.documentView {
                    let y = doc.isFlipped ? max(0, doc.bounds.maxY - sv.contentView.bounds.height) : 0
                    sv.contentView.scroll(to: NSPoint(x: 0, y: y)); sv.reflectScrolledClipView(sv.contentView)
                }
                self.after(0.4) {
                    m.inject(["type": "delta", "text": " Back at the bottom: this line scrolls into view."])
                    m.inject(["type": "turn_done"]); m.inject(["type": "ready"])
                    self.after(0.6) {
                        print("shots: re-stuck held=\(m.scrollHeld)")
                        self.shot("16-scroll-restuck")
                        self.showcaseCard()
                    }
                }
            }
        }
    }

    /// `card` {text, n}: numbered as "state #N" in the card header and the status bar; an older Elisa's {text} card
    /// (no n) shows no number
    private func showcaseCard() {
        guard let m else { return }
        turn("carry the plan forward", [
            ["type": "delta", "text": "Older Elisa (no `n`):"],
            ["type": "card", "text": "goal: fix split()\nfiles: src/split.rs"],
            ["type": "delta", "text": "Current Elisa:"],
            ["type": "card", "text": "goal: fix split() on empty input\nfiles: src/split.rs, tests/split.rs\nnext: run cargo test\nopen: none", "n": 15],
        ])
        after(1.0) {
            print("shots: cardNo=\(m.cardNo.map(String.init) ?? "nil")")
            self.shot("20-state-card")
            self.showcaseLongEdit()
        }
    }

    /// 27 an edit whose lines are wider than the panel (no wrap, no cut), 27b the same panel scrolled sideways
    private func showcaseLongEdit() {
        guard m != nil else { return }
        let long = "        let msg = format!(\"split_words: boundary walk over {} chars found {} words, {} skipped (empty), first={:?} last={:?}\", input.len(), out.len(), skipped, out.first(), out.last());"
        let lines: [[String: Any]] = [
            ["k": "ctx", "o": 18, "n": 18, "t": "    pub fn split_words(input: &str) -> Vec<&str> {"],
            ["k": "ctx", "o": 19, "n": 19, "t": "        let mut out = Vec::with_capacity(8);"],
            ["k": "del", "o": 20, "t": "        log::debug!(\"split {} words\", out.len());"],
            ["k": "add", "n": 20, "t": long],
            ["k": "add", "n": 21, "t": "        log::debug!(\"{msg}\");"],
            ["k": "ctx", "o": 21, "n": 22, "t": "        out"],
            ["k": "ctx", "o": 22, "n": 23, "t": "    }"],
        ]
        turn("log the split stats", [
            ["type": "tool_start", "name": "edit", "summary": "edit crates/core/src/split.rs"],
            ["type": "tool_detail", "target": "crates/core/src/split.rs",
             "detail": ["kind": "edit", "path": "crates/core/src/split.rs", "add": 2, "del": 1, "lines": lines]],
            ["type": "tool_done", "is_error": false, "note": "applied"],
            ["type": "delta", "text": "Logged the split stats on one line."],
        ])
        after(1.0) {
            self.shot("27-edits-long-line")
            self.showcaseEditsWidth {
            // scroll the widest horizontally scrollable view (the diff) all the way right
            @MainActor func all(_ v: NSView) -> [NSScrollView] { ((v as? NSScrollView).map { [$0] } ?? []) + v.subviews.flatMap(all) }
            let root = self.window?.contentView
            let svs = root.map(all) ?? []
            let right = (root?.bounds.width ?? 0) / 2
            if let sv = svs.first(where: { ($0.documentView?.frame.width ?? 0) > $0.contentView.bounds.width + 20
                                          && $0.convert($0.bounds, to: root).minX > right }) {
                let x = (sv.documentView?.frame.width ?? 0) - sv.contentView.bounds.width
                sv.contentView.scroll(to: NSPoint(x: x, y: sv.contentView.bounds.minY)); sv.reflectScrolledClipView(sv.contentView)
                print("shots: diff scrolled x=\(Int(x))")
            } else { print("shots: no horizontally scrollable diff found") }
            self.after(0.6) {
                self.shot("27b-edits-long-line-scrolled")
                self.showcaseShade { self.showcaseNarrow() }
            }
            }
        }
    }

    /// the edit panel at its max and min width (dragged divider), the long-line diff showing
    private func showcaseEditsWidth(then: @escaping () -> Void) {
        guard let m else { return }
        m.setEditsWidth(AppModel.editsWidthRange.upperBound)
        after(0.8) {
            self.shot("28-edits-width-max")
            m.setEditsWidth(AppModel.editsWidthRange.lowerBound)
            self.after(0.8) {
                self.shot("28b-edits-width-min")
                m.setEditsWidth(AppModel.editsWidthRange.upperBound)
                self.after(0.6) { then() }
            }
        }
    }

    /// 40..: one dock crop per activity state (the `shade` lines elisa sends for them: shade/mod.rs `Companion`), and a
    /// full window for the first
    private func showcaseShade(then: @escaping () -> Void) {
        guard let m else { return then() }
        let rows: [(String, [String: Any])] = [
            ("reading", ["state": "reading", "verb": "READING", "target": "main.py"]),
            ("reading-long-path", ["state": "reading", "verb": "READING", "target": "…/core/src/split.rs"]),
            ("looking-for", ["state": "searching", "verb": "LOOKING FOR", "target": "create_item"]),
            ("writing-code", ["state": "streaming", "verb": "WRITING", "target": "main.py"]),
            ("editing", ["state": "editing", "verb": "EDITING", "target": "main.py › create_item"]),
            ("building", ["state": "building", "verb": "BUILDING", "target": ""]),
            ("testing", ["state": "testing", "verb": "TESTING", "target": "3 tests"]),
            ("testing-fidget", ["state": "testing", "verb": "TESTING", "target": "cargo test", "fidget": true]),
            ("happy", ["state": "success", "verb": "DONE", "target": "3 tests pass"]),
            ("worried", ["state": "error", "verb": "ERROR", "target": "see transcript", "tone": "error", "pop": true]),
            ("waiting-in-line", ["state": "idle", "verb": "WAITING", "target": "in line"]),
            ("reading-context", ["state": "reading", "verb": "READING", "target": "the context"]),
            ("thinking", ["state": "thinking", "verb": "THINKING", "target": ""]),
            ("writing-answer", ["state": "streaming", "verb": "WRITING", "target": "the answer"]),
            ("needs-you", ["state": "permission", "verb": "NEEDS YOU", "target": "pytest tests/test_items.py", "tone": "warn", "pop": true]),
            ("long-target", ["state": "editing", "verb": "EDITING", "target": "…/handlers/items/create.py › create_item_with_a_very_long_name"]),
        ]
        func step(_ i: Int) {
            guard i < rows.count else { m.inject(["type": "shade", "state": "idle", "verb": "", "target": "", "tone": "accent", "pop": false, "face": "idle"]); return then() }
            var e = rows[i].1
            e["type"] = "shade"
            if e["tone"] == nil { e["tone"] = "accent" }
            if e["pop"] == nil { e["pop"] = false }
            e["face"] = e["state"]
            m.inject(e)
            after(0.9) {
                let name = String(format: "%02d-shade-%@", 40 + i, rows[i].0)
                if let rep = (i == 0 ? self.shot(name + "-window") : self.offscreen()) { self.cardCrop(rep, "dock", name) }
                step(i + 1)
            }
        }
        step(0)
    }

    /// the window as a bitmap (as `shot`), not written
    private func offscreen() -> NSBitmapImageRep? {
        guard let v = view, let rep = NSBitmapImageRep(bitmapDataPlanes: nil, pixelsWide: Int(v.bounds.width * 2), pixelsHigh: Int(v.bounds.height * 2), bitsPerSample: 8, samplesPerPixel: 4, hasAlpha: true, isPlanar: false, colorSpaceName: .deviceRGB, bytesPerRow: 0, bitsPerPixel: 0) else { return nil }
        rep.size = v.bounds.size
        v.cacheDisplay(in: v.bounds, to: rep)
        return rep
    }

    private func showcaseNarrow() {
        guard let m, let w = window else { return }
        w.setContentSize(NSSize(width: 860, height: 760))
        after(1.0) {
            self.shot("17-narrow")
            m.toggleEdits()
            self.after(0.8) {
                self.shot("18-narrow-edits-overlay")
                self.menubarShot()
                m.toggleEdits()
                w.setContentSize(NSSize(width: 1280, height: 820))
                print("shots: done")
                NSApp.terminate(nil)
            }
        }
    }

    /// the menu-bar template icon drawn offscreen at 1x and 2x on a light and a dark menu bar (black / white, as
    /// macOS tints a template), 1:1 on top and nearest-neighbour blow-ups below (x8 for 1x, x4 for 2x: same size)
    private func menubarShot() {
        guard let icon = MenuBarItem.icon(m?.kit) else { print("shots: no menu-bar icon in the kit"); return }
        func strip(_ sc: CGFloat, _ bg: NSColor, _ fg: NSColor) -> CGImage? {
            let w = 30, h = 22
            guard let rep = NSBitmapImageRep(bitmapDataPlanes: nil, pixelsWide: Int(CGFloat(w) * sc), pixelsHigh: Int(CGFloat(h) * sc), bitsPerSample: 8,
                                             samplesPerPixel: 4, hasAlpha: true, isPlanar: false, colorSpaceName: .deviceRGB, bytesPerRow: 0, bitsPerPixel: 0),
                  let tint = NSBitmapImageRep(bitmapDataPlanes: nil, pixelsWide: Int(16 * sc), pixelsHigh: Int(16 * sc), bitsPerSample: 8,
                                              samplesPerPixel: 4, hasAlpha: true, isPlanar: false, colorSpaceName: .deviceRGB, bytesPerRow: 0, bitsPerPixel: 0)
            else { return nil }
            // the template tinted (what the menu bar does), vector-drawn at this scale
            NSGraphicsContext.saveGraphicsState()
            NSGraphicsContext.current = NSGraphicsContext(bitmapImageRep: tint)
            let r = NSRect(x: 0, y: 0, width: 16 * sc, height: 16 * sc)
            icon.draw(in: r, from: .zero, operation: .sourceOver, fraction: 1)
            fg.set(); r.fill(using: .sourceAtop)
            NSGraphicsContext.restoreGraphicsState()
            NSGraphicsContext.saveGraphicsState()
            NSGraphicsContext.current = NSGraphicsContext(bitmapImageRep: rep)
            bg.setFill(); NSRect(x: 0, y: 0, width: CGFloat(w) * sc, height: CGFloat(h) * sc).fill()
            if let t = tint.cgImage { NSGraphicsContext.current?.cgContext.draw(t, in: CGRect(x: 7 * sc, y: 3 * sc, width: 16 * sc, height: 16 * sc)) }
            NSGraphicsContext.restoreGraphicsState()
            return rep.cgImage
        }
        let light = NSColor(srgbRed: 0.93, green: 0.93, blue: 0.94, alpha: 1), dark = NSColor(srgbRed: 0.16, green: 0.16, blue: 0.18, alpha: 1)
        let items: [(String, CGImage?, Int)] = [
            ("light bar 1x", strip(1, light, .black), 8), ("light bar 2x", strip(2, light, .black), 4),
            ("dark bar 1x", strip(1, dark, .white), 8), ("dark bar 2x", strip(2, dark, .white), 4),
        ]
        let cell = 30 * 8, W = 40 + items.count * (cell + 30), H = 70 + 44 + 30 + 22 * 8 + 30
        guard let out = NSBitmapImageRep(bitmapDataPlanes: nil, pixelsWide: W, pixelsHigh: H, bitsPerSample: 8, samplesPerPixel: 4, hasAlpha: true,
                                         isPlanar: false, colorSpaceName: .deviceRGB, bytesPerRow: 0, bitsPerPixel: 0),
              let ctx = NSGraphicsContext(bitmapImageRep: out) else { return }
        NSGraphicsContext.saveGraphicsState()
        NSGraphicsContext.current = ctx
        NSColor(white: 0.5, alpha: 1).setFill(); NSRect(x: 0, y: 0, width: W, height: H).fill()
        let attrs: [NSAttributedString.Key: Any] = [.font: NSFont.monospacedSystemFont(ofSize: 13, weight: .regular), .foregroundColor: NSColor.white]
        let src = MenuBarItem.iconPaths.first { m?.kit?.file($0) != nil } ?? "?"
        ("16 pt template · \((src as NSString).lastPathComponent) · 1:1 top, blow-ups below" as NSString).draw(at: NSPoint(x: 20, y: H - 26), withAttributes: attrs)
        let cg = ctx.cgContext
        cg.interpolationQuality = .none
        for (i, it) in items.enumerated() {
            let x = 20 + i * (cell + 30)
            (it.0 as NSString).draw(at: NSPoint(x: x, y: H - 54), withAttributes: attrs)
            guard let img = it.1 else { continue }
            cg.draw(img, in: CGRect(x: x, y: H - 64 - img.height, width: img.width, height: img.height))
            cg.draw(img, in: CGRect(x: x, y: 20, width: img.width * it.2, height: img.height * it.2))
        }
        NSGraphicsContext.restoreGraphicsState()
        write(out, "19-menubar-icon")
    }

    private var window: NSWindow? { NSApp.windows.first { $0.isVisible && $0.contentView != nil && $0.title == "Elisa" } ?? NSApp.windows.first }

    private var view: NSView? { NSApp.windows.first { $0.isVisible && $0.contentView != nil && $0.title == "Elisa" }?.contentView ?? NSApp.windows.first?.contentView }

    /// the self-test's pictures: the window as it is now, into `d`/`name`.png
    func capture(_ name: String, into d: String) { dir = d; Shots.scale = 2; _ = shot(name) }

    @discardableResult
    func shot(_ name: String) -> NSBitmapImageRep? {
        guard let v = view, let rep = NSBitmapImageRep(bitmapDataPlanes: nil, pixelsWide: Int(v.bounds.width * 2), pixelsHigh: Int(v.bounds.height * 2), bitsPerSample: 8, samplesPerPixel: 4, hasAlpha: true, isPlanar: false, colorSpaceName: .deviceRGB, bytesPerRow: 0, bitsPerPixel: 0) else { print("shots: no window"); return nil }
        rep.size = v.bounds.size
        v.cacheDisplay(in: v.bounds, to: rep)
        write(rep, name)
        return rep
    }

    private func write(_ rep: NSBitmapImageRep, _ name: String) {
        let path = (dir as NSString).appendingPathComponent(name + ".png")
        if let d = rep.representation(using: .png, properties: [:]) { try? d.write(to: URL(fileURLWithPath: path)); print("shot: \(path) \(rep.pixelsWide)x\(rep.pixelsHigh)") }
    }

    /// 1:1 crops (window backing scale) of the status-bar face and the live 24 px mini Shade, beside the kit's own SVGs
    /// for the same states drawn at the same scale, plus x6 nearest-neighbour blow-ups of all four for inspection
    private func crops(_ rep: NSBitmapImageRep, _ name: String) {
        guard let m, let v = view, let kit = m.kit else { return }
        let sc = CGFloat(rep.pixelsWide) / v.bounds.width
        let bg = parseColor(m.pal.bgHex)
        func crop(_ key: String, _ pt: CGFloat) -> CGImage? {
            guard let r = rects[key], let cg = rep.cgImage else { return nil }
            let c = CGRect(x: (r.midX - pt / 2) * sc, y: (r.midY - pt / 2) * sc, width: pt * sc, height: pt * sc).integral
            return cg.cropping(to: c)
        }
        func kitImg(_ rel: String, _ size: CGFloat, _ pt: CGFloat) -> CGImage? {
            guard let s = kit.text(rel), let img = ShadeKit.raster(s, pt: size, scale: sc) else { return nil }
            let o = ((pt - size) / 2 * sc).rounded(), sp = (size * sc).rounded()
            let px = Int((pt * sc).rounded())
            guard let out = NSBitmapImageRep(bitmapDataPlanes: nil, pixelsWide: px, pixelsHigh: px, bitsPerSample: 8, samplesPerPixel: 4, hasAlpha: true, isPlanar: false, colorSpaceName: .deviceRGB, bytesPerRow: 0, bitsPerPixel: 0) else { return nil }
            NSGraphicsContext.saveGraphicsState()
            NSGraphicsContext.current = NSGraphicsContext(bitmapImageRep: out)
            bg.setFill(); NSRect(x: 0, y: 0, width: px, height: px).fill()
            img.draw(in: NSRect(x: o, y: o, width: sp, height: sp))
            NSGraphicsContext.restoreGraphicsState()
            return out.cgImage
        }
        let face = m.shade.face, st = m.shade.state
        let theme = m.dark ? "dark" : "light"
        let pad: CGFloat = 4
        let items: [(String, CGImage?)] = [
            ("app face 22pt (\(face))", crop("face", 22 + 2 * pad)),
            ("kit svg-statusbar \(face)-\(theme)-22", kitImg("05-character/shade/svg-statusbar/elisa-shade-face-\(face)-\(theme)-22.svg", 22, 22 + 2 * pad)),
            ("app mini 24pt (\(st), live)", crop("mini", 24 + 2 * pad)),
            ("kit svg \(st)-24 (key pose)", kitImg("05-character/shade/svg/elisa-shade-\(st)-24.svg", 24, 24 + 2 * pad)),
        ]
        let big = 6
        let cell = Int((24 + 2 * pad) * sc) * big
        let W = 40 + items.count * (cell + 40), H = 60 + Int((24 + 2 * pad) * sc) + 40 + cell + 30
        guard let out = NSBitmapImageRep(bitmapDataPlanes: nil, pixelsWide: W, pixelsHigh: H, bitsPerSample: 8, samplesPerPixel: 4, hasAlpha: true, isPlanar: false, colorSpaceName: .deviceRGB, bytesPerRow: 0, bitsPerPixel: 0),
              let ctx = NSGraphicsContext(bitmapImageRep: out) else { return }
        NSGraphicsContext.saveGraphicsState()
        NSGraphicsContext.current = ctx
        NSColor(white: 0.5, alpha: 1).setFill(); NSRect(x: 0, y: 0, width: W, height: H).fill()
        let attrs: [NSAttributedString.Key: Any] = [.font: NSFont.monospacedSystemFont(ofSize: 13, weight: .regular), .foregroundColor: NSColor.white]
        ("1:1 at \(Int(sc))x (top) · x\(big) nearest-neighbour (bottom) · window bg \(m.pal.bgHex)" as NSString).draw(at: NSPoint(x: 20, y: H - 26), withAttributes: attrs)
        let cg = ctx.cgContext
        for (i, it) in items.enumerated() {
            let x = 20 + i * (cell + 40)
            (it.0 as NSString).draw(at: NSPoint(x: x, y: H - 50), withAttributes: attrs)
            guard let img = it.1 else { continue }
            cg.interpolationQuality = .none
            cg.draw(img, in: CGRect(x: x, y: H - 60 - img.height, width: img.width, height: img.height))
            cg.draw(img, in: CGRect(x: x, y: 20, width: img.width * big, height: img.height * big))
        }
        NSGraphicsContext.restoreGraphicsState()
        write(out, name)
    }
}
