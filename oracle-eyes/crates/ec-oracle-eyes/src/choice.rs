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
    /// Which list on the screen this option belongs to, from 0 in reading
    /// order. Two questions on one screen are two groups with the same
    /// labels, so a label alone does not say which option the model meant.
    pub group: usize,
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

/// How many alternatives a question asks for, from its own wording: a
/// multiple-choice item that says `(Choose two.)` wants two options, and
/// `Select all that apply.` wants any number. A single pick is the default
/// when nothing says otherwise, so this is `None` for most questions.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Multi {
    /// Exactly this many (`Choose two` is `Count(2)`).
    Count(usize),
    /// Any number of the options, at least one (`Select all that apply`).
    Any,
}

/// The multi-select wording in one line of text, if it has any.
pub fn choose_of(text: &str) -> Option<Multi> {
    let t = text.to_ascii_lowercase();
    if [
        "select all that apply",
        "choose all that apply",
        "check all that apply",
    ]
    .iter()
    .any(|p| t.contains(p))
    {
        return Some(Multi::Any);
    }
    let at = ["choose ", "select ", "pick "]
        .iter()
        .find_map(|k| t.find(k))?;
    let word = t[at..].split(|c: char| !c.is_alphanumeric()).nth(1)?;
    let n = match word {
        "one" | "1" => 1,
        "two" | "2" => 2,
        "three" | "3" => 3,
        "four" | "4" => 4,
        "five" | "5" => 5,
        "six" | "6" => 6,
        _ => return None,
    };
    Some(Multi::Count(n))
}

/// How many options `group` asks for, from the wording around its options.
/// The question's own lines above the first option are searched nearest first
/// (a passage can sit a few lines up), and a trailing hint below the last one;
/// the search stops at the option lines of another list, so one question's
/// wording never reads as another's.
pub fn multi_of(lines: &[Line], options: &[Choice], group: usize) -> Option<Multi> {
    let at: Vec<usize> = options
        .iter()
        .filter(|o| o.group == group)
        .filter_map(|o| lines.iter().position(|l| l.id == o.line))
        .collect();
    let (first, last) = (*at.iter().min()?, *at.iter().max()?);
    let is_option = |i: usize| options.iter().any(|o| o.line == lines[i].id);
    for i in (first.saturating_sub(6)..first).rev() {
        if is_option(i) {
            break;
        }
        if let Some(m) = choose_of(&lines[i].text) {
            return Some(m);
        }
    }
    for line in lines.iter().skip(last + 1).take(2) {
        if options.iter().any(|o| o.line == line.id) {
            break;
        }
        if let Some(m) = choose_of(&line.text) {
            return Some(m);
        }
    }
    None
}

/// The label a line opens with, if any.
fn key_of(text: &str) -> Option<Key> {
    let mut words = text.split_whitespace();
    let first = words.next()?;
    // Something has to follow the label, or it is a lone letter, not an option.
    let second = words.next()?;
    if let Some(k) = label(first).or_else(|| boxed(first)) {
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
    // A checkbox read as its own token, the letter after it in the next:
    // `[ JE`, `[ ] E`. The box is the bracket; the letter is the label.
    if !first.is_empty()
        && first.chars().count() <= 2
        && first
            .chars()
            .all(|c| matches!(c, '[' | ']' | '(' | ')' | '|'))
    {
        if let Some(k) = label(second).or_else(|| bare_after_box(second)) {
            return Some(k);
        }
    }
    BULLETS.contains(&first).then_some(Key::Bullet)
}

/// A checkbox or radio that OCR read as a bracket or a digit stuck to its
/// label: `[TA` (box, A), `1B`, `[JE`. The leading run is box junk and the
/// final character, uppercase, is the label. Tight on purpose: `OK` or `IT`
/// are words, not boxes, so the lead has to open with a bracket or a `1`.
fn boxed(tok: &str) -> Option<Key> {
    let cs: Vec<char> = tok.chars().collect();
    if !(2..=4).contains(&cs.len()) {
        return None;
    }
    let (last, lead) = (*cs.last()?, &cs[..cs.len() - 1]);
    if !last.is_ascii_uppercase() {
        return None;
    }
    if !matches!(lead.first()?, '[' | '(' | '|' | '1' | '□' | '☐') {
        return None;
    }
    lead.iter()
        .all(|c| {
            matches!(
                c,
                '[' | ']' | '(' | ')' | '|' | '1' | 'I' | 'J' | 'T' | '□' | '☐'
            )
        })
        .then_some(Key::Letter(last as u8))
}

/// The label after a box that was read on its own: a single capital, or a
/// capital that OCR read with a box glyph (`J`, `T`, `I`) in front of it.
fn bare_after_box(tok: &str) -> Option<Key> {
    let cs: Vec<char> = tok.chars().collect();
    match cs.as_slice() {
        [c] if c.is_ascii_uppercase() => Some(Key::Letter(*c as u8)),
        [j, c] if matches!(j, 'J' | 'T' | 'I') && c.is_ascii_uppercase() => {
            Some(Key::Letter(*c as u8))
        }
        _ => None,
    }
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

/// Find every run of labelled alternatives, or failing that one unlabelled
/// run after a question. A run counts when it has at least two options,
/// left-aligned with each other; each run is a [`Choice::group`].
pub fn detect(lines: &[Line]) -> Vec<Choice> {
    let labelled = labelled(lines);
    let (found, kind) = if labelled.is_empty() {
        (unlabelled(lines), "unlabelled")
    } else {
        (labelled, "labelled")
    };
    if tracing::enabled!(tracing::Level::DEBUG) {
        let opts: Vec<String> = found
            .iter()
            .map(|c| {
                format!(
                    "{}{}=L{}",
                    c.group,
                    crate::logsafe::log_safe(&c.label),
                    c.line
                )
            })
            .collect();
        tracing::debug!(lines = lines.len(), kind, options = %opts.join(","), "choice: detected");
    }
    found
}

/// Most wrapped lines one option may take. Past this it is prose that
/// happened to follow the last option, not more of it.
const MAX_WRAP: usize = 3;

fn labelled(lines: &[Line]) -> Vec<Choice> {
    let mut runs: Vec<Vec<(Key, Choice)>> = Vec::new();
    let mut run: Vec<(Key, Choice)> = Vec::new();
    let mut wraps = 0;

    for (i, l) in lines.iter().enumerate() {
        let key = key_of(&l.text);
        let tol = l.h.max(12);
        let aligned = |above: &Choice| (l.x - above.x).abs() <= tol;
        let continues = match (run.last(), key) {
            (Some((prev, above)), Some(k)) => prev.follows(k) && aligned(above),
            _ => false,
        };
        if continues {
            run.push((key.expect("matched above"), choice(l)));
            wraps = 0;
            continue;
        }
        // A misread label (`C)` read as `Q)` or `©)`): a short first word,
        // aligned with the list, between two labels that would follow each
        // other with one in between. Taken as that one; anything less sure
        // ends the run.
        let short = l
            .text
            .split_whitespace()
            .next()
            .is_some_and(|t| t.chars().count() <= 3);
        if let (Some((prev, above)), true) = (run.last(), key.is_some() || short) {
            let next = lines[i + 1..]
                .iter()
                .find_map(|n| key_of(&n.text).map(|k| (k, n)));
            if let (Some(mid), Some((after, n))) = (successor(*prev), next) {
                if aligned(above) && mid.follows(after) && (n.x - above.x).abs() <= tol {
                    run.push((mid, choice(l)));
                    wraps = 0;
                    continue;
                }
            }
        }
        // A wrapped option: no label, no paragraph break, and set where the
        // option's text is (a hanging indent) or back at its label. Anything
        // else ends the run.
        if key.is_none() && !l.para && wraps < MAX_WRAP {
            if let Some((_, c)) = run.last_mut() {
                if l.x + tol >= c.x && !l.text.trim_end().ends_with('?') {
                    grow(c, l);
                    wraps += 1;
                    continue;
                }
            }
        }
        if run.len() >= 2 {
            runs.push(std::mem::take(&mut run));
        }
        run.clear();
        wraps = 0;
        if let Some(k) = key.filter(|k| starts(*k)) {
            run.push((k, choice(l)));
        }
    }
    if run.len() >= 2 {
        runs.push(run);
    }
    runs.into_iter()
        .enumerate()
        .flat_map(|(g, run)| {
            run.into_iter().enumerate().map(move |(i, (k, mut c))| {
                c.group = g;
                c.label = match k {
                    Key::Letter(b) => (b as char).to_string(),
                    Key::Number(n) => n.to_string(),
                    Key::Bullet => letter(i),
                };
                c
            })
        })
        .collect()
}

/// The label after `k` in a list, if it has one.
fn successor(k: Key) -> Option<Key> {
    match k {
        Key::Letter(b) if b < b'Z' => Some(Key::Letter(b + 1)),
        Key::Number(n) if n < 99 => Some(Key::Number(n + 1)),
        _ => None,
    }
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
///
/// Every question on the screen gets its own list: each stem that is followed
/// by a run is one group, and the scan resumes after that run, so two stacked
/// questions are two groups and a stem inside a run is not a second question.
fn unlabelled(lines: &[Line]) -> Vec<Choice> {
    let mut out: Vec<Choice> = Vec::new();
    let mut group = 0;
    let mut i = 0;
    while i < lines.len() {
        if !lines[i].text.trim_end().ends_with('?') {
            i += 1;
            continue;
        }
        let run = unlabelled_run(&lines[i], &lines[i + 1..]);
        if !(MIN_UNLABELLED..=MAX_UNLABELLED).contains(&run.len()) {
            i += 1;
            continue;
        }
        // The run is the next lines in order, so the scan picks up after it.
        i += 1 + run.len();
        for (k, mut c) in run.into_iter().enumerate() {
            c.group = group;
            c.label = letter(k);
            out.push(c);
        }
        group += 1;
    }
    out
}

/// The options that follow one stem: consecutive short, evenly spaced lines
/// left-aligned with each other, up to [`MAX_UNLABELLED`] + 1 (more than that
/// is not a question's answers, and the caller rejects it).
fn unlabelled_run(stem: &Line, after: &[Line]) -> Vec<Choice> {
    let mut run: Vec<Choice> = Vec::new();
    let mut step: Option<i32> = None;
    for l in after {
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
    run
}

fn letter(i: usize) -> String {
    ((b'A' + (i as u8).min(25)) as char).to_string()
}

fn choice(l: &Line) -> Choice {
    Choice {
        label: String::new(),
        line: l.id,
        group: 0,
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

    #[test]
    fn greek_labels_are_text_not_pick_keys() {
        // Pick letters are ASCII (COMP-18 §3.1, ADR 0054): a Greek label reads
        // as body text, and nothing about it panics on a char boundary.
        for t in ["α) London", "β) Paris", "Σ) 4", "(γ) Rome", "δ. Berlin"] {
            assert!(key_of(t).is_none(), "{t}");
        }
        let l = [
            line(1, "Ποιο είναι το σωστό;", 0, 0),
            line(2, "α) 2", 24, 20),
            line(3, "β) 4", 24, 40),
        ];
        let _ = detect(&l);
    }

    #[test]
    fn choose_n_wording_beside_non_ascii_parses_or_declines() {
        assert!(matches!(
            choose_of("Choose two — α) β)"),
            Some(Multi::Count(2))
        ));
        assert!(matches!(choose_of("Σ choose two"), Some(Multi::Count(2))));
        assert!(matches!(
            choose_of("ÉCHOOSE ÉÉÉ select all that apply"),
            Some(Multi::Any)
        ));
        assert!(choose_of("Choose β two").is_none());
        assert!(choose_of("Επιλέξτε δύο").is_none());
    }

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

    #[test]
    fn an_option_wrapped_back_to_its_label_keeps_its_line() {
        let l = [
            line(1, "A) Slow start doubles the window until it", 16, 75),
            line(2, "reaches the threshold", 47, 100),
            line(
                3,
                "B) Fast retransmit waits for a timeout before it",
                18,
                125,
            ),
            line(4, "resends a missing segment", 17, 150),
            line(5, "C) The receive window is the congestion window", 17, 175),
            line(6, "D) Avoidance halves the window", 17, 250),
        ];
        let c = detect(&l);
        assert_eq!(labels(&c), ["A", "B", "C", "D"]);
        assert_eq!(
            (c[1].y, c[1].h),
            (125, 150 + 16 - 125),
            "B spans both its lines"
        );
    }

    #[test]
    fn a_misread_label_between_two_good_ones_is_the_missing_one() {
        let l = [
            line(1, "A) 3", 16, 51),
            line(2, "B) 6", 18, 101),
            line(3, "Q) [2, 4, 6]", 17, 125),
            line(4, "D) TypeError", 17, 200),
        ];
        assert_eq!(labels(&detect(&l)), ["A", "B", "C", "D"]);
        // Without a D after it, a stray label is not taken on trust.
        assert_eq!(labels(&detect(&l[..3])), ["A", "B"]);
    }

    #[test]
    fn two_questions_are_two_groups() {
        let l = [
            line(1, "1. Capital of Australia?", 18, 25),
            line(2, "A) Sydney", 46, 50),
            line(3, "B) Canberra", 48, 75),
            line(4, "2. Most of the atmosphere?", 17, 175),
            line(5, "A) Oxygen", 46, 201),
            line(6, "B) Nitrogen", 48, 225),
        ];
        let c = detect(&l);
        assert_eq!(labels(&c), ["A", "B", "A", "B"]);
        let groups: Vec<usize> = c.iter().map(|c| c.group).collect();
        assert_eq!(groups, [0, 0, 1, 1]);
    }

    #[test]
    fn choose_wording_is_parsed_in_its_usual_spellings() {
        assert_eq!(choose_of("(Choose two.)"), Some(Multi::Count(2)));
        assert_eq!(choose_of("Select three.)"), Some(Multi::Count(3)));
        assert_eq!(choose_of("Choose 4 answers"), Some(Multi::Count(4)));
        assert_eq!(choose_of("Select all that apply."), Some(Multi::Any));
        // Ordinary prose with the same verbs is not a count.
        assert_eq!(choose_of("Choose the VLAN interface"), None);
        assert_eq!(choose_of("Choose Your Own Device"), None);
        assert_eq!(choose_of("choose esc back"), None);
    }

    #[test]
    fn a_multi_select_question_says_how_many_it_wants() {
        let l = [
            line(1, "Which two are routing protocols? (Choose two.)", 0, 0),
            line(2, "A) RIP", 0, 20),
            line(3, "B) DNS", 0, 40),
            line(4, "C) OSPF", 0, 60),
        ];
        let c = detect(&l);
        assert_eq!(multi_of(&l, &c, 0), Some(Multi::Count(2)));
    }

    #[test]
    fn a_trailing_select_all_hint_is_found_too() {
        let l = [
            line(1, "Which are primes?", 0, 0),
            line(2, "[ ] 2", 0, 20),
            line(3, "[ ] 4", 0, 40),
            line(4, "Select all that apply.", 0, 60),
        ];
        let c = detect(&l);
        assert_eq!(multi_of(&l, &c, 0), Some(Multi::Any));
    }

    #[test]
    fn a_single_pick_question_has_no_multi() {
        let l = [
            line(1, "Largest planet?", 0, 0),
            line(2, "A) Mars", 0, 20),
            line(3, "B) Jupiter", 0, 40),
        ];
        assert_eq!(multi_of(&l, &detect(&l), 0), None);
    }

    #[test]
    fn one_questions_wording_does_not_leak_into_the_next() {
        let l = [
            line(1, "1. Pick the planet (Choose one.)", 18, 25),
            line(2, "A) Mars", 46, 50),
            line(3, "B) Venus", 46, 75),
            line(4, "2. Which two are gases?", 17, 125),
            line(5, "A) Neon", 46, 150),
            line(6, "B) Oxygen", 46, 175),
        ];
        let c = detect(&l);
        assert_eq!(multi_of(&l, &c, 0), Some(Multi::Count(1)));
        assert_eq!(multi_of(&l, &c, 1), None, "not the one above it");
    }

    #[test]
    fn a_checkbox_read_as_brackets_and_a_letter_is_an_option() {
        let l = [
            line(1, "Select the correct answers.", 0, 0),
            line(2, "[TA Ring topology", 0, 20),
            line(3, "1B Star topology", 0, 40),
            line(4, "[ JC Mesh topology", 0, 60),
        ];
        assert_eq!(labels(&detect(&l)), ["A", "B", "C"]);
    }

    #[test]
    fn a_word_that_looks_like_a_box_and_letter_is_not_an_option() {
        assert_eq!(key_of("OK then"), None);
        assert_eq!(key_of("IT works"), None);
    }

    #[test]
    fn two_stacked_unlabelled_questions_are_two_groups() {
        let l = [
            line(1, "What is the capital?", 0, 0),
            line(2, "Paris", 24, 30),
            line(3, "Rome", 24, 58),
            line(4, "Madrid", 24, 86),
            line(5, "Which is the largest?", 0, 150),
            line(6, "Asia", 24, 180),
            line(7, "Africa", 24, 208),
            line(8, "Europe", 24, 236),
        ];
        let c = detect(&l);
        let groups: Vec<usize> = c.iter().map(|c| c.group).collect();
        assert_eq!(groups, [0, 0, 0, 1, 1, 1]);
        assert_eq!(labels(&c), ["A", "B", "C", "A", "B", "C"]);
    }
}
