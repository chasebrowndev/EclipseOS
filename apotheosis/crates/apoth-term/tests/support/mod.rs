// SPDX-License-Identifier: AGPL-3.0-only
//! Golden-grid fixture harness (T-25 §1) for `apoth-term`: parse `.kdl` fixtures, replay
//! them at several chunkings and compare against the expectations they carry.
//!
//! This is the testkit's fixture machinery, kept beside the crate until `apoth-testkit`
//! takes it over. Blessing is never automatic: with `APOTH_FIXTURE_SHOW=1` a failing
//! fixture prints the actual state in fixture syntax so a human can review and paste it.
#![allow(dead_code, unreachable_pub)]

pub mod kdl;

use std::time::{Duration, Instant};

use apoth_grid::{SnapRow, Underline, Wide};
use apoth_input::modes::bit;
use apoth_term::{ModeState, Term, TermConfig, abs_row_text, row_text};
use kdl::{Node, Val};

#[derive(Clone, Debug)]
pub struct Fixture {
    pub id: String,
    pub file: String,
    pub node: Node,
}

#[derive(Clone, Copy, Debug)]
pub enum Chunking {
    Whole,
    Size(usize),
    Random(u64),
}

pub const CHUNKINGS: &[Chunking] = &[
    Chunking::Whole,
    Chunking::Size(1),
    Chunking::Size(7),
    Chunking::Size(4096),
    Chunking::Random(1),
    Chunking::Random(2),
    Chunking::Random(3),
];

pub fn fixtures_dir() -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures")
}

pub fn load_str(src: &str, file: &str) -> Result<Vec<Fixture>, String> {
    let nodes = kdl::parse(src).map_err(|e| format!("{file}: {e}"))?;
    let mut out = Vec::new();
    for n in nodes {
        if n.name != "fixture" {
            return Err(format!(
                "{file}: top-level node {} is not `fixture`",
                n.name
            ));
        }
        let id = n
            .str_arg(0)
            .map_err(|e| format!("{file}: {e}"))?
            .to_string();
        out.push(Fixture {
            id,
            file: file.to_string(),
            node: n,
        });
    }
    Ok(out)
}

pub fn load_all() -> Vec<Fixture> {
    let mut files: Vec<_> = std::fs::read_dir(fixtures_dir())
        .expect("fixtures dir")
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|x| x == "kdl"))
        .collect();
    files.sort();
    let mut all = Vec::new();
    for f in files {
        let src = std::fs::read_to_string(&f).expect("read fixture");
        let name = f.file_name().unwrap().to_string_lossy().into_owned();
        all.extend(load_str(&src, &name).unwrap_or_else(|e| panic!("{e}")));
    }
    all
}

fn hash(s: &str) -> u64 {
    let mut h = 0xcbf29ce484222325u64;
    for b in s.bytes() {
        h = (h ^ u64::from(b)).wrapping_mul(0x100000001b3);
    }
    h
}

struct Rng(u64);
impl Rng {
    fn step(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
}

pub fn split(bytes: &[u8], ch: Chunking, id: &str) -> Vec<Vec<u8>> {
    match ch {
        Chunking::Whole => vec![bytes.to_vec()],
        Chunking::Size(n) => bytes.chunks(n.max(1)).map(<[u8]>::to_vec).collect(),
        Chunking::Random(seed) => {
            let mut r = Rng(hash(id) ^ seed.wrapping_mul(0x9e3779b97f4a7c15) | 1);
            let mut out = Vec::new();
            let mut i = 0;
            while i < bytes.len() {
                let n = 1 + (r.step() % 16) as usize;
                let e = (i + n).min(bytes.len());
                out.push(bytes[i..e].to_vec());
                i = e;
            }
            out
        }
    }
}

pub fn text_bytes(s: &str) -> Vec<u8> {
    s.as_bytes().to_vec()
}

fn hex_bytes(s: &str) -> Result<Vec<u8>, String> {
    let h: String = s.chars().filter(|c| !c.is_whitespace()).collect();
    if !h.len().is_multiple_of(2) {
        return Err("odd hex".into());
    }
    (0..h.len() / 2)
        .map(|i| u8::from_str_radix(&h[2 * i..2 * i + 2], 16).map_err(|_| "bad hex".to_string()))
        .collect()
}

pub fn config_from(node: &Node) -> Result<TermConfig, String> {
    let mut cfg = TermConfig {
        rows: node.prop_int("rows").unwrap_or(24) as usize,
        cols: node.prop_int("cols").unwrap_or(80) as usize,
        ..TermConfig::default()
    };
    for c in node.children.iter().filter(|c| c.name == "config") {
        if let Some(v) = c.prop_str("version") {
            let p: Vec<u32> = v.split('.').map(|x| x.parse().unwrap_or(0)).collect();
            cfg.version = (p[0], p[1], p[2]);
        }
        if let Some(b) = c.prop_bool("sixel") {
            cfg.sixel_advertised = b;
        }
        if let Some(m) = c.prop_str("width-method") {
            cfg.widths.method = match m {
                "wcswidth" => apoth_unicode::WidthMethod::Wcswidth,
                _ => apoth_unicode::WidthMethod::Unicode,
            };
        }
        if c.prop_bool("ambiguous-wide") == Some(true) {
            cfg.widths.ambiguous = apoth_unicode::Ambiguous::Wide;
        }
        if let Some(n) = c.prop_int("osc52-max") {
            cfg.osc52_max = n as usize;
        }
        if let Some(b) = c.prop_bool("osc52-write") {
            cfg.osc52_write = b;
        }
        if let Some(n) = c.prop_int("sync-timeout-ms") {
            cfg.sync_timeout = Duration::from_millis(n as u64);
        }
        if let Some(s) = c.prop_str("cell-px") {
            let (w, h) = s.split_once(',').ok_or("cell-px W,H")?;
            cfg.cell_px = (
                w.parse().map_err(|_| "cell-px")?,
                h.parse().map_err(|_| "cell-px")?,
            );
        }
        if let Some(b) = c.prop_bool("dark") {
            cfg.dark = b;
        }
    }
    Ok(cfg)
}

fn show(b: &[u8]) -> String {
    let mut s = String::new();
    for &c in b {
        match c {
            0x1b => s.push_str("\\e"),
            0x07 => s.push_str("\\a"),
            0x20..=0x7e => s.push(c as char),
            _ => s.push_str(&format!("\\x{c:02x}")),
        }
    }
    s
}

fn color_str(c: apoth_grid::Color) -> String {
    match c {
        apoth_grid::Color::Default => "default".into(),
        apoth_grid::Color::Palette(n) => format!("p{n}"),
        apoth_grid::Color::Rgb(r, g, b) => format!("{r},{g},{b}"),
    }
}

fn input_mask(name: &str) -> Result<u64, String> {
    Ok(match name {
        "decckm" => bit::DECCKM,
        "deckpam" => bit::DECKPAM,
        "alt-esc" => bit::ALT_ESC,
        "bpaste" => bit::BRACKETED_PASTE,
        "m9" => bit::MOUSE_9,
        "m1000" => bit::MOUSE_1000,
        "m1002" => bit::MOUSE_1002,
        "m1003" => bit::MOUSE_1003,
        "focus" => bit::FOCUS_1004,
        "m1005" => bit::MOUSE_1005,
        "sgr" => bit::MOUSE_SGR_1006,
        "m1015" => bit::MOUSE_1015,
        "pixels" => bit::MOUSE_PIXELS_1016,
        "decbkm" => bit::DECBKM,
        "altscreen" => bit::ALT_SCREEN,
        "altscroll" => bit::ALT_SCROLL_1007,
        other => return Err(format!("unknown input mode name {other}")),
    })
}

fn expect_eq<T: PartialEq + std::fmt::Debug>(what: &str, got: T, want: T) -> Result<(), String> {
    if got == want {
        Ok(())
    } else {
        Err(format!("{what}: got {got:?}, want {want:?}"))
    }
}

fn cell_check(term: &Term, n: &Node) -> Result<(), String> {
    let y = n.int_arg(0)? as usize;
    let x = n.int_arg(1)? as usize;
    let mut row = SnapRow::default();
    term.grid().copy_row(y, &mut row);
    let c = *row.cells.get(x).ok_or("cell out of range")?;
    let at = format!("cell {y} {x}");
    for (k, v) in &n.props {
        match (k.as_str(), v) {
            ("ch", Val::Str(s)) => {
                let mut cps = Vec::new();
                term.grid().cell_text(y, x, &mut cps);
                let got: String = if cps.is_empty() {
                    " ".into()
                } else {
                    cps.iter().filter_map(|&c| char::from_u32(c)).collect()
                };
                expect_eq(&format!("{at} ch"), got, s.clone())?;
            }
            ("fg", Val::Str(s)) => {
                expect_eq(&format!("{at} fg"), color_str(c.style.fg), s.clone())?
            }
            ("bg", Val::Str(s)) => {
                expect_eq(&format!("{at} bg"), color_str(c.style.bg), s.clone())?
            }
            ("ul", Val::Str(s)) => {
                expect_eq(&format!("{at} ul"), color_str(c.style.ul_color), s.clone())?;
            }
            ("underline", Val::Str(s)) => {
                let got = match c.style.underline {
                    Underline::None => "none",
                    Underline::Single => "single",
                    Underline::Double => "double",
                    Underline::Curly => "curly",
                    Underline::Dotted => "dotted",
                    Underline::Dashed => "dashed",
                };
                expect_eq(&format!("{at} underline"), got.to_string(), s.clone())?;
            }
            ("wide", Val::Str(s)) => {
                let got = match c.wide {
                    Wide::Narrow => "narrow",
                    Wide::Wide => "wide",
                    Wide::SpacerTail => "tail",
                    Wide::SpacerHead => "head",
                };
                expect_eq(&format!("{at} wide"), got.to_string(), s.clone())?;
            }
            ("semantic", Val::Str(s)) => {
                let got = match c.semantic {
                    apoth_grid::Semantic::Output => "output",
                    apoth_grid::Semantic::Input => "input",
                    apoth_grid::Semantic::Prompt => "prompt",
                };
                expect_eq(&format!("{at} semantic"), got.to_string(), s.clone())?;
            }
            ("link", Val::Bool(b)) => expect_eq(&format!("{at} link"), c.link, *b)?,
            ("protected", Val::Bool(b)) => expect_eq(&format!("{at} protected"), c.protected, *b)?,
            (a, Val::Bool(b))
                if matches!(
                    a,
                    "bold"
                        | "faint"
                        | "italic"
                        | "blink"
                        | "inverse"
                        | "invisible"
                        | "strike"
                        | "overline"
                ) =>
            {
                let mask = match a {
                    "bold" => apoth_grid::Attrs::BOLD,
                    "faint" => apoth_grid::Attrs::FAINT,
                    "italic" => apoth_grid::Attrs::ITALIC,
                    "blink" => apoth_grid::Attrs::BLINK,
                    "inverse" => apoth_grid::Attrs::INVERSE,
                    "invisible" => apoth_grid::Attrs::INVISIBLE,
                    "strike" => apoth_grid::Attrs::STRIKE,
                    _ => apoth_grid::Attrs::OVERLINE,
                };
                expect_eq(&format!("{at} {a}"), c.style.attrs.contains(mask), *b)?;
            }
            (other, _) => return Err(format!("{at}: unknown property {other}")),
        }
    }
    Ok(())
}

fn rgb_str(c: (u8, u8, u8)) -> String {
    format!("{},{},{}", c.0, c.1, c.2)
}

/// Replay one fixture at one chunking. `Err` carries a description of the first mismatch.
pub fn run(fx: &Fixture, ch: Chunking) -> Result<(), String> {
    let cfg = config_from(&fx.node)?;
    let mut term = Term::new(cfg);
    let base = Instant::now();
    let mut now = base;
    for n in &fx.node.children {
        let ctx = |e: String| format!("[{} @ {:?}] {} -> {e}", fx.id, ch, n.name);
        step(&mut term, n, &mut now, base, ch, &fx.id).map_err(ctx)?;
    }
    Ok(())
}

fn feed_chunks(term: &mut Term, bytes: &[u8], now: Instant, ch: Chunking, id: &str) {
    for c in split(bytes, ch, id) {
        term.feed(&c, now);
    }
}

fn step(
    term: &mut Term,
    n: &Node,
    now: &mut Instant,
    base: Instant,
    ch: Chunking,
    id: &str,
) -> Result<(), String> {
    match n.name.as_str() {
        "config" => {}
        "feed" => {
            let mut b = Vec::new();
            for s in n.strs() {
                b.extend(text_bytes(&s));
            }
            feed_chunks(term, &b, *now, ch, id);
        }
        "feed-hex" => {
            let mut b = Vec::new();
            for s in n.strs() {
                b.extend(hex_bytes(&s)?);
            }
            feed_chunks(term, &b, *now, ch, id);
        }
        "feed-repeat" => {
            let s = n.str_arg(0)?;
            let k = n.int_arg(1)? as usize;
            feed_chunks(term, &s.repeat(k).into_bytes(), *now, ch, id);
        }
        "advance" => {
            *now += Duration::from_millis(n.int_arg(0)? as u64);
            term.tick(*now);
        }
        "resize" => {
            term.resize(n.int_arg(0)? as usize, n.int_arg(1)? as usize);
        }
        "cell-px" => term.set_cell_px(n.int_arg(0)? as u32, n.int_arg(1)? as u32),
        "reset" => term.reset(),
        "grid" => {
            let want = n.strs();
            for y in 0..term.rows() {
                let got = row_text(term.grid(), y);
                let w = want.get(y).map_or("", String::as_str);
                expect_eq(
                    &format!("row {y}"),
                    got.trim_end_matches(' '),
                    w.trim_end_matches(' '),
                )?;
            }
            if want.len() > term.rows() {
                return Err("more grid lines than rows".into());
            }
        }
        "row" => {
            let y = n.int_arg(0)? as usize;
            let got = row_text(term.grid(), y);
            expect_eq(
                &format!("row {y}"),
                got.trim_end_matches(' '),
                n.str_arg(1)?.trim_end_matches(' '),
            )?;
        }
        "cursor" => {
            let c = term.cursor();
            let (y, x) = (n.int_arg(0)? as usize, n.int_arg(1)? as usize);
            expect_eq("cursor", (c.y, c.x), (y, x))?;
            if let Some(b) = n.prop_bool("wrap") {
                expect_eq("cursor wrap", c.wrap_pending, b)?;
            }
            if let Some(b) = n.prop_bool("visible") {
                expect_eq("cursor visible", c.visible, b)?;
            }
            if let Some(s) = n.prop_int("shape") {
                expect_eq("cursor shape", i64::from(c.style.shape), s)?;
            }
            if let Some(b) = n.prop_bool("blink") {
                expect_eq("cursor blink", c.style.blink, b)?;
            }
        }
        "cell" => cell_check(term, n)?,
        "size" => expect_eq(
            "size",
            (term.rows(), term.cols()),
            (n.int_arg(0)? as usize, n.int_arg(1)? as usize),
        )?,
        "mode" => {
            let spec = n.str_arg(0)?;
            let (private, num) = match spec.strip_prefix('?') {
                Some(r) => (true, r),
                None => (false, spec),
            };
            let num: u16 = num.parse().map_err(|_| "bad mode number")?;
            let st: ModeState = term.mode_state(!private, num);
            expect_eq(&format!("mode {spec}"), st.name(), n.str_arg(1)?)?;
        }
        "replies" => {
            let want: Vec<u8> = n.strs().iter().flat_map(|s| text_bytes(s)).collect();
            let got = term.replies().take_all();
            expect_eq("replies", show(&got), show(&want))?;
        }
        "events" => {
            let got = term.events().render();
            term.events().clear();
            expect_eq("events", got, n.strs())?;
        }
        "history" => expect_eq(
            "history rows",
            term.primary().history_rows(),
            n.int_arg(0)? as usize,
        )?,
        "alt-history" => expect_eq(
            "alt history rows",
            term.alternate().history_rows(),
            n.int_arg(0)? as usize,
        )?,
        "history-row" => {
            let i = n.int_arg(0)? as usize;
            let got = abs_row_text(term.primary(), i).ok_or("history row out of range")?;
            expect_eq(
                &format!("history row {i}"),
                got.trim_end_matches(' '),
                n.str_arg(1)?.trim_end_matches(' '),
            )?;
        }
        "screen" => expect_eq("screen", term.on_alternate(), n.str_arg(0)? == "alt")?,
        "hold" => expect_eq(
            "hold",
            term.presentation_hold(),
            matches!(n.args.first(), Some(Val::Bool(true))),
        )?,
        "deadline-ms" => {
            let got = term
                .next_deadline()
                .map(|d| d.duration_since(base).as_millis() as i64);
            let want = match n.args.first() {
                Some(Val::Int(v)) => Some(*v),
                _ => None,
            };
            expect_eq("deadline", got, want)?;
        }
        "input" | "input-clear" => {
            let m = term.input_mirror().load();
            for name in n.strs() {
                let on = m.has(input_mask(&name)?);
                expect_eq(&format!("input {name}"), on, n.name == "input")?;
            }
        }
        "kitty" => expect_eq(
            "kitty flags",
            term.input_mirror().load().kitty_flags(),
            n.int_arg(0)? as u32,
        )?,
        "mok" => expect_eq(
            "mok level",
            i64::from(term.input_mirror().load().mok_level()),
            n.int_arg(0)?,
        )?,
        "title" => expect_eq("title", term.title().to_string(), n.str_arg(0)?.to_string())?,
        "title-len" => expect_eq("title len", term.title().len(), n.int_arg(0)? as usize)?,
        "palette" => {
            let i = n.int_arg(0)? as u8;
            expect_eq(
                &format!("palette {i}"),
                rgb_str(term.palette(i)),
                n.str_arg(1)?.to_string(),
            )?;
        }
        "color" => {
            let (fg, bg, cur) = term.colors();
            let got = match n.str_arg(0)? {
                "fg" => fg,
                "bg" => bg,
                _ => cur,
            };
            expect_eq("color", rgb_str(got), n.str_arg(1)?.to_string())?;
        }
        "dropped-replies" => expect_eq(
            "dropped replies",
            term.replies().dropped(),
            n.int_arg(0)? as u64,
        )?,
        other => return Err(format!("unknown step `{other}`")),
    }
    Ok(())
}

/// Digest of everything observable after feeding `bytes` in the given chunking, for the
/// chunking-invariance meta-test and the random-stream property test.
pub fn digest(cfg: TermConfig, bytes: &[u8], ch: Chunking) -> String {
    let mut term = Term::new(cfg);
    let now = Instant::now();
    for c in split(bytes, ch, "digest") {
        term.feed(&c, now);
    }
    let mut s = String::new();
    for y in 0..term.rows() {
        s.push_str(&row_text(term.grid(), y));
        s.push('|');
    }
    let c = term.cursor();
    s.push_str(&format!(
        "{:?} hist={} alt={} hold={} ",
        c,
        term.primary().history_rows(),
        term.on_alternate(),
        term.presentation_hold()
    ));
    s.push_str(&show(&term.replies().take_all()));
    s.push_str(&format!("{:?}", term.events().render()));
    s.push_str(&format!("{:?}", term.input_mirror().load()));
    let mut row = SnapRow::default();
    for y in 0..term.rows() {
        term.grid().copy_row(y, &mut row);
        for c in &row.cells {
            s.push_str(&format!("{:?}", c.style));
        }
    }
    s
}

/// All fixtures at all chunkings; returns the failures.
pub fn run_everything(fixtures: &[Fixture]) -> Vec<String> {
    let mut fails = Vec::new();
    for fx in fixtures {
        for &ch in CHUNKINGS {
            if let Err(e) = run(fx, ch) {
                fails.push(e);
                break;
            }
        }
    }
    fails
}
