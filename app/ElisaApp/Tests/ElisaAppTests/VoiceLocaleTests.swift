// Voice input (Voice.swift): the language choice, whisper text cleanup, the WAV header, and (opt-in) a benchmark of the
// real recorder -> whisper.cpp path on `say` clips: ELISA_WHISPER_BENCH=1 ELISA_WHISPER_MODEL=/path/model.bin swift test
import AVFoundation
import XCTest
@testable import ElisaApp

final class VoiceLocaleTests: XCTestCase {
    func testVietnameseDetection() {
        XCTAssertTrue(VoiceLocale.hasVietnamese("Xin chào, bạn khỏe không?"))
        XCTAssertTrue(VoiceLocale.hasVietnamese("đi đâu"))              // đ alone
        XCTAssertTrue(VoiceLocale.hasVietnamese("không có"))            // only shared accents, two of them
        XCTAssertTrue(VoiceLocale.hasVietnamese("vie\u{0302}\u{0323}t")) // decomposed ệ
        XCTAssertFalse(VoiceLocale.hasVietnamese("fix the split test"))
        XCTAssertFalse(VoiceLocale.hasVietnamese("a café"))             // one shared accent is not enough
        XCTAssertFalse(VoiceLocale.hasVietnamese(""))
    }

    func testSystemLanguages() {
        XCTAssertEqual(VoiceLocale.fromSystem(["vi-VN", "en-US"]), "vi")
        XCTAssertEqual(VoiceLocale.fromSystem(["en-VN", "vi-VN"]), "en")
        XCTAssertEqual(VoiceLocale.fromSystem(["fr-FR", "vi"]), "vi")
        XCTAssertEqual(VoiceLocale.fromSystem(["fr-FR"]), "en")
        XCTAssertEqual(VoiceLocale.fromSystem([]), "en")
    }

    func testAuto() {
        // nothing said yet: whisper detects the language
        XCTAssertEqual(VoiceLocale.auto(recent: []), "auto")
        // a Vietnamese message among the last three
        XCTAssertEqual(VoiceLocale.auto(recent: ["hello", "cảm ơn nhé", "ok"]), "vi")
        // only the last three count
        XCTAssertEqual(VoiceLocale.auto(recent: ["cảm ơn", "a", "b", "c"]), "en")
    }

    func testResolve() {
        let vi = ["chào bạn"]
        XCTAssertEqual(VoiceLocale.resolve(.en, recent: vi, preferred: ["vi-VN"]), "en")
        XCTAssertEqual(VoiceLocale.resolve(.vi, recent: [], preferred: ["en-US"]), "vi")
        XCTAssertEqual(VoiceLocale.resolve(.system, recent: vi, preferred: ["en-US"]), "en")
        XCTAssertEqual(VoiceLocale.resolve(.auto, recent: vi, preferred: ["en-US"]), "vi")
        XCTAssertEqual(VoiceLocale.tag("auto"), "AUTO")
    }

    func testWhisperTextClean() {
        XCTAssertEqual(WhisperText.clean(" [BLANK_AUDIO]\n"), "")
        XCTAssertEqual(WhisperText.clean("\n Hello  there.\n How are you? [Music]\n"), "Hello there. How are you?")
    }

    func testWAVHeader() {
        let w = WAV.make(Data(count: 32_000))
        XCTAssertEqual(w.count, 44 + 32_000)
        XCTAssertEqual(String(decoding: w.prefix(4), as: UTF8.self), "RIFF")
        XCTAssertEqual(String(decoding: w[8..<16], as: UTF8.self), "WAVEfmt ")
        XCTAssertEqual(w[24..<28].withUnsafeBytes { $0.loadUnaligned(as: UInt32.self) }, 16_000)
    }

    // MARK: - benchmark (opt-in)

    private func sayClip(_ text: String, voice: String) throws -> URL {
        let u = FileManager.default.temporaryDirectory.appendingPathComponent("elisa-bench-\(voice).aiff")
        let p = Process()
        p.executableURL = URL(fileURLWithPath: "/usr/bin/say")
        p.arguments = ["-v", voice, "-o", u.path, text]
        try p.run(); p.waitUntilExit()
        return u
    }

    /// the clip through the app's own recorder (any format -> 16 kHz mono Int16) into a WAV
    private func wav(from u: URL) throws -> (Data, Double) {
        let f = try AVAudioFile(forReading: u)
        let rec = VoiceRecorder(input: f.processingFormat)
        while f.framePosition < f.length {
            let b = AVAudioPCMBuffer(pcmFormat: f.processingFormat, frameCapacity: 4096)!
            try f.read(into: b)
            if b.frameLength == 0 { break }
            rec.append(b)
        }
        return (rec.wav(), rec.seconds)
    }

    func testBenchmarkWhisper() async throws {
        let env = ProcessInfo.processInfo.environment
        guard env["ELISA_WHISPER_BENCH"] == "1", let model = env["ELISA_WHISPER_MODEL"] else { throw XCTSkip("set ELISA_WHISPER_BENCH=1 and ELISA_WHISPER_MODEL") }
        let clips = [
            ("en", "Samantha", "Please summarise the meeting notes and send me the three most important action items."),
            ("vi", "Linh", "Xin chào Elisa, hãy tóm tắt ghi chú cuộc họp và gửi cho tôi ba việc quan trọng nhất."),
        ]
        let t0 = Date()
        await Whisper.shared.warm(model: model)
        print("BENCH server cold start (model load): \(String(format: "%.2f", Date().timeIntervalSince(t0))) s")
        for (lang, voice, text) in clips {
            let (w, secs) = try wav(from: try sayClip(text, voice: voice))
            for run in 1...2 {
                let t = Date()
                let out = try await Whisper.shared.transcribe(w, language: lang, model: model)
                let path = await Whisper.shared.lastPath
                print("BENCH \(lang) \(String(format: "%.1f", secs)) s clip, run \(run) via \(path): \(String(format: "%.2f", Date().timeIntervalSince(t))) s -> \(out)")
                XCTAssertFalse(out.isEmpty)
            }
            let ta = Date()
            let auto = try await Whisper.shared.transcribe(w, language: "auto", model: model)
            print("BENCH \(lang) with language=auto: \(String(format: "%.2f", Date().timeIntervalSince(ta))) s -> \(auto)")
        }
        // the CLI fallback: the server stopped and marked broken is the same path the app takes when it fails
        await Whisper.shared.stopServer()
        await Whisper.shared.breakServerForTest()
        for (lang, voice, text) in clips {
            let (w, secs) = try wav(from: try sayClip(text, voice: voice))
            let t = Date()
            let out = try await Whisper.shared.transcribe(w, language: lang, model: model)
            let path = await Whisper.shared.lastPath
            print("BENCH \(lang) \(String(format: "%.1f", secs)) s clip via \(path): \(String(format: "%.2f", Date().timeIntervalSince(t))) s -> \(out)")
        }
    }
}
