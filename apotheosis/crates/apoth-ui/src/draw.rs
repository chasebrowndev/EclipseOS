// SPDX-License-Identifier: AGPL-3.0-only
//! The draw list. No blur, glow, bloom or gradient primitive exists, by design (T-08 §1).

use crate::geom::{Point, Rect};
use crate::text::TextRun;
use crate::tokens::Rgba;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Rim {
    pub width: f32,
    pub colour: Rgba,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum IconId {
    Close,
    Plus,
    Warning,
    Shield,
    ArrowUp,
    ArrowDown,
    /// A play triangle, centred on its centroid. Lowered as a shape, never a font glyph.
    Play,
    /// A speaker with a cross beside it (muted). Lowered as shapes.
    SpeakerMuted,
    /// A close cross drawn as two strokes (the media window's close button).
    Cross,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Cmd {
    RoundedRect {
        rect: Rect,
        radius: f32,
        fill: Rgba,
        rim: Option<Rim>,
    },
    Text {
        run: TextRun,
        pos: Point,
        colour: Rgba,
    },
    Icon {
        icon: IconId,
        rect: Rect,
        colour: Rgba,
    },
    PushClip {
        rect: Rect,
        radius: f32,
    },
    PopClip,
    /// Analytic rounded-rect shadow (no blur pass); blur-off fallback only.
    Shadow {
        rect: Rect,
        radius: f32,
        spread: f32,
        alpha: f32,
    },
    /// Ends a layer: what follows is drawn over the next floating pane's grid, what came
    /// before under it (a float's glass backing is the last thing before its mark).
    Layer,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct DrawList {
    pub cmds: Vec<Cmd>,
}

impl DrawList {
    pub fn new() -> DrawList {
        DrawList::default()
    }

    pub fn rect(&mut self, rect: Rect, radius: f32, fill: Rgba, rim: Option<Rim>) {
        self.cmds.push(Cmd::RoundedRect {
            rect,
            radius,
            fill,
            rim,
        });
    }

    pub fn text(&mut self, run: TextRun, pos: Point, colour: Rgba) {
        self.cmds.push(Cmd::Text { run, pos, colour });
    }

    pub fn icon(&mut self, icon: IconId, rect: Rect, colour: Rgba) {
        self.cmds.push(Cmd::Icon { icon, rect, colour });
    }

    pub fn push_clip(&mut self, rect: Rect, radius: f32) {
        self.cmds.push(Cmd::PushClip { rect, radius });
    }

    pub fn pop_clip(&mut self) {
        self.cmds.push(Cmd::PopClip);
    }

    /// See [`Cmd::Layer`].
    pub fn layer(&mut self) {
        self.cmds.push(Cmd::Layer);
    }

    pub fn len(&self) -> usize {
        self.cmds.len()
    }

    pub fn is_empty(&self) -> bool {
        self.cmds.is_empty()
    }

    /// Stable one-line-per-command dump used by the goldens. Colours print as
    /// `rrggbbaa` hex bytes.
    pub fn dump(&self) -> String {
        let mut s = String::new();
        for c in &self.cmds {
            match c {
                Cmd::RoundedRect {
                    rect,
                    radius,
                    fill,
                    rim,
                } => {
                    s += &format!("rect {} r{:.1} fill {}", r(rect), radius, col(*fill));
                    if let Some(rim) = rim {
                        s += &format!(" rim {:.1} {}", rim.width, col(rim.colour));
                    }
                }
                Cmd::Text { run, pos, colour } => {
                    s += &format!(
                        "text {:?} @{:.1},{:.1} {:?}/{:?} sz{:.1} {}",
                        run.text,
                        pos.x,
                        pos.y,
                        run.origin,
                        run.weight,
                        run.size,
                        col(*colour)
                    );
                }
                Cmd::Icon { icon, rect, colour } => {
                    s += &format!("icon {:?} {} {}", icon, r(rect), col(*colour));
                }
                Cmd::PushClip { rect, radius } => s += &format!("clip {} r{:.1}", r(rect), radius),
                Cmd::PopClip => s += "unclip",
                Cmd::Layer => s += "layer",
                Cmd::Shadow {
                    rect,
                    radius,
                    spread,
                    alpha,
                } => {
                    s += &format!(
                        "shadow {} r{:.1} spread {:.1} a{:.2}",
                        r(rect),
                        radius,
                        spread,
                        alpha
                    );
                }
            }
            s.push('\n');
        }
        s
    }
}

fn r(rect: &Rect) -> String {
    format!("[{:.1},{:.1} {:.1}x{:.1}]", rect.x, rect.y, rect.w, rect.h)
}

fn col(c: Rgba) -> String {
    let b = |v: f32| (v.clamp(0.0, 1.0) * 255.0).round() as u8;
    format!(
        "{:02x}{:02x}{:02x}{:02x}",
        b(c.0[0]),
        b(c.0[1]),
        b(c.0[2]),
        b(c.0[3])
    )
}
