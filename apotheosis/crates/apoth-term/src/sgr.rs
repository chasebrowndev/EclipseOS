// SPDX-License-Identifier: AGPL-3.0-only
//! SGR (T-03 §3.1, VTCORE-16, VTCORE-58): `;` and `:` forms, the ambiguity table is
//! normative. Unknown sub-parameter forms are consumed, never reinterpreted.

use apoth_grid::{Attrs, Color, Style, Underline};
use apoth_vt::Params;

fn comp(v: Option<u32>) -> u8 {
    v.unwrap_or(0).min(255) as u8
}

/// End index (inclusive) of the colon chain starting at `i`.
fn chain_end(p: &Params, i: usize) -> usize {
    let mut j = i;
    while j + 1 < p.len() && p.colon_after(j) {
        j += 1;
    }
    j
}

/// Parse the colour spec after a `38`/`48`/`58` at index `i`. Returns the new colour (if
/// any), the index of the last parameter consumed, and whether the sequence ends here.
fn colour(p: &Params, i: usize) -> (Option<Color>, usize, bool) {
    if p.colon_after(i) {
        let end = chain_end(p, i);
        let kind = p.get(i + 1);
        let after = end.saturating_sub(i + 1);
        let c = match kind {
            Some(2) if after == 4 => Some(Color::Rgb(
                comp(p.get(i + 3)),
                comp(p.get(i + 4)),
                comp(p.get(i + 5)),
            )),
            Some(2) if after == 3 => Some(Color::Rgb(
                comp(p.get(i + 2)),
                comp(p.get(i + 3)),
                comp(p.get(i + 4)),
            )),
            Some(5) if after == 1 => match p.get(i + 2) {
                Some(n) if n <= 255 => Some(Color::Palette(n as u8)),
                None => Some(Color::Palette(0)),
                _ => None,
            },
            _ => None,
        };
        return (c, end, false);
    }
    // `;` form (or a colon chain starting one parameter later: the mixed case).
    let Some(k) = (i + 1 < p.len()).then(|| p.get_or(i + 1, 0)) else {
        return (None, i, true);
    };
    let need = match k {
        2 => 4,
        5 => 2,
        _ => return (None, i + 1, false),
    };
    if i + need >= p.len() {
        return (None, p.len(), true);
    }
    let mixed = (i + 1..=i + need).any(|j| p.colon_after(j));
    if mixed {
        return (None, chain_end(p, i + 1).max(i + 1), false);
    }
    let c = if k == 2 {
        Some(Color::Rgb(
            comp(p.get(i + 2)),
            comp(p.get(i + 3)),
            comp(p.get(i + 4)),
        ))
    } else {
        match p.get(i + 2) {
            Some(n) if n <= 255 => Some(Color::Palette(n as u8)),
            None => Some(Color::Palette(0)),
            _ => None,
        }
    };
    (c, i + need, false)
}

/// Apply an SGR parameter list to `st`.
pub(crate) fn apply(st: &mut Style, p: &Params) {
    if p.is_empty() {
        *st = Style::DEFAULT;
        return;
    }
    let mut i = 0;
    while i < p.len() {
        let code = p.get(i).unwrap_or(0);
        let end = chain_end(p, i);
        if code == 4 {
            if end > i {
                match p.get(i + 1).unwrap_or(0) {
                    0 => st.underline = Underline::None,
                    1 => st.underline = Underline::Single,
                    2 => st.underline = Underline::Double,
                    3 => st.underline = Underline::Curly,
                    4 => st.underline = Underline::Dotted,
                    5 => st.underline = Underline::Dashed,
                    _ => {}
                }
            } else {
                st.underline = Underline::Single;
            }
            i = end + 1;
            continue;
        }
        if matches!(code, 38 | 48 | 58) {
            let (c, last, stop) = colour(p, i);
            if let Some(c) = c {
                match code {
                    38 => st.fg = c,
                    48 => st.bg = c,
                    _ => st.ul_color = c,
                }
            }
            if stop {
                return;
            }
            i = last + 1;
            continue;
        }
        if end > i {
            // A colon chain on any other code: consumed whole, no effect.
            i = end + 1;
            continue;
        }
        simple(st, code);
        i += 1;
    }
}

fn simple(st: &mut Style, code: u32) {
    let a = st.attrs;
    match code {
        0 => *st = Style::DEFAULT,
        1 => st.attrs = a.with(Attrs::BOLD, true),
        2 => st.attrs = a.with(Attrs::FAINT, true),
        3 => st.attrs = a.with(Attrs::ITALIC, true),
        5 | 6 => st.attrs = a.with(Attrs::BLINK, true),
        7 => st.attrs = a.with(Attrs::INVERSE, true),
        8 => st.attrs = a.with(Attrs::INVISIBLE, true),
        9 => st.attrs = a.with(Attrs::STRIKE, true),
        21 => st.underline = Underline::Double,
        22 => st.attrs = a.with(Attrs::BOLD | Attrs::FAINT, false),
        23 => st.attrs = a.with(Attrs::ITALIC, false),
        24 => st.underline = Underline::None,
        25 => st.attrs = a.with(Attrs::BLINK, false),
        27 => st.attrs = a.with(Attrs::INVERSE, false),
        28 => st.attrs = a.with(Attrs::INVISIBLE, false),
        29 => st.attrs = a.with(Attrs::STRIKE, false),
        30..=37 => st.fg = Color::Palette((code - 30) as u8),
        39 => st.fg = Color::Default,
        40..=47 => st.bg = Color::Palette((code - 40) as u8),
        49 => st.bg = Color::Default,
        53 => st.attrs = a.with(Attrs::OVERLINE, true),
        55 => st.attrs = a.with(Attrs::OVERLINE, false),
        59 => st.ul_color = Color::Default,
        90..=97 => st.fg = Color::Palette((code - 90 + 8) as u8),
        100..=107 => st.bg = Color::Palette((code - 100 + 8) as u8),
        _ => {}
    }
}

/// DECRQSS `m` body (without the final `m`): `0` then each active attribute.
pub(crate) fn report(st: &Style) -> String {
    let mut s = String::from("0");
    let at = |b| st.attrs.contains(b);
    if at(Attrs::BOLD) {
        s.push_str(";1");
    }
    if at(Attrs::FAINT) {
        s.push_str(";2");
    }
    if at(Attrs::ITALIC) {
        s.push_str(";3");
    }
    if st.underline != Underline::None {
        s.push_str(&format!(";4:{}", st.underline as u8));
    }
    if at(Attrs::BLINK) {
        s.push_str(";5");
    }
    if at(Attrs::INVERSE) {
        s.push_str(";7");
    }
    if at(Attrs::INVISIBLE) {
        s.push_str(";8");
    }
    if at(Attrs::STRIKE) {
        s.push_str(";9");
    }
    if at(Attrs::OVERLINE) {
        s.push_str(";53");
    }
    colour_report(&mut s, st.fg, 30, 90, 38);
    colour_report(&mut s, st.bg, 40, 100, 48);
    colour_report(&mut s, st.ul_color, 0, 0, 58);
    s
}

fn colour_report(s: &mut String, c: Color, lo: u32, hi: u32, ext: u32) {
    match c {
        Color::Default => {}
        Color::Palette(n) if lo != 0 && n < 8 => s.push_str(&format!(";{}", lo + u32::from(n))),
        Color::Palette(n) if lo != 0 && n < 16 => {
            s.push_str(&format!(";{}", hi + u32::from(n) - 8));
        }
        Color::Palette(n) => s.push_str(&format!(";{ext};5;{n}")),
        Color::Rgb(r, g, b) => s.push_str(&format!(";{ext};2;{r};{g};{b}")),
    }
}
