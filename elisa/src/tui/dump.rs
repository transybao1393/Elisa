use super::*;

// ---------- entry point ----------
// ---- offscreen screen dump (screenshots without screen-recording permission) ----------------------------------------

/// `ELISA_SCREEN_DUMP=FILE`: after every draw that changed the screen, the frame is written to FILE as HTML (one
/// fixed-width cell per column, the real colors), atomically. A dev aid for screenshots (render the HTML with a
/// headless browser); off unless the variable is set.
pub(super) struct ScreenDump { path: std::path::PathBuf, last: String }

impl ScreenDump {
    pub(super) fn from_env() -> Option<ScreenDump> {
        std::env::var_os("ELISA_SCREEN_DUMP").filter(|p| !p.is_empty()).map(|p| ScreenDump { path: p.into(), last: String::new() })
    }
    pub(super) fn write(&mut self, b: &Buffer) {
        let html = buffer_html(b);
        if html == self.last { return; }
        let tmp = self.path.with_extension("tmp");
        if std::fs::write(&tmp, &html).is_ok() { let _ = std::fs::rename(&tmp, &self.path); }
        self.last = html;
    }
}

fn css_color(c: Color, fg: bool) -> Option<String> {
    const ANSI: [(u8, u8, u8); 16] = [(0, 0, 0), (205, 49, 49), (13, 188, 121), (229, 229, 16), (36, 114, 200), (188, 63, 188), (17, 168, 205), (229, 229, 229),
        (102, 102, 102), (241, 76, 76), (35, 209, 139), (245, 245, 67), (59, 142, 234), (214, 112, 214), (41, 184, 219), (255, 255, 255)];
    let (r, g, b) = match c {
        Color::Reset => return None,
        Color::Rgb(r, g, b) => (r, g, b),
        Color::Indexed(n) if n < 16 => ANSI[n as usize],
        Color::Indexed(n) if n >= 232 => { let v = 8 + 10 * (n - 232); (v, v, v) }
        Color::Indexed(n) => {
            let i = n - 16;
            let s = |x: u8| if x == 0 { 0 } else { 55 + 40 * x };
            (s(i / 36), s((i / 6) % 6), s(i % 6))
        }
        Color::Black => ANSI[0], Color::Red => ANSI[1], Color::Green => ANSI[2], Color::Yellow => ANSI[3], Color::Blue => ANSI[4],
        Color::Magenta => ANSI[5], Color::Cyan => ANSI[6], Color::Gray => ANSI[7], Color::DarkGray => ANSI[8], Color::LightRed => ANSI[9],
        Color::LightGreen => ANSI[10], Color::LightYellow => ANSI[11], Color::LightBlue => ANSI[12], Color::LightMagenta => ANSI[13],
        Color::LightCyan => ANSI[14], Color::White => ANSI[15],
    };
    let _ = fg;
    Some(format!("#{r:02x}{g:02x}{b:02x}"))
}

/// The buffer as a standalone HTML page (dark terminal colors; wide symbols span two cells).
fn buffer_html(b: &Buffer) -> String {
    let a = b.area;
    let mut s = format!("<!doctype html><meta charset=utf-8><style>body{{margin:0;background:{}}}#t{{font:15px/19px Menlo,monospace;color:#d4d4d4;\
        padding:8px;white-space:pre}}.r{{height:19px}}.c{{display:inline-block;width:9.03px;height:19px;overflow:visible;vertical-align:top}}.w{{width:18.06px}}</style><div id=t data-cols={} data-rows={}>",
        css_color(crate::brand::c(crate::brand::pal().bg), false).unwrap_or_default(), a.width, a.height);
    for y in a.top()..a.bottom() {
        s += "<div class=r>";
        let mut x = a.left();
        while x < a.right() {
            let c = &b[(x, y)];
            let sym = c.symbol();
            let w = Span::raw(sym).width().max(1);
            let m = c.modifier;
            let (mut fg, mut bg) = (css_color(c.fg, true), css_color(c.bg, false));
            if m.contains(Modifier::REVERSED) { std::mem::swap(&mut fg, &mut bg); fg.get_or_insert("#1e1e1e".into()); bg.get_or_insert("#d4d4d4".into()); }
            let mut st = String::new();
            if let Some(f) = &fg { st += &format!("color:{f};"); }
            if let Some(g) = &bg { st += &format!("background:{g};"); }
            if m.contains(Modifier::BOLD) { st += "font-weight:bold;"; }
            if m.contains(Modifier::DIM) { st += "opacity:.6;"; }
            if m.contains(Modifier::ITALIC) { st += "font-style:italic;"; }
            if m.contains(Modifier::UNDERLINED) { st += "text-decoration:underline;"; }
            // half blocks: paint the two pixels exactly (a font's block glyph may not fill the line box)
            let half = match sym { "▀" => Some((fg.clone(), bg.clone())), "▄" => Some((bg.clone(), fg.clone())), _ => None };
            if let Some((t, u)) = half.clone() {
                let d = css_color(crate::brand::c(crate::brand::pal().bg), false).unwrap_or_default();
                st += &format!("background:linear-gradient({} 50%,{} 50%);", t.unwrap_or(d.clone()), u.unwrap_or(d));
            }
            let esc = if half.is_some() { String::new() } else { sym.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;") };
            s += &format!("<span class=\"c{}\" style=\"{st}\">{esc}</span>", if w > 1 { " w" } else { "" });
            x += w as u16;
        }
        s += "</div>";
    }
    s + "</div>"
}
