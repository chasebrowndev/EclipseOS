// SPDX-License-Identifier: AGPL-3.0-only
//! The notification body markup subset of the Desktop Notifications spec:
//! `<b>`, `<i>`, `<u>` and `<a>`. Every other tag is dropped and its text
//! kept, so a body is never shown with a raw tag in it.
//!
//! Shared by the toast stack (styled runs) and the bar's notification widget
//! (plain text, [`plain`]), so a body reads the same in both.
//!
//! A pure function from the body to styled runs; the view turns runs into
//! `rich_text` spans. Links are styled but never followed: a notification is
//! other people's text, and opening what it names is not this stack's call.

/// A stretch of body text in one style.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Run {
    pub text: String,
    pub bold: bool,
    pub italic: bool,
    /// Underlined by `<u>` or by being inside `<a>`.
    pub underline: bool,
}

#[derive(Default)]
struct Depth {
    bold: u32,
    italic: u32,
    underline: u32,
    link: u32,
}

impl Depth {
    fn adjust(&mut self, name: &str, open: bool) {
        let slot = match name {
            "b" => &mut self.bold,
            "i" => &mut self.italic,
            "u" => &mut self.underline,
            "a" => &mut self.link,
            _ => return,
        };
        *slot = if open { *slot + 1 } else { slot.saturating_sub(1) };
    }
}

/// Parse `src` into runs. Adjacent text of one style is one run.
pub fn parse(src: &str) -> Vec<Run> {
    let mut runs: Vec<Run> = Vec::new();
    let mut depth = Depth::default();
    let mut rest = src;

    let push = |runs: &mut Vec<Run>, depth: &Depth, text: &str| {
        if text.is_empty() {
            return;
        }
        let (bold, italic, underline) = (
            depth.bold > 0,
            depth.italic > 0,
            depth.underline > 0 || depth.link > 0,
        );
        match runs.last_mut() {
            Some(last) if (last.bold, last.italic, last.underline) == (bold, italic, underline) => {
                last.text.push_str(text);
            }
            _ => runs.push(Run {
                text: text.to_owned(),
                bold,
                italic,
                underline,
            }),
        }
    };

    while !rest.is_empty() {
        let next = rest.find(['<', '&']).unwrap_or(rest.len());
        push(&mut runs, &depth, &rest[..next]);
        rest = &rest[next..];
        let Some(first) = rest.chars().next() else {
            break;
        };
        if first == '<' {
            match rest.find('>').map(|end| (&rest[1..end], end)) {
                Some((inner, end)) if looks_like_tag(inner) => {
                    let closing = inner.starts_with('/');
                    let name = inner
                        .trim_start_matches('/')
                        .split(|c: char| c.is_whitespace() || c == '/')
                        .next()
                        .unwrap_or("")
                        .to_ascii_lowercase();
                    if name == "br" {
                        push(&mut runs, &depth, "\n");
                    } else {
                        depth.adjust(&name, !closing);
                    }
                    rest = &rest[end + 1..];
                }
                // A lone `<` that opens nothing is text.
                _ => {
                    push(&mut runs, &depth, "<");
                    rest = &rest[1..];
                }
            }
        } else {
            match entity(rest) {
                Some((ch, len)) => {
                    let mut buf = [0u8; 4];
                    push(&mut runs, &depth, ch.encode_utf8(&mut buf));
                    rest = &rest[len..];
                }
                None => {
                    push(&mut runs, &depth, "&");
                    rest = &rest[1..];
                }
            }
        }
    }
    runs
}

/// `src` with its markup stripped and entities decoded: what the runs say,
/// unstyled. For views that draw plain text.
pub fn plain(src: &str) -> String {
    parse(src).into_iter().map(|r| r.text).collect()
}

/// Cut `runs` to at most `lines` lines of about `chars` columns, word-wrapped
/// the way a text widget would roughly wrap them, ending the last kept line
/// with an ellipsis when text was left. A heuristic: glyph widths vary, so
/// callers pass a slightly conservative `chars`. Text that fits is returned
/// unchanged.
pub fn clamp(runs: &[Run], lines: usize, chars: usize) -> Vec<Run> {
    let flat: Vec<(char, usize)> = runs
        .iter()
        .enumerate()
        .flat_map(|(r, run)| run.text.chars().map(move |c| (c, r)))
        .collect();
    let n = flat.len();
    let (mut line, mut col, mut i) = (0usize, 0usize, 0usize);
    let mut cut: Option<usize> = None;
    'walk: while i < n {
        let c = flat[i].0;
        if c == '\n' {
            line += 1;
            col = 0;
            if line >= lines {
                if flat[i + 1..].iter().any(|(c, _)| !c.is_whitespace()) {
                    cut = Some(i);
                }
                break;
            }
            i += 1;
        } else if c.is_whitespace() {
            col += 1;
            i += 1;
        } else {
            let mut j = i;
            while j < n && !flat[j].0.is_whitespace() {
                j += 1;
            }
            if col > 0 && col + (j - i) > chars {
                line += 1;
                col = 0;
                if line >= lines {
                    cut = Some(i);
                    break;
                }
            }
            for k in i..j {
                if col >= chars {
                    line += 1;
                    col = 0;
                    if line >= lines {
                        cut = Some(k);
                        break 'walk;
                    }
                }
                col += 1;
            }
            i = j;
        }
    }
    let Some(end) = cut else {
        return runs.to_vec();
    };
    let mut kept = &flat[..end];
    while kept.last().is_some_and(|(c, _)| c.is_whitespace()) {
        kept = &kept[..kept.len() - 1];
    }
    let style = kept.last().map_or(0, |(_, r)| *r);
    let mut out: Vec<Run> = Vec::new();
    for &(c, r) in kept.iter().chain(std::iter::once(&('\u{2026}', style))) {
        let src = &runs[r];
        match out.last_mut() {
            Some(l) if (l.bold, l.italic, l.underline) == (src.bold, src.italic, src.underline) => {
                l.text.push(c);
            }
            _ => out.push(Run {
                text: c.to_string(),
                bold: src.bold,
                italic: src.italic,
                underline: src.underline,
            }),
        }
    }
    out
}

/// `a`, `/a`, `img src="..."`, `br/` — a letter after an optional slash.
fn looks_like_tag(inner: &str) -> bool {
    inner
        .trim_start_matches('/')
        .chars()
        .next()
        .is_some_and(|c| c.is_ascii_alphabetic())
}

/// The character an entity at the start of `s` stands for, and its length.
fn entity(s: &str) -> Option<(char, usize)> {
    let end = s.find(';').filter(|&e| e <= 8)?;
    let ch = match &s[1..end] {
        "amp" => '&',
        "lt" => '<',
        "gt" => '>',
        "quot" => '"',
        "apos" => '\'',
        num => {
            let code = if let Some(hex) = num.strip_prefix("#x").or_else(|| num.strip_prefix("#X")) {
                u32::from_str_radix(hex, 16).ok()?
            } else {
                num.strip_prefix('#')?.parse().ok()?
            };
            char::from_u32(code)?
        }
    };
    Some((ch, end + 1))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn flat(runs: &[Run]) -> String {
        runs.iter().map(|r| r.text.as_str()).collect()
    }

    #[test]
    fn plain_text_is_one_run() {
        assert_eq!(
            parse("hello"),
            vec![Run {
                text: "hello".into(),
                ..Run::default()
            }]
        );
        assert!(parse("").is_empty());
    }

    #[test]
    fn the_subset_styles_its_text() {
        let runs = parse("a <b>bold</b> <i>it</i> <u>un</u> <a href=\"x\">go</a>");
        assert_eq!(flat(&runs), "a bold it un go");
        assert!(runs.iter().any(|r| r.text == "bold" && r.bold));
        assert!(runs.iter().any(|r| r.text == "it" && r.italic));
        assert!(runs.iter().any(|r| r.text == "un" && r.underline));
        assert!(runs.iter().any(|r| r.text == "go" && r.underline && !r.bold));
    }

    #[test]
    fn other_tags_are_dropped_and_never_shown() {
        let runs = parse("x<img src=\"y.png\" alt=\"z\"/>y <span>s</span><script>q</script>");
        assert_eq!(flat(&runs), "xy sq");
        assert!(!flat(&runs).contains('<'));
    }

    #[test]
    fn nesting_and_stray_closers_survive() {
        let runs = parse("</b><b>a<i>b</i>c</b>d</i>");
        assert_eq!(flat(&runs), "abcd");
        assert!(runs.iter().any(|r| r.text == "b" && r.bold && r.italic));
        assert!(runs.iter().any(|r| r.text == "d" && !r.bold && !r.italic));
    }

    #[test]
    fn entities_decode_and_a_lone_angle_is_text() {
        assert_eq!(
            flat(&parse("a &amp; b &lt;c&gt; &#65;&#x42; &bogus; 1 < 2")),
            "a & b <c> AB &bogus; 1 < 2"
        );
    }

    #[test]
    fn a_br_is_a_newline() {
        assert_eq!(flat(&parse("a<br>b<br/>c")), "a\nb\nc");
    }

    #[test]
    fn plain_strips_the_tags() {
        assert_eq!(plain("a <b>b</b> &amp; c"), "a b & c");
    }

    #[test]
    fn clamp_ellipsizes_overflow_and_keeps_what_fits() {
        let short = parse("hello <b>there</b>");
        assert_eq!(clamp(&short, 2, 40), short);
        let long = parse(&"word ".repeat(60));
        let text = flat(&clamp(&long, 2, 20));
        assert!(text.ends_with('\u{2026}'));
        assert!(text.chars().count() <= 41);
        assert_eq!(flat(&clamp(&parse("a\nb\nc"), 2, 20)), "a\nb\u{2026}");
    }
}
