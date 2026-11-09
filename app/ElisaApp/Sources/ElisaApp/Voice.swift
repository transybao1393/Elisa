// Voice input: the 🎤 button at the right of the message field. Click (or ⌘⇧D) to record, click again to stop; the clip
// is transcribed and the text goes into the input field, where it can be edited. ⏎ sends as usual (nothing is sent by
// itself).
// Engine: whisper.cpp (the installed Homebrew binaries; MIT) with the model ggml-large-v3-turbo-q5_0.bin. Fully local:
// AVAudioEngine records, the clip is converted to 16 kHz mono 16-bit PCM WAV, and a warm `whisper-server` (started on the
// first use, on 127.0.0.1 on a free port, the model loaded once; a child of this app, killed by its PID when the app
// quits or after 10 idle minutes) transcribes it. If the server fails, `whisper-cli` runs on the clip instead. The audio
// never leaves this Mac; a temporary WAV (the CLI path only) is deleted right after.
// Model: ~/Library/Application Support/Elisa/models/ggml-large-v3-turbo-q5_0.bin, or the file in the "whisperModelPath"
// default, or $ELISA_WHISPER_MODEL. When it is missing the first mic click asks: copy a copy found on this Mac, download it
// from Hugging Face (574 MB, size and SHA-256 checked), or choose a file. Nothing is downloaded without that click.
// Languages: English and Vietnamese. Right-click the mic (or click its tag) to choose: System (the macOS preferred
// languages, the default), Auto (Vietnamese when the last messages have Vietnamese diacritics, English when they don't,
// whisper's own detection before anything was said), English, Vietnamese. Remembered in UserDefaults ("voiceLang").
// Permission: the microphone (NSMicrophoneUsageDescription in app/ElisaApp/Info.plist, embedded in the unbundled binary
// by Package.swift). Recording stops when a message is sent, the input box goes away (an approval, the session picker),
// the window closes, or the session restarts.
import AppKit
import AVFoundation
import CryptoKit
import Darwin
import SwiftUI

/// the language choice (the "voiceLang" default)
enum VoiceLang: String, CaseIterable, Identifiable {
    case system, auto, en, vi
    var id: String { rawValue }
    var label: String {
        switch self {
        case .system: return "System Languages"
        case .auto: return "Auto (from your messages)"
        case .en: return "English"
        case .vi: return "Tiếng Việt (Vietnamese)"
        }
    }
}

/// which language whisper is told (pure, unit-tested): "en", "vi", or "auto" (whisper detects it)
enum VoiceLocale {
    static let en = "en"
    static let vi = "vi"
    static let detect = "auto"

    /// letters only Vietnamese uses among Latin scripts: đ ă ơ ư and the tone-marked vowels U+1EA0–U+1EF9 (ạ ả ấ ầ …)
    private static func vietnameseOnly(_ v: UInt32) -> Bool {
        switch v {
        case 0x0110, 0x0111, 0x0102, 0x0103, 0x01A0, 0x01A1, 0x01AF, 0x01B0, 0x1EA0...0x1EF9: return true
        default: return false
        }
    }
    /// accented vowels Vietnamese shares with other languages (à á â ã è é ê ì í ò ó ô õ ù ú ý)
    private static let shared: Set<UInt32> = Set("àáâãèéêìíòóôõùúýÀÁÂÃÈÉÊÌÍÒÓÔÕÙÚÝ".unicodeScalars.map(\.value))

    /// the text looks Vietnamese: a Vietnamese-only letter, or two or more of the shared accented vowels ("không có")
    static func hasVietnamese(_ s: String) -> Bool {
        var sharedCount = 0
        for u in s.precomposedStringWithCanonicalMapping.unicodeScalars {
            if vietnameseOnly(u.value) { return true }
            if shared.contains(u.value) { sharedCount += 1; if sharedCount >= 2 { return true } }
        }
        return false
    }

    /// the first English or Vietnamese entry of the macOS preferred languages; English when neither is there
    static func fromSystem(_ preferred: [String]) -> String {
        for l in preferred {
            let c = l.lowercased()
            if c.hasPrefix("vi") { return vi }
            if c.hasPrefix("en") { return en }
        }
        return en
    }

    /// Auto: Vietnamese when any of the last three messages looks Vietnamese, English when they don't, whisper's own
    /// detection when nothing has been said yet
    static func auto(recent: [String]) -> String {
        let r = recent.suffix(3)
        if r.isEmpty { return detect }
        return r.contains(where: hasVietnamese) ? vi : en
    }

    static func resolve(_ choice: VoiceLang, recent: [String], preferred: [String]) -> String {
        switch choice {
        case .system: return fromSystem(preferred)
        case .auto: return auto(recent: recent)
        case .en: return en
        case .vi: return vi
        }
    }

    static func name(_ id: String) -> String { id == vi ? "Vietnamese" : id == en ? "English" : "the detected language" }
    static func tag(_ id: String) -> String { id == vi ? "VI" : id == en ? "EN" : "AUTO" }
}

/// whisper's text, tidied (pure, unit-tested): no [BLANK_AUDIO] / [Music] markers, one line, single spaces
enum WhisperText {
    static func clean(_ s: String) -> String {
        let noMarks = s.replacingOccurrences(of: #"\[[^\]]*\]"#, with: " ", options: .regularExpression)
        return noMarks.split(whereSeparator: \.isWhitespace).joined(separator: " ")
    }
}

/// 16 kHz mono 16-bit PCM WAV (pure, unit-tested)
enum WAV {
    static let rate = 16_000
    static func make(_ pcm: Data) -> Data {
        var d = Data()
        func u32(_ v: UInt32) { withUnsafeBytes(of: v.littleEndian) { d.append(contentsOf: $0) } }
        func u16(_ v: UInt16) { withUnsafeBytes(of: v.littleEndian) { d.append(contentsOf: $0) } }
        d.append(contentsOf: Array("RIFF".utf8)); u32(UInt32(36 + pcm.count)); d.append(contentsOf: Array("WAVE".utf8))
        d.append(contentsOf: Array("fmt ".utf8)); u32(16); u16(1); u16(1); u32(UInt32(rate)); u32(UInt32(rate * 2)); u16(2); u16(16)
        d.append(contentsOf: Array("data".utf8)); u32(UInt32(pcm.count))
        d.append(pcm)
        return d
    }
}

/// the audio, converted as it arrives to 16 kHz mono Int16 (the tap runs on the audio thread: the buffer is locked)
final class VoiceRecorder: @unchecked Sendable {
    private let lock = NSLock()
    private var pcm = Data()
    private let converter: AVAudioConverter?
    private let out = AVAudioFormat(commonFormat: .pcmFormatInt16, sampleRate: Double(WAV.rate), channels: 1, interleaved: true)!

    init(input: AVAudioFormat) {
        converter = AVAudioConverter(from: input, to: out)
        converter?.downmix = true
    }

    var seconds: Double { lock.lock(); defer { lock.unlock() }; return Double(pcm.count) / Double(WAV.rate * 2) }

    func append(_ buf: AVAudioPCMBuffer) {
        guard let conv = converter, buf.frameLength > 0 else { return }
        let cap = AVAudioFrameCount(Double(buf.frameLength) * out.sampleRate / buf.format.sampleRate) + 64
        guard let ob = AVAudioPCMBuffer(pcmFormat: out, frameCapacity: cap) else { return }
        var fed = false
        var err: NSError?
        conv.convert(to: ob, error: &err) { _, status in
            if fed { status.pointee = .noDataNow; return nil }
            fed = true
            status.pointee = .haveData
            return buf
        }
        guard err == nil, ob.frameLength > 0, let ch = ob.int16ChannelData else { return }
        let bytes = Data(bytes: ch[0], count: Int(ob.frameLength) * 2)
        lock.lock(); pcm.append(bytes); lock.unlock()
    }

    func wav() -> Data { lock.lock(); defer { lock.unlock() }; return WAV.make(pcm) }
}

/// the model file: where it is, where copies are, and getting it
enum WhisperModel {
    static let file = "ggml-large-v3-turbo-q5_0.bin"
    /// the official file (Hugging Face ggerganov/whisper.cpp: X-Linked-Size and X-Linked-ETag)
    static let size: Int64 = 574_041_195
    static let sha256 = "394221709cd5ad1f40c46e6031ca61bce88931e6e088c188294c6d5a55ffa7e2"
    static let url = URL(string: "https://huggingface.co/ggerganov/whisper.cpp/resolve/main/ggml-large-v3-turbo-q5_0.bin")!

    static var defaultPath: String {
        let base = FileManager.default.urls(for: .applicationSupportDirectory, in: .userDomainMask).first
            ?? URL(fileURLWithPath: NSHomeDirectory() + "/Library/Application Support")
        return base.appendingPathComponent("Elisa/models/\(file)").path
    }
    static var path: String {
        if let e = ProcessInfo.processInfo.environment["ELISA_WHISPER_MODEL"], !e.isEmpty { return e }
        if let d = UserDefaults.standard.string(forKey: "whisperModelPath"), !d.isEmpty { return d }
        return defaultPath
    }
    static func fileSize(_ p: String) -> Int64 { ((try? FileManager.default.attributesOfItem(atPath: p))?[.size] as? NSNumber)?.int64Value ?? 0 }
    static func present(_ p: String) -> Bool { fileSize(p) > 1_000_000 }

    /// copies of the official file elsewhere on this Mac (Spotlight), the right size; never modified, only read
    static func findCopies() -> [String] {
        let pr = Process()
        pr.executableURL = URL(fileURLWithPath: "/usr/bin/mdfind")
        pr.arguments = ["-name", file]
        let pipe = Pipe()
        pr.standardOutput = pipe; pr.standardError = FileHandle.nullDevice
        guard (try? pr.run()) != nil else { return [] }
        let d = pipe.fileHandleForReading.readDataToEndOfFile()
        pr.waitUntilExit()
        let target = path
        return String(decoding: d, as: UTF8.self).split(separator: "\n").map(String.init)
            .filter { $0.hasSuffix("/" + file) && $0 != target && fileSize($0) == size }
    }

    static func sha256Hex(_ p: String) -> String? {
        guard let h = FileHandle(forReadingAtPath: p) else { return nil }
        defer { try? h.close() }
        var sha = SHA256()
        while true {
            let chunk = h.readData(ofLength: 8 << 20)
            if chunk.isEmpty { break }
            sha.update(data: chunk)
        }
        return sha.finalize().map { String(format: "%02x", $0) }.joined()
    }

    /// size and SHA-256 match the official file
    static func verify(_ p: String) -> Bool { fileSize(p) == size && sha256Hex(p) == sha256 }
}

/// the PID of the running whisper-server, readable from any thread (app quit kills it synchronously)
final class WhisperPID: @unchecked Sendable {
    static let shared = WhisperPID()
    private let lock = NSLock()
    private var pid: pid_t = 0
    var value: pid_t { get { lock.lock(); defer { lock.unlock() }; return pid } set { lock.lock(); pid = newValue; lock.unlock() } }
    /// SIGTERM to our own server by its PID (never by name)
    func kill() {
        let p = value
        if p > 0 { Darwin.kill(p, SIGTERM) }
        value = 0
        UserDefaults.standard.removeObject(forKey: "whisperServerPID")
    }
}

struct WhisperError: Error { let why: String }

/// whisper.cpp: a warm whisper-server, else whisper-cli per clip
actor Whisper {
    static let shared = Whisper()
    static let idleTimeout: UInt64 = 600

    private var server: Process?
    private var port = 0
    private var serverModel = ""
    private var starting: Task<Int?, Never>?
    private var serverBroken = false
    private var idle: Task<Void, Never>?
    /// how the last clip was transcribed ("server" / "cli"), for the report and the tests
    private(set) var lastPath = ""

    private static let session: URLSession = {
        let c = URLSessionConfiguration.ephemeral
        c.connectionProxyDictionary = [:]  // 127.0.0.1 only, never through a proxy
        c.timeoutIntervalForRequest = 300
        return URLSession(configuration: c)
    }()

    static var threads: Int { min(8, max(2, ProcessInfo.processInfo.activeProcessorCount / 2)) }

    /// /opt/homebrew/bin, then /usr/local/bin, then $PATH
    static func binary(_ name: String) -> String? {
        let path = (ProcessInfo.processInfo.environment["PATH"] ?? "").split(separator: ":").map(String.init)
        for d in ["/opt/homebrew/bin", "/usr/local/bin"] + path {
            let p = (d as NSString).appendingPathComponent(name)
            if FileManager.default.isExecutableFile(atPath: p) { return p }
        }
        return nil
    }

    static func freePort() -> Int? {
        let s = socket(AF_INET, SOCK_STREAM, 0)
        guard s >= 0 else { return nil }
        defer { close(s) }
        var a = sockaddr_in()
        a.sin_len = UInt8(MemoryLayout<sockaddr_in>.size)
        a.sin_family = sa_family_t(AF_INET)
        a.sin_addr.s_addr = inet_addr("127.0.0.1")
        a.sin_port = 0
        let ok = withUnsafePointer(to: &a) { $0.withMemoryRebound(to: sockaddr.self, capacity: 1) { bind(s, $0, socklen_t(MemoryLayout<sockaddr_in>.size)) } }
        guard ok == 0 else { return nil }
        var len = socklen_t(MemoryLayout<sockaddr_in>.size)
        guard withUnsafeMutablePointer(to: &a, { $0.withMemoryRebound(to: sockaddr.self, capacity: 1) { getsockname(s, $0, &len) } }) == 0 else { return nil }
        return Int(UInt16(bigEndian: a.sin_port))
    }

    /// a server a crashed earlier run left behind: its PID was saved; killed only if that PID is still a whisper-server
    private static func killStale() {
        let p = pid_t(UserDefaults.standard.integer(forKey: "whisperServerPID"))
        guard p > 0, Darwin.kill(p, 0) == 0 else { return }
        var buf = [CChar](repeating: 0, count: 4096)
        guard proc_pidpath(p, &buf, UInt32(buf.count)) > 0 else { return }
        if String(cString: buf).hasSuffix("/whisper-server") { Darwin.kill(p, SIGTERM) }
        UserDefaults.standard.removeObject(forKey: "whisperServerPID")
    }

    /// start the server now so the model is loaded by the time the clip is ready
    func warm(model: String) async { _ = await ensureServer(model) }

    private func ensureServer(_ model: String) async -> Int? {
        if let s = server, s.isRunning, serverModel == model { return port }
        if let t = starting { return await t.value }
        if serverBroken { return nil }
        stopServer()
        let t = Task { await launch(model) }
        starting = t
        let p = await t.value
        starting = nil
        return p
    }

    private func launch(_ model: String) async -> Int? {
        guard let bin = Self.binary("whisper-server"), let p = Self.freePort() else { serverBroken = true; return nil }
        Self.killStale()
        let pr = Process()
        pr.executableURL = URL(fileURLWithPath: bin)
        pr.arguments = ["-m", model, "--host", "127.0.0.1", "--port", String(p), "-t", String(Self.threads)]
        pr.standardOutput = FileHandle.nullDevice
        pr.standardError = FileHandle.nullDevice
        do { try pr.run() } catch { serverBroken = true; return nil }
        WhisperPID.shared.value = pr.processIdentifier
        UserDefaults.standard.set(Int(pr.processIdentifier), forKey: "whisperServerPID")
        server = pr; port = p; serverModel = model
        // it answers once the model is loaded (a few seconds)
        let deadline = Date().addingTimeInterval(120)
        while Date() < deadline, pr.isRunning {
            if await Self.answers(p) { return p }
            try? await Task.sleep(nanoseconds: 200_000_000)
        }
        stopServer()
        serverBroken = true
        return nil
    }

    private static func answers(_ p: Int) async -> Bool {
        var r = URLRequest(url: URL(string: "http://127.0.0.1:\(p)/")!)
        r.timeoutInterval = 1
        return ((try? await session.data(for: r))?.1 as? HTTPURLResponse) != nil
    }

    func stopServer() {
        idle?.cancel(); idle = nil
        if server?.isRunning == true { WhisperPID.shared.kill() }
        server = nil; port = 0; serverModel = ""
    }

    /// tests: take the whisper-cli path, as after a server failure
    func breakServerForTest() { serverBroken = true }

    /// the clip's text; the server first, whisper-cli when it fails
    func transcribe(_ wav: Data, language: String, model: String) async throws -> String {
        defer { scheduleIdle() }
        if let p = await ensureServer(model) {
            do { let t = try await post(wav, language: language, port: p); lastPath = "server"; return t }
            catch { stopServer(); serverBroken = true }
        }
        let t = try await cli(wav, language: language, model: model)
        lastPath = "cli"
        return t
    }

    private func scheduleIdle() {
        idle?.cancel()
        idle = Task {
            try? await Task.sleep(nanoseconds: Self.idleTimeout * 1_000_000_000)
            if !Task.isCancelled { stopServer() }
        }
    }

    private func post(_ wav: Data, language: String, port p: Int) async throws -> String {
        let boundary = "elisa-\(UUID().uuidString)"
        var body = Data()
        func field(_ name: String, _ value: String) {
            body.append(Data("--\(boundary)\r\nContent-Disposition: form-data; name=\"\(name)\"\r\n\r\n\(value)\r\n".utf8))
        }
        field("language", language)
        field("response_format", "json")
        field("temperature", "0.0")
        body.append(Data("--\(boundary)\r\nContent-Disposition: form-data; name=\"file\"; filename=\"clip.wav\"\r\nContent-Type: audio/wav\r\n\r\n".utf8))
        body.append(wav)
        body.append(Data("\r\n--\(boundary)--\r\n".utf8))
        var r = URLRequest(url: URL(string: "http://127.0.0.1:\(p)/inference")!)
        r.httpMethod = "POST"
        r.setValue("multipart/form-data; boundary=\(boundary)", forHTTPHeaderField: "Content-Type")
        let (d, resp) = try await Self.session.upload(for: r, from: body)
        guard (resp as? HTTPURLResponse)?.statusCode == 200,
              let j = try JSONSerialization.jsonObject(with: d) as? [String: Any], let t = j["text"] as? String
        else { throw WhisperError(why: "whisper-server: " + String(decoding: d.prefix(200), as: UTF8.self)) }
        return WhisperText.clean(t)
    }

    private func cli(_ wav: Data, language: String, model: String) async throws -> String {
        guard let bin = Self.binary("whisper-cli") else {
            throw WhisperError(why: "whisper.cpp is not installed (brew install whisper-cpp)")
        }
        let tmp = FileManager.default.temporaryDirectory.appendingPathComponent("elisa-voice-\(UUID().uuidString).wav")
        try wav.write(to: tmp)
        defer { try? FileManager.default.removeItem(at: tmp) }
        let args = ["-m", model, "-f", tmp.path, "-l", language, "-nt", "-np", "-t", String(Self.threads)]
        let (code, out) = await Task.detached { () -> (Int32, Data) in
            let pr = Process()
            pr.executableURL = URL(fileURLWithPath: bin)
            pr.arguments = args
            let pipe = Pipe()
            pr.standardOutput = pipe
            pr.standardError = FileHandle.nullDevice
            guard (try? pr.run()) != nil else { return (-1, Data()) }
            let d = pipe.fileHandleForReading.readDataToEndOfFile()
            pr.waitUntilExit()
            return (pr.terminationStatus, d)
        }.value
        guard code == 0 else { throw WhisperError(why: "whisper-cli failed (exit \(code))") }
        return WhisperText.clean(String(decoding: out, as: UTF8.self))
    }
}

extension AppModel {
    /// the user's own messages (not slash commands), oldest first
    var userTexts: [String] {
        blocks.compactMap { if case .user(let t, let cmd) = $0.kind, !cmd { return t }; return nil }
    }

    /// the language whisper is told now
    var voiceLocale: String {
        let choice = VoiceLang(rawValue: UserDefaults.standard.string(forKey: "voiceLang") ?? "") ?? .system
        return VoiceLocale.resolve(choice, recent: userTexts, preferred: Locale.preferredLanguages)
    }
}

/// the mic button's state and what it drives: recording, transcribing, getting the model
@MainActor
final class Voice: NSObject, ObservableObject, URLSessionDownloadDelegate {
    static let shared = Voice()

    enum State: Equatable { case idle, recording, transcribing, downloading(Double) }
    @Published private(set) var state: State = .idle
    /// the language of the clip being recorded / transcribed
    @Published private(set) var lang = ""

    private weak var m: AppModel?
    private weak var window: NSWindow?
    private var engine: AVAudioEngine?
    private var recorder: VoiceRecorder?
    /// bumped on every start / cancel: an older clip's text is dropped
    private var session = 0
    private var download: URLSessionDownloadTask?
    private var downloadTo = ""

    private override init() {
        super.init()
        // the warm server is this app's child: it goes when the app quits (by its PID)
        NotificationCenter.default.addObserver(forName: NSApplication.willTerminateNotification, object: nil, queue: nil) { _ in
            WhisperPID.shared.kill()
        }
        NotificationCenter.default.addObserver(forName: NSWindow.willCloseNotification, object: nil, queue: .main) { n in
            let w = n.object as? NSWindow
            MainActor.assumeIsolated { if w != nil, w === Voice.shared.window { Voice.shared.cancel() } }
        }
    }

    func click(_ m: AppModel) {
        switch state {
        case .idle: start(m)
        case .recording: stop()
        case .transcribing: break
        case .downloading: cancelDownload()
        }
    }

    private func note(_ s: String) { m?.blocks.append(Block(kind: .info("[voice: \(s)]", .warn))) }

    // MARK: - recording

    func start(_ m: AppModel) {
        guard state == .idle else { return }
        self.m = m
        window = NSApp.keyWindow
        guard Whisper.binary("whisper-server") != nil || Whisper.binary("whisper-cli") != nil else {
            note("whisper.cpp is not installed. Install it with: brew install whisper-cpp"); return
        }
        let model = WhisperModel.path
        guard WhisperModel.present(model) else { offerModel(); return }
        guard Bundle.main.object(forInfoDictionaryKey: "NSMicrophoneUsageDescription") != nil else {
            // without it macOS would end the app on the first request: say so instead
            note("this build has no microphone usage string (app/ElisaApp/Info.plist, embedded by Package.swift); rebuild with swift build -c release")
            return
        }
        session += 1
        let s = session
        switch AVCaptureDevice.authorizationStatus(for: .audio) {
        case .authorized: record(s, model: model)
        case .notDetermined:
            AVCaptureDevice.requestAccess(for: .audio) { ok in
                Task { @MainActor in
                    guard s == Voice.shared.session else { return }
                    if ok { Voice.shared.record(s, model: model) } else { Voice.shared.micDenied() }
                }
            }
        case .restricted: note("the microphone is restricted on this Mac (Screen Time or a profile)")
        default: micDenied()
        }
    }

    private func micDenied() {
        note("microphone access is off for Elisa. Turn it on in System Settings > Privacy & Security > Microphone "
             + "(for the app that launched Elisa, e.g. Terminal), then click the mic again")
    }

    private func record(_ s: Int, model: String) {
        guard let m else { return }
        let eng = AVAudioEngine()
        let input = eng.inputNode
        let fmt = input.outputFormat(forBus: 0)
        guard fmt.sampleRate > 0, fmt.channelCount > 0 else { note("no microphone found (System Settings > Sound > Input)"); return }
        let rec = VoiceRecorder(input: fmt)
        input.installTap(onBus: 0, bufferSize: 4096, format: fmt, block: Self.tap(rec))
        eng.prepare()
        do { try eng.start() } catch {
            input.removeTap(onBus: 0)
            note("the microphone could not start: \(error.localizedDescription)"); return
        }
        engine = eng; recorder = rec
        lang = m.voiceLocale
        state = .recording
        // load the model while the user speaks
        Task.detached { await Whisper.shared.warm(model: model) }
    }

    nonisolated private static func tap(_ rec: VoiceRecorder) -> AVAudioNodeTapBlock {
        { buf, _ in rec.append(buf) }
    }

    private func stopAudio() {
        if let e = engine { e.stop(); e.inputNode.removeTap(onBus: 0) }
        engine = nil
    }

    /// stop recording and transcribe the clip into the field
    func stop() {
        guard state == .recording, let rec = recorder else { return }
        stopAudio()
        recorder = nil
        guard rec.seconds >= 0.4 else { state = .idle; return }
        state = .transcribing
        m?.showLocalActivity("Transcribing your voice…")
        let (s, wav, language, model) = (session, rec.wav(), lang, WhisperModel.path)
        Task {
            let r: Result<String, Error>
            do { r = .success(try await Whisper.shared.transcribe(wav, language: language, model: model)) } catch { r = .failure(error) }
            guard s == session else { return }
            state = .idle
            m?.clearLocalActivity()
            switch r {
            case .success(let t):
                guard !t.isEmpty, let m else { return }
                let sep = m.input.isEmpty || m.input.hasSuffix(" ") || m.input.hasSuffix("\n") ? "" : " "
                m.input += sep + t
            case .failure(let e):
                note("transcription failed: \((e as? WhisperError)?.why ?? e.localizedDescription)")
            }
        }
    }

    /// stop at once and drop the clip (a message was sent, the window closed, the session reset); a download goes on
    func cancel() {
        session += 1
        stopAudio()
        recorder = nil
        if state == .transcribing { m?.clearLocalActivity() }
        if state == .recording || state == .transcribing { state = .idle }
    }

    // MARK: - the model

    private func offerModel() {
        let dest = WhisperModel.path
        let copies = WhisperModel.findCopies()
        let a = NSAlert()
        a.messageText = "Voice input needs the Whisper model"
        a.informativeText = "\(WhisperModel.file) (574 MB) is not at \((dest as NSString).abbreviatingWithTildeInPath).\n\n"
            + "Elisa transcribes your voice on this Mac with whisper.cpp; the audio never leaves it."
            + (copies.isEmpty ? "" : "\n\nA copy was found at \((copies[0] as NSString).abbreviatingWithTildeInPath) (it is only read).")
        if let c = copies.first { a.addButton(withTitle: "Copy from \((c as NSString).deletingLastPathComponent.split(separator: "/").last ?? "found copy")") }
        a.addButton(withTitle: "Download (574 MB)")
        a.addButton(withTitle: "Choose File…")
        a.addButton(withTitle: "Cancel")
        var r = a.runModal().rawValue - NSApplication.ModalResponse.alertFirstButtonReturn.rawValue
        if copies.isEmpty { r += 1 }
        switch r {
        case 0: copyModel(copies[0], to: dest)
        case 1: startDownload(to: dest)
        case 2: chooseModel()
        default: note("no model, so no voice input yet. Click the mic again to copy, download, or choose \(WhisperModel.file)")
        }
    }

    private func copyModel(_ src: String, to dest: String) {
        state = .downloading(0)
        Task.detached {
            let ok = Self.install(copyOf: src, to: dest)
            await MainActor.run {
                Voice.shared.state = .idle
                Voice.shared.note(ok ? "model copied to \((dest as NSString).abbreviatingWithTildeInPath). Click the mic to talk"
                                     : "copying the model failed; click the mic to try again")
            }
        }
    }

    /// a copy of the model, put in place only when its size is right
    nonisolated private static func install(copyOf src: String, to dest: String) -> Bool {
        let fm = FileManager.default
        let part = dest + ".part"
        do {
            try fm.createDirectory(atPath: (dest as NSString).deletingLastPathComponent, withIntermediateDirectories: true)
            try? fm.removeItem(atPath: part)
            try fm.copyItem(atPath: src, toPath: part)
            guard WhisperModel.fileSize(part) == WhisperModel.size else { try? fm.removeItem(atPath: part); return false }
            try? fm.removeItem(atPath: dest)
            try fm.moveItem(atPath: part, toPath: dest)
            return true
        } catch { try? fm.removeItem(atPath: part); return false }
    }

    /// the downloaded file, put in place only when its size and SHA-256 match the official one
    nonisolated private static func install(download part: String, to dest: String) -> Bool {
        let fm = FileManager.default
        defer { try? fm.removeItem(atPath: part) }
        guard WhisperModel.verify(part) else { return false }
        do {
            try fm.createDirectory(atPath: (dest as NSString).deletingLastPathComponent, withIntermediateDirectories: true)
            try? fm.removeItem(atPath: dest)
            try fm.moveItem(atPath: part, toPath: dest)
            return true
        } catch { return false }
    }

    private func chooseModel() {
        let p = NSOpenPanel()
        p.allowsMultipleSelection = false
        p.canChooseDirectories = false
        p.message = "Choose a whisper.cpp model (\(WhisperModel.file))"
        guard p.runModal() == .OK, let u = p.url else { return }
        UserDefaults.standard.set(u.path, forKey: "whisperModelPath")
        note("using the model at \((u.path as NSString).abbreviatingWithTildeInPath). Click the mic to talk")
    }

    private func startDownload(to dest: String) {
        downloadTo = dest
        let s = URLSession(configuration: .default, delegate: self, delegateQueue: nil)
        let t = s.downloadTask(with: WhisperModel.url)
        download = t
        state = .downloading(0)
        note("downloading \(WhisperModel.file) (574 MB) from huggingface.co; click the mic to cancel")
        t.resume()
    }

    private func cancelDownload() {
        download?.cancel(); download = nil
        state = .idle
        note("download cancelled")
    }

    nonisolated func urlSession(_ session: URLSession, downloadTask: URLSessionDownloadTask, didWriteData _: Int64, totalBytesWritten: Int64, totalBytesExpectedToWrite _: Int64) {
        let f = Double(totalBytesWritten) / Double(WhisperModel.size)
        Task { @MainActor in if case .downloading = Voice.shared.state { Voice.shared.state = .downloading(min(1, f)) } }
    }

    nonisolated func urlSession(_ session: URLSession, downloadTask: URLSessionDownloadTask, didFinishDownloadingTo location: URL) {
        // the file must leave `location` before this returns
        let part = FileManager.default.temporaryDirectory.appendingPathComponent("elisa-\(WhisperModel.file).part")
        try? FileManager.default.removeItem(at: part)
        let moved = (try? FileManager.default.moveItem(at: location, to: part)) != nil
        session.finishTasksAndInvalidate()
        Task.detached {
            let dest = await MainActor.run { Voice.shared.downloadTo }
            let ok = moved && Voice.install(download: part.path, to: dest)
            await MainActor.run {
                Voice.shared.download = nil
                Voice.shared.state = .idle
                Voice.shared.note(ok ? "model downloaded and checked (SHA-256). Click the mic to talk"
                                     : "the download did not match the official size / SHA-256 and was deleted; click the mic to try again")
            }
        }
    }

    nonisolated func urlSession(_ session: URLSession, task: URLSessionTask, didCompleteWithError error: Error?) {
        guard let error else { return }
        session.finishTasksAndInvalidate()
        guard (error as NSError).code != NSURLErrorCancelled else { return }
        let why = error.localizedDescription
        Task { @MainActor in
            Voice.shared.download = nil
            Voice.shared.state = .idle
            Voice.shared.note("the model download failed: \(why)")
        }
    }
}

/// the 🎤 button (styled like the 📎 one) and its EN / VI / AUTO tag; right-click for the language menu
struct VoiceButton: View {
    @EnvironmentObject var m: AppModel
    @Environment(\.pal) var p
    @ObservedObject private var v = Voice.shared
    @AppStorage("voiceLang") private var choice = VoiceLang.system.rawValue
    @State private var hover = false

    var body: some View {
        let busy = v.state != .idle
        let id = busy && !v.lang.isEmpty ? v.lang : m.voiceLocale
        let sel = VoiceLang(rawValue: choice) ?? .system
        HStack(spacing: 4) {
            switch v.state {
            case .recording:
                Circle().fill(p.acc).frame(width: 6, height: 6)
                    .phaseAnimator([1.0, 0.25]) { c, o in c.opacity(o) } animation: { _ in .easeInOut(duration: 0.6) }
                    .help("Recording")
            case .transcribing:
                Text("transcribing…").font(Fonts.mono(10)).foregroundStyle(p.fg2).frame(height: 22)
            case .downloading(let f):
                Text("model \(Int(f * 100))%").font(Fonts.mono(10)).foregroundStyle(p.fg2).frame(height: 22)
            case .idle:
                EmptyView()
            }
            Button {
                // EN → VI → AUTO → EN
                choice = (id == VoiceLocale.en ? VoiceLang.vi : id == VoiceLocale.vi ? .auto : .en).rawValue
            } label: {
                Text(VoiceLocale.tag(id)).font(Fonts.mono(10, .semibold))
                    .foregroundStyle(busy ? p.acc : p.fg2)
                    .frame(height: 22)
            }
            .buttonStyle(.plain)
            .disabled(busy)
            .help("Voice language: \(VoiceLocale.name(id)) (\(sel.label)) · click to switch EN / VI / AUTO · right-click the mic for more")
            Button { v.click(m) } label: {
                Image(systemName: v.state == .recording ? "mic.fill" : "mic").font(.system(size: 13, weight: .medium))
                    .foregroundStyle(busy || hover ? p.acc : p.fg)
                    .symbolEffect(.pulse, isActive: busy)
                    .frame(width: 22, height: 22)
            }
            .buttonStyle(.plain)
            .keyboardShortcut("d", modifiers: [.command, .shift])
            .onHover { hover = $0 }
            .help(help(id))
            .contextMenu {
                Picker("Voice Language", selection: $choice) {
                    ForEach(VoiceLang.allCases) { l in Text(l.label).tag(l.rawValue) }
                }
                .pickerStyle(.inline)
            }
        }
        .onDisappear { v.cancel() }
        .onChange(of: m.phase) { _, ph in if ph == .starting || ph == .exited { v.cancel() } }
    }

    private func help(_ id: String) -> String {
        switch v.state {
        case .recording: return "Recording (\(VoiceLocale.name(id))) · click (⌘⇧D) to stop and transcribe on this Mac"
        case .transcribing: return "Transcribing on this Mac with whisper.cpp…"
        case .downloading: return "Getting the Whisper model · click to cancel a download"
        case .idle: return "Voice input in \(VoiceLocale.name(id)) with whisper.cpp, on this Mac only (⌘⇧D) · right-click for the language"
        }
    }
}
