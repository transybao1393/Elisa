// Syntax colours for fenced code blocks: a small native tokenizer (keywords, strings, comments, numbers, calls) for
// Rust, Python, Swift, JS/TS, SQL, bash and JSON. Colours follow the kit prototype's own highlighter (08-source
// "Elisa Prototype": keywords accent, strings ok, comments textMuted, calls code, the rest text); numbers take warn.
// No dependencies; unknown languages stay plain text.
import SwiftUI

enum Syn { case plain, kw, str, com, num, call, key, vari }

struct SynLang {
    var kws: Set<String>
    /// line-comment openers
    var line: [String] = []
    var block: (String, String)?
    var quotes: Set<Character> = ["\""]
    /// Python ''' / """ strings
    var triple = false
    /// SQL: keywords are case-insensitive
    var nocase = false
    /// bash: `#` opens a comment only at a word start; `$VAR` / `${…}` are variables
    var shell = false
    /// JSON: a string followed by `:` is a key
    var json = false
    /// Rust: `'a` is a lifetime unless it closes as a char literal
    var rust = false
    /// a word followed by `(` (or `!` in Rust) is a call
    var calls = true
}

@MainActor
enum Highlight {
    private static let c = ["break", "continue", "else", "for", "if", "in", "return", "while", "true", "false"]
    static let rust = SynLang(kws: Set(c + ["as", "async", "await", "const", "crate", "dyn", "enum", "extern", "fn", "impl", "let", "loop",
                                            "match", "mod", "move", "mut", "pub", "ref", "self", "Self", "static", "struct", "super",
                                            "trait", "type", "unsafe", "use", "where"]),
                              line: ["//"], block: ("/*", "*/"), rust: true)
    static let python = SynLang(kws: Set(c.filter { $0 != "true" && $0 != "false" } + ["False", "None", "True", "and", "as", "assert", "async",
                                            "await", "class", "def", "del", "elif", "except", "finally", "from", "global", "import", "is",
                                            "lambda", "nonlocal", "not", "or", "pass", "raise", "try", "with", "yield", "self"]),
                                line: ["#"], quotes: ["\"", "'"], triple: true)
    static let swift = SynLang(kws: Set(c + ["actor", "any", "as", "async", "await", "case", "catch", "class", "default", "defer", "do",
                                             "enum", "extension", "fallthrough", "fileprivate", "func", "guard", "import", "init", "inout",
                                             "internal", "is", "let", "nil", "open", "private", "protocol", "public", "repeat", "rethrows",
                                             "self", "Self", "some", "static", "struct", "subscript", "super", "switch", "throw", "throws",
                                             "try", "typealias", "var", "weak", "where"]),
                               line: ["//"], block: ("/*", "*/"))
    static let js = SynLang(kws: Set(c + ["as", "async", "await", "case", "catch", "class", "const", "default", "delete", "do", "enum",
                                          "export", "extends", "finally", "from", "function", "implements", "import", "instanceof",
                                          "interface", "let", "new", "null", "of", "private", "protected", "public", "readonly", "static",
                                          "super", "switch", "this", "throw", "try", "type", "typeof", "undefined", "var", "void", "yield"]),
                            line: ["//"], block: ("/*", "*/"), quotes: ["\"", "'", "`"])
    static let sql = SynLang(kws: ["select", "from", "where", "and", "or", "not", "insert", "into", "values", "update", "set", "delete",
                                   "create", "table", "index", "view", "drop", "alter", "add", "join", "inner", "left", "right", "outer",
                                   "full", "on", "as", "group", "by", "order", "having", "limit", "offset", "union", "all", "distinct",
                                   "case", "when", "then", "else", "end", "null", "is", "in", "like", "between", "exists", "primary", "key",
                                   "foreign", "references", "default", "unique", "check", "constraint", "begin", "commit", "rollback",
                                   "with", "returning", "asc", "desc", "integer", "int", "text", "varchar", "boolean", "true", "false",
                                   "if"],
                             line: ["--"], block: ("/*", "*/"), quotes: ["'", "\""], nocase: true)
    static let bash = SynLang(kws: ["if", "then", "else", "elif", "fi", "for", "while", "until", "do", "done", "case", "esac", "in",
                                    "function", "return", "local", "export", "readonly", "declare", "set", "unset", "shift", "exit",
                                    "break", "continue", "source", "echo", "cd", "sudo"],
                              line: ["#"], quotes: ["\"", "'"], shell: true, calls: false)
    static let json = SynLang(kws: ["true", "false", "null"], json: true, calls: false)

    static func lang(_ name: String) -> SynLang? {
        let n = name.trimmingCharacters(in: .whitespaces).lowercased().split(separator: " ").first.map(String.init) ?? ""
        switch n {
        case "rust", "rs": return rust
        case "python", "py", "python3": return python
        case "swift": return swift
        case "js", "javascript", "jsx", "mjs", "cjs", "ts", "typescript", "tsx": return js
        case "sql", "sqlite", "postgres", "postgresql", "mysql", "psql": return sql
        case "bash", "sh", "shell", "zsh", "console", "shell-session": return bash
        case "json", "jsonc", "json5": return json
        default: return nil
        }
    }

    private static func ident(_ ch: Character) -> Bool { ch == "_" || ch.isLetter || ch.isNumber }

    /// the code split into coloured runs (concatenating the runs gives the code back unchanged)
    static func tokens(_ code: String, _ l: SynLang) -> [(String, Syn)] {
        let c = Array(code)
        var out: [(String, Syn)] = []
        var i = 0
        func push(_ s: Int, _ e: Int, _ k: Syn) { if e > s { out.append((String(c[s..<e]), k)) } }
        func at(_ p: String, _ i: Int) -> Bool {
            var j = i
            for ch in p { guard j < c.count, c[j] == ch else { return false }; j += 1 }
            return true
        }
        /// index just past the closing `q` (escapes skipped); a single-line string stops at the line end
        func closeQuote(_ q: String, from s: Int, multiline: Bool) -> Int {
            var j = s
            while j < c.count {
                if c[j] == "\\" && !l.shell || c[j] == "\\" && q == "\"" { j += 2; continue }
                if at(q, j) { return j + q.count }
                if c[j] == "\n" && !multiline { return j }
                j += 1
            }
            return c.count
        }
        while i < c.count {
            let ch = c[i]
            if let lc = l.line.first(where: { at($0, i) }), !(l.shell && i > 0 && !c[i - 1].isWhitespace) {
                var j = i + lc.count
                while j < c.count && c[j] != "\n" { j += 1 }
                push(i, j, .com); i = j; continue
            }
            if let (o, e) = l.block, at(o, i) {
                var j = i + o.count
                while j < c.count && !at(e, j) { j += 1 }
                j = min(c.count, j + e.count)
                push(i, j, .com); i = j; continue
            }
            if l.triple, let q = ["\"\"\"", "'''"].first(where: { at($0, i) }) {
                let j = closeQuote(q, from: i + 3, multiline: true)
                push(i, j, .str); i = j; continue
            }
            if l.rust && ch == "'" {
                // char literal ('a', '\n', '\u{1F600}') or a lifetime ('a, 'static)
                if i + 1 < c.count, c[i + 1] == "\\" {
                    let j = closeQuote("'", from: i + 1, multiline: false)
                    push(i, j, .str); i = j; continue
                }
                if i + 2 < c.count, c[i + 2] == "'" { push(i, i + 3, .str); i += 3; continue }
                var j = i + 1
                while j < c.count && ident(c[j]) { j += 1 }
                push(i, j, .plain); i = max(j, i + 1); continue
            }
            if l.quotes.contains(ch) {
                let j = closeQuote(String(ch), from: i + 1, multiline: ch == "`" || l.shell)
                var k: Syn = .str
                if l.json {
                    var n = j
                    while n < c.count && (c[n] == " " || c[n] == "\t") { n += 1 }
                    if n < c.count && c[n] == ":" { k = .key }
                }
                push(i, j, k); i = j; continue
            }
            if l.shell && ch == "$" && i + 1 < c.count {
                var j = i + 1
                if c[j] == "{" {
                    while j < c.count && c[j] != "}" && c[j] != "\n" { j += 1 }
                    j = min(c.count, j + 1)
                } else if ident(c[j]) {
                    while j < c.count && ident(c[j]) { j += 1 }
                } else if "?#@*!$0123456789".contains(c[j]) {
                    j += 1
                }
                if j > i + 1 { push(i, j, .vari); i = j; continue }
            }
            if ch.isASCII && ch.isNumber && (i == 0 || !ident(c[i - 1])) || ch == "-" && l.json && i + 1 < c.count && c[i + 1].isASCII && c[i + 1].isNumber {
                var j = i + 1
                while j < c.count && (ident(c[j]) || c[j] == "." && j + 1 < c.count && c[j + 1].isNumber) { j += 1 }
                push(i, j, .num); i = j; continue
            }
            if ch == "_" || ch.isLetter {
                var j = i + 1
                while j < c.count && ident(c[j]) { j += 1 }
                let w = String(c[i..<j])
                let k: Syn
                if l.kws.contains(l.nocase ? w.lowercased() : w) {
                    k = .kw
                } else if l.calls, j < c.count, c[j] == "(" || l.rust && c[j] == "!" {
                    k = .call
                } else {
                    k = .plain
                }
                push(i, j, k); i = j; continue
            }
            // a run of anything else up to the next interesting character
            var j = i + 1
            while j < c.count, !(c[j] == "_" || c[j].isLetter || c[j].isNumber || l.quotes.contains(c[j]) || c[j] == "/" || c[j] == "#"
                                 || c[j] == "-" || c[j] == "$" || c[j] == "'" || c[j] == "\"") { j += 1 }
            push(i, j, .plain); i = j
        }
        return out
    }

    private static var cache: [String: AttributedString] = [:]

    /// `code` coloured for `lang` in the palette `p`, at the code font; nil when the language is not one of ours
    static func attributed(_ code: String, lang: String, pal p: Palette, font: Font) -> AttributedString? {
        guard let l = Highlight.lang(lang) else { return nil }
        let key = "\(lang)|\(p.dark)|\(code)"
        if let a = cache[key] { return a }
        var a = AttributedString()
        for (t, k) in tokens(code, l) {
            var r = AttributedString(t)
            r.foregroundColor = switch k {
            case .plain: p.tx
            case .kw: p.acc
            case .str: p.ok
            case .com: p.fg2
            case .num: p.warn
            case .call, .key, .vari: p.code
            }
            a += r
        }
        a.font = font
        if cache.count > 160 { cache.removeAll() }
        cache[key] = a
        return a
    }
}
