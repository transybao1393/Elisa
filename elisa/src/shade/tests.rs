use super::motion::{pose, Ease, Eye};
use super::*;

fn look() -> Look {
    let p = crate::brand::DARK;
    Look { bg: p.bg, accent: p.accent, warn: p.warn, error: p.error, accent_a: p.glow as f32 / 100.0 }
}

#[test]
fn event_to_state_follows_state_mapping_md() {
    use Event as E;
    let rows: &[(Event, St)] = &[
        (E::Launch, St::Greeting), (E::Typing, St::Listening), (E::Waiting, St::Thinking), (E::Delta, St::Streaming),
        (E::Tool("read", "src/split.rs"), St::Reading), (E::Tool("grep", "split_words"), St::Searching),
        (E::Tool("glob", "src/**/*.rs"), St::Searching), (E::Tool("edit", "src/split.rs"), St::Editing),
        (E::Tool("write", "tests/split.rs"), St::Editing), (E::Tool("bash", "cargo build --release"), St::Building),
        (E::Tool("bash", "cargo check"), St::Building), (E::Tool("bash", "cargo clippy --all-targets"), St::Building),
        (E::Tool("bash", "cargo test -q"), St::Testing), (E::Tool("bash", "cargo nextest run"), St::Testing),
        (E::Tool("bash", "ls -la"), St::Running), (E::Tool("bash", "rg split_words src"), St::Searching),
        (E::Permission, St::Permission), (E::Finished(false), St::Success), (E::Finished(true), St::Celebrating),
        (E::UnknownCommand, St::Confused), (E::ToolDone(true), St::Error), (E::Failed, St::Error), (E::Esc, St::Interrupted),
        (E::Recalled, St::Memory), (E::IdleTimeout, St::Sleeping), (E::ModelUnloaded, St::Sleeping), (E::ModelReloading, St::Waking),
        (E::Summarizing, St::Reading),
        // care (v2 state-mapping.md): click → poked, water → drinking, food → eating, toy → playing
        (E::Poke, St::Poked), (E::Water, St::Drinking), (E::Food, St::Eating), (E::Toy, St::Playing),
    ];
    for &(e, s) in rows { assert_eq!(state_for(e), s, "{e:?}"); }
    // attention pop: greeting, permission, celebrating, confused, error (+ waking, as in the prototype), v2: poked, playing
    let pops: Vec<St> = St::ALL.into_iter().filter(|s| s.pops()).collect();
    assert_eq!(pops, [St::Permission, St::Error, St::Waking, St::Greeting, St::Celebrating, St::Confused, St::Poked, St::Playing]);
    assert_eq!(St::ALL.len(), 24);
    assert_eq!(St::ALL.into_iter().filter(|s| s.care()).collect::<Vec<_>>(), [St::Poked, St::Drinking, St::Eating, St::Playing]);
    for st in St::ALL { assert_eq!(St::parse(st.name()), Some(st)); }
}

#[test]
fn a_session_plays_the_right_states_and_bubbles() {
    let mut c = Companion::new(0, Some(45_000));
    let b = |v: &View| v.bubble.as_ref().map(|b| format!("{} {}", b.verb, b.target).trim().to_string()).unwrap_or_default();
    c.on(0, Event::Launch);
    let v = c.view(10);
    assert_eq!((v.st, b(&v).as_str()), (St::Greeting, "HI! ready when you are"));
    assert!(v.bubble.as_ref().unwrap().big);
    assert_eq!(c.view(2300).st, St::Idle, "greeting plays once");
    assert_eq!(c.view(2300).bubble, None, "idle: no bubble");
    c.typing(2400, true);
    assert_eq!(c.view(2400).st, St::Listening);
    c.on(3000, Event::Waiting);
    assert_eq!(b(&c.view(3000)), "THINKING");
    c.on(3500, Event::Tool("read", "crates/core/src/split.rs"));
    assert_eq!(b(&c.view(3500)), "READING crates/core/src/split.rs");
    c.on(3600, Event::ToolDone(false));
    c.on(3700, Event::Tool("bash", "cargo test"));
    c.on(3700, Event::Permission);
    let v = c.view(3700);
    assert_eq!((v.st, b(&v).as_str()), (St::Permission, "NEEDS YOU cargo test"));
    assert_eq!(v.bubble.unwrap().tone, Tone::Warn);
    assert_eq!(c.view(3700 + 400).pop, 1.22, "popped after the 350 ms spring");
    c.on(4000, Event::Tool("bash", "cargo test"));
    assert_eq!(b(&c.view(4000)), "TESTING cargo test");
    c.on(5000, Event::ToolDone(false));
    assert_eq!(b(&c.view(5000)), "DONE tests pass", "a brief happy reaction after the tests");
    c.on(5100, Event::Waiting);
    c.on(5500, Event::Delta);
    assert_eq!(b(&c.view(5500)), "WRITING the answer");
    c.on(6000, Event::Finished(true));
    let v = c.view(6000);
    assert_eq!((v.st, b(&v).as_str()), (St::Celebrating, "ALL DONE! tests green"));
    assert_eq!(c.view(6000 + 1800).st, St::Idle);
    assert_eq!(c.view(6000 + 1800 + 400).pop, 1.0);
    // idle timeout → sleeping; a key wakes: waking, then greeting
    assert_eq!(c.view(6000 + 45_001).st, St::Sleeping);
    c.typing(60_000, true);
    assert_eq!(c.view(60_000).st, St::Waking);
    assert_eq!(c.view(60_000 + 2400).st, St::Greeting);
    assert_eq!(c.view(60_000 + 2400 + 2000).st, St::Listening);
}

#[test]
fn flashes_errors_esc_and_reply_without_tools() {
    let mut c = Companion::new(0, None);
    c.on(0, Event::Waiting);
    c.on(10, Event::Delta);
    c.on(20, Event::Finished(false));
    let v = c.view(20);
    assert_eq!((v.st, v.bubble.unwrap().verb), (St::Success, "DONE"));
    c.on(5000, Event::UnknownCommand);
    assert_eq!(c.view(5000).bubble.unwrap().target, "try /help");
    c.on(9000, Event::Waiting);
    c.on(9100, Event::Tool("bash", "cargo build"));
    assert_eq!(c.view(9100).st, St::Building);
    c.on(9200, Event::ToolDone(true));
    let v = c.view(9200);
    assert_eq!((v.st, v.bubble.unwrap().tone), (St::Error, Tone::Error));
    c.on(9300, Event::Waiting);
    assert_eq!(c.view(9300).st, St::Error, "a brief worried reaction before work goes on");
    assert_eq!(c.view(9200 + WORRIED_MS).st, St::Thinking);
    c.on(12_000, Event::Esc);
    c.on(12_100, Event::Finished(true));
    assert_eq!(c.view(12_100).st, St::Interrupted);
    assert_eq!(c.view(12_000 + 1400).st, St::Idle, "an interrupted turn does not celebrate");
    c.recalled(20_000, 2);
    assert_eq!(c.view(20_000).bubble.unwrap().target, "2 memories");
    // permission cuts any flash short
    c.on(20_100, Event::Permission);
    assert_eq!(c.view(20_100).st, St::Permission);
    // no sleep timeout configured: idle stays idle
    c.on(21_000, Event::Finished(false));
    assert_eq!(c.view(10_000_000).st, St::Idle);
}

#[test]
fn poses_follow_the_kit_keyframes() {
    let p = pose(St::Idle, 0.5);
    assert!((p.sy - 1.035).abs() < 1e-6 && (p.sx - 0.985).abs() < 1e-6);
    assert!((pose(St::Idle, 0.8).eye_open - 0.05).abs() < 1e-6, "blink");
    assert_eq!(pose(St::Celebrating, 0.1).eye, Eye::Happy);
    assert_eq!(pose(St::Waking, 0.2).eye, Eye::Closed);
    assert_eq!(pose(St::Waking, 0.5).eye, Eye::Block);
    assert!((pose(St::Celebrating, 0.32).y + 34.0).abs() < 1e-4, "jump apex");
    assert_eq!(Ease::Back.f(1.0), 1.0);
    assert!(motion::bezier(0.34, 1.7, 0.64, 1.0, 0.5) > 1.0, "the spring overshoots");
}

#[test]
fn frames_are_deterministic_and_animate() {
    let l = look();
    for st in St::ALL {
        let a = render(st, 700, 24, 13, 1.0, &l);
        assert_eq!(a, render(st, 700, 24, 13, 1.0, &l), "{st:?}");
        assert_eq!((a.w, a.h, a.cells.len()), (24, 13, 24 * 13));
        // same frame index → same art; a full loop later → same art
        assert_eq!(render(st, 700 + st.dur_ms() as u64 / FRAME_MS * FRAME_MS, 24, 13, 1.0, &l).cells.len(), a.cells.len());
        let frames: std::collections::HashSet<Vec<Cell>> = (0..st.dur_ms() as u64).step_by(FRAME_MS as usize).map(|ms| render(st, ms, 24, 13, 1.0, &l).cells).collect();
        assert!(frames.len() > 1, "{st:?} animates");
        // the mini build at small sizes
        assert!(render(st, 0, 16, 9, 1.0, &l).cells.iter().any(|c| c.ch != ' ' || c.bg.is_some()));
    }
    assert_eq!(frame_of(St::Idle, 0), frame_of(St::Idle, 4000 / FRAME_MS * FRAME_MS + FRAME_MS - 1 + FRAME_MS), "loops");
}

fn has(a: &Art, f: impl Fn(Rgb) -> bool) -> bool { a.cells.iter().any(|c| c.fg.is_some_and(&f) || c.bg.is_some_and(&f)) }
fn near(x: Rgb, y: Rgb, d: i32) -> bool { (x.0 as i32 - y.0 as i32).abs() <= d && (x.1 as i32 - y.1 as i32).abs() <= d && (x.2 as i32 - y.2 as i32).abs() <= d }

#[test]
fn frames_show_the_state_props_in_their_colours() {
    let l = look();
    let at = |st: St, u: f32| render(st, (u * st.dur_ms() as f32) as u64, 32, 17, 1.0, &l);
    // big pale eyes in every awake state, cyan inner ears
    assert!(has(&at(St::Idle, 0.1), |c| near(c, (0xEE, 0xF0, 0xE6), 24)));
    assert!(has(&at(St::Idle, 0.1), |c| near(c, (0x82, 0xC8, 0xDC), 40)));
    // permission: amber "?" badge; error: red badge; success: green check; thinking: cyan beads (no pink glow at idle)
    assert!(has(&at(St::Permission, 0.0), |c| near(c, (0xE6, 0xBE, 0x50), 40)));
    assert!(has(&at(St::Error, 0.3), |c| near(c, (0xF0, 0x64, 0x64), 40)));
    assert!(has(&at(St::Success, 0.6), |c| near(c, (0x78, 0xC8, 0x78), 40)));
    // the attention glow tints the background around a popped Shade
    let perm = render(St::Permission, 0, 32, 17, 1.22, &l);
    assert!(perm.cells.iter().any(|c| c.ch == ' ' && c.bg.is_some()), "glow behind");
    assert_ne!(perm, render(St::Permission, 0, 32, 17, 1.0, &l), "pop scales the figure");
}

/// the generated motion tables equal the kit's motion JSON (skipped when the kit is not next to the crate)
#[test]
fn motion_tables_match_the_kit() {
    // the v2 update is an overlay on the v1 kit: its file wins where both exist
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("design");
    let dirs = ["Elisa Brand Kit v2 update", "Elisa Brand Kit"].map(|k| root.join(k).join("05-character/shade/motion"));
    if !dirs.iter().all(|d| d.exists()) { eprintln!("brand kit (v1 + v2 update) not found: skipped"); return; }
    for st in St::ALL {
        let f = format!("elisa-shade-{}.json", st.name());
        let path = dirs.iter().map(|d| d.join(&f)).find(|p| p.exists()).unwrap_or_else(|| panic!("{f} missing from the kit"));
        let j: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
        assert_eq!(j["durationMs"].as_u64(), Some(st.dur_ms() as u64), "{st:?}");
        // sample the pose against a direct evaluation of the JSON at many phases
        for i in 0..=40 {
            let u = i as f32 / 40.0;
            let p = pose(st, u.min(0.9999));
            for (ch, keys) in j["tracks"].as_object().unwrap() {
                let keys = keys.as_array().unwrap();
                let v = json_val(keys, u.min(0.9999));
                let got = match ch.as_str() {
                    "x" => p.x, "y" => p.y, "sx" => p.sx, "sy" => p.sy, "rot" => p.rot, "earL" => p.ear_l, "earR" => p.ear_r,
                    "eyeOpen" => p.eye_open, "lookX" => p.look_x, "lookY" => p.look_y, "tail" => p.tail, "pawR" => p.paw_r,
                    "glow" => p.glow, "dim" => p.dim, "p1" => p.p1, "p2" => p.p2, "p3" => p.p3,
                    "eye" => { assert_eq!(format!("{:?}", p.eye).to_lowercase(), v.as_str().unwrap(), "{st:?} eye @{u}"); continue; }
                    o => panic!("unknown channel {o}"),
                };
                assert!((got - v.as_f64().unwrap() as f32).abs() < 1e-3, "{st:?} {ch} @{u}: {got} vs {v}");
            }
        }
    }
}

fn json_val(k: &[serde_json::Value], t: f32) -> serde_json::Value {
    let t0 = |i: usize| k[i]["t"].as_f64().unwrap() as f32;
    if k.len() == 1 || t <= t0(0) { return k[0]["value"].clone(); }
    for i in 0..k.len() - 1 {
        if t <= t0(i + 1) {
            let (a, b) = (&k[i]["value"], &k[i + 1]["value"]);
            let (Some(a), Some(b)) = (a.as_f64(), b.as_f64()) else { return a.clone() };
            let span = t0(i + 1) - t0(i);
            let u = (t - t0(i)) / if span == 0.0 { 1.0 } else { span };
            let e = match k[i]["ease"].as_str().unwrap() { "io" => Ease::Io, "out" => Ease::Out, "in" => Ease::In, "back" => Ease::Back, "lin" => Ease::Lin, _ => Ease::Step };
            return serde_json::json!(a as f32 + (b as f32 - a as f32) * e.f(u));
        }
    }
    k[k.len() - 1]["value"].clone()
}

/// `ELISA_SHADE_SHEET=out.html cargo test --lib shade_sheet -- --ignored`: every state at three dock sizes, as HTML
/// (each cell = two stacked pixels), for eyeballing the terminal frames in a browser
#[test]
#[ignore]
fn shade_sheet() {
    let Some(out) = std::env::var_os("ELISA_SHADE_SHEET") else { return };
    let l = look();
    let hx = |c: Rgb| format!("#{:02x}{:02x}{:02x}", c.0, c.1, c.2);
    let bg = hx(l.bg);
    let mut s = format!("<!doctype html><meta charset=utf-8><body style='margin:0;background:{bg};color:#ECE8F2;font:12px Menlo'><div style='display:flex;flex-wrap:wrap;gap:14px;padding:14px'>");
    // ELISA_FACE_SHEET=1: the status-bar faces (12 states) at trial sizes, kit window then the cropped one
    let faces = std::env::var_os("ELISA_FACE_SHEET").is_some();
    let sts: Vec<St> = if faces { St::ALL.into_iter().filter(|&s| face_state(s) == s).collect() } else { St::ALL.to_vec() };
    for st in sts {
        let sizes: &[(usize, usize, f32)] = if faces { &[(6, 3, 0.0), (8, 4, 0.0), (10, 5, 0.0), (6, 3, 1.0), (8, 4, 1.0), (10, 5, 1.0)] }
            else { &[(32, 19, if st.pops() { 1.22 } else { 1.0 }), (24, 15, 1.0), (16, 9, 1.0)] };
        for &(w, h, pop) in sizes {
            let u = 0.3;
            let ms = (u * st.dur_ms() as f32) as u64;
            let a = if faces { render_face(st, ms, w, h, l.bg, if pop > 0.5 { FACE_VIEW } else { FACE_KIT }) } else { render(st, ms, w, h, pop, &l) };
            s += &format!("<div><div style='display:grid;grid-template-columns:repeat({w},9px)'>");
            for c in &a.cells {
                let (t, b) = match c.ch {
                    '▀' => (c.fg, c.bg), '▄' => (c.bg, c.fg), _ => (c.bg, c.bg),
                };
                let (t, b) = (hx(t.unwrap_or(l.bg)), hx(b.unwrap_or(l.bg)));
                s += &format!("<div style='height:19px;background:linear-gradient({t} 50%,{b} 50%)'></div>");
            }
            s += &format!("</div>{} {w}</div>", st.name());
        }
    }
    std::fs::write(out, s + "</div>").unwrap();
}

#[test]
fn care_actions_play_at_once_and_hand_back() {
    let b = |v: &View| v.bubble.as_ref().map(|b| format!("{} {}", b.verb, b.target)).unwrap_or_default();
    let mut c = Companion::new(0, Some(45_000));
    c.on(0, Event::Waiting);
    // a poke plays over the agent's state for 1.4 s, pops, then thinking comes back
    c.on(100, Event::Poke);
    let v = c.view(100);
    assert_eq!((v.st, b(&v).as_str()), (St::Poked, "HEY! that tickles"));
    assert!(v.bubble.unwrap().big);
    assert_eq!(c.view(100 + 1399).st, St::Poked);
    assert_eq!(c.view(100 + 1400).st, St::Thinking);
    // water / food / toy: the prototype's 4.8 s / 4.8 s / 6 s; a new care action replaces the running one
    c.on(2000, Event::Water);
    assert_eq!(b(&c.view(2000)), "GLUG water");
    assert!(!c.view(2000).bubble.unwrap().big, "drinking does not pop");
    c.on(3000, Event::Food);
    assert_eq!(b(&c.view(3000)), "NOM NOM snack");
    assert_eq!(c.view(3000 + 4799).st, St::Eating);
    assert_eq!(c.view(3000 + 4800).st, St::Thinking);
    c.on(9000, Event::Toy);
    let v = c.view(9000);
    assert_eq!((v.st, b(&v).as_str()), (St::Playing, "PLAY yarn ball"));
    assert_eq!(c.view(9000 + 400).pop, 1.22, "playing pops");
    assert_eq!(c.view(15_000).st, St::Thinking);
    // never over a permission prompt
    c.on(16_000, Event::Permission);
    c.on(16_100, Event::Poke);
    assert_eq!(c.view(16_100).st, St::Permission);
    // asleep: the care action plays straight away (no waking first) and resets the idle clock
    let mut d = Companion::new(0, Some(1000));
    assert_eq!(d.view(5000).st, St::Sleeping);
    d.on(5000, Event::Food);
    assert_eq!(d.view(5000).st, St::Eating);
    assert_eq!(d.view(5000 + 4800).st, St::Idle, "the idle clock starts after the snack");
    assert_eq!(d.view(5000 + 4800 + 1001).st, St::Sleeping);
    // a keypress during the care action (the Enter of /feed) does not pull the idle clock back
    let mut e = Companion::new(0, Some(1000));
    e.on(0, Event::Toy);
    e.typing(10, false);
    assert_eq!(e.view(6000 + 900).st, St::Idle);
}

#[test]
fn care_states_draw_their_props() {
    // the kit v2 props, by colour: the water bottle (#DDEFF4), food box (#C8A27A), yarn ball (pink), poke "!" (amber)
    let l = look();
    let has = |st: St, ms: u64, rgb: Rgb| render(st, ms, 64, 35, 1.0, &l).cells.iter().any(|c| [c.fg, c.bg].iter().any(|x| x.is_some_and(|x| near(x, rgb, 20))));
    assert!(has(St::Drinking, 900, (0xDD, 0xEF, 0xF4)), "water bottle");
    assert!(has(St::Eating, 300, (0xC8, 0xA2, 0x7A)), "food box");
    assert!(has(St::Playing, 500, (0xFF, 0x8F, 0xB1)), "yarn ball");
    assert!(has(St::Poked, 100, (0xE6, 0xBE, 0x50)), "poke mark");
    assert!(!has(St::Idle, 100, (0xC8, 0xA2, 0x7A)));
}

#[test]
fn status_face_maps_to_the_kit_twelve_and_masks_with_bg() {
    // 05-character/shade/svg-statusbar: 12 faces; the rest fall back as in the prototype's FALLBACK
    let kit12 = [St::Error, St::Idle, St::Interrupted, St::Listening, St::Memory, St::Permission, St::Running, St::Sleeping,
        St::Streaming, St::Success, St::Thinking, St::Waking];
    for st in St::ALL { assert!(kit12.contains(&face_state(st)), "{st:?}"); }
    for st in kit12 { assert_eq!(face_state(st), st); }
    let rows = [(St::Poked, St::Interrupted), (St::Drinking, St::Success), (St::Eating, St::Success), (St::Playing, St::Success),
        (St::Greeting, St::Success), (St::Celebrating, St::Success), (St::Reading, St::Running), (St::Editing, St::Running),
        (St::Building, St::Running), (St::Testing, St::Running), (St::Searching, St::Thinking), (St::Confused, St::Error)];
    for (a, b) in rows { assert_eq!(face_state(a), b, "{a:?}"); }
    // deterministic, animated, and the sad lid (error) is filled with the window bg: dark and light faces differ
    let (dark, light) = (crate::brand::DARK.bg, crate::brand::LIGHT.bg);
    assert_eq!(render_face(St::Thinking, 500, 10, 5, dark, FACE_VIEW), render_face(St::Thinking, 500, 10, 5, dark, FACE_VIEW));
    assert_ne!(render_face(St::Listening, 0, 10, 5, dark, FACE_VIEW), render_face(St::Listening, 1000, 10, 5, dark, FACE_VIEW));
    let lit = |a: &Art, bg: Rgb| a.cells.iter().filter(|c| c.fg.is_some_and(|x| x != bg) || c.bg.is_some_and(|x| x != bg)).count();
    let (fd, fl) = (render_face(St::Error, 300, 10, 5, dark, FACE_VIEW), render_face(St::Error, 300, 10, 5, light, FACE_VIEW));
    assert!(lit(&fd, dark) > 10 && lit(&fl, light) > 10, "the face is drawn");
    assert_ne!(fd, fl, "the lid takes the theme bg");
}

#[test]
fn crisp_pixel_art_uses_only_the_kit_palette() {
    // the macOS Terminal path: every drawn pixel is one solid kit colour (no anti-aliased greys, no glow blended into
    // the background), in every state, at every dock size, for the face too, and on both themes
    const { assert!(CRISP) };
    for l in [look(), { let p = crate::brand::LIGHT; Look { bg: p.bg, accent: p.accent, warn: p.warn, error: p.error, accent_a: 0.16 } }] {
        for st in St::ALL {
            let mut arts = vec![];
            for (w, h, pop) in [(32, 19, if st.pops() { 1.22 } else { 1.0 }), (24, 15, 1.0), (16, 9, 1.0)] {
                arts.push(render(st, 500, w, h, pop, &l));
            }
            arts.push(render_face(st, 500, 10, 5, l.bg, FACE_VIEW));
            for a in &arts {
                let mut ink = 0;
                for c in &a.cells {
                    for x in [c.fg, c.bg].into_iter().flatten() {
                        assert!(PALETTE.contains(&x), "{st:?} {}x{}: {x:?} is not a kit colour", a.w, a.h);
                        ink += 1;
                    }
                }
                assert!(ink > 8, "{st:?} {}x{} draws something", a.w, a.h);
            }
        }
    }
    // snapping keeps kit colours and pulls an in-between grey to one of them
    for p in PALETTE { assert_eq!(snap(p), p); }
    assert!(PALETTE.contains(&snap((0x80, 0x80, 0x80))));
    // the sleeping Shade is not dimmed into mixed colours: her inner ears stay the kit cyan
    let a = render(St::Sleeping, 500, 32, 19, 1.0, &look());
    assert!(a.cells.iter().any(|c| c.fg == Some((0x82, 0xC8, 0xDC)) || c.bg == Some((0x82, 0xC8, 0xDC))));
}

/// the bubble as one line: VERB target
fn line(v: &View) -> String { v.bubble.as_ref().map(|b| format!("{} {}", b.verb, b.target).trim().to_string()).unwrap_or_default() }

/// a gateway phase as `api::phase()` publishes it (each publish moves the counter)
struct Gw(u64);
impl Gw {
    fn set(&mut self, c: &mut Companion, now: u64, p: crate::api::Phase, call: (&str, &str)) {
        self.0 += 1;
        c.gateway(now, (p, self.0), || (call.0.to_string(), call.1.to_string()));
    }
}

#[test]
fn each_tool_shows_its_verb_and_target() {
    let mut c = Companion::new(0, None);
    let rows: &[(&str, &str, &str)] = &[
        ("read", "main.py", "READING main.py"),
        ("read", "crates/elisa_core/src/deep/split.rs", "READING …/src/deep/split.rs"),
        ("read_symbol", "app/main.py create_item", "READING app/main.py › create_item"),
        ("grep", "create_item", "LOOKING FOR create_item"),
        ("bash", "rg -n create_item src", "LOOKING FOR create_item"),
        ("edit", "main.py", "EDITING main.py"),
        ("edit_symbol", "main.py create_item", "EDITING main.py › create_item"),
        ("write", "tests/test_items.py (12 lines)", "EDITING tests/test_items.py"),
        ("bash", "cargo build --release", "BUILDING"),
        ("check", "check cargo clippy --all-targets", "BUILDING"),
        ("bash", "pytest tests/test_a.py::t1 tests/test_a.py::t2 tests/test_b.py::t3", "TESTING 3 tests"),
        ("bash", "cargo test -q", "TESTING cargo test -q"),
        ("bash", "ls -la", "RUNNING ls -la"),
    ];
    for (i, &(n, t, want)) in rows.iter().enumerate() {
        let now = 1000 * i as u64;
        c.on(now, Event::Tool(n, t));
        assert_eq!(line(&c.view(now)), want, "{n} {t}");
    }
    assert_eq!(short_path("/Users/x/Development/proj/core/split.rs", 18), "…/core/split.rs");
    assert_eq!(short_path("a_really_long_file_name_here.rs", 10), "…e_here.rs");
    assert_eq!(passed_tests("running 3 tests\n...\ntest result: ok. 3 passed; 0 failed"), Some(3));
    assert_eq!(passed_tests("==== 5 passed in 0.12s ===="), Some(5));
    assert_eq!(passed_tests("nothing here"), None);
}

#[test]
fn a_tool_run_never_shows_thinking() {
    use crate::api::Phase as P;
    let mut c = Companion::new(0, None);
    let mut g = Gw(0);
    c.on(0, Event::Waiting);
    // a read that takes 4 ms, then at once the next model call: the read stays on show for MIN_TOOL_MS
    c.on(1000, Event::Tool("read", "main.py"));
    for t in (1000..1004).step_by(1) { assert_eq!(c.view(t).st, St::Reading); }
    c.on(1004, Event::ToolDone(false));
    c.on(1005, Event::Waiting);
    g.set(&mut c, 1005, P::Sent, ("", ""));
    g.set(&mut c, 1006, P::Started, ("", ""));
    for t in (1004..1000 + MIN_TOOL_MS).step_by(10) {
        let v = c.view(t);
        assert_eq!((v.st, line(&v).as_str()), (St::Reading, "READING main.py"), "at {t}");
    }
    // a long test run: TESTING the whole time, fidgeting now and then after 20 s, never THINKING
    c.on(5000, Event::Tool("bash", "cargo test"));
    let mut fidgets = 0;
    for t in (5000..5000 + 60_000).step_by(100) {
        let v = c.view(t);
        assert_eq!(v.st, St::Testing, "at {t}");
        assert_ne!(v.bubble.as_ref().map(|b| b.verb), Some("THINKING"));
        if v.fidget { fidgets += 1; assert!(t >= 5000 + FIDGET_AFTER_MS); }
    }
    assert!(fidgets > 0, "a long tool fidgets");
    // stale gateway phases (a poll that lags the events) never cover a running tool
    g.set(&mut c, 65_100, P::Decode, ("", ""));
    g.set(&mut c, 65_200, P::Prefill, ("", ""));
    assert_eq!(c.view(65_600).st, St::Testing);
}

#[test]
fn thinking_writing_editing_testing() {
    use crate::api::Phase as P;
    let mut c = Companion::new(0, None);
    let mut g = Gw(0);
    c.on(0, Event::Waiting);
    g.set(&mut c, 0, P::Sent, ("", ""));
    // the queue: WAITING in line once it lasts
    assert_eq!(line(&c.view(PHASE_SHOW_MS + 100)), "WAITING in line");
    g.set(&mut c, 1000, P::Started, ("", ""));
    g.set(&mut c, 1000, P::Prefill, ("", ""));
    let v = c.view(1000 + PHASE_SHOW_MS);
    assert_eq!((v.st, line(&v).as_str()), (St::Reading, "READING the context"));
    // reasoning, then the model writes an edit call's code, then the tool applies it, then the tests
    g.set(&mut c, 3000, P::Decode, ("", ""));
    let v = c.view(3000);
    assert_eq!((v.st, line(&v).as_str()), (St::Thinking, "THINKING"));
    g.set(&mut c, 4000, P::Call, ("edit", ""));
    assert_eq!(line(&c.view(4000)), "WRITING code");
    g.set(&mut c, 4100, P::Call, ("edit", "app/main.py"));
    let v = c.view(4100);
    assert_eq!((v.st, line(&v).as_str()), (St::Streaming, "WRITING app/main.py"));
    g.set(&mut c, 5000, P::Idle, ("", ""));
    c.on(5000, Event::Tool("edit", "app/main.py"));
    let v = c.view(5000);
    assert_eq!((v.st, line(&v).as_str()), (St::Editing, "EDITING app/main.py"), "the switch is immediate");
    c.on(5050, Event::ToolDone(false));
    c.on(5060, Event::Waiting);
    g.set(&mut c, 5060, P::Sent, ("", ""));
    g.set(&mut c, 5070, P::Started, ("", ""));
    g.set(&mut c, 5080, P::Decode, ("", ""));
    assert_eq!(c.view(5000 + MIN_TOOL_MS).st, St::Thinking);
    g.set(&mut c, 7000, P::Idle, ("", ""));
    c.on(7000, Event::Tool("bash", "pytest tests/test_items.py::test_create tests/test_items.py::test_list tests/test_items.py::test_get"));
    assert_eq!(line(&c.view(7000)), "TESTING 3 tests");
    // a passing run: a brief happy reaction with the count, then back to work
    c.tool_done(9000, false, "==== 3 passed in 0.4s ====");
    let v = c.view(9000);
    assert_eq!((v.st, line(&v).as_str()), (St::Success, "DONE 3 tests pass"));
    assert_eq!(c.view(9000 + HAPPY_MS).st, St::Thinking);
    // visible reply text: WRITING the answer; then a non-edit tool call after the text: THINKING
    c.on(12_000, Event::Delta);
    assert_eq!(line(&c.view(12_000)), "WRITING the answer");
    g.set(&mut c, 12_500, P::Call, ("bash", ""));
    assert_eq!(c.view(12_500).st, St::Thinking);
    g.set(&mut c, 12_600, P::Call, ("write", "notes.md"));
    assert_eq!(line(&c.view(12_600)), "WRITING notes.md");
}

#[test]
fn failures_worry_briefly() {
    let mut c = Companion::new(0, None);
    c.on(0, Event::Tool("bash", "cargo test"));
    c.tool_done(100, true, "test result: FAILED. 2 passed; 1 failed");
    let v = c.view(100);
    assert_eq!((v.st, v.bubble.unwrap().tone), (St::Error, Tone::Error));
    // back to work: the next tool cuts the reaction at once
    c.on(400, Event::Tool("read", "src/split.rs"));
    assert_eq!(c.view(400).st, St::Reading);
    c.tool_done(500, true, "no such file");
    assert_eq!(c.view(500 + WORRIED_MS).st, St::Thinking, "the worry is brief");
    c.on(32_000, Event::Finished(true));
    c.on(32_000, Event::Esc);
    assert_eq!(c.view(32_000 + 5000).st, St::Idle);
    assert_eq!(c.view(32_000 + 5000).bubble, None, "done: idle, no bubble");
}
