// SPDX-License-Identifier: AGPL-3.0-only
//! Text runs and measurement. Shaping lives in `apoth-font`; layout here only needs
//! advances, so it asks a [`Measure`].

use crate::tokens::limits;

/// Who owns the text. Program text is drawn in the monospace family so it is visually
/// distinct from interface text (T-18 §6).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TextOrigin {
    Interface,
    Program,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Weight {
    Regular,
    Strong,
}

#[derive(Clone, Debug, PartialEq)]
pub struct TextRun {
    pub text: String,
    pub size: f32,
    pub weight: Weight,
    pub origin: TextOrigin,
    /// Truncate with an ellipsis past this width.
    pub max_width: Option<f32>,
}

impl TextRun {
    pub fn interface(text: impl Into<String>, size: f32) -> TextRun {
        TextRun {
            text: text.into(),
            size,
            weight: Weight::Regular,
            origin: TextOrigin::Interface,
            max_width: None,
        }
    }

    /// Program-supplied text: sanitised and length-capped here, so no caller can forget.
    pub fn program(text: &str, size: f32) -> TextRun {
        TextRun {
            text: sanitise_program_text(text),
            size,
            weight: Weight::Regular,
            origin: TextOrigin::Program,
            max_width: None,
        }
    }

    pub fn strong(mut self) -> TextRun {
        self.weight = Weight::Strong;
        self
    }

    pub fn clipped(mut self, w: f32) -> TextRun {
        self.max_width = Some(w.max(0.0));
        self
    }
}

/// T-18 §6: control characters, bidi overrides and isolates stripped; length capped.
pub fn sanitise_program_text(s: &str) -> String {
    let mut out = String::new();
    for (n, c) in s.chars().filter(|c| !is_unsafe_char(*c)).enumerate() {
        if n >= limits::PROGRAM_TEXT_CHARS {
            out.push('\u{2026}');
            break;
        }
        out.push(c);
    }
    out
}

fn is_unsafe_char(c: char) -> bool {
    c.is_control()
        || matches!(c, '\u{061C}' | '\u{200E}' | '\u{200F}' | '\u{202A}'..='\u{202E}' | '\u{2066}'..='\u{2069}')
}

/// Width of a run in logical pixels.
pub trait Measure {
    fn width(&self, run: &TextRun) -> f32;
}

/// Every char is `advance` em wide. Deterministic stand-in for tests and first light.
#[derive(Clone, Copy, Debug)]
pub struct FixedAdvance(pub f32);

impl Measure for FixedAdvance {
    fn width(&self, run: &TextRun) -> f32 {
        let w = run.text.chars().count() as f32 * self.0 * run.size;
        match run.max_width {
            Some(m) => w.min(m),
            None => w,
        }
    }
}

/// The advance, in ems, every layout that decides a hit rectangle assumes. The main thread
/// has no shaper, so geometry that main hit-tests (bar buttons, search chips) is computed
/// from this on both threads and the drawn text is clipped into it.
pub const GEOMETRY_ADVANCE: f32 = 0.6;

/// The measure for hit geometry (see [`GEOMETRY_ADVANCE`]).
pub fn geometry() -> FixedAdvance {
    FixedAdvance(GEOMETRY_ADVANCE)
}
