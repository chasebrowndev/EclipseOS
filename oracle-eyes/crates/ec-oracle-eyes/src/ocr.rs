// SPDX-License-Identifier: AGPL-3.0-only

//! Pixels to words: spec §3.2, the stage that must run locally so that only
//! text ever leaves the machine.
//!
//! The engine sits behind [`Ocr`] because §7 leaves the choice open and wants
//! a bake-off against one or two alternatives before anything is locked in.
//! Everything downstream — redaction, the classifier, the answerer — is
//! written against the trait and the [`Word`] list, so swapping engines is a
//! new `impl Ocr` and nothing else. That is also why [`Word`] carries no
//! Tesseract bookkeeping (block/par/line ids): a second engine would not have
//! them, and [`text_of`] therefore recovers layout from geometry alone.
//!
//! The Tesseract backend shells out to the CLI rather than linking
//! libtesseract. Two reasons: it adds no crates to a supply chain we have to
//! audit, and the process boundary means a segfault in a C++ OCR engine fed
//! attacker-influenced pixels kills a child, not the daemon.
//!
//! **State of this machine (checked 2026-09-15):** `tesseract` 5.5.3 with
//! leptonica 1.87.0 is installed at `/usr/bin/tesseract`, but
//! `/usr/share/tessdata` holds only `afr` and `osd` — **there is no `eng`
//! language data**, so [`Tesseract::recognise`] fails at runtime until
//! `tesseract-data-eng` is installed. [`Tesseract::new`] deliberately does not
//! probe for language data; the error tesseract itself prints when a language
//! is missing is clearer than anything we would synthesise, and it surfaces
//! through the same "fail visibly" path as any other engine error.

use std::io::Write;
use std::process::{Command, Stdio};
use std::time::Instant;

use crate::frame::Frame;

/// One recognised word. `x`/`y`/`w`/`h` are **compositor-logical screen**
/// coordinates, already translated out of frame space, so a caller can hand
/// the box straight to the annotation pass as an anchor.
#[derive(Debug, Clone, PartialEq)]
pub struct Word {
    pub text: String,
    pub conf: f32,
    pub x: i32,
    pub y: i32,
    pub w: i32,
    pub h: i32,
}

pub trait Ocr {
    fn recognise(&mut self, frame: &Frame) -> Result<Vec<Word>, String>;
}

/// One reconstructed line of text, with the logical rectangle its words cover.
/// Numbered from 1 in reading order: the model refers to lines by `id`, and
/// the daemon maps an id back to `rect` — the model never supplies geometry.
#[derive(Debug, Clone, PartialEq)]
pub struct Line {
    pub id: usize,
    pub text: String,
    pub x: i32,
    pub y: i32,
    pub w: i32,
    pub h: i32,
    /// About a blank line of space sits above this one: a paragraph or a
    /// different block on the page.
    pub para: bool,
}

impl Line {
    fn start(id: usize, w: &Word, para: bool) -> Line {
        Line {
            id,
            text: w.text.clone(),
            x: w.x,
            y: w.y,
            w: w.w,
            h: w.h,
            para,
        }
    }

    fn push(&mut self, w: &Word) {
        self.text.push(' ');
        self.text.push_str(&w.text);
        let (x1, y1) = (
            (self.x + self.w).max(w.x + w.w),
            (self.y + self.h).max(w.y + w.h),
        );
        self.x = self.x.min(w.x);
        self.y = self.y.min(w.y);
        self.w = x1 - self.x;
        self.h = y1 - self.y;
    }
}

/// Group words into lines. Joining everything with spaces loses the
/// difference between a heading and the sentence under it, and the model is
/// the only consumer that cares, so breaks are restored here rather than being
/// carried through [`Word`].
///
/// The rule is geometric because it has to work for any engine (§7):
///
/// - A word whose vertical midpoint leaves the current line's band, or which
///   starts to the left of where the line began, opens a new line.
/// - A word in the band but past a gutter — more blank space than
///   [`GUTTER`] typical word heights — opens a new line too: it is the next
///   column, not the next word. Without this a two-column page read as one
///   line per row, half from each column, and no question or option on it
///   could be found or bracketed.
/// - When a gutter split happened, lines are put back in reading order
///   column by column ([`by_column`]), so a column's lines stay adjacent.
/// - A line starts a paragraph ([`Line::para`]) when it sits more than
///   [`PARA_PITCH`] line pitches below the one before (a blank line), above
///   it (the next column or block), or beside it with no overlap.
///
/// The yardsticks are medians over the whole read, not the line above: a
/// line band is as tall as its glyphs, so `Jupiter` (descender) and `Mars`
/// (none) set different bands at the same pitch, and an all-lowercase prose
/// line (`would save a`) is a few pixels shorter than its neighbours. Either
/// measured against the line above made evenly set lines flip between
/// "paragraph" and "not".
pub fn lines_of(words: &[Word]) -> Vec<Line> {
    let typical = {
        let mut hs: Vec<i32> = words.iter().map(|w| w.h).collect();
        hs.sort_unstable();
        hs.get(hs.len() / 2).copied().unwrap_or(1).max(1)
    };
    let mut lines: Vec<Line> = Vec::new();
    let mut split = false;
    // The current line's band and left edge. Kept apart from the line's rect
    // because the rect's left edge is the same thing, but the band is only
    // stretched by same-line words, never reset by them.
    let (mut top, mut bottom, mut left) = (0i32, 0i32, 0i32);

    for w in words {
        let Some(cur) = lines.last_mut() else {
            lines.push(Line::start(0, w, false));
            (top, bottom, left) = (w.y, w.y + w.h, w.x);
            continue;
        };
        let mid = w.y + w.h / 2;
        let in_band = mid >= top && mid < bottom && w.x >= left;
        let past_gutter = w.x - (cur.x + cur.w) > GUTTER * typical;
        if in_band && !past_gutter {
            // Tall glyphs (parentheses, capitals) stretch the band so the
            // rest of the line keeps matching it.
            top = top.min(w.y);
            bottom = bottom.max(w.y + w.h);
            cur.push(w);
        } else {
            split |= in_band;
            lines.push(Line::start(0, w, false));
            (top, bottom, left) = (w.y, w.y + w.h, w.x);
        }
    }
    if split {
        lines = by_column(lines);
    }
    let pitch = pitch(&lines);
    for i in 0..lines.len() {
        lines[i].id = i + 1;
        if i > 0 {
            let (above, l) = (&lines[i - 1], &lines[i]);
            let step = l.y - above.y;
            lines[i].para = step <= 0 || step * 10 > pitch * PARA_PITCH || !overlaps(above, l);
        }
    }
    // Not traced here: these lines are not redacted yet, and only the
    // caller sees the context (a label on the line above) that makes a bare
    // value a secret. `pipeline` traces them after redaction.
    lines
}

/// Blank space between two words of one band, in typical word heights, past
/// which the second is in another column. A word space is well under one.
const GUTTER: i32 = 3;

/// Top-to-top distance, in tenths of the line pitch, past which a line
/// starts a paragraph. One blank line is 2.0; ragged tops are well under 0.6.
pub const PARA_PITCH: i32 = 16;

/// Whether two lines share any horizontal extent.
pub fn overlaps(a: &Line, b: &Line) -> bool {
    a.x < b.x + b.w && b.x < a.x + a.w
}

/// The read's line pitch: the usual top-to-top distance between a line and
/// the one below it in the same column. The lower median, so the blank lines
/// between paragraphs do not count as the pitch; with too few lines to tell,
/// no more than one and a half line heights.
pub fn pitch(lines: &[Line]) -> i32 {
    let mut steps: Vec<i32> = lines
        .windows(2)
        .filter(|p| overlaps(&p[0], &p[1]))
        .map(|p| p[1].y - p[0].y)
        .filter(|s| *s > 0)
        .collect();
    let mut hs: Vec<i32> = lines.iter().map(|l| l.h).collect();
    hs.sort_unstable();
    let height = hs.get(hs.len().saturating_sub(1) / 2).copied().unwrap_or(1);
    steps.sort_unstable();
    let median = steps.get(steps.len().saturating_sub(1) / 2).copied();
    let p = match median {
        Some(m) if steps.len() >= 3 => m,
        Some(m) => m.min(height * 3 / 2),
        None => height * 3 / 2,
    };
    p.max(1)
}

/// Lines back in reading order, one column after another. Each line joins
/// the column whose last line it sits under, overlapping it horizontally;
/// one that fits no column starts a new one. Columns come out in the order
/// they started, which keeps a heading above two columns first and a page's
/// left column before its right.
fn by_column(lines: Vec<Line>) -> Vec<Line> {
    let mut columns: Vec<Vec<Line>> = Vec::new();
    for l in lines {
        let home = columns
            .iter_mut()
            .filter(|c| {
                let last = c.last().expect("never empty");
                last.y < l.y && overlaps(last, &l)
            })
            .max_by_key(|c| c.last().expect("never empty").y);
        match home {
            Some(c) => c.push(l),
            None => columns.push(vec![l]),
        }
    }
    columns.into_iter().flatten().collect()
}

#[cfg(test)]
/// The lines as plain text, a blank line between paragraphs.
pub fn text_of(words: &[Word]) -> String {
    let mut out = String::new();
    for (i, l) in lines_of(words).iter().enumerate() {
        if i > 0 {
            out.push_str(if l.para { "\n\n" } else { "\n" });
        }
        out.push_str(&l.text);
    }
    out
}

#[derive(Debug)]
pub struct Tesseract {
    bin: String,
    lang: String,
}

impl Tesseract {
    /// Verifies the binary answers `--version` now, rather than letting the
    /// first capture of the session be the thing that discovers it is absent:
    /// a missing dependency is a startup error the user can act on, not a
    /// mysterious dead hotkey (invariant: fail visibly).
    pub fn new(bin: &str) -> Result<Tesseract, String> {
        let out = Command::new(bin)
            .arg("--version")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .map_err(|e| format!("ocr: cannot run `{bin}`: {e}"))?;
        if !out.success() {
            return Err(format!("ocr: `{bin} --version` failed: {out}"));
        }
        Ok(Tesseract {
            bin: bin.to_string(),
            lang: "eng".to_string(),
        })
    }

    /// Language data to use. Left settable because the installed set is a
    /// property of the machine, not of the code (see the module note).
    pub fn with_lang(mut self, lang: &str) -> Tesseract {
        self.lang = lang.to_string();
        self
    }
}

impl Ocr for Tesseract {
    fn recognise(&mut self, frame: &Frame) -> Result<Vec<Word>, String> {
        let started = Instant::now();
        let ppm = encode_ppm(frame);

        // Image in on stdin, TSV out on stdout: screen pixels never touch the
        // filesystem, so there is no window in which a temp file under
        // `$XDG_RUNTIME_DIR` exists for something else to read and no cleanup
        // path to get wrong on a crash.
        //
        // `--psm 6` — "a single uniform block of text". The input is a region
        // the user selected or damage settled on, not a scanned page, so
        // Tesseract's default page segmentation spends time looking for a
        // column layout that is not there and sometimes invents one.
        let mut child = Command::new(&self.bin)
            .args(["-l", &self.lang, "--psm", "6", "-", "-", "tsv"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| format!("ocr: cannot run `{}`: {e}", self.bin))?;

        // Written from a thread because a full-screen PPM is far larger than a
        // pipe buffer; writing inline would deadlock against a child that is
        // waiting on us while we wait on its output.
        let mut stdin = child
            .stdin
            .take()
            .ok_or_else(|| "ocr: no stdin on child".to_string())?;
        let writer = std::thread::spawn(move || stdin.write_all(&ppm));

        let out = child
            .wait_with_output()
            .map_err(|e| format!("ocr: `{}` failed: {e}", self.bin))?;
        // A broken pipe here just means the child died first; its exit status
        // and stderr say why, and that is the better message.
        let _ = writer.join();

        if !out.status.success() {
            let why = String::from_utf8_lossy(&out.stderr);
            let why = why.trim();
            tracing::debug!(status = %out.status, ms = started.elapsed().as_millis() as u64, "ocr: engine failed");
            return Err(format!(
                "ocr: `{}` exited {}: {}",
                self.bin,
                out.status,
                if why.is_empty() { "no output" } else { why }
            ));
        }

        let tsv = String::from_utf8_lossy(&out.stdout);
        let words = parse_tsv(&tsv, frame);
        tracing::debug!(
            words = words.len(),
            ms = started.elapsed().as_millis() as u64,
            ppm_bytes = frame.pixels.len(),
            "ocr: recognised"
        );
        if tracing::enabled!(tracing::Level::TRACE) {
            // Boxes and confidence only: a single word is too small a unit
            // for the redaction rules to judge, so word text is not logged.
            for w in &words {
                tracing::trace!(
                    x = w.x,
                    y = w.y,
                    w = w.w,
                    h = w.h,
                    conf = w.conf,
                    "ocr: word box"
                );
            }
        }
        Ok(words)
    }
}

/// PPM "P6": a three-line ASCII header then raw RGB. Chosen because it is the
/// only lossless format we can emit correctly in ten lines with no image crate
/// in the dependency list, and leptonica reads it natively.
///
/// Alpha is dropped rather than composited: a capture of the composited output
/// is already opaque, so there is nothing to blend against.
fn encode_ppm(frame: &Frame) -> Vec<u8> {
    let w = frame.width as usize;
    let h = frame.height as usize;
    let header = format!("P6\n{w} {h}\n255\n");
    let mut out = Vec::with_capacity(header.len() + w * h * 3);
    out.extend_from_slice(header.as_bytes());
    for row in 0..h {
        let start = row * frame.stride as usize;
        for col in 0..w {
            let p = start + col * 4;
            // A short buffer is a bug upstream, but emitting black beats
            // panicking in the middle of the user's hotkey.
            let px = frame.pixels.get(p..p + 3).unwrap_or(&[0, 0, 0]);
            out.extend_from_slice(px);
        }
    }
    out
}

/// Tesseract's TSV: a header row then one row per element, columns
/// `level page block par line word left top width height conf text`.
/// Only `level == 5` rows are words; the rest are the containing boxes, and
/// they carry `conf == -1`, which is the same signal as a word Tesseract
/// refused to commit to — so filtering `conf < 0` handles both.
///
/// Rows that do not parse are skipped rather than failing the whole capture:
/// the alternative is one malformed line costing the user an answer.
fn parse_tsv(tsv: &str, frame: &Frame) -> Vec<Word> {
    // The buffer-to-logical ratio, from the frame itself: `origin` is the
    // logical rect the pixels cover and `width`/`height` are the pixels. This
    // is the same ratio `capture` cropped with, fractional scales included.
    let sx = f64::from(frame.origin.w.max(1)) / f64::from(frame.width.max(1));
    let sy = f64::from(frame.origin.h.max(1)) / f64::from(frame.height.max(1));
    let mut words = Vec::new();
    for line in tsv.lines() {
        // `splitn(12)` so a recognised word containing a tab — Tesseract does
        // not escape the text column — stays in one piece.
        let cols: Vec<&str> = line.splitn(12, '\t').collect();
        if cols.len() < 12 || cols[0] != "5" {
            continue;
        }
        let (Ok(left), Ok(top), Ok(w), Ok(h), Ok(conf)) = (
            cols[6].parse::<i32>(),
            cols[7].parse::<i32>(),
            cols[8].parse::<i32>(),
            cols[9].parse::<i32>(),
            cols[10].parse::<f32>(),
        ) else {
            continue;
        };
        let text = cols[11].trim();
        if conf < 0.0 || text.is_empty() {
            continue;
        }

        // Frame-relative physical pixels to screen-absolute logical ones.
        // Callers anchor annotations on these boxes, and the compositor only
        // speaks logical coordinates; doing it anywhere else means every
        // caller repeating it. Adding physical offsets to a logical origin put
        // every box twice as far out on a scale-2 output.
        words.push(Word {
            text: text.to_string(),
            conf,
            x: frame.origin.x + (f64::from(left) * sx).round() as i32,
            y: frame.origin.y + (f64::from(top) * sy).round() as i32,
            w: (f64::from(w) * sx).round().max(1.0) as i32,
            h: (f64::from(h) * sy).round().max(1.0) as i32,
        });
    }
    words
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::frame::Region;

    fn frame(origin: Region) -> Frame {
        Frame {
            width: 2,
            height: 2,
            stride: 8,
            pixels: vec![
                1, 2, 3, 255, 4, 5, 6, 255, //
                7, 8, 9, 255, 10, 11, 12, 255,
            ],
            origin,
        }
    }

    fn at(text: &str, x: i32, y: i32, w: i32, h: i32) -> Word {
        Word {
            text: text.into(),
            conf: 90.0,
            x,
            y,
            w,
            h,
        }
    }

    const HEADER: &str =
        "level\tpage_num\tblock_num\tpar_num\tline_num\tword_num\tleft\ttop\twidth\theight\tconf\ttext";

    #[test]
    fn ppm_header_and_body_drop_alpha() {
        let out = encode_ppm(&frame(Region {
            x: 0,
            y: 0,
            w: 2,
            h: 2,
        }));
        assert_eq!(&out[..11], b"P6\n2 2\n255\n");
        assert_eq!(&out[11..], &[1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12]);
    }

    #[test]
    fn ppm_respects_stride_padding() {
        let f = Frame {
            width: 1,
            height: 2,
            stride: 8, // one pixel of row padding
            pixels: vec![1, 1, 1, 255, 9, 9, 9, 255, 2, 2, 2, 255, 9, 9, 9, 255],
            origin: Region {
                x: 0,
                y: 0,
                w: 1,
                h: 2,
            },
        };
        let out = encode_ppm(&f);
        assert_eq!(&out[out.len() - 6..], &[1, 1, 1, 2, 2, 2]);
    }

    #[test]
    fn parses_words_and_translates_to_screen_coordinates() {
        let tsv = format!(
            "{HEADER}\n\
             5\t1\t1\t1\t1\t1\t10\t20\t30\t12\t96.5\thello\n\
             5\t1\t1\t1\t1\t2\t50\t20\t25\t12\t91\tworld\n"
        );
        let got = parse_tsv(
            &tsv,
            &frame(Region {
                x: 100,
                y: 200,
                w: 2,
                h: 2,
            }),
        );
        assert_eq!(got.len(), 2);
        assert_eq!(got[0], at("hello", 110, 220, 30, 12).into_conf(96.5));
        assert_eq!(got[1].x, 150);
        assert_eq!(got[1].y, 220);
    }

    #[test]
    fn a_scale_two_frame_maps_boxes_back_to_logical_pixels() {
        let tsv = format!("{HEADER}\n5\t1\t1\t1\t1\t1\t200\t100\t60\t24\t90\thi\n");
        let f = Frame {
            width: 2000,
            height: 1000,
            stride: 8000,
            pixels: Vec::new(),
            origin: Region {
                x: 1920,
                y: 0,
                w: 1000,
                h: 500,
            },
        };
        let got = parse_tsv(&tsv, &f);
        assert_eq!(got[0], at("hi", 2020, 50, 30, 12));
    }

    #[test]
    fn a_fractional_scale_frame_maps_boxes_back_to_logical_pixels() {
        // 1.5x: 300 physical pixels cover 200 logical ones.
        let tsv = format!("{HEADER}\n5\t1\t1\t1\t1\t1\t150\t30\t45\t15\t90\thi\n");
        let f = Frame {
            width: 300,
            height: 300,
            stride: 1200,
            pixels: Vec::new(),
            origin: Region {
                x: 0,
                y: 0,
                w: 200,
                h: 200,
            },
        };
        let got = parse_tsv(&tsv, &f);
        assert_eq!(got[0], at("hi", 100, 20, 30, 10));
    }

    #[test]
    fn skips_container_rows_negative_conf_and_blank_text() {
        let tsv = format!(
            "{HEADER}\n\
             1\t1\t0\t0\t0\t0\t0\t0\t800\t600\t-1\t\n\
             4\t1\t1\t1\t1\t0\t10\t20\t70\t12\t-1\t\n\
             5\t1\t1\t1\t1\t1\t10\t20\t30\t12\t-1\tghost\n\
             5\t1\t1\t1\t1\t2\t50\t20\t25\t12\t88\t   \n\
             5\t1\t1\t1\t1\t3\t80\t20\t25\t12\t88\tkept\n"
        );
        let got = parse_tsv(
            &tsv,
            &frame(Region {
                x: 0,
                y: 0,
                w: 2,
                h: 2,
            }),
        );
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].text, "kept");
    }

    #[test]
    fn malformed_rows_do_not_kill_the_capture() {
        let tsv = format!(
            "{HEADER}\n\
             \n\
             5\ttoo\tfew\tcolumns\n\
             5\t1\t1\t1\t1\t1\tx\t20\t30\t12\t90\tbad\n\
             5\t1\t1\t1\t1\t2\t10\t20\t30\t12\tNaNish\tbad2\n\
             5\t1\t1\t1\t1\t3\t10\t20\t30\t12\t90\tgood\n"
        );
        let got = parse_tsv(
            &tsv,
            &frame(Region {
                x: 0,
                y: 0,
                w: 2,
                h: 2,
            }),
        );
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].text, "good");
    }

    #[test]
    fn text_of_keeps_lines_and_paragraphs_apart() {
        let words = vec![
            at("What", 0, 0, 40, 10),
            at("is", 45, 0, 15, 10),
            at("this?", 65, 0, 35, 10),
            at("It", 0, 12, 15, 10),
            at("depends.", 20, 12, 60, 10),
            at("Footer", 0, 90, 50, 10),
        ];
        assert_eq!(text_of(&words), "What is this?\nIt depends.\n\nFooter");
    }

    #[test]
    fn lines_carry_ids_and_the_rect_of_their_words() {
        let words = vec![
            at("What", 0, 2, 40, 10),
            at("is", 45, 0, 15, 12),
            at("It", 0, 14, 15, 10),
            at("Footer", 0, 90, 50, 10),
        ];
        let l = lines_of(&words);
        assert_eq!(l.len(), 3);
        assert_eq!((l[0].id, l[0].x, l[0].y, l[0].w, l[0].h), (1, 0, 0, 60, 12));
        assert_eq!(l[0].text, "What is");
        assert!(!l[1].para);
        assert!(l[2].para);
        assert_eq!(l[2].id, 3);
    }

    #[test]
    fn evenly_spaced_options_are_one_paragraph_whatever_their_glyphs() {
        // From a live run: 25 px pitch, bands of 14 (no descender) and 18
        // (descender). Measured against the line above, B and D came out as
        // paragraphs and A and C did not.
        let words = vec![
            at("Which", 17, 69, 50, 14),
            at("planet?", 72, 69, 60, 18),
            at("A)", 17, 119, 20, 14),
            at("Mercury", 42, 119, 60, 14),
            at("B)", 17, 144, 20, 14),
            at("Jupiter", 42, 144, 60, 18),
            at("C)", 17, 169, 20, 14),
            at("Mars", 42, 169, 40, 14),
            at("D)", 17, 194, 20, 14),
            at("Venus", 42, 194, 50, 14),
        ];
        let paras: Vec<bool> = lines_of(&words).iter().map(|l| l.para).collect();
        assert_eq!(paras, [false, true, false, false, false]);
    }

    #[test]
    fn text_of_handles_empty_and_single() {
        assert_eq!(text_of(&[]), "");
        assert_eq!(text_of(&[at("solo", 0, 0, 10, 10)]), "solo");
    }

    #[test]
    fn new_names_the_missing_binary() {
        let err = Tesseract::new("definitely-not-a-real-ocr-binary").unwrap_err();
        assert!(err.contains("definitely-not-a-real-ocr-binary"), "{err}");
    }

    /// Skips cleanly where tesseract is absent; this is the only test that
    /// touches the real engine, and it asserts the handshake, not recognition
    /// quality (which depends on language data that may not be installed).
    #[test]
    fn real_binary_constructs_when_present() {
        if Tesseract::new("tesseract").is_err() {
            eprintln!("skipping: tesseract not installed");
            return;
        }
        assert!(Tesseract::new("tesseract").is_ok());
    }

    impl Word {
        fn into_conf(mut self, conf: f32) -> Word {
            self.conf = conf;
            self
        }
    }

    #[test]
    fn a_two_column_row_splits_at_the_gutter_and_reads_column_by_column() {
        // Tesseract's order on a two-column page: each row of both columns.
        let words = vec![
            at("The", 17, 25, 30, 18),
            at("Revolution", 57, 25, 100, 18),
            at("Quiz:", 537, 25, 50, 18),
            at("which", 597, 25, 50, 18),
            at("and", 17, 50, 30, 18),
            at("reshaped", 57, 50, 80, 18),
            at("begin?", 538, 50, 60, 18),
            at("A)", 536, 101, 20, 16),
            at("1776", 566, 101, 40, 16),
        ];
        let l = lines_of(&words);
        let text: Vec<&str> = l.iter().map(|l| l.text.as_str()).collect();
        assert_eq!(
            text,
            [
                "The Revolution",
                "and reshaped",
                "Quiz: which",
                "begin?",
                "A) 1776"
            ]
        );
        let ids: Vec<usize> = l.iter().map(|l| l.id).collect();
        assert_eq!(ids, [1, 2, 3, 4, 5]);
        assert!(l[2].para, "the right column starts a block");
        assert!(!l[3].para && l[4].para);
    }

    #[test]
    fn a_lowercase_prose_line_is_not_a_paragraph() {
        // Live: `would save local businesses about ...` boxed 14 px tall
        // among 18 px lines, and with most words x-height only (10 px) the
        // median word was shorter than the 11 px between that line and the
        // next: `id=7 ... para=true` in the middle of a paragraph.
        let mut words = Vec::new();
        for (y, tall, h) in [
            (175, "The", 18),
            (200, "would", 14),
            (225, "year", 18),
            (250, "dollars", 18),
            (300, "The", 18),
        ] {
            words.push(at(tall, 17, y, 50, h));
            for (i, low) in ["save", "a", "now", "or", "vow"].iter().enumerate() {
                words.push(at(low, 80 + 50 * i as i32, y + 4, 40, 10));
            }
        }
        let paras: Vec<bool> = lines_of(&words).iter().map(|l| l.para).collect();
        assert_eq!(paras, [false, false, false, false, true]);
    }
}
