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

/// Rebuild something a model can read. Joining everything with spaces loses
/// the difference between a heading and the sentence under it, and the model
/// is the only consumer that cares, so breaks are restored here rather than
/// being carried through [`Word`].
///
/// The rule is geometric because it has to work for any engine (§7): a word
/// whose vertical midpoint leaves the current line's band, or which starts to
/// the left of where the line began, opens a new line; if more than half a
/// line's worth of blank space sits between the two, it opens a blank line
/// instead, which is what a paragraph or a separate block looks like from here.
pub fn text_of(words: &[Word]) -> String {
    let mut out = String::new();
    let mut line_top = 0i32;
    let mut line_bottom = 0i32;
    let mut line_left = 0i32;
    let mut first = true;

    for w in words {
        if first {
            line_top = w.y;
            line_bottom = w.y + w.h;
            line_left = w.x;
            out.push_str(&w.text);
            first = false;
            continue;
        }

        let mid = w.y + w.h / 2;
        let height = (line_bottom - line_top).max(1);
        let same_line = mid >= line_top && mid < line_bottom && w.x >= line_left;

        if same_line {
            out.push(' ');
        } else if w.y - line_bottom > height / 2 {
            // A gap of more than half a line's worth of empty space below the
            // previous line: a paragraph or a different block on the page.
            out.push_str("\n\n");
            line_top = w.y;
            line_bottom = w.y + w.h;
            line_left = w.x;
        } else {
            out.push('\n');
            line_top = w.y;
            line_bottom = w.y + w.h;
            line_left = w.x;
        }

        if same_line {
            // Tall glyphs (parentheses, capitals) stretch the band so the rest
            // of the line keeps matching it.
            line_top = line_top.min(w.y);
            line_bottom = line_bottom.max(w.y + w.h);
        }
        out.push_str(&w.text);
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
            return Err(format!(
                "ocr: `{}` exited {}: {}",
                self.bin,
                out.status,
                if why.is_empty() { "no output" } else { why }
            ));
        }

        let tsv = String::from_utf8_lossy(&out.stdout);
        Ok(parse_tsv(&tsv, frame))
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

        // Frame-relative to screen-absolute. Callers anchor annotations on
        // these boxes, and the compositor only speaks logical screen
        // coordinates; doing it anywhere else means every caller repeating it.
        words.push(Word {
            text: text.to_string(),
            conf,
            x: frame.origin.x + left,
            y: frame.origin.y + top,
            w,
            h,
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
}
