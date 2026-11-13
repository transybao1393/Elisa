use super::*;

pub(super) const SPLIT: f32 = 0.70;
const SPIN: &[char] = &['⠋', '⠙', '⠹', '⠸', '⠼', '⠴', '⠦', '⠧', '⠇', '⠏'];
/// spinner frame time (the kit: 90 ms)
const SPIN_MS: u128 = 90;
impl App {

    // ---------- drawing ----------
    pub(super) fn draw(&mut self, f: &mut Frame) {
        let area = f.area();
        f.render_widget(Fill(BG), area);
        let now = self.now();
        // the gateway's phase for the running model call (queue, prefill, reasoning, a tool call being written)
        if let Some(c) = &mut self.shade { c.gateway(now, crate::api::phase(), crate::api::call_target); }
        // with a companion window linked, the view is mirrored to it and its care clicks come back (shade::link)
        self.view = self.shade.as_mut().map(|c| match &mut self.link { Some(l) => l.pump(c, now, crate::brand::light()), None => c.view(now) });
        self.dock = None;
        self.tray.clear();
        self.face = None;
        let iw = area.width.saturating_sub(4).max(4) as usize;
        let (in_lines, cpos) = self.input_lines(iw);
        let ql = self.queue_lines(area.width as usize);
        let qh = (ql.len() as u16).min(area.height.saturating_sub(6));
        self.jobs.retain(|j| j.p.done.is_none() || j.at.elapsed() < DONE_KEEP);
        if self.rss_at.is_none_or(|t| t.elapsed() >= Duration::from_secs(1)) { self.rss = own_rss(); self.rss_at = Some(Instant::now()); }
        let ph = !self.jobs.is_empty() as u16;
        let wrows = self.watch.as_ref().map_or(vec![], |v| watch_rows(v, area.width as usize));
        let wh = wrows.len() as u16;
        let ah = self.audit.is_some() as u16;
        let stats = self.stats_line();
        let xh = stats.is_some() as u16;
        // the status area: Shade's ears-and-eyes face with a row of padding above and below on a tall enough terminal
        // (kit v2 layout.json statusBar "face"), else one text line under a blank row
        let face = self.face_size(area.height);
        let sb_h = face.map_or(1, |(_, h)| h as u16 + 2);
        let gap = (face.is_none() && area.height >= 12) as u16;
        // the permission box takes the input box's rows (no input field while it asks)
        let asking = matches!(self.pending, Some(Pending::Approve(..)));
        let in_h = if asking { 0 } else { (in_lines.len() as u16 + 2).min(area.height.saturating_sub(3 + sb_h + gap + xh + qh + ph + wh + ah).max(3)) };
        // under the input box: context bar, then speed / Σ (both right-aligned), watcher, audit, background jobs
        let [tr, _gap, qa, sb, inp, cl, xl, wl, al, pa] = Layout::vertical([Constraint::Min(1), Constraint::Length(gap),
            Constraint::Length(qh), Constraint::Length(sb_h), Constraint::Length(in_h), Constraint::Length(1), Constraint::Length(xh),
            Constraint::Length(wh), Constraint::Length(ah), Constraint::Length(ph)]).areas(area);
        f.render_widget(Paragraph::new(self.ctx_line()), cl);
        if let Some(l) = stats { f.render_widget(Paragraph::new(l), xl); }
        f.render_widget(Paragraph::new(wrows), wl);
        if let Some(t) = &self.audit { f.render_widget(Paragraph::new(audit_cells_line(t, al.width as usize)), al); }
        if let Some(l) = self.progress_line(pa.width as usize) { f.render_widget(Paragraph::new(l), pa); }
        f.render_widget(Paragraph::new(ql), qa);

        // chat | divider | edit panel
        self.term_w = area.width;
        self.main_r = tr;
        let cw = ((tr.width as f32 * self.split).round() as u16).min(tr.width.saturating_sub(12));
        let (tr, pr) = if self.panel_on() && cw >= 20 {
            let [c, d, p] = Layout::horizontal([Constraint::Length(cw), Constraint::Length(1), Constraint::Min(1)]).areas(tr);
            let bar = vec![Line::styled("│", st(SEP)); d.height as usize];
            f.render_widget(Paragraph::new(bar), d);
            (c, Some(p.inner(Margin { horizontal: 1, vertical: 0 })))
        } else { (tr, None) };
        if let Some(p) = pr { self.draw_panel(f, p); }

        // approval box at the bottom of the chat column
        let ask = match &self.pending { Some(Pending::Approve(t, s, i, _)) => Some((t.clone(), s.clone(), *i)), _ => None };
        let tr = match ask {
            Some((t, s, i)) => {
                let w = tr.width.saturating_sub(4) as usize;
                let (lines, top) = self.approve_lines(&t, &s, i, w, tr.height.saturating_sub(3) as usize);
                self.ascroll = self.ascroll.min(top);
                let h = (lines.len() as u16 + 2).min(tr.height.saturating_sub(1));
                let [c, b] = Layout::vertical([Constraint::Min(1), Constraint::Length(h)]).areas(tr);
                let b = b.inner(Margin { horizontal: 1, vertical: 0 });
                f.render_widget(Fill(BG2), b);
                f.render_widget(Paragraph::new(lines).block(Block::bordered().border_type(BorderType::Rounded)
                    .border_style(st(YELLOW))), b);
                c
            }
            _ => tr,
        };

        // the companion: bottom-right of the chat column, its care tray on the left; the transcript never runs under it
        let tr = match self.dock_size(tr) {
            Some((w, h)) => {
                let dw = w as u16 + 3 + TRAY_W;
                let [t, d] = Layout::horizontal([Constraint::Min(1), Constraint::Length(dw)]).areas(tr);
                let d = Rect { y: d.bottom() - (h as u16 + 3), height: h as u16 + 3, ..d };
                self.draw_dock(f, d, w, h);
                t
            }
            None => tr,
        };
        // transcript
        let tr = tr.inner(Margin { horizontal: 1, vertical: 0 });
        let lines = self.transcript(tr.width as usize, tr.height as usize);
        f.render_widget(Paragraph::new(lines), tr);

        // input box (none while the permission box asks)
        let blk = Block::new().borders(Borders::TOP | Borders::BOTTOM).border_style(st(ACCENT)).style(Style::new().bg(BG2));
        let body = blk.inner(inp);
        if !asking { f.render_widget(blk, inp); }
        match &self.pending {
            Some(Pending::Input(_)) | None if self.summarizing() => {
                f.render_widget(Paragraph::new(Span::styled("summarizing earlier turns… input paused (esc interrupts)", st(DIM))), body);
            }
            Some(Pending::Input(_)) | None => {
                f.render_widget(Paragraph::new(in_lines), body);
                let y = body.y + (cpos.1 as u16).min(body.height.saturating_sub(1));
                f.set_cursor_position((body.x + cpos.0 as u16, y));
            }
            Some(Pending::Approve(..)) => {}
            Some(Pending::Pick(..)) => {
                f.render_widget(Paragraph::new(Span::styled("pick a session · ↑↓ enter · esc cancels", st(DIM))), body);
            }
        }

        // status bar above the input box: the face on the left, the text centred beside it
        match face {
            Some((w, h)) => {
                let fr = Rect { x: sb.x + 1, y: sb.y + 1, width: w as u16, height: h as u16 };
                self.draw_face(f, fr);
                let tx = fr.right() + 2;
                let tl = Rect { x: tx, y: fr.y + h as u16 / 2, width: sb.right().saturating_sub(tx), height: 1 };
                f.render_widget(Paragraph::new(self.status_line(tl.width as usize)), tl);
            }
            None => f.render_widget(Paragraph::new(self.status_line(sb.width as usize)), sb),
        }

        // popups
        let menu = self.menu();
        if !menu.is_empty() {
            let inp = sb;
            let sel = self.menu_sel.min(menu.len() - 1);
            let h = (menu.len() as u16 + 2).min(inp.y);
            let w = area.width.min(64);
            let r = Rect { x: inp.x, y: inp.y - h, width: w, height: h };
            let items: Vec<ListItem> = menu.iter().map(|(n, d)| ListItem::new(Line::from(vec![
                Span::styled(format!("{n:<10}"), st(TITLE).bold()), Span::styled(*d, st(DIM)),
            ]))).collect();
            let mut ls = ListState::default().with_selected(Some(sel));
            f.render_widget(Fill(BG2), r);
            f.render_stateful_widget(List::new(items).block(popup_block("commands")).highlight_style(Style::new().bg(SEL_BG)), r, &mut ls);
        }
        if let Some(Pending::Pick(l, i, _)) = &self.pending {
            let w = area.width.saturating_sub(4).min(100);
            let h = (l.len() as u16 + 2).min(area.height.saturating_sub(4)).max(3);
            let r = Rect { x: (area.width - w) / 2, y: area.height.saturating_sub(h) / 2, width: w, height: h };
            let items: Vec<ListItem> = l.iter().map(|s| ListItem::new(Line::from(vec![
                Span::styled(fmt_ts(s.ts), st(CODE)), Span::styled(format!("  {:>4} msgs  ", s.n_msgs), st(DIM)),
                Span::styled(s.title.clone(), st(FG)),
            ]))).collect();
            let mut ls = ListState::default().with_selected(Some(*i));
            f.render_widget(Fill(BG2), r);
            f.render_stateful_widget(List::new(items).block(popup_block("resume a session"))
                .highlight_style(Style::new().bg(SEL_BG)).highlight_symbol("› "), r, &mut ls);
        }
    }

    /// the rects drawn for the theme already (`brand::finish` leaves them alone): Shade's dock art and the face
    pub(super) fn keep(&self) -> Vec<Rect> { self.dock.into_iter().chain(self.face).collect() }

    /// the status-bar face size (cells) for a terminal height: 10 x 5 on tall terminals, 8 x 4 on medium ones (the
    /// smallest that reads: below it the eyes are grey mush), else None = the one-line text status. Off with the
    /// character (--no-character / character = false).
    fn face_size(&self, rows: u16) -> Option<(usize, usize)> {
        self.shade.as_ref()?;
        [(10usize, 5usize, FACE_TALL), (8, 4, FACE_MIN)].into_iter().find(|&(_, _, r)| rows >= r).map(|(w, h, _)| (w, h))
    }

    /// the status-bar face (kit v2 `faceSvg`): ears and eyes, animated with Shade's state, on the theme background
    fn draw_face(&mut self, f: &mut Frame, r: Rect) {
        let Some(v) = &self.view else { return };
        let bg = crate::brand::pal().bg;
        let art = crate::shade::render_face(v.st, v.ms, r.width as usize, r.height as usize, bg, crate::shade::FACE_VIEW);
        let rgb = |c: (u8, u8, u8)| Color::Rgb(c.0, c.1, c.2);
        let buf = f.buffer_mut();
        for (i, c) in art.cells.iter().enumerate() {
            let cell = &mut buf[(r.x + (i % art.w) as u16, r.y + (i / art.w) as u16)];
            cell.set_char(c.ch);
            cell.set_fg(c.fg.map_or(Color::Reset, rgb));
            cell.set_bg(rgb(c.bg.unwrap_or(bg)));
        }
        self.face = Some(r);
    }

    /// the companion's art size (cells) for a chat column: large / medium / small (mini build), or None when the
    /// character is off or the column is too small (the transcript and the input always win)
    fn dock_size(&self, r: Rect) -> Option<(usize, usize)> {
        self.shade.as_ref()?;
        // the companion window shows Shade instead (the status line stays)
        if self.link.as_ref().is_some_and(|l| l.connected()) { return None; }
        let fits = |w: u16, h: u16| r.width >= w && r.height >= h + 3 + 4;
        [(32usize, 19usize, 128u16), (24, 15, 84), (16, 9, 56)].into_iter().find(|&(_, h, cw)| fits(cw, h as u16)).map(|(w, h, _)| (w, h))
    }

    /// bubble (3 rows: VERB target, tail ▽) over Shade's art, right-aligned in `r`
    fn draw_dock(&mut self, f: &mut Frame, r: Rect, w: usize, h: usize) {
        let Some(v) = self.view.clone() else { return };
        let pal = crate::brand::pal();
        // a long tool's fidget: the listening pose for a moment (the bubble stays the tool's)
        let pose = if v.fidget { St::Listening } else { v.st };
        let key: ArtKey = (pose, crate::shade::frame_of(pose, v.ms), w, h, (v.pop * 100.0).round() as i32, crate::brand::light());
        if self.art.as_ref().is_none_or(|(k, _)| *k != key) {
            let look = Look { bg: pal.bg, accent: pal.accent, warn: pal.warn, error: pal.error, accent_a: pal.glow as f32 / 100.0 };
            self.art = Some((key, crate::shade::render(pose, v.ms, w, h, v.pop, &look)));
        }
        let Some((_, art)) = &self.art else { return };
        let ax = r.right().saturating_sub(w as u16 + 1);
        let ay = r.bottom() - h as u16;
        let rgb = |c: (u8, u8, u8)| Color::Rgb(c.0, c.1, c.2);
        let buf = f.buffer_mut();
        for (i, c) in art.cells.iter().enumerate() {
            let (x, y) = (ax + (i % w) as u16, ay + (i / w) as u16);
            let cell = &mut buf[(x, y)];
            cell.set_char(c.ch);
            cell.set_fg(c.fg.map_or(Color::Reset, rgb));
            cell.set_bg(rgb(c.bg.unwrap_or(pal.bg)));
        }
        self.dock = Some(Rect { x: ax, y: ay, width: w as u16, height: h as u16 });
        // the care tray (layout.json `careTray`): water, food, toy stacked left of Shade, bottom-aligned, a row apart,
        // one row of padding below; the button of the running care state is lit (the kit's hover/active accent)
        let tx = ax.saturating_sub(TRAY_W);
        for (i, (glyph, cs)) in [(crate::brand::icon::WATER, St::Drinking), (crate::brand::icon::FOOD, St::Eating), (crate::brand::icon::TOY, St::Playing)].into_iter().enumerate() {
            let y = ay + h as u16 - 2 - 2 * (2 - i as u16);
            let b = Rect { x: tx, y, width: TRAY_W - 1, height: 1 };
            let sty = if v.st == cs { st(ACCENT).bg(SEL_BG).bold() } else { st(DIM).bg(BG2) };
            f.render_widget(Paragraph::new(Line::styled(format!(" {glyph} "), sty)), b);
            self.tray.push((b, cs));
        }
        let Some(b) = v.bubble else { return };
        let maxw = r.width.saturating_sub(1) as usize;
        let tone = match b.tone { crate::shade::Tone::Error => RED, crate::shade::Tone::Warn => YELLOW, _ => ACCENT };
        let mut body = cells(b.verb, st(tone).bold().bg(BG2));
        if !b.target.is_empty() { body.extend(cells(&format!(" {}", b.target), st(DIM).bg(BG2))); }
        let body = cut(body, maxw.saturating_sub(4));
        let bw = width(&body) + 4;
        let border = st(if b.big { ACCENT } else { SEP });
        // right edge 18% of the art in from its right side (the kit: marginRight size*.18), kept inside the dock
        let right = (ax as usize + w).saturating_sub(w * 18 / 100).max(r.x as usize + bw).min(r.right() as usize);
        let bx = (right - bw) as u16;
        let by = ay.saturating_sub(3);
        let tail = (bw - 2).saturating_sub((w * 14 / 100).max(1)).max(1);
        let top = format!("╭{}╮", "─".repeat(bw - 2));
        let bot: String = (0..bw).map(|i| if i == 0 { '╰' } else if i == bw - 1 { '╯' } else if i == tail { '▽' } else { '─' }).collect();
        let mut mid = vec![('│', border), (' ', Style::new().bg(BG2))];
        mid.extend(body);
        mid.extend([(' ', Style::new().bg(BG2)), ('│', border)]);
        let lines = vec![Line::styled(top, border), group(mid), Line::styled(bot, border)];
        f.render_widget(Paragraph::new(lines), Rect { x: bx, y: by, width: bw as u16, height: 3 });
    }

    fn draw_panel(&mut self, f: &mut Frame, r: Rect) {
        let w = r.width as usize;
        let one = |s: String, sty: Style| group(fit(cells(&s, sty), w, Style::new()));
        let mut top: Vec<Line> = vec![];
        if let Some((c, ok)) = &self.last_cmd {
            let mark = match ok { Some(true) => ("✓ ", GREEN), Some(false) => ("✗ ", RED), None => ("› ", ACCENT) };
            let mut cl = cells(mark.0, st(mark.1));
            cl.extend(cells(c, st(DIM)));
            top.push(group(fit(cl, w, Style::new())));
        }
        let vi = if self.views.is_empty() { None } else { Some(self.sel.unwrap_or(self.views.len() - 1)) };
        let empty = vec![PL::Note(if self.last_read.is_some() { "" } else { "edits will show here" }.into())];
        let body: &[PL] = match (vi, &self.last_read) {
            (Some(i), rd) => {
                if let Some(rd) = rd { top.push(one(format!("read {} L{}–{}", rd.path, rd.a, rd.b), st(DIM))); }
                let v = &self.views[i];
                top.push(one(v.path.clone(), st(TITLE).bold()));
                let mut h = cells(&format!("{} {}/{}", v.kind, i + 1, self.views.len()), st(ACCENT));
                match &v.status {
                    Some(Ok(())) => h.extend(cells("  ✓ applied", st(GREEN))),
                    Some(Err(e)) => h.extend(cells(&format!("  ✗ {e}"), st(RED))),
                    None => h.extend(cells("  …", st(DIM))),
                }
                top.push(group(fit(h, w, Style::new())));
                &v.body
            }
            (None, Some(rd)) => {
                top.push(one(rd.path.clone(), st(TITLE).bold()));
                top.push(one(format!("read L{}–{}", rd.a, rd.b), st(DIM)));
                &rd.body
            }
            (None, None) => { top.push(one("edits".into(), st(TITLE).bold())); &empty }
        };
        top.push(Line::styled("─".repeat(w), st(SEP)));
        let th = (top.len() as u16).min(r.height);
        f.render_widget(Paragraph::new(top), Rect { height: th, ..r });
        let h = r.height.saturating_sub(th) as usize;
        self.panel_h = h;
        self.pscroll = self.pscroll.min(body.len().saturating_sub(h));
        let nw = body.iter().map(|p| match p { PL::Ctx(n, _) | PL::Del(n, _) | PL::Add(n, _) => n.to_string().len(), _ => 0 }).max().unwrap_or(0).max(3);
        let lines: Vec<Line> = body.iter().skip(self.pscroll).take(h).map(|p| pl_line(p, nw, w)).collect();
        f.render_widget(Paragraph::new(lines), Rect { y: r.y + th, height: h as u16, ..r });
    }

    /// approval box body: title, optional Write/Edit detail, the choices (`approve_choices`, `sel` highlighted)
    /// The approval box's lines in at most `max_h` rows, and the furthest its whole command can scroll: the question,
    /// the call's whole command or code (`tools::FULL_LINE` lines, wrapped, never cut; PgUp / PgDn move through what
    /// does not fit), its notes, the choices.
    fn approve_lines(&self, tool: &str, summary: &str, sel: usize, w: usize, max_h: usize) -> (Vec<Line<'static>>, usize) {
        let choices = approve_choices(tool);
        let choice_lines = |out: &mut Vec<Line<'static>>| for (i, (l, _)) in choices.iter().enumerate() {
            let on = i == sel;
            let sty = if on { st(FG).bg(SEL_BG).bold() } else { st(FG) };
            let mut c = cells(if on { "› " } else { "  " }, if on { st(ACCENT).bg(SEL_BG).bold() } else { sty });
            c.extend(cells(&format!("{}. {l}", i + 1), sty));
            out.push(group(fit(c, w, if on { Style::new().bg(SEL_BG) } else { Style::new() })));
        };
        if tool.starts_with(QUESTION) {
            let mut out: Vec<Line<'static>> = summary.lines().map(|l| group(cut(cells(l, st(TITLE).bold()), w))).collect();
            choice_lines(&mut out);
            return (out, 0);
        }
        let tool = tool.split_once(RULE).map_or(tool, |(t, _)| t);
        let is_out = tool.starts_with(OUTSIDE);
        let (summary, where_) = summary.split_once('\n').unwrap_or((summary, ""));
        let act = self.entries.iter().rev().find_map(|i| match &i.e { Entry::Tool(a) => Some(&**a), _ => None });
        let (verb, target) = act.map_or_else(|| { let (n, t) = summary.split_once(' ').unwrap_or((summary, "")); (cap(n), one(t)) },
            |a| (a.verb.clone(), a.target.clone()));
        let mut c = cells("Allow ", st(TITLE).bold());
        c.extend(cells(&verb, st(TITLE).bold()));
        if !target.is_empty() {
            c.push((' ', st(FG)));
            let tw = w.saturating_sub(width(&c) + 1);
            c.extend(cut(cells(&target, st(CODE)), tw));
        }
        c.push(('?', st(TITLE).bold()));
        let mut out = vec![group(c)];
        let det = match act.map(|a| (a.name.as_str(), a.info)) {
            Some(("write", Info::Write(new, n))) => Some(format!("{} · {}", if new { "new file" } else { "overwrite" }, plural(n, "line"))),
            Some(("edit", Info::Edit(p, m, all))) => Some(format!("+{p} −{m} lines{}", if all { " · all matches" } else { "" })),
            _ => None,
        };
        if let Some(d) = det { out.push(group(cut(cells(&d, st(DIM)), w))); }
        // the whole command or code, wrapped to the box (every character shown, none cut)
        let mark = crate::tools::FULL_LINE;
        let tw = w.saturating_sub(mark.chars().count()).max(1);
        let rows: Vec<Line<'static>> = where_.lines().filter_map(|l| l.strip_prefix(mark)).flat_map(|l| {
            let c = cells(l, st(CODE));
            let parts: Vec<Cells> = if c.is_empty() { vec![Vec::new()] } else { c.chunks(tw).map(<[_]>::to_vec).collect() };
            parts.into_iter().map(|p| { let mut r = cells(mark, st(DIM)); r.extend(p); group(r) }).collect::<Vec<_>>()
        }).collect();
        let notes = where_.lines().filter(|l| !l.trim().is_empty() && !l.starts_with(mark)).count();
        let room = max_h.saturating_sub(out.len() + notes + choices.len());
        let top = if rows.len() > room { rows.len() - room.saturating_sub(1).max(1) } else { 0 };
        if rows.len() > room && room >= 2 {
            let from = self.ascroll.min(top);
            let shown = room - 1;
            out.extend(rows.iter().skip(from).take(shown).cloned());
            let hint = format!("lines {}-{} of {} · PgUp / PgDn shows the rest", from + 1, (from + shown).min(rows.len()), rows.len());
            out.push(group(cut(cells(&hint, st(YELLOW)), w)));
        } else { out.extend(rows); }
        // the summary's later lines: the outside line (⚠), or a note such as the rule that asked
        for l in where_.lines().filter(|l| !l.trim().is_empty() && !l.starts_with(mark)) {
            let warn = is_out && l.starts_with("outside the project");
            out.push(group(cut(cells(&if warn { format!("⚠ {l}") } else { l.to_string() }, if warn { st(YELLOW).bold() } else { st(DIM) }), w)));
        }
        choice_lines(&mut out);
        (out, top)
    }

    /// wrapped input lines and the cursor (col, row) inside the box
    fn input_lines(&self, w: usize) -> (Vec<Line<'static>>, (usize, usize)) {
        let mut rows: Vec<Vec<(char, Style)>> = vec![vec![('›', st(ACCENT).bold()), (' ', st(FG))]];
        let mut pos = (2, 0);
        for (i, &c) in self.input.iter().enumerate() {
            if i == self.cur { pos = (rows.last().unwrap().len(), rows.len() - 1); }
            if c == '\n' { rows.push(vec![(' ', st(FG)); 2]); continue; }
            if rows.last().unwrap().len() >= w + 2 { rows.push(vec![(' ', st(FG)); 2]); }
            rows.last_mut().unwrap().push((c, st(FG)));
        }
        if self.cur >= self.input.len() {
            let last = rows.last().unwrap().len();
            pos = if last >= w + 2 { rows.push(vec![(' ', st(FG)); 2]); (2, rows.len() - 1) } else { (last, rows.len() - 1) };
        }
        let total = rows.len();
        // keep at most 8 rows visible, scrolled to the cursor
        let skip = (pos.1 + 1).saturating_sub(8).min(total.saturating_sub(1));
        let lines = rows.into_iter().skip(skip).take(8).map(group).collect();
        (lines, (pos.0, pos.1 - skip))
    }

    /// the live line's words and how long that step has run
    fn live_text(&self) -> Option<(String, u128)> {
        let cur = match &self.act_next {
            Some(n) if n.1.elapsed().as_millis() as u64 >= crate::activity::QUICK_MS => Some(n),
            _ => self.act.as_ref(),
        }?;
        let ms = cur.1.elapsed().as_millis();
        Some((cur.0.live_text(ms as u64).to_string(), ms))
    }

    fn status_line(&self, w: usize) -> Line<'static> {
        let sep = || Span::styled(" · ", st(DIM));
        let mut v = vec![];
        let shown = self.view.as_ref().map(|v| v.st);
        match &self.pending {
            Some(Pending::Approve(..)) => v.extend([Span::styled(format!("{} waiting for permission", crate::brand::icon::PERMISSION), st(YELLOW).bold()), sep()]),
            Some(Pending::Input(_)) if shown == Some(St::Sleeping) => v.extend([Span::styled("sleeping · type to wake", st(DIM)), sep()]),
            Some(Pending::Input(_)) if self.quit_at.is_none() => v.extend([Span::styled("● ready", st(GREEN)), sep()]),
            _ => {}
        }
        if self.pending.is_none() {
            let ms = self.since.elapsed().as_millis();
            let live = self.live_text();
            let what = match self.phase {
                _ if self.quit_at.is_some() => "quitting…".to_string(),
                // the step in plain words; a wait longer than a few seconds shows its time
                _ if live.is_some() => {
                    let (t, ms) = live.unwrap_or_default();
                    if ms >= 3000 && !t.starts_with("Writing") { format!("{t} {}", crate::ui::dur((ms / 1000) as f64, 0)) } else { t }
                }
                Phase::Streaming => "streaming…".to_string(),
                Phase::Tool => "running tool…".to_string(),
                _ => format!("thinking… {}", crate::ui::dur((ms / 1000) as f64, 0)),
            };
            v.extend([Span::styled(format!("{} {what}", SPIN[(ms / SPIN_MS) as usize % SPIN.len()]), st(ACCENT)),
                Span::styled(" (esc to interrupt)", st(DIM)), sep()]);
        }
        v.push(Span::styled(if self.model.is_empty() { "elisa".into() } else { self.model.clone() }, st(TITLE).bold()));
        if self.yolo { v.extend([sep(), Span::styled("yolo", st(YELLOW).bold())]); }
        if let Some(j) = self.jobs.iter().rev().find(|j| j.p.done.is_none()) { v.extend([sep(), Span::styled(format!("{}…", j.key), st(ACCENT))]); }
        if !self.queue.is_empty() { v.extend([sep(), Span::styled(format!("queued {}", self.queue.len()), st(YELLOW))]); }
        if self.cards > 0 { v.extend([sep(), Span::styled(format!("card #{}", self.cards), st(DIM))]); }
        if self.rss > RSS_WARN { v.extend([sep(), Span::styled(format!("elisa {} MB", self.rss >> 20), st(YELLOW))]); }
        let hint = match &self.flash {
            Some((s, t)) if t.elapsed() < Duration::from_secs(3) => Span::styled(s.clone(), st(YELLOW)),
            _ if self.scroll > 0 => Span::styled(format!("↓ {} more lines (PgDn)", self.scroll), st(DIM)),
            _ if !self.panel_on() && !self.views.is_empty() => Span::styled("ctrl-o show edits · / commands · ctrl-c quit", st(DIM)),
            _ if !self.panel_on() => Span::styled("/ commands · ctrl-o edit panel · ctrl-c quit", st(DIM)),
            _ => Span::styled("/ commands · [ ] edits · alt-↑↓ panel · ctrl-o hide · esc interrupt", st(DIM)),
        };
        let used_w: usize = v.iter().map(|s| s.width()).sum();
        if used_w + hint.width() + 2 < w {
            v.push(Span::raw(" ".repeat(w - used_w - hint.width())));
            v.push(hint);
        }
        Line::from(v)
    }

    /// the context bar, right-aligned under the input box
    fn ctx_line(&self) -> Line<'static> {
        let (used, max) = self.status.map_or((0, self.max_ctx), |s| (s.ctx_used, s.max_ctx));
        let est = if self.status.is_some_and(|s| s.estimated) { "~" } else { "" };
        let pct = (used * 100).checked_div(max).unwrap_or(0);
        let col = if pct > 85 { RED } else if pct > 70 { YELLOW } else { GREEN };
        let fill = (pct.min(100) + 5) / 10;
        Line::from(vec![
            Span::styled(format!("ctx {est}{}/{} ", k(used), k(max)), st(FG)),
            Span::styled("▓".repeat(fill), st(col)), Span::styled("░".repeat(10 - fill), st(DIM)),
            Span::styled(format!(" {pct}% "), st(col)),
        ]).right_aligned()
    }

    /// speed · reused · Σ in/out · memory, right-aligned under the context bar; None while there is nothing to show
    fn stats_line(&self) -> Option<Line<'static>> {
        let sep = || Span::styled(" · ", st(DIM));
        let mut v = vec![];
        let live = self.first_delta.map(|t| t.elapsed().as_secs_f64()).filter(|&e| e > 0.2 && self.phase == Phase::Streaming);
        if let Some(e) = live {
            v.push(Span::styled(format!("~{:.1} tok/s", self.dchars as f64 / 3.3 / e), st(ACCENT)));
        } else if let Some(s) = self.status {
            v.push(Span::styled(crate::ui::speed(&s), st(FG)));
            if s.reused > 0 { v.extend([sep(), Span::styled(format!("reused {}", k(s.reused)), st(FG))]); }
        }
        if self.sum_out > 0 {
            if !v.is_empty() { v.push(sep()); }
            v.push(Span::styled(format!("Σ in {} · out {}", k(self.sum_in), k(self.sum_out)), st(FG)));
        }
        if self.memory && !self.jobs.iter().any(|j| j.p.done.is_none()) {
            if !v.is_empty() { v.push(sep()); }
            v.push(Span::styled("memory", st(DIM)));
        }
        if v.is_empty() { return None; }
        v.push(Span::raw(" "));
        Some(Line::from(v).right_aligned())
    }

    /// the background-job line under the input box: every job joined by ` · ` (running: spinner · label · bar ·
    /// elapsed; finished: ✓/✗ + the done text); when that does not fit, the bars go, then only the newest + `(+N)`
    fn progress_line(&self, w: usize) -> Option<Line<'static>> {
        if self.jobs.is_empty() { return None; }
        let join = |segs: Vec<Cells>| {
            let mut c: Cells = vec![(' ', Style::new())];
            for (i, s) in segs.into_iter().enumerate() {
                if i > 0 { c.extend(cells(" · ", st(DIM))); }
                c.extend(s);
            }
            c
        };
        for bars in [true, false] {
            let c = join(self.jobs.iter().map(|j| job_cells(j, bars, usize::MAX)).collect());
            if width(&c) <= w { return Some(group(c)); }
        }
        let newest = self.jobs.iter().max_by_key(|j| j.at)?;
        let more = cells(&format!(" (+{})", self.jobs.len() - 1), st(DIM));
        let mut c = join(vec![job_cells(newest, true, w.saturating_sub(1 + width(&more)))]);
        c.extend(more);
        Some(group(fit(c, w, Style::new())))
    }

    /// queued messages, oldest first: at most 3 lines, then `+N more`
    fn queue_lines(&self, w: usize) -> Vec<Line<'static>> {
        let mut out: Vec<Line> = self.queue.iter().take(3).enumerate().map(|(i, q)| {
            let t: String = q.chars().map(|c| if c == '\n' { ' ' } else { c }).take(60).collect();
            let more = if q.chars().count() > 60 { "…" } else { "" };
            group(fit(cells(&format!(" ⏳ {}. {t}{more}", i + 1), st(DIM)), w, Style::new()))
        }).collect();
        if self.queue.len() > 3 { out.push(Line::styled(format!("    +{} more", self.queue.len() - 3), st(DIM))); }
        out
    }

    /// the visible transcript lines. Heights are cached per entry for the width; rendered lines are kept only for
    /// entries within one screen of the view and re-rendered when scrolled back in.
    fn transcript(&mut self, w: usize, h: usize) -> Vec<Line<'static>> {
        let w = w.max(10);
        // heights, bottom up; keep the lines of what is probably on screen (the view is measured from the bottom)
        let (lo, hi) = (self.scroll.saturating_sub(h), self.scroll + 2 * h);
        let mut from_bot = 0;
        for it in self.entries.iter_mut().rev() {
            if it.w != w || it.live() {
                let keep = from_bot < hi && from_bot + it.h.max(1) > lo;
                let mut v = vec![];
                render_entry(&it.e, w, &mut v);
                it.w = w;
                it.h = v.len();
                it.lines = keep.then_some(v);
            }
            from_bot += it.h;
        }
        let top = self.dropped as usize;
        let total = top + from_bot;
        if self.scroll > 0 && total > self.last_total { self.scroll += total - self.last_total; }
        self.last_total = total;
        self.view_h = h;
        self.scroll = self.scroll.min(total.saturating_sub(h));
        let end = total - self.scroll;
        let start = end.saturating_sub(h);
        let (ks, ke) = (start.saturating_sub(h), end + h);
        let mut out = vec![];
        if self.dropped && start == 0 { out.push(Line::styled("  … earlier messages are saved (see /resume)", st(DIM))); }
        let mut y = top;
        for it in self.entries.iter_mut() {
            let (a, b) = (y, y + it.h);
            y = b;
            if b <= ks || a >= ke { it.lines = None; continue; }
            let ls = it.lines.get_or_insert_with(|| { let mut v = vec![]; render_entry(&it.e, w, &mut v); v });
            if b > start && a < end {
                let e = (end.min(b) - a).min(ls.len());
                let s0 = (start.max(a) - a).min(e);
                out.extend(ls[s0..e].iter().cloned());
            }
        }
        out
    }
}

/// one job of the progress line; `lw` caps the label width
fn job_cells(j: &Job, bar: bool, lw: usize) -> Cells {
    let p = &j.p;
    if let Some(d) = &p.done {
        let l = d.trim_start().to_lowercase();
        let bad = l.starts_with("failed") || l.starts_with("error") || l.contains(" failed");
        let mut c = cells(if bad { "✗ " } else { "✓ " }, st(if bad { RED } else { GREEN }).bold());
        c.extend(cut(cells(d, st(if bad { RED } else { FG })), lw.saturating_sub(2)));
        return c;
    }
    let ms = j.at.elapsed().as_millis();
    let mut tail: Cells = vec![];
    if bar {
        const BW: usize = 10;
        let b: String = match p.frac {
            Some(fr) => { let n = ((fr.clamp(0.0, 1.0) * BW as f32).round() as usize).min(BW); "█".repeat(n) + &"░".repeat(BW - n) }
            None => {
                // a 3-cell block bouncing across the track
                let span = BW - 3;
                let k = (ms / 90) as usize % (2 * span);
                let x = if k <= span { k } else { 2 * span - k };
                (0..BW).map(|i| if i >= x && i < x + 3 { '█' } else { '░' }).collect()
            }
        };
        tail.extend(cells(" ▕", st(DIM)));
        tail.extend(cells(&b, st(ACCENT)));
        tail.extend(cells("▏", st(DIM)));
    }
    if let Some(fr) = p.frac { tail.extend(cells(&format!(" {:.0}%", fr.clamp(0.0, 1.0) * 100.0), st(FG))); }
    tail.extend(cells(&format!(" {}", crate::ui::dur(p.elapsed_s + j.at.elapsed().as_secs_f64(), 1)), st(DIM)));
    let mut c = cells(&format!("{} ", SPIN[(ms / SPIN_MS) as usize % SPIN.len()]), st(ACCENT));
    let lw = lw.saturating_sub(width(&c) + width(&tail));
    c.extend(cut(cells(&p.label, st(FG)), lw));
    c.extend(tail);
    c
}

/// one transcript entry: a blank separator line (for most kinds) + its lines
pub(super) fn render_entry(e: &Entry, w: usize, out: &mut Vec<Line<'static>>) {
    if !matches!(e, Entry::Foot(..) | Entry::Recalled(_) | Entry::Card(..)) && !matches!(e, Entry::Info(s) if s.starts_with("  ↳")) { out.push(Line::default()); }
    match e {
        Entry::Foot(s) => {
            let mut t = format!("  in {}", k(s.ctx_used.saturating_sub(s.output_tokens)));
            if s.reused > 0 { t += &format!(" (reused {})", k(s.reused)); }
            wrap(cells(&(t + &format!(" · out {} · {}", s.output_tokens, crate::ui::speed(s))), st(DIM)), w, 4, out);
        }
        Entry::Full(title, text) => {
            let mut body = vec![];
            md(text, w.saturating_sub(4).max(8), &mut body);
            boxed_lines(title, body, w, out);
        }
        Entry::Banner(b) => banner(b, w, out),
        Entry::User(t) => {
            for (i, l) in t.lines().enumerate() {
                let mut c = cells(if i == 0 { "› " } else { "  " }, st(ACCENT).bold());
                c.extend(cells(l, st(TITLE).bold()));
                wrap(c, w, 2, out);
            }
        }
        Entry::Text(t) => md(t, w, out),
        Entry::Recalled(h) => {
            out.push(Line::styled(format!("  ↺ recalled {} {}", h.len(), if h.len() == 1 { "memory" } else { "memories" }), st(DIM)));
            for l in h {
                let mut c = cells("    · ", st(DIM));
                c.extend(cut(cells(&one(l), st(DIM)), w.saturating_sub(6)));
                out.push(group(c));
            }
        }
        Entry::Tool(a) => {
            let ms = a.t0.elapsed().as_millis();
            // the kit's tool badge: OK / ERR / RUN / ASK / NO on the status colour
            let (badge, col) = match &a.end {
                Some(e) if e.note == "denied" => ("NO", RED),
                Some(e) => if e.red { ("ERR", RED) } else { ("OK", GREEN) },
                None if a.wait => ("ASK", YELLOW),
                None => ("RUN", ACCENT),
            };
            let tail = if a.end.is_none() && !a.wait && ms >= 1000 { format!(" {}", crate::ui::dur((ms / 1000) as f64, 0)) } else { String::new() };
            let mut c = cells(&format!(" {badge:<3} "), Style::new().fg(BG).bg(col).bold());
            c.push((' ', Style::new()));
            c.extend(cells(&a.verb, st(TITLE).bold()));
            if !a.target.is_empty() {
                c.push((' ', st(FG)));
                let tw = w.saturating_sub(width(&c) + tail.chars().count());
                c.extend(cut(cells(&a.target, st(FG)), tw));
            }
            c.extend(cells(&tail, st(DIM)));
            out.push(group(c));
            match &a.end {
                None if a.wait => out.push(Line::styled("  ↳ waiting for approval", st(DIM))),
                None => {}
                Some(e) => {
                    let mut c = cells("  ↳ ", st(DIM));
                    c.extend(cut(cells(&e.note, st(if e.red { RED } else { DIM })), w.saturating_sub(4)));
                    out.push(group(c));
                    for l in &e.prev {
                        let mut c = cells("    ", st(DIM));
                        c.extend(cut(cells(l, st(DIM)), w.saturating_sub(4)));
                        out.push(group(c));
                    }
                    if e.more > 0 { out.push(Line::styled(format!("    … +{} lines", e.more), st(DIM))); }
                }
            }
        }
        Entry::Info(s) => for l in s.lines() { wrap(cells(l, st(DIM)), w, 2, out) },
        Entry::Err(s) => for l in s.lines() { wrap(cells(l, st(RED)), w, 2, out) },
        Entry::Notes(n) => boxed("notes saved", n, w, out),
        Entry::Watch(s) => {
            let (a, b) = s.split_once(" · ").unwrap_or((s, ""));
            let mut c = cells("  ", st(FG));
            c.extend(cells(a, st(ACCENT).bold()));
            if !b.is_empty() { c.extend(cells(&format!(" · {b}"), st(FG))); }
            wrap(c, w, 4, out);
        }
        Entry::WatchView(v) => watch_box(v, w, out),
        Entry::Card(c, false) => {
            let now = c.lines().map(str::trim).find_map(|l| l.strip_prefix("now:").or_else(|| l.strip_prefix("Now:")))
                .or_else(|| c.lines().map(str::trim).find(|l| !l.is_empty())).unwrap_or("");
            let mut t = cells("  ◇ now: ", st(DIM));
            t.extend(cells(&one(now), st(DIM)));
            out.push(group(cut(t, w)));
        }
        Entry::Card(c, true) => {
            out.push(Line::styled("  ◇ running facts (ctrl-s to collapse)", st(DIM)));
            for l in c.lines().filter(|l| !l.trim().is_empty()) { wrap(cells(&format!("    {l}"), st(DIM)), w, 6, out) }
        }
    }
}

/// paint a rect: spaces on `bg` (the brand background everywhere; raised surfaces on bg2)
struct Fill(Color);
impl Widget for Fill {
    fn render(self, a: Rect, buf: &mut Buffer) {
        for y in a.top()..a.bottom() { for x in a.left()..a.right() { buf[(x, y)].reset(); buf[(x, y)].set_bg(self.0); } }
    }
}

fn popup_block(title: &str) -> Block<'static> {
    Block::bordered().border_type(BorderType::Rounded).border_style(st(ACCENT))
        .title(Span::styled(format!(" {title} "), st(TITLE).bold()))
}

/// the welcome box (the kit's prototype `BOX.welcome`): ELISA label, model / server / folder / context, the notes
fn banner(b: &Banner, w: usize, out: &mut Vec<Line<'static>>) {
    let kv = |k: &str, v: &str, s: Style| { let mut c = cells(&format!("{k:<9}"), st(DIM)); c.extend(cells(v, s)); c };
    let mut ctx = format!("{} tokens", k(b.max_ctx));
    if let Some(n) = b.resumed { ctx += &format!(" · resumed {n} messages"); }
    let mut rows = vec![kv("model", &b.model, st(TITLE)), kv("server", &b.base_url, st(CODE)), kv("folder", &b.cwd, st(CODE)), kv("context", &ctx, st(FG))];
    if let Some(n) = &b.notes {
        rows.push(cells(&format!("notes · loaded ({} words)", n.split_whitespace().count()), st(DIM)));
        let ls: Vec<&str> = n.lines().filter(|l| !l.trim().is_empty()).collect();
        for l in ls.iter().take(6) { let mut c = cells("– ", st(DIM)); c.extend(cells(l, st(FG))); rows.push(c); }
        if ls.len() > 6 { rows.push(cells(&format!("… +{} lines (/notes)", ls.len() - 6), st(DIM))); }
    }
    let mut body = vec![];
    for c in rows { wrap(c, w.max(12) - 4, 9, &mut body); }
    labeled_box("ELISA", "local assistant", body, w, out);
    out.push(Line::styled("  / commands · esc interrupts · ctrl-o edits · /resume past sessions · ctrl-c quits", st(DIM)));
    out.push(Line::styled("  wheel, ↑↓ (empty input), PgUp/PgDn scroll · ctrl-p/n history · ctrl-s running facts · /copy copies the last reply", st(DIM)));
    out.push(Line::styled("  click Elisa or ctrl-g to poke · /water /feed /play or F2 F3 F4 · idle, and it dozes off", st(DIM)));
}

/// terminal heights for the status-bar face: 10 x 5 from `FACE_TALL` rows, 8 x 4 from `FACE_MIN`, else text only
const FACE_TALL: u16 = 40;
const FACE_MIN: u16 = 30;

/// the care tray's width in cells: a 3-cell button and a 1-cell gap to Shade (layout.json gap 6 px)
const TRAY_W: u16 = 4;

/// a rounded accent box with a pixel-style label (uppercase, accent, bold: the terminal's stand-in for Silkscreen)
fn labeled_box(label: &str, sub: &str, body: Vec<Line<'static>>, w: usize, out: &mut Vec<Line<'static>>) {
    let w = w.max(12);
    let b = st(ACCENT);
    let head = format!(" {label} ");
    let sub = if sub.is_empty() { String::new() } else { format!("{sub} ") };
    let used = 2 + head.chars().count() + sub.chars().count() + 1;
    let sub: String = if used > w { String::new() } else { sub };
    let used = 2 + head.chars().count() + sub.chars().count() + 1;
    out.push(Line::from(vec![
        Span::styled("╭─", b), Span::styled(head, st(ACCENT).bold()), Span::styled(sub, st(DIM)),
        Span::styled(format!("{}╮", "─".repeat(w.saturating_sub(used))), b),
    ]));
    for l in body {
        let pad = (w - 4).saturating_sub(l.width());
        let mut s = vec![Span::styled("│ ", b)];
        s.extend(l.spans);
        s.push(Span::raw(" ".repeat(pad)));
        s.push(Span::styled(" │", b));
        out.push(Line::from(s));
    }
    out.push(Line::styled(format!("╰{}╯", "─".repeat(w - 2)), b));
}

/// a rounded box around full text (the notes summary)
fn boxed(title: &str, text: &str, w: usize, out: &mut Vec<Line<'static>>) {
    boxed_cells(title, text.trim().lines().map(|l| inline(l, st(FG))).collect(), w, out)
}

fn boxed_cells(title: &str, rows: Vec<Cells>, w: usize, out: &mut Vec<Line<'static>>) {
    let mut body = vec![];
    for c in rows { wrap(c, w.max(12) - 4, 0, &mut body); }
    boxed_lines(title, body, w, out)
}

/// a rounded box around lines already wrapped to `w - 4`
fn boxed_lines(title: &str, body: Vec<Line<'static>>, w: usize, out: &mut Vec<Line<'static>>) {
    let w = w.max(12);
    let b = st(ACCENT);
    let t = format!(" {title} ");
    out.push(Line::from(vec![
        Span::styled("╭─", b), Span::styled(t.clone(), st(ACCENT).bold()),
        Span::styled(format!("{}╮", "─".repeat(w.saturating_sub(3 + t.chars().count()))), b),
    ]));
    for l in body {
        let pad = (w - 4).saturating_sub(l.width());
        let mut s = vec![Span::styled("│ ", b)];
        s.extend(l.spans);
        s.push(Span::raw(" ".repeat(pad)));
        s.push(Span::styled(" │", b));
        out.push(Line::from(s));
    }
    out.push(Line::styled(format!("╰{}╯", "─".repeat(w - 2)), b));
}

/// the audit line under the input box: ✓ green, ✗ red, the rest in the normal text colour
fn audit_cells_line(t: &str, w: usize) -> Line<'static> {
    let mut c = cells(" ", st(FG));
    for ch in t.chars() { c.push((ch, match ch { '✓' => st(GREEN).bold(), '✗' => st(RED).bold(), _ => st(FG) })); }
    group(fit(c, w, Style::new()))
}

/// the watcher under the input box, right-aligned like the ctx / stats lines, one part per row:
/// `watcher: <domain> <conf>` / `since turn … · would unload …` / `−… ms/pass · dry run`
pub(super) fn watch_rows(v: &WatcherView, w: usize) -> Vec<Line<'static>> {
    crate::watcher::lines(v).into_iter().enumerate().map(|(i, s)| {
        let mut c = match (i, s.strip_prefix("watcher:")) {
            (0, Some(rest)) => {
                let mut c = cells("watcher:", st(ACCENT).bold());
                let (d, tail) = rest.trim_start().split_once(' ').unwrap_or((rest.trim_start(), ""));
                c.extend(cells(&format!(" {d}"), st(TITLE).bold()));
                if !tail.is_empty() { c.extend(cells(&format!(" {tail}"), st(FG))); }
                c
            }
            _ if s.ends_with("dry run") => {
                let (a, _) = s.rsplit_once("dry run").unwrap_or((&s, ""));
                let mut c = cells(a, st(FG));
                c.extend(cells("dry run", st(YELLOW)));
                c
            }
            _ => cells(&s, st(FG)),
        };
        c.push((' ', Style::new()));
        let c = if width(&c) > w { fit(c, w, Style::new()) } else { c };
        group(c).right_aligned()
    }).collect()
}

/// /watcher: the full view in a box; section titles bold, the per-layer bars in the accent colour
fn watch_box(v: &WatcherView, w: usize, out: &mut Vec<Line<'static>>) {
    let rows = crate::watcher::full_lines(v).into_iter().map(|l| {
        if let Some(t) = l.strip_prefix("## ") { return cells(t, st(TITLE).bold()); }
        if l.starts_with("dry run") { return cells(&l, st(YELLOW).bold()); }
        match l.trim_start().starts_with('L').then(|| l.rfind(' ')).flatten() {
            Some(i) if l[i + 1..].chars().all(|c| ('▁'..='█').contains(&c)) => {
                let mut c = cells(&l[..=i], st(FG));
                c.extend(cells(&l[i + 1..], st(ACCENT)));
                c
            }
            _ => cells(&l, st(FG)),
        }
    }).collect();
    boxed_cells(if v.dry() { "watcher · dry run" } else { "watcher" }, rows, w, out);
}

fn fmt_ts(ts: u64) -> String {
    let t = ts as libc::time_t;
    let mut tm: libc::tm = unsafe { std::mem::zeroed() };
    unsafe { libc::localtime_r(&t, &mut tm) };
    format!("{:04}-{:02}-{:02} {:02}:{:02}", tm.tm_year + 1900, tm.tm_mon + 1, tm.tm_mday, tm.tm_hour, tm.tm_min)
}
