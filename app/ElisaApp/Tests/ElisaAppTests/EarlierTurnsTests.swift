// A resumed conversation's earlier turns (`history` event, Model.swift EarlierTurns): shown as normal blocks.
import XCTest
@testable import ElisaApp

final class EarlierTurnsTests: XCTestCase {
    func testHistoryBecomesUserAndReplyBlocksBetweenDividers() {
        let e: [String: Any] = ["type": "history", "items": [
            ["role": "user", "text": "what day is today?"],
            ["role": "assistant", "text": "Today is **Monday**."],
            ["role": "tool", "text": "ignored"],
            ["role": "user", "text": "  "],
        ]]
        let b = EarlierTurns.blocks(e)
        XCTAssertEqual(b.count, 4)
        guard case .info(let first, _) = b[0].kind, case .user(let u, let cmd) = b[1].kind, case .reply = b[2].kind, case .info(let last, _) = b[3].kind else {
            return XCTFail("kinds: \(b.map { $0.kind })")
        }
        XCTAssertEqual(first, EarlierTurns.before)
        XCTAssertEqual(last, EarlierTurns.after)
        XCTAssertEqual(u, "what day is today?")
        XCTAssertFalse(cmd)
        guard case .prose(let p) = b[2].parts.first?.kind else { return XCTFail("no prose") }
        XCTAssertEqual(p, "Today is **Monday**.")
    }

    func testNoItemsNoBlocks() {
        XCTAssertTrue(EarlierTurns.blocks(["type": "history"]).isEmpty)
    }
}
