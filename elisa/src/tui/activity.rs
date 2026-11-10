use super::*;

pub(super) fn cap(n: &str) -> String { let mut c = n.chars(); c.next().map_or(String::new(), |f| f.to_uppercase().chain(c).collect()) }
pub(super) fn one(s: &str) -> String { s.split_whitespace().collect::<Vec<_>>().join(" ") }
/// cut to `w` columns with … (no padding)
pub(super) fn cut(c: Cells, w: usize) -> Cells { if width(&c) > w { fit(c, w, Style::new()) } else { c } }
pub(super) fn plural(n: usize, s: &str) -> String { format!("{n} {s}{}", if n == 1 { "" } else { "s" }) }

pub(crate) fn act_target(name: &str, input: &serde_json::Value, cwd: &std::path::Path) -> Option<String> {
    let sv = |k: &str| input.get(k).and_then(|v| v.as_str()).unwrap_or("");
    Some(match name {
        "read" | "write" | "edit" | "outline" => { let f = cwd.join(sv("path")); f.strip_prefix(cwd).unwrap_or(&f).display().to_string() }
        "read_symbol" | "edit_symbol" => { let f = cwd.join(sv("path")); format!("{} · {}", f.strip_prefix(cwd).unwrap_or(&f).display(), sv("name")) }
        "bash" => sv("command").into(),
        "glob" | "grep" => match sv("path") { "" | "." => sv("pattern").into(), p => format!("{} in {p}", sv("pattern")) },
        _ => return None,
    })
}

pub(super) fn act_info(name: &str, input: &serde_json::Value, cwd: &std::path::Path) -> Info {
    let sv = |k: &str| input.get(k).and_then(|v| v.as_str()).unwrap_or("");
    match name {
        "write" => Info::Write(!cwd.join(sv("path")).exists(), sv("content").lines().count()),
        "edit" => {
            let (o, n): (Vec<&str>, Vec<&str>) = (sv("old").lines().collect(), sv("new").lines().collect());
            let d = diff(&o, &n);
            let all = matches!(input.get("all"), Some(serde_json::Value::Bool(true))) || sv("all") == "true";
            Info::Edit(d.iter().filter(|x| x.0 == '+').count(), d.iter().filter(|x| x.0 == '-').count(), all)
        }
        _ => Info::None,
    }
}

/// `24 symbols` from an outline header (`path · rust · 375 lines · 24 symbols`), `7 files` for a directory
pub(crate) fn outline_note(o: &str) -> String {
    let first = o.lines().next().unwrap_or("");
    match first.rsplit(" · ").next().filter(|s| s.ends_with("symbols") || s.ends_with("symbol")) {
        Some(s) => s.to_string(),
        None => plural(o.lines().filter(|l| l.contains(" lines): ")).count(), "file"),
    }
}

/// `lines 12–30` from a read_symbol header (`path:12-30 fn Foo::bar`)
pub(crate) fn symbol_note(o: &str) -> String {
    let first = o.lines().next().unwrap_or("");
    let r = first.split_whitespace().next().and_then(|p| p.rsplit(':').next()).unwrap_or("");
    format!("lines {}", r.replacen('-', "–", 1))
}

/// the file part of a grep hit `file:line: text`
fn grep_file(l: &str) -> &str {
    for (i, _) in l.match_indices(':') {
        let r = &l[i + 1..];
        let d = r.bytes().take_while(u8::is_ascii_digit).count();
        if d > 0 && r[d..].starts_with(':') { return &l[..i]; }
    }
    l
}

/// the gray note under an action, from the tool output
pub(super) fn finish(a: &Act, o: &str, err: bool) -> End {
    let first = || crate::jsonl::first_note(o).unwrap_or_else(|| (if err { "failed" } else { "(no output)" }).to_string());
    let red = |note: String| End { note, red: true, prev: vec![], more: 0 };
    if err && o.starts_with("denied") { return red("denied".into()); }
    if a.name == "bash" {
        let mut ls: Vec<&str> = o.lines().map(str::trim_end).filter(|l| !l.trim().is_empty()).collect();
        let code = ls.last().and_then(|l| l.strip_prefix("[exit ")).and_then(|r| r.strip_suffix(']')).map(str::to_string);
        if code.is_some() { ls.pop(); }
        let bad = err || code.as_deref().is_some_and(|c| c != "0");
        let mut parts: Vec<String> = if bad { ls.last() } else { ls.first() }.map(|l| l.trim().to_string()).into_iter().collect();
        if let Some(c) = code { parts.push(format!("exit {c}")); }
        parts.push(crate::ui::dur(a.t0.elapsed().as_secs_f64(), 1));
        let prev = if ls.len() > 1 { ls.iter().take(3).map(|l| l.to_string()).collect() } else { vec![] };
        return End { note: parts.join(" · "), red: bad, prev, more: ls.len().saturating_sub(3) };
    }
    if err { return red(first()); }
    let last = o.lines().last().unwrap_or("");
    let note = match (a.name.as_str(), a.info) {
        ("outline", _) => outline_note(o),
        ("read_symbol", _) => symbol_note(o),
        ("read", _) => match last.strip_prefix("[lines ") {
            Some(r) => format!("lines {}", r.split(']').next().unwrap_or("").replacen('-', "–", 1)),
            None if o.starts_with('[') => "empty file".into(),
            None => plural(o.lines().count(), "line"),
        },
        ("write", Info::Write(new, n)) => format!("{} · {}", if new { "new file" } else { "rewrote" }, plural(n, "line")),
        ("edit", Info::Edit(_, _, true)) => {
            let n = o.rsplit('(').next().and_then(|r| r.split_whitespace().next()).and_then(|d| d.parse().ok()).unwrap_or(1);
            plural(n, "replacement")
        }
        ("edit", Info::Edit(p, m, false)) => format!("+{p} −{m} lines"),
        ("glob", _) if o == "no files found" => "0 files".into(),
        ("glob", _) => match last.strip_prefix('[').and_then(|r| r.split_whitespace().nth(2)) {
            Some(n) => format!("{n} files"),
            None => plural(o.lines().count(), "file"),
        },
        ("grep", _) if o == "no matches" => "0 matches".into(),
        ("grep", _) => {
            let hits: Vec<&str> = o.lines().filter(|l| !l.starts_with("[stopped")).collect();
            let mut files: Vec<&str> = hits.iter().map(|l| grep_file(l)).collect();
            files.sort_unstable();
            files.dedup();
            if hits.len() < o.lines().count() { format!("{}+ matches (capped) in {}", hits.len(), plural(files.len(), "file")) }
            else { format!("{} {} in {}", hits.len(), if hits.len() == 1 { "match" } else { "matches" }, plural(files.len(), "file")) }
        }
        _ => first(),
    };
    End { note, red: false, prev: vec![], more: 0 }
}

// ---------- edit panel model (built on the worker thread in tool_detail, before the tool runs) ----------
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum PL { Ctx(usize, String), Del(usize, String), Add(usize, String), Gap, Note(String) }
pub(crate) struct View { pub(crate) path: String, pub(crate) kind: &'static str, pub(crate) body: Vec<PL>, pub(super) status: Option<Result<(), String>> }
pub(crate) struct ReadV { pub(crate) path: String, pub(crate) a: usize, pub(crate) b: usize, pub(super) body: Vec<PL> }
pub(crate) enum Detail { View(View), Read(ReadV), Cmd(String) }
#[derive(PartialEq)]
pub(super) enum Await { No, View(usize), Cmd }

const MAXL: usize = 400; // chars kept per code line
const MAXN: usize = 500; // lines kept per view

fn own(l: &str) -> String { l.chars().take(MAXL).collect() }

pub(crate) fn detail(name: &str, input: &serde_json::Value, cwd: &std::path::Path) -> Option<Detail> {
    let sv = |k: &str| input.get(k).and_then(|v| v.as_str()).unwrap_or("");
    let num = |k: &str| input.get(k).and_then(|v| v.as_u64().or_else(|| v.as_str().and_then(|s| s.trim().parse().ok()))).map(|v| v as usize);
    let full = cwd.join(sv("path"));
    let path = full.strip_prefix(cwd).unwrap_or(&full).display().to_string();
    match name {
        "edit" => {
            let (old, new) = (sv("old"), sv("new"));
            let all = matches!(input.get("all"), Some(serde_json::Value::Bool(true))) || sv("all") == "true";
            let body = match std::fs::read_to_string(&full) {
                Err(e) => vec![PL::Note(format!("cannot read: {e}"))],
                Ok(t) => edit_body(&t, old, new, all),
            };
            Some(Detail::View(View { path, kind: "edit", body, status: None }))
        }
        "write" => {
            let new: Vec<&str> = sv("content").lines().collect();
            let (kind, body) = match std::fs::read_to_string(&full) {
                Ok(old) => {
                    let old: Vec<&str> = old.lines().collect();
                    ("rewrite", hunks(&diff(&old, &new), &old, &new, 1, 1, 2))
                }
                Err(_) => {
                    let mut b: Vec<PL> = new.iter().take(MAXN).enumerate().map(|(i, l)| PL::Add(i + 1, own(l))).collect();
                    if new.len() > MAXN { b.push(PL::Note(format!("… +{} lines", new.len() - MAXN))); }
                    ("new file", b)
                }
            };
            Some(Detail::View(View { path, kind, body, status: None }))
        }
        "read" => {
            let t = std::fs::read_to_string(&full).ok()?;
            let ls: Vec<&str> = t.lines().collect();
            let a = num("offset").unwrap_or(1).max(1);
            let b = (a - 1 + num("limit").filter(|&l| l > 0).unwrap_or(400)).min(ls.len());
            let body = (a..=b).take(MAXN).map(|i| PL::Ctx(i, own(ls[i - 1]))).collect();
            Some(Detail::Read(ReadV { path, a, b, body }))
        }
        "read_symbol" => {
            let ctx = crate::tools::ToolCtx { cwd: cwd.to_path_buf(), ..Default::default() };
            let (a, b) = crate::tools::symbol_lines(input, &ctx)?;
            let t = std::fs::read_to_string(&full).ok()?;
            let ls: Vec<&str> = t.lines().collect();
            let body = (a..=b.min(ls.len())).take(MAXN).map(|i| PL::Ctx(i, own(ls[i - 1]))).collect();
            Some(Detail::Read(ReadV { path, a, b, body }))
        }
        "edit_symbol" => {
            let ctx = crate::tools::ToolCtx { cwd: cwd.to_path_buf(), ..Default::default() };
            let body = match crate::tools::edit_symbol_preview(input, &ctx) {
                Some((old, new)) => {
                    let (o, n): (Vec<&str>, Vec<&str>) = (old.lines().collect(), new.lines().collect());
                    hunks(&diff(&o, &n), &o, &n, 1, 1, 3)
                }
                None => vec![PL::Note(format!("symbol `{}` not found (or ambiguous) in the file", sv("name")))],
            };
            Some(Detail::View(View { path, kind: "edit", body, status: None }))
        }
        "outline" => Some(Detail::Cmd(format!("outline {path}"))),
        "bash" => Some(Detail::Cmd(format!("$ {}", sv("command").split_whitespace().collect::<Vec<_>>().join(" ")))),
        "glob" | "grep" => Some(Detail::Cmd(format!("{name} {}", sv("pattern")))),
        _ => None,
    }
}

/// The edit as a diff with 3 context lines, computed from the file text the edit would write (the same
/// `replacen` / `replace` as `tools::edit`), so the preview is what lands on disk. Splicing `new` in as whole lines
/// showed an old text ending in a newline replaced by a new one without it as "no changes" / two separate lines,
/// while the file got the two lines joined into one.
fn edit_body(text: &str, old: &str, new: &str, all: bool) -> Vec<PL> {
    let n = if old.is_empty() { 0 } else { text.matches(old).count() };
    if n == 0 { return vec![PL::Note("old text not found in the file".into())]; }
    let after = if all { text.replace(old, new) } else { text.replacen(old, new, 1) };
    let (o, nw): (Vec<&str>, Vec<&str>) = (text.lines().collect(), after.lines().collect());
    let ops = diff(&o, &nw);
    let mut out = if ops.iter().all(|x| x.0 == ' ') && after != text {
        vec![PL::Note("only line endings change (a newline added or removed at the end of the file)".into())]
    } else {
        hunks(&ops, &o, &nw, 1, 1, 3)
    };
    if n > 1 { out.push(PL::Note(format!("{n} matches{}", if all { ", all replaced" } else { " (not unique: the edit will fail)" }))); }
    out
}

/// line diff: (op, old idx, new idx); op ' ' / '-' / '+'. Prefix/suffix trim, LCS on the middle (bounded).
fn diff(a: &[&str], b: &[&str]) -> Vec<(char, usize, usize)> {
    let p = a.iter().zip(b).take_while(|(x, y)| x == y).count();
    let s = a[p..].iter().rev().zip(b[p..].iter().rev()).take_while(|(x, y)| x == y).count();
    let (am, bm) = (&a[p..a.len() - s], &b[p..b.len() - s]);
    let mut ops: Vec<(char, usize, usize)> = (0..p).map(|i| (' ', i, i)).collect();
    let (n, m) = (am.len(), bm.len());
    if n * m > 2_000_000 {
        ops.extend((0..n).map(|i| ('-', p + i, p)));
        ops.extend((0..m).map(|j| ('+', p + n, p + j)));
    } else {
        let mut dp = vec![0u32; (n + 1) * (m + 1)];
        let ix = |i: usize, j: usize| i * (m + 1) + j;
        for i in (0..n).rev() {
            for j in (0..m).rev() {
                dp[ix(i, j)] = if am[i] == bm[j] { dp[ix(i + 1, j + 1)] + 1 } else { dp[ix(i + 1, j)].max(dp[ix(i, j + 1)]) };
            }
        }
        let (mut i, mut j) = (0, 0);
        while i < n || j < m {
            if i < n && j < m && am[i] == bm[j] { ops.push((' ', p + i, p + j)); i += 1; j += 1; }
            else if j < m && (i == n || dp[ix(i, j + 1)] >= dp[ix(i + 1, j)]) { ops.push(('+', p + i, p + j)); j += 1; }
            else { ops.push(('-', p + i, p + j)); i += 1; }
        }
    }
    let (ea, eb) = (a.len() - s, b.len() - s);
    ops.extend((0..s).map(|k| (' ', ea + k, eb + k)));
    ops
}

/// changed hunks with `c` context lines; line numbers start at `ao` / `bo`
fn hunks(ops: &[(char, usize, usize)], a: &[&str], b: &[&str], ao: usize, bo: usize, c: usize) -> Vec<PL> {
    let ch: Vec<usize> = ops.iter().enumerate().filter(|o| o.1 .0 != ' ').map(|o| o.0).collect();
    if ch.is_empty() { return vec![PL::Note("no changes".into())]; }
    let (mut out, mut gap, mut k) = (vec![], false, 0);
    for (i, &(op, x, y)) in ops.iter().enumerate() {
        while k < ch.len() && ch[k] + c < i { k += 1; }
        let near = k < ch.len() && ch[k] <= i + c;
        if !near { if !gap && !out.is_empty() { out.push(PL::Gap); } gap = true; continue; }
        gap = false;
        if out.len() >= MAXN { out.push(PL::Note("… (diff cut)".into())); break; }
        out.push(match op { ' ' => PL::Ctx(bo + y, own(b[y])), '-' => PL::Del(ao + x, own(a[x])), _ => PL::Add(bo + y, own(b[y])) });
    }
    out
}

pub(super) fn pl_line(p: &PL, nw: usize, w: usize) -> Line<'static> {
    let (no, sign, text, col) = match p {
        PL::Ctx(n, t) => (n.to_string(), ' ', t.as_str(), DIM),
        PL::Del(n, t) => (n.to_string(), '-', t.as_str(), RED),
        PL::Add(n, t) => (n.to_string(), '+', t.as_str(), GREEN),
        PL::Gap => (String::new(), '⋯', "", DIM),
        PL::Note(t) => return group(fit(cells(t, st(DIM).italic()), w, Style::new())),
    };
    let mut c = cells(&format!("{no:>nw$} "), st(GUTTER));
    c.extend(cells(&format!("{sign} "), st(col)));
    c.extend(cells(text, st(if matches!(p, PL::Ctx(..)) { CTX } else { col })));
    group(fit(c, w, Style::new()))
}

#[cfg(test)]
mod edit_preview_tests {
    use super::*;

    /// Old ends in a newline, new does not: the file gets two lines joined, and the preview must say so
    #[test]
    fn preview_shows_joined_lines() {
        let text = "def t():\n    a = 1\n    assert a\n    b = 2\n";
        let body = edit_body(text, "    assert a\n", "    assert a", false);
        assert!(body.contains(&PL::Add(3, "    assert a    b = 2".into())), "{body:?}");
        assert!(body.contains(&PL::Del(3, "    assert a".into())) && body.contains(&PL::Del(4, "    b = 2".into())), "{body:?}");
    }

    #[test]
    fn preview_matches_written_text() {
        let text = "a\nb\nc\n";
        let body = edit_body(text, "b", "b1\nb2", false);
        assert!(body.contains(&PL::Add(2, "b1".into())) && body.contains(&PL::Add(3, "b2".into())), "{body:?}");
        let tail = edit_body("a\nb\n", "b\n", "b", false);
        assert!(matches!(&tail[..], [PL::Note(t)] if t.contains("line endings")), "{tail:?}");
        let two = edit_body("x\ny\nx\n", "x", "z", true);
        assert!(two.contains(&PL::Add(1, "z".into())) && two.contains(&PL::Add(3, "z".into())), "{two:?}");
    }
}
