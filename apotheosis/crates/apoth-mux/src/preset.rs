// SPDX-License-Identifier: AGPL-3.0-only
//! Layout presets (MUX-13): `even-h`, `even-v`, `main-h`, `main-v`, `tall`, `grid`, `stack`,
//! applied over the pane list. The spec names them without parameters; the definitions here
//! are the ones this crate pins (reported as a spec gap):
//!
//! - `even-h` / `even-v`: equal columns / rows.
//! - `main-h`: first pane on top (half the height), the rest equal columns below.
//! - `main-v`: first pane left (half the width), the rest equal rows on the right.
//! - `tall`: like `main-v` with the main pane at 60 %.
//! - `grid`: `ceil(sqrt n)` columns, rows of equal height, the last row may be short.
//! - `stack`: all panes in one vertical stack, the focused (else first) one expanded.

use crate::geom::Orient;
use crate::tree::{Node, PaneId, RATIO_ONE, Split, Stack, StackStyle, ratio_of};

/// A layout preset.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Preset {
    /// Equal columns.
    EvenH,
    /// Equal rows.
    EvenV,
    /// Main pane on top.
    MainH,
    /// Main pane on the left.
    MainV,
    /// Main pane on the left at 60 %.
    Tall,
    /// Grid.
    Grid,
    /// One stack.
    Stack,
}

impl Preset {
    /// Cycle order of `layout.next`.
    pub const ALL: [Preset; 7] = [
        Preset::EvenH,
        Preset::EvenV,
        Preset::MainH,
        Preset::MainV,
        Preset::Tall,
        Preset::Grid,
        Preset::Stack,
    ];

    /// The config / action name.
    pub const fn name(self) -> &'static str {
        match self {
            Preset::EvenH => "even-h",
            Preset::EvenV => "even-v",
            Preset::MainH => "main-h",
            Preset::MainV => "main-v",
            Preset::Tall => "tall",
            Preset::Grid => "grid",
            Preset::Stack => "stack",
        }
    }

    /// Position in [`Preset::ALL`].
    pub fn index(self) -> usize {
        Preset::ALL.iter().position(|&p| p == self).unwrap_or(0)
    }

    /// Exact name, or a unique prefix (`main` is ambiguous, `ma` too; `g` is `grid`).
    pub fn parse(s: &str) -> Option<Preset> {
        if s.is_empty() {
            return None;
        }
        if let Some(p) = Preset::ALL.iter().find(|p| p.name() == s) {
            return Some(*p);
        }
        let mut hits = Preset::ALL.iter().filter(|p| p.name().starts_with(s));
        match (hits.next(), hits.next()) {
            (Some(p), None) => Some(*p),
            _ => None,
        }
    }
}

fn pane_nodes(ids: &[PaneId]) -> Vec<Node> {
    ids.iter().map(|&p| Node::Pane(p)).collect()
}

/// Right-nested chain giving every item an equal share along `orient`.
fn chain(orient: Orient, mut items: Vec<Node>) -> Node {
    let first = items.remove(0);
    if items.is_empty() {
        return first;
    }
    let k = items.len() as u32 + 1;
    let rest = chain(orient, items);
    Node::Split(Box::new(Split {
        orient,
        ratio: ratio_of(1, k),
        a: first,
        b: rest,
    }))
}

fn main_and_rest(orient: Orient, rest_orient: Orient, main_ratio: u32, ids: &[PaneId]) -> Node {
    let rest = chain(rest_orient, pane_nodes(&ids[1..]));
    Node::Split(Box::new(Split {
        orient,
        ratio: main_ratio.clamp(1, RATIO_ONE - 1),
        a: Node::Pane(ids[0]),
        b: rest,
    }))
}

/// Build the tree for `preset` over `ids` (tree order). `ids` must be non-empty.
pub fn build(preset: Preset, ids: &[PaneId], focus: Option<PaneId>) -> Node {
    if ids.len() == 1 {
        return Node::Pane(ids[0]);
    }
    match preset {
        Preset::EvenH => chain(Orient::Horizontal, pane_nodes(ids)),
        Preset::EvenV => chain(Orient::Vertical, pane_nodes(ids)),
        Preset::MainH => main_and_rest(Orient::Vertical, Orient::Horizontal, RATIO_ONE / 2, ids),
        Preset::MainV => main_and_rest(Orient::Horizontal, Orient::Vertical, RATIO_ONE / 2, ids),
        Preset::Tall => main_and_rest(Orient::Horizontal, Orient::Vertical, 600_000, ids),
        Preset::Grid => {
            let n = ids.len();
            let mut cols = 1;
            while cols * cols < n {
                cols += 1;
            }
            let rows: Vec<Node> = ids
                .chunks(cols)
                .map(|c| chain(Orient::Horizontal, pane_nodes(c)))
                .collect();
            chain(Orient::Vertical, rows)
        }
        Preset::Stack => {
            let active = focus
                .and_then(|f| ids.iter().position(|&p| p == f))
                .unwrap_or(0);
            Node::Stack(Stack {
                panes: ids.to_vec(),
                active,
                style: StackStyle::Vertical,
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::geom::Rect;

    fn widths(n: &Node, r: Rect) -> Vec<(u64, Rect)> {
        let mut out = vec![];
        n.layout(r, &mut out);
        out.into_iter().map(|p| (p.pane, p.rect)).collect()
    }

    #[test]
    fn even_h_equal() {
        let t = build(Preset::EvenH, &[1, 2, 3, 4], None);
        let w = widths(&t, Rect::new(0, 0, 80, 10));
        assert!(w.iter().all(|(_, r)| r.w == 20));
    }

    #[test]
    fn grid_five() {
        let t = build(Preset::Grid, &[1, 2, 3, 4, 5], None);
        let mut v = vec![];
        t.leaves(&mut v);
        assert_eq!(v, [1, 2, 3, 4, 5]);
        let w = widths(&t, Rect::new(0, 0, 90, 30));
        assert_eq!(w.len(), 5);
        // 5 panes: 3 columns, rows of 3 and 2
        assert_eq!(w[0].1.h, 15);
        assert_eq!(w[0].1.w, 30);
        assert_eq!(w[4].1.w, 45);
    }

    #[test]
    fn main_v_shape() {
        let t = build(Preset::MainV, &[1, 2, 3], None);
        let w = widths(&t, Rect::new(0, 0, 100, 20));
        assert_eq!(w[0].1, Rect::new(0, 0, 50, 20));
        assert_eq!(w[1].1.h, 10);
    }

    #[test]
    fn stack_uses_focus() {
        let Node::Stack(s) = build(Preset::Stack, &[1, 2, 3], Some(3)) else {
            panic!()
        };
        assert_eq!(s.active, 2);
    }

    #[test]
    fn parse_prefix() {
        assert_eq!(Preset::parse("grid"), Some(Preset::Grid));
        assert_eq!(Preset::parse("g"), Some(Preset::Grid));
        assert_eq!(Preset::parse("main"), None);
        assert_eq!(Preset::parse("main-v"), Some(Preset::MainV));
        assert_eq!(Preset::parse(""), None);
        assert_eq!(Preset::parse("zz"), None);
        for p in Preset::ALL {
            assert_eq!(Preset::parse(p.name()), Some(p));
            assert_eq!(Preset::ALL[p.index()], p);
        }
    }
}
