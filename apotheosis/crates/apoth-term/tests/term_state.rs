// SPDX-License-Identifier: AGPL-3.0-only
//! Term-state checks the fixture format of `apoth-testkit` cannot express yet (T-25 §1): the
//! input-mirror and kitty flag stack, `TermConfig` knobs, palette/colour/title accessors,
//! history counts, sync deadlines and cell-size changes. Everything else of the old
//! `apoth-term` corpus lives in `apoth-testkit/fixtures`. Each check runs at every chunking.
#![allow(clippy::too_many_lines, dead_code)]

mod support;

use std::time::{Duration, Instant};

use apoth_grid::{SnapRow, Underline, Wide};
use apoth_input::modes::bit;
use apoth_term::{Term, TermConfig, abs_row_text, row_text};
use support::{CHUNKINGS, Chunking, show, split};

struct H {
    term: Term,
    now: Instant,
    base: Instant,
    ch: Chunking,
    id: &'static str,
}

fn color_str(c: apoth_grid::Color) -> String {
    match c {
        apoth_grid::Color::Default => "default".into(),
        apoth_grid::Color::Palette(n) => format!("p{n}"),
        apoth_grid::Color::Rgb(r, g, b) => format!("{r},{g},{b}"),
    }
}

fn input_mask(name: &str) -> u64 {
    match name {
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
        other => panic!("unknown input mode name {other}"),
    }
}

fn rgb_str(c: (u8, u8, u8)) -> String {
    format!("{},{},{}", c.0, c.1, c.2)
}

impl H {
    fn eq<T: PartialEq + std::fmt::Debug>(&self, what: &str, got: T, want: T) {
        assert!(
            got == want,
            "[{} @ {:?}] {what}: got {got:?}, want {want:?}",
            self.id,
            self.ch
        );
    }
    fn bytes(&mut self, b: &[u8]) {
        for c in split(b, self.ch, self.id) {
            self.term.feed(&c, self.now);
        }
    }
    fn feed(&mut self, s: &str) {
        self.bytes(s.as_bytes());
    }
    fn feed_hex(&mut self, s: &str) {
        let h: String = s.chars().filter(|c| !c.is_whitespace()).collect();
        let b: Vec<u8> = (0..h.len() / 2)
            .map(|i| u8::from_str_radix(&h[2 * i..2 * i + 2], 16).unwrap())
            .collect();
        self.bytes(&b);
    }
    fn feed_repeat(&mut self, s: &str, n: usize) {
        self.bytes(s.repeat(n).as_bytes());
    }
    fn advance(&mut self, ms: u64) {
        self.now += Duration::from_millis(ms);
        self.term.tick(self.now);
    }
    fn resize(&mut self, r: usize, c: usize) {
        self.term.resize(r, c);
    }
    fn cell_px(&mut self, w: u32, h: u32) {
        self.term.set_cell_px(w, h);
    }
    fn reset(&mut self) {
        self.term.reset();
    }
    fn grid(&mut self, want: &[&str]) {
        assert!(want.len() <= self.term.rows(), "more grid lines than rows");
        for y in 0..self.term.rows() {
            let got = row_text(self.term.grid(), y);
            let w = want.get(y).copied().unwrap_or("");
            self.eq(
                &format!("row {y}"),
                got.trim_end_matches(' ').to_string(),
                w.trim_end_matches(' ').to_string(),
            );
        }
    }
    fn row(&mut self, y: usize, want: &str) {
        let got = row_text(self.term.grid(), y);
        self.eq(
            &format!("row {y}"),
            got.trim_end_matches(' ').to_string(),
            want.trim_end_matches(' ').to_string(),
        );
    }
    fn cursor(&mut self, y: usize, x: usize) {
        let c = self.term.cursor();
        self.eq("cursor", (c.y, c.x), (y, x));
    }
    fn cursor_wrap(&mut self, b: bool) {
        self.eq("cursor wrap", self.term.cursor().wrap_pending, b);
    }
    fn cursor_visible(&mut self, b: bool) {
        self.eq("cursor visible", self.term.cursor().visible, b);
    }
    fn cursor_shape(&mut self, s: i64) {
        self.eq("cursor shape", i64::from(self.term.cursor().style.shape), s);
    }
    fn cursor_blink(&mut self, b: bool) {
        self.eq("cursor blink", self.term.cursor().style.blink, b);
    }
    fn cell(&mut self, y: usize, x: usize, props: &[(&str, &str)]) {
        let mut row = SnapRow::default();
        self.term.grid().copy_row(y, &mut row);
        let c = row.cells[x];
        let at = format!("cell {y} {x}");
        for &(k, v) in props {
            let b = v == "true";
            match k {
                "ch" => {
                    let mut cps = Vec::new();
                    self.term.grid().cell_text(y, x, &mut cps);
                    let got: String = if cps.is_empty() {
                        " ".into()
                    } else {
                        cps.iter().filter_map(|&c| char::from_u32(c)).collect()
                    };
                    self.eq(&format!("{at} ch"), got, v.to_string());
                }
                "fg" => self.eq(&format!("{at} fg"), color_str(c.style.fg), v.to_string()),
                "bg" => self.eq(&format!("{at} bg"), color_str(c.style.bg), v.to_string()),
                "ul" => self.eq(
                    &format!("{at} ul"),
                    color_str(c.style.ul_color),
                    v.to_string(),
                ),
                "underline" => {
                    let got = match c.style.underline {
                        Underline::None => "none",
                        Underline::Single => "single",
                        Underline::Double => "double",
                        Underline::Curly => "curly",
                        Underline::Dotted => "dotted",
                        Underline::Dashed => "dashed",
                    };
                    self.eq(&format!("{at} underline"), got, v);
                }
                "wide" => {
                    let got = match c.wide {
                        Wide::Narrow => "narrow",
                        Wide::Wide => "wide",
                        Wide::SpacerTail => "tail",
                        Wide::SpacerHead => "head",
                    };
                    self.eq(&format!("{at} wide"), got, v);
                }
                "semantic" => {
                    let got = match c.semantic {
                        apoth_grid::Semantic::Output => "output",
                        apoth_grid::Semantic::Input => "input",
                        apoth_grid::Semantic::Prompt => "prompt",
                    };
                    self.eq(&format!("{at} semantic"), got, v);
                }
                "link" => self.eq(&format!("{at} link"), c.link, b),
                "protected" => self.eq(&format!("{at} protected"), c.protected, b),
                a => {
                    let mask = match a {
                        "bold" => apoth_grid::Attrs::BOLD,
                        "faint" => apoth_grid::Attrs::FAINT,
                        "italic" => apoth_grid::Attrs::ITALIC,
                        "blink" => apoth_grid::Attrs::BLINK,
                        "inverse" => apoth_grid::Attrs::INVERSE,
                        "invisible" => apoth_grid::Attrs::INVISIBLE,
                        "strike" => apoth_grid::Attrs::STRIKE,
                        "overline" => apoth_grid::Attrs::OVERLINE,
                        other => panic!("{at}: unknown property {other}"),
                    };
                    self.eq(&format!("{at} {a}"), c.style.attrs.contains(mask), b);
                }
            }
        }
    }
    fn size(&mut self, r: usize, c: usize) {
        self.eq("size", (self.term.rows(), self.term.cols()), (r, c));
    }
    fn mode(&mut self, spec: &str, want: &str) {
        let (private, num) = match spec.strip_prefix('?') {
            Some(r) => (true, r),
            None => (false, spec),
        };
        let st = self.term.mode_state(!private, num.parse().unwrap());
        self.eq(&format!("mode {spec}"), st.name(), want);
    }
    fn replies(&mut self, want: &str) {
        let got = self.term.replies().take_all();
        self.eq("replies", show(&got), show(want.as_bytes()));
    }
    fn events(&mut self, want: &[&str]) {
        let got = self.term.events().render();
        self.term.events().clear();
        let want: Vec<String> = want.iter().map(|s| (*s).to_string()).collect();
        self.eq("events", got, want);
    }
    fn history(&mut self, n: usize) {
        self.eq("history rows", self.term.primary().history_rows(), n);
    }
    fn alt_history(&mut self, n: usize) {
        self.eq("alt history rows", self.term.alternate().history_rows(), n);
    }
    fn history_row(&mut self, i: usize, want: &str) {
        let got = abs_row_text(self.term.primary(), i).expect("history row out of range");
        self.eq(
            &format!("history row {i}"),
            got.trim_end_matches(' ').to_string(),
            want.trim_end_matches(' ').to_string(),
        );
    }
    fn screen(&mut self, which: &str) {
        self.eq("screen", self.term.on_alternate(), which == "alt");
    }
    fn hold(&mut self, b: bool) {
        self.eq("hold", self.term.presentation_hold(), b);
    }
    fn deadline_ms(&mut self, want: Option<i64>) {
        let got = self
            .term
            .next_deadline()
            .map(|d| d.duration_since(self.base).as_millis() as i64);
        self.eq("deadline", got, want);
    }
    fn input(&mut self, names: &[&str], on: bool) {
        let m = self.term.input_mirror().load();
        for n in names {
            self.eq(&format!("input {n}"), m.has(input_mask(n)), on);
        }
    }
    fn kitty(&mut self, n: u32) {
        self.eq(
            "kitty flags",
            self.term.input_mirror().load().kitty_flags(),
            n,
        );
    }
    fn mok(&mut self, n: i64) {
        self.eq(
            "mok level",
            i64::from(self.term.input_mirror().load().mok_level()),
            n,
        );
    }
    fn title(&mut self, s: &str) {
        self.eq("title", self.term.title().to_string(), s.to_string());
    }
    fn title_len(&mut self, n: usize) {
        self.eq("title len", self.term.title().len(), n);
    }
    fn palette(&mut self, i: u8, want: &str) {
        self.eq(
            &format!("palette {i}"),
            rgb_str(self.term.palette(i)),
            want.to_string(),
        );
    }
    fn color(&mut self, which: &str, want: &str) {
        let (fg, bg, cur) = self.term.colors();
        let got = match which {
            "fg" => fg,
            "bg" => bg,
            _ => cur,
        };
        self.eq("color", rgb_str(got), want.to_string());
    }
    fn dropped_replies(&mut self, n: u64) {
        let d = self.term.replies().dropped();
        self.eq("dropped replies", d, n);
    }
}

fn check(
    id: &'static str,
    rows: usize,
    cols: usize,
    cfg: impl Fn(&mut TermConfig),
    body: impl Fn(&mut H),
) {
    for &ch in CHUNKINGS {
        let mut c = TermConfig {
            rows,
            cols,
            ..TermConfig::default()
        };
        cfg(&mut c);
        let base = Instant::now();
        let mut h = H {
            term: Term::new(c),
            now: base,
            base,
            ch,
            id,
        };
        body(&mut h);
    }
}
#[test]
fn esc_ris_resets_modes_and_mirror() {
    check(
        "esc-ris-resets-modes-and-mirror",
        2,
        5,
        |_| {},
        |h| {
            h.feed("\u{1b}[?25l\u{1b}[?1h\u{1b}c");
            h.mode("?25", "set");
            h.mode("?1", "reset");
            h.input(&["decckm"], false);
        },
    );
}

#[test]
fn esc_deckpam_deckpnm() {
    check(
        "esc-deckpam-deckpnm",
        1,
        4,
        |_| {},
        |h| {
            h.feed("\u{1b}=");
            h.input(&["deckpam"], true);
            h.mode("?66", "set");
            h.feed("\u{1b}>");
            h.input(&["deckpam"], false);
            h.mode("?66", "reset");
        },
    );
}

#[test]
fn w_ambiguous_wide_by_config() {
    check(
        "w-ambiguous-wide-by-config",
        1,
        4,
        |c| {
            c.widths.ambiguous = apoth_unicode::Ambiguous::Wide;
        },
        |h| {
            h.feed("±x");
            h.cursor(0, 3);
        },
    );
}

#[test]
fn w_wcswidth_combining_zero() {
    check(
        "w-wcswidth-combining-zero",
        1,
        4,
        |c| {
            c.widths.method = apoth_unicode::WidthMethod::Wcswidth;
        },
        |h| {
            h.feed("éx");
            h.cursor(0, 2);
        },
    );
}

#[test]
fn w_wcswidth_zwj_sums_parts() {
    check(
        "w-wcswidth-zwj-sums-parts",
        1,
        8,
        |c| {
            c.widths.method = apoth_unicode::WidthMethod::Wcswidth;
        },
        |h| {
            h.feed("👨‍👩");
            h.cursor(0, 4);
        },
    );
}

#[test]
fn mode_roundtrip_dec_2027() {
    check(
        "mode-roundtrip-dec-2027",
        2,
        4,
        |c| {
            c.widths.method = apoth_unicode::WidthMethod::Wcswidth;
        },
        |h| {
            h.feed("\u{1b}[?2027h");
            h.mode("?2027", "set");
            h.feed("\u{1b}[?2027l");
            h.mode("?2027", "reset");
        },
    );
}

#[test]
fn mode_2027_set_uses_grapheme_width() {
    check(
        "mode-2027-set-uses-grapheme-width",
        1,
        6,
        |c| {
            c.widths.method = apoth_unicode::WidthMethod::Wcswidth;
        },
        |h| {
            h.feed("\u{1b}[?2027h");
            h.mode("?2027", "set");
        },
    );
}

#[test]
fn input_decckm() {
    check(
        "input-decckm",
        2,
        4,
        |_| {},
        |h| {
            h.input(&["decckm"], false);
            h.feed("\u{1b}[?1h");
            h.input(&["decckm"], true);
            h.feed("\u{1b}[?1l");
            h.input(&["decckm"], false);
        },
    );
}

#[test]
fn input_bpaste() {
    check(
        "input-bpaste",
        2,
        4,
        |_| {},
        |h| {
            h.input(&["bpaste"], false);
            h.feed("\u{1b}[?2004h");
            h.input(&["bpaste"], true);
            h.feed("\u{1b}[?2004l");
            h.input(&["bpaste"], false);
        },
    );
}

#[test]
fn input_m9() {
    check(
        "input-m9",
        2,
        4,
        |_| {},
        |h| {
            h.input(&["m9"], false);
            h.feed("\u{1b}[?9h");
            h.input(&["m9"], true);
            h.feed("\u{1b}[?9l");
            h.input(&["m9"], false);
        },
    );
}

#[test]
fn input_m1000() {
    check(
        "input-m1000",
        2,
        4,
        |_| {},
        |h| {
            h.input(&["m1000"], false);
            h.feed("\u{1b}[?1000h");
            h.input(&["m1000"], true);
            h.feed("\u{1b}[?1000l");
            h.input(&["m1000"], false);
        },
    );
}

#[test]
fn input_m1002() {
    check(
        "input-m1002",
        2,
        4,
        |_| {},
        |h| {
            h.input(&["m1002"], false);
            h.feed("\u{1b}[?1002h");
            h.input(&["m1002"], true);
            h.feed("\u{1b}[?1002l");
            h.input(&["m1002"], false);
        },
    );
}

#[test]
fn input_m1003() {
    check(
        "input-m1003",
        2,
        4,
        |_| {},
        |h| {
            h.input(&["m1003"], false);
            h.feed("\u{1b}[?1003h");
            h.input(&["m1003"], true);
            h.feed("\u{1b}[?1003l");
            h.input(&["m1003"], false);
        },
    );
}

#[test]
fn input_focus() {
    check(
        "input-focus",
        2,
        4,
        |_| {},
        |h| {
            h.input(&["focus"], false);
            h.feed("\u{1b}[?1004h");
            h.input(&["focus"], true);
            h.feed("\u{1b}[?1004l");
            h.input(&["focus"], false);
        },
    );
}

#[test]
fn input_m1005() {
    check(
        "input-m1005",
        2,
        4,
        |_| {},
        |h| {
            h.input(&["m1005"], false);
            h.feed("\u{1b}[?1005h");
            h.input(&["m1005"], true);
            h.feed("\u{1b}[?1005l");
            h.input(&["m1005"], false);
        },
    );
}

#[test]
fn input_sgr() {
    check(
        "input-sgr",
        2,
        4,
        |_| {},
        |h| {
            h.input(&["sgr"], false);
            h.feed("\u{1b}[?1006h");
            h.input(&["sgr"], true);
            h.feed("\u{1b}[?1006l");
            h.input(&["sgr"], false);
        },
    );
}

#[test]
fn input_m1015() {
    check(
        "input-m1015",
        2,
        4,
        |_| {},
        |h| {
            h.input(&["m1015"], false);
            h.feed("\u{1b}[?1015h");
            h.input(&["m1015"], true);
            h.feed("\u{1b}[?1015l");
            h.input(&["m1015"], false);
        },
    );
}

#[test]
fn input_pixels() {
    check(
        "input-pixels",
        2,
        4,
        |_| {},
        |h| {
            h.input(&["pixels"], false);
            h.feed("\u{1b}[?1016h");
            h.input(&["pixels"], true);
            h.feed("\u{1b}[?1016l");
            h.input(&["pixels"], false);
        },
    );
}

#[test]
fn input_decbkm() {
    check(
        "input-decbkm",
        2,
        4,
        |_| {},
        |h| {
            h.input(&["decbkm"], false);
            h.feed("\u{1b}[?67h");
            h.input(&["decbkm"], true);
            h.feed("\u{1b}[?67l");
            h.input(&["decbkm"], false);
        },
    );
}

#[test]
fn input_altscroll() {
    check(
        "input-altscroll",
        2,
        4,
        |_| {},
        |h| {
            h.input(&["altscroll"], true);
            h.feed("\u{1b}[?1007l");
            h.input(&["altscroll"], false);
            h.feed("\u{1b}[?1007h");
            h.input(&["altscroll"], true);
        },
    );
}

#[test]
fn input_alt_esc() {
    check(
        "input-alt-esc",
        2,
        4,
        |_| {},
        |h| {
            h.input(&["alt-esc"], true);
            h.feed("\u{1b}[?1036l");
            h.input(&["alt-esc"], false);
            h.feed("\u{1b}[?1036h");
            h.input(&["alt-esc"], true);
        },
    );
}

#[test]
fn input_deckpam_by_esc_and_mode() {
    check(
        "input-deckpam-by-esc-and-mode",
        2,
        4,
        |_| {},
        |h| {
            h.feed("\u{1b}=");
            h.input(&["deckpam"], true);
            h.feed("\u{1b}[?66l");
            h.input(&["deckpam"], false);
            h.feed("\u{1b}[?66h");
            h.input(&["deckpam"], true);
        },
    );
}

#[test]
fn input_altscreen_bit_follows_1049() {
    check(
        "input-altscreen-bit-follows-1049",
        2,
        4,
        |_| {},
        |h| {
            h.input(&["altscreen"], false);
            h.feed("\u{1b}[?1049h");
            h.input(&["altscreen"], true);
            h.feed("\u{1b}[?1049l");
            h.input(&["altscreen"], false);
        },
    );
}

#[test]
fn input_altscreen_bit_follows_47() {
    check(
        "input-altscreen-bit-follows-47",
        2,
        4,
        |_| {},
        |h| {
            h.feed("\u{1b}[?47h");
            h.input(&["altscreen"], true);
            h.feed("\u{1b}[?47l");
            h.input(&["altscreen"], false);
        },
    );
}

#[test]
fn input_altscreen_bit_follows_1047() {
    check(
        "input-altscreen-bit-follows-1047",
        2,
        4,
        |_| {},
        |h| {
            h.feed("\u{1b}[?1047h");
            h.input(&["altscreen"], true);
            h.feed("\u{1b}[?1047l");
            h.input(&["altscreen"], false);
        },
    );
}

#[test]
fn input_defaults() {
    check(
        "input-defaults",
        2,
        4,
        |_| {},
        |h| {
            h.input(&["altscroll", "alt-esc"], true);
            h.input(
                &[
                    "decckm",
                    "deckpam",
                    "bpaste",
                    "m1000",
                    "m1002",
                    "m1003",
                    "focus",
                    "sgr",
                    "altscreen",
                ],
                false,
            );
        },
    );
}

#[test]
fn input_multiple_mouse_modes_independent() {
    check(
        "input-multiple-mouse-modes-independent",
        2,
        4,
        |_| {},
        |h| {
            h.feed("\u{1b}[?1000h\u{1b}[?1006h");
            h.input(&["m1000", "sgr"], true);
            h.input(&["m1002"], false);
            h.feed("\u{1b}[?1000l");
            h.input(&["m1000"], false);
            h.input(&["sgr"], true);
        },
    );
}

#[test]
fn input_ris_clears_mirror() {
    check(
        "input-ris-clears-mirror",
        2,
        4,
        |_| {},
        |h| {
            h.feed("\u{1b}[?1h\u{1b}[?2004h\u{1b}[?1000h\u{1b}c");
            h.input(&["decckm", "m1000"], false);
        },
    );
}

#[test]
fn input_decstr_clears_mirror() {
    check(
        "input-decstr-clears-mirror",
        2,
        4,
        |_| {},
        |h| {
            h.feed("\u{1b}[?1h\u{1b}[?2004h\u{1b}[!p");
            h.input(&["decckm"], false);
        },
    );
}

#[test]
fn input_stored_before_next_byte() {
    check(
        "input-stored-before-next-byte",
        2,
        4,
        |_| {},
        |h| {
            h.feed("\u{1b}[?1h");
            h.input(&["decckm"], true);
            h.feed("\u{1b}[?");
            h.feed("1l");
            h.input(&["decckm"], false);
        },
    );
}

#[test]
fn input_mok_set() {
    check(
        "input-mok-set",
        2,
        4,
        |_| {},
        |h| {
            h.mok(0);
            h.feed("\u{1b}[>4;2m");
            h.mok(2);
            h.feed("\u{1b}[>4;0m");
            h.mok(0);
        },
    );
}

#[test]
fn input_mok_level_1() {
    check(
        "input-mok-level-1",
        2,
        4,
        |_| {},
        |h| {
            h.feed("\u{1b}[>4;1m");
            h.mok(1);
        },
    );
}

#[test]
fn input_mok_reset_by_ris() {
    check(
        "input-mok-reset-by-ris",
        2,
        4,
        |_| {},
        |h| {
            h.feed("\u{1b}[>4;2m\u{1b}c");
            h.mok(0);
        },
    );
}

#[test]
fn input_mok_bare_resets() {
    check(
        "input-mok-bare-resets",
        2,
        4,
        |_| {},
        |h| {
            h.feed("\u{1b}[>4;2m\u{1b}[>4m");
            h.mok(0);
        },
    );
}

#[test]
fn kitty_default_zero() {
    check(
        "kitty-default-zero",
        2,
        4,
        |_| {},
        |h| {
            h.kitty(0);
        },
    );
}

#[test]
fn kitty_push() {
    check(
        "kitty-push",
        2,
        4,
        |_| {},
        |h| {
            h.feed("\u{1b}[>5u");
            h.kitty(5);
        },
    );
}

#[test]
fn kitty_push_pop() {
    check(
        "kitty-push-pop",
        2,
        4,
        |_| {},
        |h| {
            h.feed("\u{1b}[>5u\u{1b}[>3u");
            h.kitty(3);
            h.feed("\u{1b}[<u");
            h.kitty(5);
            h.feed("\u{1b}[<u");
            h.kitty(0);
        },
    );
}

#[test]
fn kitty_pop_count() {
    check(
        "kitty-pop-count",
        2,
        4,
        |_| {},
        |h| {
            h.feed("\u{1b}[>1u\u{1b}[>2u\u{1b}[>4u\u{1b}[<2u");
            h.kitty(1);
        },
    );
}

#[test]
fn kitty_pop_empty_is_safe() {
    check(
        "kitty-pop-empty-is-safe",
        2,
        4,
        |_| {},
        |h| {
            h.feed("\u{1b}[<9u");
            h.kitty(0);
        },
    );
}

#[test]
fn kitty_set_replace() {
    check(
        "kitty-set-replace",
        2,
        4,
        |_| {},
        |h| {
            h.feed("\u{1b}[>1u\u{1b}[=6;1u");
            h.kitty(6);
        },
    );
}

#[test]
fn kitty_set_or() {
    check(
        "kitty-set-or",
        2,
        4,
        |_| {},
        |h| {
            h.feed("\u{1b}[>1u\u{1b}[=6;2u");
            h.kitty(7);
        },
    );
}

#[test]
fn kitty_set_and_not() {
    check(
        "kitty-set-and-not",
        2,
        4,
        |_| {},
        |h| {
            h.feed("\u{1b}[>7u\u{1b}[=2;3u");
            h.kitty(5);
        },
    );
}

#[test]
fn kitty_stack_is_per_screen() {
    check(
        "kitty-stack-is-per-screen",
        2,
        4,
        |_| {},
        |h| {
            h.feed("\u{1b}[>1u\u{1b}[?1049h");
            h.kitty(0);
            h.feed("\u{1b}[>4u");
            h.kitty(4);
            h.feed("\u{1b}[?1049l");
            h.kitty(1);
        },
    );
}

#[test]
fn kitty_stack_bounded() {
    check(
        "kitty-stack-bounded",
        2,
        4,
        |_| {},
        |h| {
            h.feed("\u{1b}[>1u\u{1b}[>1u\u{1b}[>1u\u{1b}[>1u\u{1b}[>1u\u{1b}[>1u\u{1b}[>1u\u{1b}[>1u\u{1b}[>1u\u{1b}[>1u\u{1b}[>1u\u{1b}[>1u");
            h.kitty(1);
            h.feed("\u{1b}[<99u");
            h.kitty(0);
        },
    );
}

#[test]
fn kitty_ris_clears_stack() {
    check(
        "kitty-ris-clears-stack",
        2,
        4,
        |_| {},
        |h| {
            h.feed("\u{1b}[>5u\u{1b}c");
            h.kitty(0);
        },
    );
}

#[test]
fn alt_has_no_scrollback() {
    check(
        "alt-has-no-scrollback",
        2,
        5,
        |_| {},
        |h| {
            h.feed("\u{1b}[?1049ha\u{d}\u{a}b\u{d}\u{a}c\u{d}\u{a}d");
            h.alt_history(0);
            h.history(0);
        },
    );
}

#[test]
fn osc_title_semicolons_kept() {
    check(
        "osc-title-semicolons-kept",
        2,
        8,
        |_| {},
        |h| {
            h.feed("\u{1b}]0;a;b;c\u{7}");
            h.title("a;b;c");
        },
    );
}

#[test]
fn osc_title_controls_stripped() {
    check(
        "osc-title-controls-stripped",
        2,
        8,
        |_| {},
        |h| {
            h.feed("\u{1b}]0;a\u{1}b\u{7f}c\u{7}");
            h.title("abc");
        },
    );
}

#[test]
fn osc_title_utf8() {
    check(
        "osc-title-utf8",
        2,
        8,
        |_| {},
        |h| {
            h.feed("\u{1b}]0;héllo ✓\u{7}");
            h.title("héllo ✓");
        },
    );
}

#[test]
fn osc_title_last_wins() {
    check(
        "osc-title-last-wins",
        2,
        8,
        |_| {},
        |h| {
            h.feed("\u{1b}]0;a\u{7}\u{1b}]0;b\u{7}");
            h.title("b");
            h.events(&["title b"]);
        },
    );
}

#[test]
fn osc_title_pop_empty_stack_keeps_title() {
    check(
        "osc-title-pop-empty-stack-keeps-title",
        2,
        8,
        |_| {},
        |h| {
            h.feed("\u{1b}]0;A\u{7}\u{1b}[23t");
            h.title("A");
        },
    );
}

#[test]
fn osc_title_4096_accepted() {
    check(
        "osc-title-4096-accepted",
        1,
        8,
        |_| {},
        |h| {
            h.feed("\u{1b}]0;");
            h.feed_repeat("a", 4096);
            h.feed("\u{7}");
            h.title_len(4096);
        },
    );
}

#[test]
fn osc_color_set_fg_query() {
    check(
        "osc-color-set-fg-query",
        1,
        8,
        |_| {},
        |h| {
            h.feed("\u{1b}]10;rgb:ff/00/00\u{7}\u{1b}]10;?\u{7}");
            h.color("fg", "255,0,0");
            h.replies("\u{1b}]10;rgb:ffff/0000/0000\u{7}");
        },
    );
}

#[test]
fn osc_color_set_bg() {
    check(
        "osc-color-set-bg",
        1,
        8,
        |_| {},
        |h| {
            h.feed("\u{1b}]11;#102030\u{7}");
            h.color("bg", "16,32,48");
        },
    );
}

#[test]
fn osc_color_set_cursor() {
    check(
        "osc-color-set-cursor",
        1,
        8,
        |_| {},
        |h| {
            h.feed("\u{1b}]12;rgb:10/20/30\u{7}");
            h.color("cursor", "16,32,48");
        },
    );
}

#[test]
fn osc_color_palette_set() {
    check(
        "osc-color-palette-set",
        1,
        8,
        |_| {},
        |h| {
            h.feed("\u{1b}]4;1;rgb:12/34/56\u{7}");
            h.palette(1, "18,52,86");
        },
    );
}

#[test]
fn osc_color_palette_multiple_pairs() {
    check(
        "osc-color-palette-multiple-pairs",
        1,
        8,
        |_| {},
        |h| {
            h.feed("\u{1b}]4;1;#ff0000;2;#00ff00\u{7}");
            h.palette(1, "255,0,0");
            h.palette(2, "0,255,0");
        },
    );
}

#[test]
fn osc_color_palette_hash_short() {
    check(
        "osc-color-palette-hash-short",
        1,
        8,
        |_| {},
        |h| {
            h.feed("\u{1b}]4;3;#f00\u{7}");
            h.palette(3, "255,0,0");
        },
    );
}

#[test]
fn osc_color_palette_reset_one() {
    check(
        "osc-color-palette-reset-one",
        1,
        8,
        |_| {},
        |h| {
            h.feed("\u{1b}]4;196;rgb:00/00/00\u{7}");
            h.palette(196, "0,0,0");
            h.feed("\u{1b}]104;196\u{7}");
            h.palette(196, "255,0,0");
        },
    );
}

#[test]
fn osc_color_palette_reset_all() {
    check(
        "osc-color-palette-reset-all",
        1,
        8,
        |_| {},
        |h| {
            h.feed("\u{1b}]4;196;rgb:00/00/00\u{7}\u{1b}]104\u{7}");
            h.palette(196, "255,0,0");
        },
    );
}

#[test]
fn osc_color_cube_default() {
    check(
        "osc-color-cube-default",
        1,
        8,
        |_| {},
        |h| {
            h.palette(16, "0,0,0");
            h.palette(231, "255,255,255");
            h.palette(196, "255,0,0");
        },
    );
}

#[test]
fn osc_color_fg_reports_changed() {
    check(
        "osc-color-fg-reports-changed",
        1,
        8,
        |_| {},
        |h| {
            h.feed("\u{1b}]10;#ffffff\u{7}");
            h.events(&["colors"]);
        },
    );
}

#[test]
fn osc_52_write_disabled_by_config() {
    check(
        "osc-52-write-disabled-by-config",
        1,
        8,
        |c| {
            c.osc52_write = false;
        },
        |h| {
            h.feed("\u{1b}]52;c;aGk=\u{7}");
            h.events(&[]);
        },
    );
}

#[test]
fn osc_52_over_max_dropped() {
    check(
        "osc-52-over-max-dropped",
        1,
        8,
        |c| {
            c.osc52_max = 8;
        },
        |h| {
            h.feed("\u{1b}]52;c;aGVsbG8gd29ybGQ=\u{7}");
            h.events(&[]);
        },
    );
}

#[test]
fn osc_52_under_max_accepted() {
    check(
        "osc-52-under-max-accepted",
        1,
        8,
        |c| {
            c.osc52_max = 64;
        },
        |h| {
            h.feed("\u{1b}]52;c;aGVsbG8=\u{7}");
            h.events(&["clip c 68656c6c6f"]);
        },
    );
}

#[test]
fn osc_133_semantic_cells() {
    check(
        "osc-133-semantic-cells",
        1,
        12,
        |_| {},
        |h| {
            h.feed("\u{1b}]133;A\u{7}$ \u{1b}]133;B\u{7}cmd\u{1b}]133;C\u{7}out");
            h.cell(0, 0, &[("semantic", "prompt")]);
            h.cell(0, 2, &[("semantic", "input")]);
            h.cell(0, 5, &[("semantic", "output")]);
        },
    );
}

#[test]
fn reply_da_1_sixel_advertised() {
    check(
        "reply-da-1-sixel-advertised",
        2,
        4,
        |c| {
            c.sixel_advertised = true;
        },
        |h| {
            h.feed("\u{1b}[c");
            h.replies("\u{1b}[?62;4;22c");
        },
    );
}

#[test]
fn reply_dsr_996_dark() {
    check(
        "reply-dsr-996-dark",
        2,
        4,
        |c| {
            c.dark = true;
        },
        |h| {
            h.feed("\u{1b}[?996n");
            h.replies("\u{1b}[?997;1n");
        },
    );
}

#[test]
fn reply_dsr_996_light() {
    check(
        "reply-dsr-996-light",
        2,
        4,
        |c| {
            c.dark = false;
        },
        |h| {
            h.feed("\u{1b}[?996n");
            h.replies("\u{1b}[?997;2n");
        },
    );
}

#[test]
fn reply_xtwinops_14_config_cell_px() {
    check(
        "reply-xtwinops-14-config-cell-px",
        5,
        10,
        |c| {
            c.cell_px = (10, 20);
        },
        |h| {
            h.feed("\u{1b}[14t");
            h.replies("\u{1b}[4;100;100t");
        },
    );
}

#[test]
fn reply_xtwinops_cell_px_after_set() {
    check(
        "reply-xtwinops-cell-px-after-set",
        5,
        10,
        |_| {},
        |h| {
            h.cell_px(10, 20);
            h.feed("\u{1b}[16t");
            h.replies("\u{1b}[6;20;10t");
        },
    );
}

#[test]
fn sync_deadline_from_config() {
    check(
        "sync-deadline-from-config",
        2,
        4,
        |c| {
            c.sync_timeout = Duration::from_millis(200);
        },
        |h| {
            h.feed("\u{1b}[?2026h");
            h.deadline_ms(Some(200));
        },
    );
}

#[test]
fn sync_expires_at_deadline() {
    check(
        "sync-expires-at-deadline",
        2,
        4,
        |c| {
            c.sync_timeout = Duration::from_millis(200);
        },
        |h| {
            h.feed("\u{1b}[?2026h");
            h.advance(199);
            h.hold(true);
            h.advance(1);
            h.hold(false);
            h.mode("?2026", "reset");
        },
    );
}

#[test]
fn sync_timeout_clamped_low() {
    check(
        "sync-timeout-clamped-low",
        2,
        4,
        |c| {
            c.sync_timeout = Duration::from_millis(10);
        },
        |h| {
            h.feed("\u{1b}[?2026h");
            h.deadline_ms(Some(100));
        },
    );
}

#[test]
fn sync_timeout_clamped_high() {
    check(
        "sync-timeout-clamped-high",
        2,
        4,
        |c| {
            c.sync_timeout = Duration::from_millis(60000);
        },
        |h| {
            h.feed("\u{1b}[?2026h");
            h.deadline_ms(Some(5000));
        },
    );
}

#[test]
fn sync_no_deadline_when_idle() {
    check(
        "sync-no-deadline-when-idle",
        2,
        4,
        |_| {},
        |h| {
            h.deadline_ms(None);
        },
    );
}

#[test]
fn sync_double_set_keeps_first_deadline() {
    check(
        "sync-double-set-keeps-first-deadline",
        2,
        4,
        |c| {
            c.sync_timeout = Duration::from_millis(200);
        },
        |h| {
            h.feed("\u{1b}[?2026h");
            h.advance(100);
            h.feed("\u{1b}[?2026h");
            h.deadline_ms(Some(200));
        },
    );
}

#[test]
fn never_reply_flood_unbounded() {
    check(
        "never-reply-flood-unbounded",
        1,
        4,
        |_| {},
        |h| {
            h.feed_repeat("\u{1b}[c", 10000);
            h.dropped_replies(2719);
        },
    );
}

#[test]
fn cap_osc_title_4096_accepted() {
    check(
        "cap-osc-title-4096-accepted",
        1,
        4,
        |_| {},
        |h| {
            h.feed("\u{1b}]2;");
            h.feed_repeat("b", 4096);
            h.feed("\u{1b}\\");
            h.title_len(4096);
        },
    );
}

#[test]
fn cap_osc_title_4097_dropped() {
    check(
        "cap-osc-title-4097-dropped",
        1,
        4,
        |_| {},
        |h| {
            h.feed("\u{1b}]2;");
            h.feed_repeat("b", 4097);
            h.feed("\u{1b}\\");
            h.title_len(0);
        },
    );
}

#[test]
fn cap_osc_title_over_keeps_previous() {
    check(
        "cap-osc-title-over-keeps-previous",
        1,
        4,
        |_| {},
        |h| {
            h.feed("\u{1b}]2;keep\u{7}\u{1b}]2;");
            h.feed_repeat("b", 5000);
            h.feed("\u{7}");
            h.title("keep");
        },
    );
}

#[test]
fn cap_osc_title_aborted_by_can_dropped() {
    check(
        "cap-osc-title-aborted-by-can-dropped",
        1,
        4,
        |_| {},
        |h| {
            h.feed("\u{1b}]2;abc\u{18}");
            h.title("");
        },
    );
}

#[test]
fn cap_osc_title_aborted_by_esc_csi_dropped() {
    check(
        "cap-osc-title-aborted-by-esc-csi-dropped",
        1,
        4,
        |_| {},
        |h| {
            h.feed("\u{1b}]2;abc\u{1b}[1mx");
            h.title("");
            h.grid(&["x"]);
        },
    );
}

#[test]
fn cap_osc_palette_over_dropped() {
    check(
        "cap-osc-palette-over-dropped",
        1,
        4,
        |_| {},
        |h| {
            h.feed("\u{1b}]4;1;#ff0000;");
            h.feed_repeat("2;#00ff00;", 600);
            h.feed("\u{7}");
            h.palette(1, "205,0,0");
        },
    );
}

#[test]
fn cap_osc52_over_configured_max() {
    check(
        "cap-osc52-over-configured-max",
        1,
        4,
        |c| {
            c.osc52_max = 16;
        },
        |h| {
            h.feed("\u{1b}]52;c;");
            h.feed_repeat("QUFB", 8);
            h.feed("\u{7}");
            h.events(&[]);
        },
    );
}

#[test]
fn cap_osc52_exactly_max_accepted() {
    check(
        "cap-osc52-exactly-max-accepted",
        1,
        4,
        |c| {
            c.osc52_max = 10;
        },
        |h| {
            h.feed("\u{1b}]52;c;QUFBQQ==\u{7}");
            h.events(&["clip c 41414141"]);
        },
    );
}

#[test]
fn cap_title_stack_bounded() {
    check(
        "cap-title-stack-bounded",
        1,
        4,
        |_| {},
        |h| {
            h.feed("\u{1b}]0;base\u{7}");
            h.feed_repeat("\u{1b}[22t", 20);
            h.feed("\u{1b}]0;x\u{7}");
            h.feed_repeat("\u{1b}[23t", 20);
            h.title("base");
        },
    );
}

#[test]
fn cap_oversized_string_then_normal_text() {
    check(
        "cap-oversized-string-then-normal-text",
        1,
        8,
        |_| {},
        |h| {
            h.feed("\u{1b}]0;");
            h.feed_repeat("z", 10000);
            h.feed("\u{7}ok");
            h.grid(&["ok"]);
            h.title_len(0);
        },
    );
}

#[test]
fn cap_reply_queue_65536_bytes() {
    check(
        "cap-reply-queue-65536-bytes",
        1,
        4,
        |_| {},
        |h| {
            h.feed_repeat("\u{1b}[5n", 20000);
            h.dropped_replies(3616);
        },
    );
}

#[test]
fn resize_cell_px_change_reflected_in_report() {
    check(
        "resize-cell-px-change-reflected-in-report",
        4,
        5,
        |_| {},
        |h| {
            h.cell_px(10, 20);
            h.resize(4, 5);
            h.feed("\u{1b}[14t");
            h.replies("\u{1b}[4;80;50t");
        },
    );
}

#[test]
fn scr_alt_has_no_history() {
    check(
        "scr-alt-has-no-history",
        2,
        3,
        |_| {},
        |h| {
            h.feed("\u{1b}[?1049ha\u{d}\u{a}b\u{d}\u{a}c");
            h.history(0);
            h.alt_history(0);
            h.grid(&["b", "c"]);
        },
    );
}
