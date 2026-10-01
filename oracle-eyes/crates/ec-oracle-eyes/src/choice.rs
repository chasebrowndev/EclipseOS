// SPDX-License-Identifier: AGPL-3.0-only

//! Multiple-choice detection: a stem followed by labelled alternatives.
//!
//! Local and geometric, over [`crate::ocr::lines_of`]. It runs here and never
//! in the compositor, which must not learn to parse screen content (ADR 0054).
//!
//! **Every option is written by the adversary.** The detector is what bounds
//! the model's pick: the reply may only name a label listed here, and each
//! label maps to a rectangle measured from OCR, never to anything the model
//! says. When detection is unsure it returns nothing, and the answer degrades
//! to prose — pointing confidently at the wrong rectangle is worse than no
//! pointer at all.

use crate::ocr::Line;

/// One detected alternative.
#[derive(Debug, Clone, PartialEq)]
pub struct Choice {
    /// What the model and the marker call it: `A`–`Z` or `1`–`99`. Bullet
    /// lists (radio buttons, checkboxes) get letters in reading order.
    pub label: String,
    /// The option's first line, as numbered for the model.
    pub line: usize,
    /// Logical rect of the option, wrapped continuation lines included.
    pub x: i32,
    pub y: i32,
    pub w: i32,
    pub h: i32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Key {
    Letter(u8),
    Number(u32),
    /// A radio or checkbox glyph. Any spelling follows any other: OCR reads
    /// the same circle as `O` on one line and `©` on the next, and an option
    /// already selected (`●`) sits among empty ones (`○`).
    Bullet,
}

impl Key {
    /// Whether `next` is the label that follows `self` in one list.
    fn follows(self, next: Key) -> bool {
        match (self, next) {
            (Key::Letter(a), Key::Letter(b)) => b == a + 1,
            (Key::Number(a), Key::Number(b)) => b == a + 1,
            (Key::Bullet, Key::Bullet) => true,
            _ => false,
        }
    }
}

/// What OCR makes of a radio button or checkbox. `O`/`o`/`0`/`©` are all
/// common readings of an empty circle.
const BULLETS: &[&str] = &[
    "○", "◯", "●", "◉", "⦿", "☐", "☑", "☒", "□", "■", "▢", "O", "o", "0", "©", "®", "()", "[]",
    "[", "(",
];

/// The label a line opens with, if any.
fn key_of(text: &str) -> Option<Key> {
    let mut words = text.split_whitespace();
    let first = words.next()?;
    // Something has to follow the label, or it is a lone letter, not an option.
    let second = words.next()?;
    if let Some(k) = label(first) {
        return Some(k);
    }
    // A list bullet before the label (`• A) London`), which OCR also reads as
    // `e`, `«` or `°`: any short mark, as long as a real label and some text
    // follow it.
    if first.chars().count() <= 2 && words.next().is_some() {
        if let Some(k) = label(second) {
            return Some(k);
        }
    }
    BULLETS.contains(&first).then_some(Key::Bullet)
}

/// A letter or number label on its own: `A)`, `(a)`, `b.`, `1:`.
fn label(tok: &str) -> Option<Key> {
    let inner = tok
        .strip_prefix('(')
        .and_then(|t| t.strip_suffix(')'))
        .or_else(|| tok.strip_suffix([')', '.', ':']))?;
    if inner.len() == 1 && inner.as_bytes()[0].is_ascii_alphabetic() {
        return Some(Key::Letter(inner.as_bytes()[0].to_ascii_uppercase()));
    }
    if (1..=2).contains(&inner.len()) && inner.bytes().all(|b| b.is_ascii_digit()) {
        return inner.parse().ok().map(Key::Number);
    }
    None
}

/// The first label of a list: `A`, `1`, or any bullet.
fn starts(k: Key) -> bool {
    matches!(k, Key::Letter(b'A') | Key::Number(1) | Key::Bullet)
}

/// Find the longest run of labelled alternatives, or failing that an
/// unlabelled one after a question. Returns nothing unless at least two were
/// found, left-aligned with each other.
pub fn detect(lines: &[Line]) -> Vec<Choice> {
    let labelled = labelled(lines);
    if labelled.is_empty() {
        unlabelled(lines)
    } else {
        labelled
    }
}

fn labelled(lines: &[Line]) -> Vec<Choice> {
    let mut best: Vec<(Key, Choice)> = Vec::new();
    let mut run: Vec<(Key, Choice)> = Vec::new();

    for l in lines {
        let key = key_of(&l.text);
        let tol = l.h.max(12);
        let continues = match (run.last(), key) {
            (Some((prev, above)), Some(k)) => prev.follows(k) && (l.x - above.x).abs() <= tol,
            _ => false,
        };
        if continues {
            run.push((key.expect("matched above"), choice(l)));
            continue;
        }
        // A wrapped option: no label, no paragraph break, indented past the
        // label. Anything else ends the run.
        if key.is_none() && !l.para {
            if let Some((_, c)) = run.last_mut() {
                if l.x > c.x + 4 {
                    grow(c, l);
                    continue;
                }
            }
        }
        if run.len() > best.len() {
            best = std::mem::take(&mut run);
        }
        run.clear();
        if let Some(k) = key.filter(|k| starts(*k)) {
            run.push((k, choice(l)));
        }
    }
    if run.len() > best.len() {
        best = run;
    }
    if best.len() < 2 {
        return Vec::new();
    }
    best.into_iter()
        .enumerate()
        .map(|(i, (k, mut c))| {
            c.label = match k {
                Key::Letter(b) => (b as char).to_string(),
                Key::Number(n) => n.to_string(),
                Key::Bullet => letter(i),
            };
            c
        })
        .collect()
}

/// Fewest options an unlabelled list may have. Two lines have one gap, so
/// nothing to show that they are evenly spaced; an unlabelled True/False stays
/// prose.
const MIN_UNLABELLED: usize = 3;
/// Most options an unlabelled list may have; past this it is a list, not a
/// question's answers.
const MAX_UNLABELLED: usize = 8;
/// Longest line that still reads as an option rather than prose.
const OPTION_WORDS: usize = 10;

/// Options with no label at all: radio buttons whose circles OCR dropped.
///
/// A line ending in `?`, then [`MIN_UNLABELLED`] to [`MAX_UNLABELLED`] short lines, left-aligned
/// with each other and evenly spaced, starting within a few lines of it. The
/// question mark is what keeps an ordinary bulleted list from reading as a
/// quiz. An option that wraps breaks the even spacing, so such a list is not
/// detected and the answer stays prose. Letters are ours, in reading order.
fn unlabelled(lines: &[Line]) -> Vec<Choice> {
    let mut best: Vec<Choice> = Vec::new();
    for (i, stem) in lines.iter().enumerate() {
        if !stem.text.trim_end().ends_with('?') {
            continue;
        }
        let mut run: Vec<Choice> = Vec::new();
        let mut step: Option<i32> = None;
        for l in &lines[i + 1..] {
            let words = l.text.split_whitespace().count();
            if words == 0 || words > OPTION_WORDS || l.text.trim_end().ends_with('?') {
                break;
            }
            let tol = l.h.max(12);
            match run.last() {
                None => {
                    if l.y - (stem.y + stem.h) > 3 * tol {
                        break;
                    }
                }
                Some(above) => {
                    let gap = l.y - above.y;
                    if (l.x - above.x).abs() > tol / 2 || gap <= 0 {
                        break;
                    }
                    if let Some(s) = step {
                        if (gap - s).abs() > (l.h / 3).max(4) {
                            break;
                        }
                    }
                    step = Some(gap);
                }
            }
            run.push(choice(l));
            if run.len() > MAX_UNLABELLED {
                break;
            }
        }
        if (MIN_UNLABELLED..=MAX_UNLABELLED).contains(&run.len()) && run.len() > best.len() {
            best = run;
        }
    }
    for (i, c) in best.iter_mut().enumerate() {
        c.label = letter(i);
    }
    best
}

fn letter(i: usize) -> String {
    ((b'A' + (i as u8).min(25)) as char).to_string()
}

fn choice(l: &Line) -> Choice {
    Choice {
        label: String::new(),
        line: l.id,
        x: l.x,
        y: l.y,
        w: l.w,
        h: l.h,
    }
}

fn grow(c: &mut Choice, l: &Line) {
    let (x1, y1) = ((c.x + c.w).max(l.x + l.w), (c.y + c.h).max(l.y + l.h));
    c.x = c.x.min(l.x);
    c.y = c.y.min(l.y);
    c.w = x1 - c.x;
    c.h = y1 - c.y;
}

#[cfg(test)]
mod tests {
    use super::*;

    fn line(id: usize, text: &str, x: i32, y: i32) -> Line {
        Line {
            id,
            text: text.into(),
            x,
            y,
            w: 200,
            h: 16,
            para: false,
        }
    }

    fn labels(c: &[Choice]) -> Vec<&str> {
        c.iter().map(|c| c.label.as_str()).collect()
    }

    #[test]
    fn lettered_options_are_found_with_their_lines() {
        let l = [
            line(1, "Which planet is largest?", 0, 0),
            line(2, "A) Mars", 0, 20),
            line(3, "B) Jupiter", 0, 40),
            line(4, "(c) Venus", 0, 60),
        ];
        let c = detect(&l);
        assert_eq!(labels(&c), ["A", "B", "C"]);
        assert_eq!(c[1].line, 3);
        assert_eq!((c[1].x, c[1].y), (0, 40));
    }

    #[test]
    fn numbered_options_are_found() {
        let l = [
            line(1, "Pick one:", 0, 0),
            line(2, "1. red", 10, 20),
            line(3, "2. green", 10, 40),
        ];
        assert_eq!(labels(&detect(&l)), ["1", "2"]);
    }

    #[test]
    fn radio_buttons_get_letters_in_reading_order() {
        let l = [
            line(1, "Best editor?", 0, 0),
            line(2, "O micro", 0, 20),
            line(3, "O vim", 0, 40),
            line(4, "O emacs", 0, 60),
        ];
        let c = detect(&l);
        assert_eq!(labels(&c), ["A", "B", "C"]);
        assert_eq!(c[2].line, 4);
    }

    #[test]
    fn a_wrapped_option_takes_its_continuation_line() {
        let mut cont = line(3, "and it keeps going", 30, 36);
        cont.w = 150;
        let l = [
            line(1, "Q?", 0, 0),
            line(2, "A) a long option that wraps", 0, 20),
            cont,
            line(4, "B) short", 0, 56),
        ];
        let c = detect(&l);
        assert_eq!(labels(&c), ["A", "B"]);
        assert_eq!((c[0].y, c[0].h), (20, 32), "A spans both of its lines");
    }

    #[test]
    fn one_label_is_not_a_list() {
        let l = [line(1, "A) lone", 0, 0), line(2, "some prose after", 0, 20)];
        assert!(detect(&l).is_empty());
    }

    #[test]
    fn a_misaligned_label_breaks_the_run() {
        let l = [line(1, "A) left", 0, 0), line(2, "B) far right", 900, 20)];
        assert!(detect(&l).is_empty());
    }

    #[test]
    fn a_list_must_start_at_its_first_label() {
        let l = [line(1, "C) three", 0, 0), line(2, "D) four", 0, 20)];
        assert!(
            detect(&l).is_empty(),
            "a run that starts mid-list is a guess"
        );
    }

    #[test]
    fn a_list_bullet_before_the_label_is_looked_past() {
        // Google's AI Overview renders options as a bulleted list, and OCR
        // spells the bullet several ways.
        let l = [
            line(1, "What is the capital city of France?", 0, 0),
            line(2, "• A) London", 10, 40),
            line(3, "e B) Paris", 10, 80),
            line(4, "« C) Rome", 10, 120),
        ];
        let c = detect(&l);
        assert_eq!(labels(&c), ["A", "B", "C"]);
        assert_eq!(c[1].line, 3);
    }

    #[test]
    fn a_short_word_before_a_label_needs_text_after_it() {
        assert_eq!(key_of("• B)"), None);
        assert_eq!(key_of("to 2."), None);
    }

    #[test]
    fn mixed_readings_of_the_same_radio_are_one_list() {
        let l = [
            line(1, "Best editor?", 0, 0),
            line(2, "O micro", 0, 20),
            line(3, "© vim", 0, 40),
            line(4, "o emacs", 0, 60),
        ];
        assert_eq!(labels(&detect(&l)), ["A", "B", "C"]);
    }

    #[test]
    fn an_already_selected_radio_stays_in_its_list() {
        let l = [
            line(1, "Best editor?", 0, 0),
            line(2, "○ micro", 0, 20),
            line(3, "● vim", 0, 40),
            line(4, "○ emacs", 0, 60),
        ];
        let c = detect(&l);
        assert_eq!(labels(&c), ["A", "B", "C"]);
        assert_eq!(c[1].line, 3);
    }

    #[test]
    fn radios_whose_circles_were_not_read_are_still_options() {
        let mut l = vec![
            line(1, "Which planet is the largest?", 0, 0),
            line(2, "Mars", 24, 30),
            line(3, "Venus", 24, 58),
            line(4, "Jupiter", 24, 86),
            line(5, "Mercury", 24, 114),
            line(6, "Next", 0, 180),
        ];
        l[1].para = true;
        let c = detect(&l);
        assert_eq!(labels(&c), ["A", "B", "C", "D"]);
        assert_eq!((c[2].line, c[2].x, c[2].y), (4, 24, 86));
    }

    #[test]
    fn a_bulleted_list_with_no_question_is_not_a_quiz() {
        let l = [
            line(1, "Things to pack:", 0, 0),
            line(2, "tent", 24, 20),
            line(3, "stove", 24, 40),
            line(4, "map", 24, 60),
        ];
        assert!(detect(&l).is_empty());
    }

    #[test]
    fn unevenly_spaced_lines_after_a_question_are_prose() {
        let l = [
            line(1, "Why is the sky blue?", 0, 0),
            line(2, "Rayleigh scattering.", 0, 20),
            line(3, "Blue scatters most.", 0, 40),
            line(4, "See also", 0, 110),
        ];
        assert!(detect(&l).is_empty());
    }

    #[test]
    fn a_long_line_after_a_question_is_an_answer_not_an_option() {
        let l = [
            line(1, "Why is the sky blue?", 0, 0),
            line(
                2,
                "Because sunlight scatters off the molecules of the air more at short wavelengths",
                0,
                20,
            ),
            line(3, "and blue is short.", 0, 40),
        ];
        assert!(detect(&l).is_empty());
    }

    #[test]
    fn prose_opening_with_a_letter_word_is_not_an_option() {
        let l = [
            line(1, "A dog ran.", 0, 0),
            line(2, "B movies are fun.", 0, 20),
        ];
        assert!(detect(&l).is_empty());
    }
}
