// SPDX-License-Identifier: AGPL-3.0-only
//! One hit-test tree per frame, front to back (T-08 §5), and the keyboard focus stack.

use crate::geom::{Point, Rect};
use crate::state::WidgetId;

/// Front to back: sheets -> bars -> tab strip -> split handles -> pane frames ->
/// resize margin -> grid.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Layer {
    Sheet,
    Bar,
    TabStrip,
    Handle,
    PaneFrame,
    ResizeMargin,
    Grid,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Hit {
    pub id: WidgetId,
    pub layer: Layer,
    pub rect: Rect,
}

/// Rebuilt every frame. Within one layer a later registration is in front.
#[derive(Clone, Debug, Default)]
pub struct HitTree {
    entries: Vec<Hit>,
}

impl HitTree {
    pub fn new() -> HitTree {
        HitTree::default()
    }

    pub fn clear(&mut self) {
        self.entries.clear();
    }

    pub fn add(&mut self, id: WidgetId, layer: Layer, rect: Rect) {
        self.entries.push(Hit { id, layer, rect });
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// The frontmost entry under `p`, or `None` (not even the grid registered).
    pub fn hit(&self, p: Point) -> Option<Hit> {
        let mut best: Option<(usize, Hit)> = None;
        for (i, h) in self.entries.iter().enumerate() {
            if !h.rect.contains(p) {
                continue;
            }
            let better = match best {
                None => true,
                Some((bi, b)) => h.layer < b.layer || (h.layer == b.layer && i > bi),
            };
            if better {
                best = Some((i, *h));
            }
        }
        best.map(|(_, h)| h)
    }

    /// A modal sheet swallows the pointer: anything not on the sheet layer hits nothing
    /// beyond the sheet while one is open.
    pub fn hit_modal(&self, p: Point) -> Option<Hit> {
        if self.entries.iter().any(|h| h.layer == Layer::Sheet) {
            self.hit(p).filter(|h| h.layer == Layer::Sheet)
        } else {
            self.hit(p)
        }
    }
}

/// Keyboard focus owners that can capture the keyboard.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Owner {
    Sheet(WidgetId),
    Palette(WidgetId),
}

/// A modal sheet or palette owns the keyboard until dismissed; Escape always dismisses.
#[derive(Clone, Debug, Default)]
pub struct FocusStack {
    stack: Vec<Owner>,
}

impl FocusStack {
    pub fn push(&mut self, o: Owner) {
        self.stack.push(o);
    }

    pub fn top(&self) -> Option<Owner> {
        self.stack.last().copied()
    }

    /// The keyboard goes to the UI, not the terminal.
    pub fn captures_keyboard(&self) -> bool {
        !self.stack.is_empty()
    }

    /// Escape: dismiss the topmost owner.
    pub fn escape(&mut self) -> Option<Owner> {
        self.stack.pop()
    }
}

/// Zones `interface.focus-next` / `focus-prev` cycle through.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Zone {
    Terminal,
    TabStrip,
    Bars,
}

/// Next zone, skipping zones that are absent. Terminal -> tab strip -> bars -> terminal.
pub fn cycle(cur: Zone, forward: bool, tabs_present: bool, bars_present: bool) -> Zone {
    const ORDER: [Zone; 3] = [Zone::Terminal, Zone::TabStrip, Zone::Bars];
    let mut i = ORDER.iter().position(|z| *z == cur).unwrap_or(0);
    for _ in 0..ORDER.len() {
        i = if forward { (i + 1) % 3 } else { (i + 2) % 3 };
        let z = ORDER[i];
        let present = match z {
            Zone::Terminal => true,
            Zone::TabStrip => tabs_present,
            Zone::Bars => bars_present,
        };
        if present {
            return z;
        }
    }
    cur
}
