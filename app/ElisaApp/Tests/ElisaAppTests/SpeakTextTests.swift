// Speak Replies (Speak.swift): what of a reply is spoken, and in which voice language.
import XCTest
@testable import ElisaApp

final class SpeakTextTests: XCTestCase {
    func testLanguage() {
        XCTAssertEqual(SpeakText.language("Thủ đô của Pháp là Paris."), "vi-VN")
        XCTAssertEqual(SpeakText.language("The capital of France is Paris."), "en-US")
        XCTAssertEqual(SpeakText.language(""), "en-US")
    }

    func testPlainStripsMarkdownCodeCardAndEmoji() {
        let md = "## Result\n**Paris** is the *capital*. See [the map](https://x.y) 😄\n\n```rust\nfn main() {}\n```\n- one\n- `two`\n<state>\ngoal: x\n</state>"
        let t = SpeakText.plain(md)
        XCTAssertEqual(t, "Result\nParis is the capital. See the map\none\ntwo")
    }

    func testFinalProseIsAfterTheLastTool() {
        let parts = [Part(kind: .prose("Let me look.")), Part(kind: .tool(ToolRow(name: "read", verb: "Read", target: "a.txt"))),
                     Part(kind: .prose("It says hi.")), Part(kind: .footer("in 1K")), Part(kind: .info("[note]", .plain))]
        XCTAssertEqual(SpeakText.finalProse(parts), "It says hi.")
    }
}
