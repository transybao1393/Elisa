// Speak replies aloud (Elisa menu > Speak Replies, off by default, remembered as "speakReplies"): the final assistant
// reply of a turn, once it is complete (turn_done), on device with a system voice (AVSpeechSynthesizer). Only the
// reply's prose after its last tool step is spoken: no tool rows, info lines, code blocks or state card; markdown,
// links and emoji are stripped. The voice follows the reply's language: Vietnamese (its letters, or
// NLLanguageRecognizer) gets a vi-VN voice, anything else en-US. Speech stops when the user types, sends, presses Esc
// (or ⌘.), or turns the toggle off.
import AVFoundation
import Foundation
import NaturalLanguage

/// the pure text side: what of a reply is spoken, and in which language (unit tested)
enum SpeakText {
    static let vi = "vi-VN"
    static let en = "en-US"

    /// the language to speak `text` in
    static func language(_ text: String) -> String {
        if VoiceLocale.hasVietnamese(text) { return vi }
        let r = NLLanguageRecognizer()
        r.languageConstraints = [.english, .vietnamese]
        r.processString(text)
        return r.dominantLanguage == .vietnamese ? vi : en
    }

    /// the reply's prose after its last tool / check step (the final answer), joined
    static func finalProse(_ parts: [Part]) -> String {
        var out: [String] = []
        for p in parts {
            switch p.kind {
            case .prose(let t): out.append(t)
            case .tool, .check: out = []
            default: break
            }
        }
        return out.joined(separator: "\n")
    }

    /// markdown to speakable plain text: no code blocks, state card, tables' rules, URLs, emoji or markup
    static func plain(_ md: String) -> String {
        var s = md
        func re(_ pattern: String, _ with: String, _ opts: NSRegularExpression.Options = []) {
            guard let r = try? NSRegularExpression(pattern: pattern, options: opts) else { return }
            s = r.stringByReplacingMatches(in: s, range: NSRange(s.startIndex..., in: s), withTemplate: with)
        }
        re("```[\\s\\S]*?(```|$)", "\n")                    // fenced code (an unclosed fence: to the end)
        re("<state>[\\s\\S]*?(</state>|$)", "")              // the state card, if it reached the prose
        re("!\\[[^\\]]*\\]\\([^)]*\\)", "")                  // images
        re("\\[([^\\]]+)\\]\\([^)]*\\)", "$1")               // links: their text
        re("https?://\\S+", "")                              // bare URLs
        re("`([^`]*)`", "$1")                                // inline code: its text
        re("<[^>\\n]+>", "")                                 // HTML tags
        re("^\\s{0,3}#{1,6}\\s*", "", .anchorsMatchLines)    // headings
        re("^\\s*>\\s?", "", .anchorsMatchLines)             // quotes
        re("^\\s*([-*+]|\\d+[.)])\\s+", "", .anchorsMatchLines) // list markers
        re("^\\s*\\|?\\s*:?-{3,}.*$", "", .anchorsMatchLines) // table rules / horizontal rules
        re("\\|", ", ")                                      // table cells
        re("(\\*\\*|__|~~)", "")                             // bold, strike
        re("(^|[\\s(])[*_]([^*_\\n]+)[*_]", "$1$2")          // italics
        // emoji (and their joiners / variation selectors): the voice would read their names
        s = String(String.UnicodeScalarView(s.unicodeScalars.filter { u in
            !(u.properties.isEmojiPresentation || (u.properties.isEmoji && u.value > 0x238C) || u.value == 0x200D || u.value == 0xFE0F)
        }))
        re("[ \\t]+", " ")
        re(" ?\\n ?", "\n")
        re("\\n{2,}", "\n")
        return s.trimmingCharacters(in: .whitespacesAndNewlines)
    }
}

@MainActor
final class Speak {
    static let shared = Speak()
    static let key = "speakReplies"

    private let synth = AVSpeechSynthesizer()
    /// the turn was interrupted (Esc): its partial reply is not spoken
    private var skipTurn = false
    /// the missing Vietnamese voice was pointed out (once per launch)
    private var viNoted = false

    var enabled: Bool { UserDefaults.standard.bool(forKey: Self.key) }

    /// stop speaking now (typing, a new message, Esc, the toggle turned off)
    func stop() {
        if synth.isSpeaking || synth.isPaused { synth.stopSpeaking(at: .immediate) }
    }

    /// Esc / ⌘.: stop, and do not speak the interrupted turn's reply when it ends
    func interrupted() { stop(); skipTurn = true }

    /// a new turn started (the `user` event)
    func newTurn() { stop(); skipTurn = false }

    /// the turn ended: speak its final reply (when on)
    func turnDone(_ parts: [Part], note: (String) -> Void) {
        defer { skipTurn = false }
        guard enabled, !skipTurn else { return }
        let text = SpeakText.plain(SpeakText.finalProse(parts))
        guard !text.isEmpty else { return }
        let lang = SpeakText.language(text)
        guard let voice = Self.voice(lang) else {
            if !viNoted {
                viNoted = true
                note("[speak: no Vietnamese voice is installed · System Settings > Accessibility > Spoken Content > System voice > Manage Voices… and add one (e.g. Linh), then replies in Vietnamese are spoken]")
            }
            return
        }
        stop()
        let u = AVSpeechUtterance(string: text)
        u.voice = voice
        synth.speak(u)
    }

    /// the voice for `lang` (vi-VN / en-US): an installed enhanced / premium one, else the system's default voice for
    /// the language (not the first in the list: en-US lists novelty voices like Zarvox); nil when none is installed
    static func voice(_ lang: String) -> AVSpeechSynthesisVoice? {
        let all = AVSpeechSynthesisVoice.speechVoices().filter { $0.language == lang }
        // installed = listed by speechVoices(): AVSpeechSynthesisVoice(language: "vi-VN") returns Linh even when it is
        // not downloaded; en-US always has its default
        let base = AVSpeechSynthesisVoice(language: lang).flatMap { v in
            v.language == lang && (lang == SpeakText.en || all.contains { $0.identifier == v.identifier }) ? v : nil
        }
        let better = all.filter { $0.quality.rawValue > (base?.quality.rawValue ?? 0) && $0.quality != .default }
        return better.max(by: { $0.quality.rawValue < $1.quality.rawValue }) ?? base ?? all.first
    }
}
