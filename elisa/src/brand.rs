//! The Elisa brand in the terminal (design/Elisa Brand Kit): the dark and light palettes of 02-color/colors.json, the
//! theme pass that turns a drawn frame into the light theme and/or 256 colours, and the 06-icons UI icons as Unicode
//! glyphs. The TUI draws with the dark palette; `finish` maps it at the end of each frame, so every widget follows the
//! theme without carrying it around.
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::Color;
use std::sync::atomic::{AtomicBool, Ordering};

pub type Rgb = (u8, u8, u8);

/// one theme (02-color/colors.json roles)
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Pal {
    /// window background
    pub bg: Rgb,
    /// raised surfaces: input, bubbles, cards, code blocks
    pub bg2: Rgb,
    /// hover, pressed, selected rows
    pub bg3: Rgb,
    /// headlines, active labels, titles
    pub fg: Rgb,
    /// body text
    pub text: Rgb,
    /// secondary text, metadata, status bar: the dimmest text there is
    pub muted: Rgb,
    /// brand pink: prompt caret, input border, focus, companion glow
    pub accent: Rgb,
    /// inline code, file paths, commands
    pub code: Rgb,
    pub ok: Rgb,
    pub error: Rgb,
    /// permission / caution
    pub warn: Rgb,
    /// hairlines, borders
    pub sep: Rgb,
    /// faint separators (never text)
    pub gutter: Rgb,
    /// selection (accent over bg: dark .15, light .12)
    pub sel: Rgb,
    /// the companion glow strength (dark .24, light .16)
    pub glow: u8,
}

const fn m1(x: u8, y: u8, t: u32) -> u8 { ((x as u32 * (100 - t) + y as u32 * t + 50) / 100) as u8 }
const fn blend(a: Rgb, b: Rgb, t: u32) -> Rgb { (m1(a.0, b.0, t), m1(a.1, b.1, t), m1(a.2, b.2, t)) }

pub const DARK: Pal = Pal {
    bg: (0x16, 0x13, 0x1C), bg2: (0x20, 0x1C, 0x29), bg3: (0x2A, 0x25, 0x36), fg: (0xFF, 0xFF, 0xFF), text: (0xEC, 0xE8, 0xF2),
    muted: (0xA9, 0xA2, 0xB6), accent: (0xFF, 0x8F, 0xB1), code: (0x9F, 0xE0, 0xF0), ok: (0x78, 0xC8, 0x78), error: (0xFF, 0x6B, 0x7A),
    warn: (0xF2, 0xCC, 0x6B), sep: (0x3F, 0x38, 0x50), gutter: (0x6A, 0x62, 0x80),
    sel: blend((0x16, 0x13, 0x1C), (0xFF, 0x8F, 0xB1), 15), glow: 24,
};

pub const LIGHT: Pal = Pal {
    bg: (0xF8, 0xF6, 0xFB), bg2: (0xEF, 0xEB, 0xF4), bg3: (0xE5, 0xE0, 0xEC), fg: (0x12, 0x0E, 0x18), text: (0x22, 0x1C, 0x2A),
    muted: (0x5E, 0x56, 0x6C), accent: (0xC2, 0x38, 0x6A), code: (0x1D, 0x6E, 0x84), ok: (0x2C, 0x7D, 0x35), error: (0xC4, 0x2B, 0x3C),
    warn: (0x8A, 0x64, 0x00), sep: (0xDA, 0xD3, 0xE4), gutter: (0x8C, 0x84, 0x99),
    sel: blend((0xF8, 0xF6, 0xFB), (0xC2, 0x38, 0x6A), 12), glow: 16,
};

impl Pal {
    fn roles(&self) -> [Rgb; 14] {
        [self.bg, self.bg2, self.bg3, self.fg, self.text, self.muted, self.accent, self.code, self.ok, self.error, self.warn, self.sep, self.gutter, self.sel]
    }
}

pub const fn c(x: Rgb) -> Color { Color::Rgb(x.0, x.1, x.2) }

static LIGHT_ON: AtomicBool = AtomicBool::new(false);
static TRUECOLOR: AtomicBool = AtomicBool::new(true);

/// "dark" | "light"; anything else keeps dark (the brand default)
pub fn set_theme(light: bool) { LIGHT_ON.store(light, Ordering::Relaxed) }
pub fn light() -> bool { LIGHT_ON.load(Ordering::Relaxed) }
pub fn pal() -> &'static Pal { if light() { &LIGHT } else { &DARK } }
pub fn set_truecolor(on: bool) { TRUECOLOR.store(on, Ordering::Relaxed) }
pub fn truecolor() -> bool { TRUECOLOR.load(Ordering::Relaxed) }

/// 24-bit colour support from the environment: COLORTERM=truecolor|24bit (macOS 26 Terminal.app 2.15, iTerm2, kitty,
/// WezTerm, Ghostty set it), or a terminal known to support it. Older Terminal.app sets nothing: 256 colours.
pub fn detect_truecolor() -> bool {
    let env = |k: &str| std::env::var(k).unwrap_or_default().to_ascii_lowercase();
    if let Ok(v) = std::env::var("ELISA_COLORS") { return v.trim() != "256"; }
    let ct = env("COLORTERM");
    if ct == "truecolor" || ct == "24bit" { return true; }
    let tp = env("TERM_PROGRAM");
    matches!(tp.as_str(), "iterm.app" | "wezterm" | "ghostty" | "vscode") || env("TERM").contains("direct") || env("TERM").contains("kitty")
}

/// nearest xterm-256 colour (6x6x6 cube or the 24-step grey ramp)
pub fn to256(r: u8, g: u8, b: u8) -> u8 {
    const STEPS: [u8; 6] = [0, 95, 135, 175, 215, 255];
    let near = |v: u8| (0..6).min_by_key(|&i| (STEPS[i] as i32 - v as i32).abs()).unwrap_or(0);
    let (ri, gi, bi) = (near(r), near(g), near(b));
    let cube = (STEPS[ri], STEPS[gi], STEPS[bi]);
    let avg = (r as u32 + g as u32 + b as u32) / 3;
    let gi2 = (((avg as i32 - 8).max(0) + 5) / 10).min(23) as u8;
    let grey = 8 + 10 * gi2;
    let d = |x: (u8, u8, u8)| { let f = |a: u8, b: u8| (a as i32 - b as i32).pow(2); f(x.0, r) * 3 + f(x.1, g) * 4 + f(x.2, b) * 2 };
    if d((grey, grey, grey)) < d(cube) { 232 + gi2 } else { 16 + 36 * ri as u8 + 6 * gi as u8 + bi as u8 }
}

fn map_light(col: Color) -> Color {
    let Color::Rgb(r, g, b) = col else { return col };
    match DARK.roles().iter().position(|&x| x == (r, g, b)) {
        Some(i) => c(LIGHT.roles()[i]),
        None => col,
    }
}

fn map256(col: Color) -> Color {
    match col { Color::Rgb(r, g, b) => Color::Indexed(to256(r, g, b)), x => x }
}

/// The end of every frame: dark → light (outside `keep`, which is drawn for the theme already: the companion and
/// the status-bar face), then 24-bit → 256 colours when the terminal has no truecolor.
pub fn finish(buf: &mut Buffer, keep: &[Rect]) {
    let (lt, tc) = (light(), truecolor());
    if !lt && tc { return; }
    let a = buf.area;
    for y in a.top()..a.bottom() {
        for x in a.left()..a.right() {
            let inside = keep.iter().any(|k| x >= k.x && x < k.x + k.width && y >= k.y && y < k.y + k.height);
            let cell = &mut buf[(x, y)];
            if lt && !inside { cell.fg = map_light(cell.fg); cell.bg = map_light(cell.bg); }
            if !tc { cell.fg = map256(cell.fg); cell.bg = map256(cell.bg); }
        }
    }
}

/// 06-icons/ui as Unicode glyphs (text presentation; no emoji code points)
pub mod icon {
    pub const READ_FILE: &str = "▤";
    pub const WRITE_FILE: &str = "✎";
    pub const EDIT_DIFF: &str = "±";
    pub const RUN_COMMAND: &str = "›";
    pub const SEARCH_FILES: &str = "⌕";
    pub const SEARCH_TEXT: &str = "⌕";
    pub const RUNNING_TOOL: &str = "▶";
    pub const THINKING: &str = "⋯";
    pub const STREAMING: &str = "≋";
    pub const SUCCESS: &str = "✓";
    pub const ERROR: &str = "✗";
    pub const WARNING: &str = "▲";
    pub const INTERRUPTED: &str = "■";
    pub const PAUSED: &str = "‖";
    pub const ALLOW_ONCE: &str = "✓";
    pub const ALWAYS_ALLOW: &str = "✓✓";
    pub const APPROVE_ALL: &str = "»";
    pub const DENY: &str = "✗";
    pub const MEMORY_RECALLED: &str = "↺";
    pub const NOTES: &str = "✐";
    pub const PINNED_FACT: &str = "⊙";
    pub const STATE_CARD: &str = "◇";
    pub const COMPACT: &str = "⇊";
    pub const CONTEXT_USAGE: &str = "▓";
    pub const SPEED: &str = "↯";
    pub const DOMAIN_WATCHER: &str = "◎";
    pub const HEAT_MAP: &str = "▦";
    pub const DRY_RUN: &str = "◌";
    pub const AUDIT_CHECK: &str = "✓";
    pub const FULL_MODEL: &str = "◉";
    pub const UNLOAD: &str = "⇣";
    pub const RESTORE: &str = "⇡";
    pub const SESSION_RESUME: &str = "↻";
    pub const COPY: &str = "⧉";
    pub const PERMISSION: &str = "◆";
    /// kit v2 care tray (water bottle, food box, yarn ball): text glyphs that read in any mono font
    pub const WATER: &str = "≈";
    pub const FOOD: &str = "∪";
    pub const TOY: &str = "@";
    /// the tool's icon
    pub fn tool(name: &str) -> &'static str {
        match name { "read" | "read_symbol" => READ_FILE, "write" => WRITE_FILE, "edit" | "edit_symbol" => EDIT_DIFF, "outline" => SEARCH_FILES, "bash" => RUN_COMMAND, "glob" => SEARCH_FILES, "grep" => SEARCH_TEXT, _ => RUNNING_TOOL }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn palettes_match_the_kit_and_quantize() {
        // 02-color/colors.json
        assert_eq!(DARK.accent, (0xFF, 0x8F, 0xB1));
        assert_eq!(LIGHT.accent, (0xC2, 0x38, 0x6A));
        assert_eq!(to256(0, 0, 0), 16);
        assert_eq!(to256(255, 255, 255), 231);
        assert!((232..=255).contains(&to256(0x16, 0x13, 0x1C)), "near-black bg → grey ramp");
        // every dark role maps to its light role
        assert_eq!(map_light(c(DARK.text)), c(LIGHT.text));
        assert_eq!(map_light(Color::Reset), Color::Reset);
    }
}
