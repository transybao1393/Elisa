import XCTest
@testable import ElisaApp

final class MarkdownTests: XCTestCase {
    func testBlocks() {
        let s = """
        # Title
        Some **bold** text.

        - one
          - nested
        1. first
        - [x] done
        > quoted
        ---
        | a | b |
        |---|--:|
        | 1 | 2 |
        """
        let b = MD.blocks(s)
        XCTAssertEqual(b[0], .heading(1, "Title"))
        XCTAssertEqual(b[1], .para("Some **bold** text."))
        XCTAssertEqual(b[2], .item(indent: 0, marker: "•", text: "one"))
        XCTAssertEqual(b[3], .item(indent: 1, marker: "•", text: "nested"))
        XCTAssertEqual(b[4], .item(indent: 0, marker: "1.", text: "first"))
        XCTAssertEqual(b[5], .item(indent: 0, marker: "☑", text: "done"))
        XCTAssertEqual(b[6], .quote("quoted"))
        XCTAssertEqual(b[7], .rule)
        XCTAssertEqual(b[8], .table(header: ["a", "b"], rows: [["1", "2"]], align: [.leading, .trailing]))
    }

    func testStreamingHalfTableIsJustText() {
        // the rule row has not arrived yet: no table, no crash
        XCTAssertEqual(MD.blocks("| a | b |"), [.para("| a | b |")])
    }

    func testPlainProseUnchanged() {
        XCTAssertEqual(MD.blocks("Hello! How can I help?"), [.para("Hello! How can I help?")])
    }

    /// /help: command lines split into command + description; other lines kept whole; prose is not a command list
    func testCommandRows() {
        let rows = InfoLine.commandRows("/help     list the commands\n/clear    save a summary\n/copy     copy the last reply\nA line ending in \\ continues.")
        XCTAssertEqual(rows?.count, 4)
        XCTAssertEqual(rows?[0].cmd, "/help")
        XCTAssertEqual(rows?[0].text, "list the commands")
        XCTAssertNil(rows?[3].cmd)
        XCTAssertNil(InfoLine.commandRows("just a note\n/one thing"))
    }
}
