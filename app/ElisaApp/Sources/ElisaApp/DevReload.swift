import Foundation

/// Development mode (`ELISA_DEV=1`, started by `dev.sh`): when the core binary is rebuilt, restart the core on the
/// same session (`--continue`) so the open window picks up core changes without losing the conversation.
/// It never interrupts testing: a reload waits until the binary has been stable for 30 s (agents save in bursts) and
/// the person has been idle for 90 s (no turn running, nothing typed, no new transcript lines).
final class DevReload {
    static let shared = DevReload()
    private var timer: Timer?
    private var stamp: Date?
    private var pending = false
    private var lastSeen: (blocks: Int, input: String) = (0, "")
    private var lastActivity = Date()

    static let stableFor: TimeInterval = 30
    static let idleFor: TimeInterval = 90

    /// off: the person decides when to restart on a new build (never reload on their behalf)
    static var enabled: Bool { ProcessInfo.processInfo.environment["ELISA_DEV_AUTORELOAD"] == "1" }

    /// the binary the app starts: $ELISA_BIN (dev.sh sets it)
    private var binary: String? { ProcessInfo.processInfo.environment["ELISA_BIN"] }

    private func mtime() -> Date? {
        guard let b = binary else { return nil }
        return (try? FileManager.default.attributesOfItem(atPath: b))?[.modificationDate] as? Date
    }

    func watch(_ m: AppModel) {
        guard Self.enabled, timer == nil else { return }
        stamp = mtime()
        // the timer fires on the main run loop: the closure runs on the main actor
        timer = Timer.scheduledTimer(withTimeInterval: 1.0, repeats: true) { [weak self, weak m] _ in MainActor.assumeIsolated {
            guard let self, let m else { return }
            // any sign of use resets the idle clock
            let seen = (m.blocks.count, m.input)
            if seen != self.lastSeen || m.busy { self.lastSeen = seen; self.lastActivity = Date() }
            let now = self.mtime()
            if let now, now != self.stamp { self.stamp = now; self.pending = true }
            guard self.pending, !m.busy, m.input.isEmpty, let t = now,
                  Date().timeIntervalSince(t) > Self.stableFor,
                  Date().timeIntervalSince(self.lastActivity) > Self.idleFor else { return }
            self.pending = false
            m.devReload()
        } }
    }
}

extension AppModel {
    /// dev mode: the core binary changed; restart it on this session and say so in the transcript
    func devReload() {
        guard !welcome else { return }
        reconnect()
        blocks.append(Block(kind: .info("[dev: Elisa updated while you were idle · same conversation]", .plain)))
    }
}
