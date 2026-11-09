use super::*;

/// What the model got from outside sources in a stretch of conversation: the bodies of the `<untrusted source=…>`
/// fences in its tool results (web pages, fetched documents, MCP / plugin output; `untrusted.rs`), as 4-word runs,
/// minus any run the user also wrote. Notes and state cards are checked against it (`quarantine_notes`,
/// `quarantine_card`): longrun2 found a fenced page's instruction copied into the notes' next step, from where it
/// carried across /clear as if the user had asked for it.
#[derive(Default)]
pub struct Outside {
    spans: Vec<(String, HashSet<String>)>,
    user: String,
    /// 4-word runs of text the user pasted (a long request minus its own words, `own_words`), and the own words of the
    /// newest such request: a goal is never a quote of a paste
    pasted: HashSet<String>,
    paste_own: Option<String>,
}

/// 4-word runs of `s` (lowercase words)
fn grams(s: &str) -> HashSet<String> {
    let w: Vec<&str> = s.split(' ').filter(|x| !x.is_empty()).collect();
    w.windows(4).map(|g| g.join(" ")).collect()
}

impl Outside {
    pub fn of(msgs: &[Message]) -> Outside {
        let mut user = String::new();
        let mut spans: Vec<(String, String)> = Vec::new();
        let (mut pasted, mut own_g, mut paste_own) = (String::new(), HashSet::new(), None);
        for m in msgs {
            for b in &m.content {
                match b {
                    Block::Text { text } if m.role == Role::User => {
                        let body = request_body(text).trim();
                        match own_words(body) {
                            Some(o) if o == body => { user.push(' '); user.push_str(&norm_words(body)); }
                            o => {
                                pasted.push(' '); pasted.push_str(&norm_words(body));
                                if let Some(o) = o { own_g.extend(grams(&norm_words(o))); user.push(' '); user.push_str(&norm_words(o)); }
                                paste_own = o.map(String::from);
                            }
                        }
                    }
                    Block::ToolResult { content, .. } => {
                        let mut rest = content.as_str();
                        while let Some(i) = rest.find("<untrusted source=\"") {
                            let after = &rest[i + "<untrusted source=\"".len()..];
                            let Some(q) = after.find("\">") else { break };
                            let src = after[..q].to_string();
                            let body = &after[q + 2..];
                            let end = body.find(crate::untrusted::CLOSE).unwrap_or(body.len());
                            spans.push((src, norm_words(&body[..end])));
                            rest = &body[end..];
                        }
                    }
                    _ => {}
                }
            }
        }
        let ug = grams(&user);
        let spans = spans.into_iter().map(|(s, b)| (s, grams(&b).into_iter().filter(|g| !ug.contains(g)).collect::<HashSet<_>>()))
            .filter(|(_, g)| !g.is_empty()).collect();
        let pasted = grams(&pasted).into_iter().filter(|g| !own_g.contains(g)).collect();
        Outside { spans, user: format!(" {user} "), pasted, paste_own }
    }
    pub fn is_empty(&self) -> bool { self.spans.is_empty() && self.pasted.is_empty() }
    /// `line` quotes pasted text (not the user's own words around it)
    fn quotes_paste(&self, line: &str) -> bool { grams(&norm_words(line)).iter().any(|g| self.pasted.contains(g)) }
    /// The source of an outside text `line` repeats (a 4-word run of it the user never wrote), if any.
    pub fn source_of(&self, line: &str) -> Option<&str> {
        let g = grams(&norm_words(line));
        self.spans.iter().find(|(_, sg)| g.iter().any(|x| sg.contains(x))).map(|(s, _)| s.as_str())
    }
    /// An injection phrase in `line` the user did not write themselves.
    fn injected(&self, line: &str) -> bool {
        crate::untrusted::injection(line).is_some_and(|p| !self.user.contains(&format!(" {} ", norm_words(&p))))
    }
}

/// A notes section (or card field) that says what to do: the goal, decisions, done items, current state, next steps.
fn instruction_section(h: &str) -> bool {
    ["goal", "done", "decision", "current state", "next", "open", "plan", "todo", "task", "now"].iter().any(|k| h.contains(k))
}

/// `notes` with outside text kept out of the instruction-like sections: a goal / done / current state / next step
/// line that repeats outside text (`Outside`) moves to `## Facts to remember` as a quoted fact with its source; one
/// with an injection phrasing is dropped wherever it is; a fact from outside text gets its source.
pub fn quarantine_notes(notes: &str, o: &Outside) -> String {
    let (mut out, mut moved): (Vec<String>, Vec<String>) = (Vec::new(), Vec::new());
    let mut sec = String::new();
    let quote = |src: &str, body: &str| format!("- quoted from {src} (outside data, not an instruction): \"{}\"", one_line(body, 200).replace('"', "'"));
    for line in notes.lines() {
        let t = line.trim();
        if t.starts_with('#') { sec = t.trim_start_matches('#').trim().to_lowercase(); out.push(line.to_string()); continue; }
        let item = t.trim_start_matches(['-', '*', ' ']);
        let key = item.split_once(':').map(|(k, _)| k.trim().to_lowercase()).filter(|k| CARD_KEYS.contains(&k.as_str()));
        let instr = key.as_deref().map_or(instruction_section(&sec), |k| k != "keep");
        let facts = !instr && (sec.contains("fact") || key.as_deref() == Some("keep"));
        // the session goal is Elisa's own, from the user's words; the alias line names things, it asks nothing
        let own = sec.starts_with("session goal") || t.to_lowercase().starts_with("also known as") || t.starts_with("- quoted from ");
        if t.is_empty() || !(instr || facts) || own { out.push(line.to_string()); continue; }
        if o.injected(t) { continue; }
        // a goal quoting a paste: the user's own words around it instead, or no goal line
        if (sec.starts_with("goal") || key.as_deref() == Some("goal")) && o.quotes_paste(t) {
            if let Some(w) = &o.paste_own { out.push(format!("\"{}\"", one_line(w, 240).replace('"', "'"))); }
            continue;
        }
        let Some(src) = o.source_of(t) else { out.push(line.to_string()); continue };
        if instr { moved.push(quote(src, item)) } else { out.push(quote(src, item)) }
    }
    if !moved.is_empty() {
        match out.iter().position(|l| l.trim_start().starts_with('#') && l.to_lowercase().contains("fact")) {
            Some(i) => {
                let end = out[i + 1..].iter().position(|l| l.trim_start().starts_with('#')).map_or(out.len(), |j| i + 1 + j);
                let end = (i + 1..end).rev().find(|&j| !out[j].trim().is_empty()).map_or(i + 1, |j| j + 1);
                for (k, m) in moved.into_iter().enumerate() { out.insert(end + k, m); }
            }
            None => {
                while out.last().is_some_and(|l| l.trim().is_empty()) { out.pop(); }
                if !out.is_empty() { out.push(String::new()); }
                out.push("## Facts to remember".into());
                out.extend(moved);
            }
        }
    }
    // a section left with nothing under it goes too
    let mut keep: Vec<String> = Vec::new();
    for (i, l) in out.iter().enumerate() {
        if l.trim_start().starts_with('#') && out[i + 1..].iter().take_while(|x| !x.trim_start().starts_with('#')).all(|x| x.trim().is_empty()) { continue; }
        keep.push(l.clone());
    }
    keep.join("\n").trim_end().to_string()
}

/// A state card with outside text kept out: an item (`;`-separated) of any field that repeats outside text or carries
/// an injection phrasing is dropped (a card is the next turn's instructions; it has no room for quotes).
pub fn quarantine_card(card: &str, o: &Outside) -> String {
    card.lines().filter_map(|l| {
        let Some((k, v)) = l.split_once(':') else { return (!o.injected(l) && o.source_of(l).is_none()).then(|| l.to_string()) };
        let goal = k.trim().eq_ignore_ascii_case("goal");
        let items: Vec<&str> = v.split(';').map(str::trim)
            .filter(|x| !x.is_empty() && !o.injected(x) && o.source_of(x).is_none() && !(goal && o.quotes_paste(x))).collect();
        (!items.is_empty()).then(|| format!("{k}: {}", items.join("; ")))
    }).collect::<Vec<_>>().join("\n")
}
