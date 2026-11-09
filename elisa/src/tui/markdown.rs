use super::*;

pub(super) type Cells = Vec<(char, Style)>;

fn sp(n: usize) -> Cells { vec![(' ', Style::new()); n] }
/// the reply marker: a short accent bar on the first line of each reply (the kit puts a tiny Shade there)
fn lead(first: &mut bool) -> Cells { if std::mem::take(first) { cells("▍ ", st(ACCENT)) } else { cells("  ", st(FG)) } }
pub(super) fn width(c: &[(char, Style)]) -> usize { c.iter().map(|&(c, _)| cw(c)).sum() }

/// cut to `w` columns (… when cut), then pad with `pad` style
pub(super) fn fit(mut c: Cells, w: usize, pad: Style) -> Cells {
    if width(&c) > w {
        let mut n = 0;
        let keep = c.iter().take_while(|&&(ch, _)| { n += cw(ch); n < w }).count();
        let s = c.get(keep).map_or(pad, |x| x.1);
        c.truncate(keep);
        c.push(('…', s));
    }
    let n = width(&c);
    c.extend(std::iter::repeat_n((' ', pad), w.saturating_sub(n)));
    c
}

/// Markdown for a (possibly still streaming) reply: headings, lists, quotes, rules, pipe tables, fenced code.
pub(super) fn md(text: &str, w: usize, out: &mut Vec<Line<'static>>) {
    let lines: Vec<&str> = text.trim_matches('\n').lines().collect();
    let (mut first, mut i) = (true, 0);
    while i < lines.len() {
        let l = lines[i];
        let t = l.trim_start();
        if let Some(lang) = t.strip_prefix("```") {
            let mut j = i + 1;
            while j < lines.len() && !lines[j].trim_start().starts_with("```") { j += 1; }
            code_block(lang.trim(), &lines[i + 1..j], w, &mut first, out);
            i = j + 1;
            continue;
        }
        if t.starts_with('|') {
            let j = i + lines[i..].iter().take_while(|l| l.trim_start().starts_with('|')).count();
            table(&lines[i..j], w, &mut first, out);
            i = j;
            continue;
        }
        i += 1;
        if t.is_empty() { out.push(Line::default()); continue; }
        let mut c = lead(&mut first);
        let hashes = t.chars().take_while(|&c| c == '#').count();
        if is_hr(t) {
            c.extend(cells(&"─".repeat(w.saturating_sub(2)), st(DIM)));
            out.push(group(c));
        } else if (1..=6).contains(&hashes) && t[hashes..].starts_with(' ') {
            let s = match hashes { 1 => st(ACCENT).bold().underlined(), 2 => st(ACCENT).bold(), _ => st(H3).bold() };
            c.extend(inline(t[hashes..].trim(), s));
            wrap_p(c, w, sp(2), out);
        } else if let Some(q) = t.strip_prefix('>') {
            c.extend(cells("▌ ", st(DIM)));
            c.extend(inline(q.trim_start(), st(FG).italic()));
            let mut cont = sp(2);
            cont.extend(cells("▌ ", st(DIM)));
            wrap_p(c, w, cont, out);
        } else if let Some((m, rest)) = list_item(t, l.len() - t.len()) {
            let nest = ((l.len() - t.len()) / 2 * 2).min(8);
            c.extend(sp(nest));
            c.extend(cells(&m, st(ACCENT)));
            c.extend(inline(rest, st(FG)));
            wrap_p(c, w, sp(2 + nest + m.chars().count()), out);
        } else {
            c.extend(inline(t, st(FG)));
            wrap_p(c, w, sp(2), out);
        }
    }
}

fn is_hr(t: &str) -> bool {
    let s: String = t.chars().filter(|c| !c.is_whitespace()).collect();
    s.len() >= 3 && ["-", "*", "_"].iter().any(|m| s.chars().all(|c| c.to_string() == *m))
}

/// "- x" / "* x" / "+ x" / "12. x" / "3) x" → (marker, rest)
fn list_item(t: &str, ind: usize) -> Option<(String, &str)> {
    for p in ["- ", "* ", "+ "] {
        if let Some(r) = t.strip_prefix(p) { return Some((if ind >= 2 { "◦ " } else { "• " }.into(), r)); }
    }
    let d = t.chars().take_while(|c| c.is_ascii_digit()).count();
    let rest = &t[d..];
    if d > 0 && d < 4 && (rest.starts_with(". ") || rest.starts_with(") ")) { return Some((format!("{}. ", &t[..d]), &rest[2..])); }
    None
}

fn code_block(lang: &str, body: &[&str], w: usize, first: &mut bool, out: &mut Vec<Line<'static>>) {
    let bw = w.saturating_sub(2).max(8);
    let bg = Style::new().bg(CODE_BG);
    let mut c = lead(first);
    c.extend(fit(cells(&format!(" {}", if lang.is_empty() { "code" } else { lang }), st(DIM).bg(CODE_BG).italic()), bw, bg));
    out.push(group(c));
    for l in body {
        let mut c = lead(first);
        c.push((' ', bg));
        c.extend(fit(cells(l, st(CODE).bg(CODE_BG)), bw - 1, bg));
        out.push(group(c));
    }
    let mut c = lead(first);
    c.extend(fit(vec![], bw, bg));
    out.push(group(c));
}

fn table(rows: &[&str], w: usize, first: &mut bool, out: &mut Vec<Line<'static>>) {
    let split = |r: &str| -> Vec<String> {
        let r = r.trim();
        let r = r.strip_prefix('|').unwrap_or(r);
        let r = r.strip_suffix('|').unwrap_or(r);
        r.split('|').map(|c| c.trim().to_string()).collect()
    };
    let is_sep = |r: &[String]| r.iter().all(|c| !c.is_empty() && c.chars().all(|ch| matches!(ch, '-' | ':' | ' ')));
    let rows: Vec<Vec<String>> = rows.iter().map(|r| split(r)).collect();
    let header = rows.len() > 1 && is_sep(&rows[1]);
    let body: Vec<(bool, Vec<Cells>)> = rows.iter().enumerate().filter(|(i, r)| !(header && *i == 1) && !is_sep(r))
        .map(|(i, r)| {
            let s = if header && i == 0 { st(TITLE).bold() } else { st(FG) };
            (header && i == 0, r.iter().map(|c| inline(c, s)).collect())
        }).collect();
    let n = body.iter().map(|r| r.1.len()).max().unwrap_or(0);
    if n == 0 { return; }
    let mut cw_: Vec<usize> = (0..n).map(|j| body.iter().map(|r| r.1.get(j).map_or(0, |c| width(c))).max().unwrap_or(0).max(1)).collect();
    let avail = w.saturating_sub(2 + 3 * (n - 1)).max(n * 3);
    while cw_.iter().sum::<usize>() > avail {
        let (j, &m) = cw_.iter().enumerate().max_by_key(|x| x.1).unwrap();
        if m <= 3 { break; }
        cw_[j] -= 1;
    }
    for (is_head, r) in body {
        let mut c = lead(first);
        for (j, &cwj) in cw_.iter().enumerate() {
            if j > 0 { c.extend(cells(" │ ", st(DIM))); }
            c.extend(fit(r.get(j).cloned().unwrap_or_default(), cwj, Style::new()));
        }
        out.push(group(c));
        if is_head {
            let mut c = lead(first);
            let s: Vec<String> = cw_.iter().map(|&x| "─".repeat(x)).collect();
            c.extend(cells(&s.join("─┼─"), st(DIM)));
            out.push(group(c));
        }
    }
}

/// inline markdown: **bold**, *italic*, `code`, [link](url)
pub(super) fn inline(l: &str, base: Style) -> Cells {
    let ch: Vec<char> = l.chars().collect();
    let (mut bold, mut ital, mut code, mut out, mut i) = (false, false, false, vec![], 0);
    while i < ch.len() {
        let c = ch[i];
        if c == '`' { code = !code; i += 1; continue; }
        if !code {
            if c == '*' && ch.get(i + 1) == Some(&'*') { bold = !bold; i += 2; continue; }
            if c == '*' && (ital || (ch.get(i + 1).is_some_and(|n| !n.is_whitespace()) && ch[i + 1..].contains(&'*'))) {
                ital = !ital;
                i += 1;
                continue;
            }
            if c == '[' {
                let rest: String = ch[i + 1..].iter().collect();
                if let Some(p) = rest.find("](") {
                    if let Some(q) = rest[p + 2..].find(')') {
                        let text = &rest[..p];
                        out.extend(text.chars().map(|c| (c, base.fg(CODE).underlined())));
                        i += 1 + rest[..p + 2 + q + 1].chars().count();
                        continue;
                    }
                }
            }
        }
        let s = if code { st(CODE).bg(CODE_BG) } else {
            let mut s = base;
            if bold { s = s.bold(); }
            if ital { s = s.italic(); }
            s
        };
        out.push(if c == '\t' { (' ', s) } else { (c, s) });
        i += 1;
    }
    out
}

pub(super) fn cells(s: &str, style: Style) -> Vec<(char, Style)> {
    s.chars().flat_map(|c| if c == '\t' { vec![(' ', style); 4] } else { vec![(c, style)] }).collect()
}

fn cw(c: char) -> usize { let mut b = [0u8; 4]; Span::raw(&*c.encode_utf8(&mut b)).width() }

pub(super) fn wrap(c: Cells, w: usize, indent: usize, out: &mut Vec<Line<'static>>) { wrap_p(c, w, sp(indent), out) }

/// word-wrap styled cells to `w` columns; continuation rows start with `cont`
fn wrap_p(cells: Cells, w: usize, cont: Cells, out: &mut Vec<Line<'static>>) {
    let ci = cont.len();
    let w = w.max(width(&cont) + 4);
    let mut row: Cells = vec![];
    let mut col = 0;
    for (c, s) in cells {
        let x = cw(c);
        if col + x > w && !row.is_empty() {
            let sp_ = row.iter().rposition(|&(c, _)| c == ' ').filter(|&p| p > ci);
            let rest = match sp_ { Some(p) => row.split_off(p + 1), None => vec![] };
            while row.last().is_some_and(|&(c, _)| c == ' ') && row.len() > 1 { row.pop(); }
            out.push(group(std::mem::take(&mut row)));
            row = cont.clone();
            row.extend(rest);
            col = width(&row);
        }
        row.push((c, s));
        col += x;
    }
    out.push(group(row));
}

pub(super) fn group(cells: Vec<(char, Style)>) -> Line<'static> {
    let mut spans: Vec<Span<'static>> = vec![];
    let mut cur = String::new();
    let mut cs = Style::new();
    for (c, s) in cells {
        if s != cs && !cur.is_empty() { spans.push(Span::styled(std::mem::take(&mut cur), cs)); }
        cs = s;
        cur.push(c);
    }
    if !cur.is_empty() { spans.push(Span::styled(cur, cs)); }
    Line::from(spans)
}
