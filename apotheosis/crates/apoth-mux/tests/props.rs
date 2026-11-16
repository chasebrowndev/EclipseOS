// SPDX-License-Identifier: AGPL-3.0-only
//! Property tests (T-13 §7, T-25 §4): random operation sequences keep the model
//! well-formed, pane rectangles tile the tab exactly, directional focus is symmetric,
//! minimum sizes hold, and layout KDL round-trips.

use apoth_mux::{
    Dir, FocusMove, LayoutSpec, MIN_COLS, MIN_ROWS, NodeSpec, Orient, PaneNode, PaneSpec, Preset,
    RATIO_ONE, Rect, Session, Size, SplitSize, StackStyle, TabSpec, WindowId, parse_layouts,
    write_layouts,
};
use apoth_mux::{FloatSpec, TabTarget};
use proptest::prelude::*;

#[derive(Clone, Debug)]
enum Op {
    Split(usize, Dir, u32, bool),
    Close(usize),
    Resize(usize, Dir, i32),
    Swap(usize, usize),
    SwapDir(usize, Dir),
    Rotate(bool),
    Zoom,
    Equalize,
    Focus(u8, Dir),
    NextPreset,
    Combine(usize, usize),
    StackAdd(usize),
    BreakOut(usize, Dir),
    Move(usize, usize, Dir),
    Orient(usize),
    ToFloat(usize),
    Embed(usize),
    BreakTab(usize),
    Join(usize, usize, Dir),
    NewTab,
    TabFocus(u8),
    WinResize(u32, u32),
}

fn dir() -> impl Strategy<Value = Dir> {
    proptest::sample::select(Dir::ALL.to_vec())
}

fn op(allow_shrink: bool) -> impl Strategy<Value = Op> {
    let idx = 0usize..64;
    let mut v: Vec<BoxedStrategy<Op>> = vec![
        (idx.clone(), dir(), 1u32..RATIO_ONE, any::<bool>())
            .prop_map(|(i, d, f, c)| Op::Split(i, d, f, c))
            .boxed(),
        idx.clone().prop_map(Op::Close).boxed(),
        (idx.clone(), dir(), -30i32..30)
            .prop_map(|(i, d, a)| Op::Resize(i, d, a))
            .boxed(),
        (idx.clone(), idx.clone())
            .prop_map(|(a, b)| Op::Swap(a, b))
            .boxed(),
        (idx.clone(), dir())
            .prop_map(|(a, d)| Op::SwapDir(a, d))
            .boxed(),
        any::<bool>().prop_map(Op::Rotate).boxed(),
        Just(Op::Zoom).boxed(),
        Just(Op::Equalize).boxed(),
        (0u8..4, dir()).prop_map(|(k, d)| Op::Focus(k, d)).boxed(),
        Just(Op::NextPreset).boxed(),
        (idx.clone(), idx.clone())
            .prop_map(|(a, b)| Op::Combine(a, b))
            .boxed(),
        idx.clone().prop_map(Op::StackAdd).boxed(),
        (idx.clone(), dir())
            .prop_map(|(a, d)| Op::BreakOut(a, d))
            .boxed(),
        (idx.clone(), idx.clone(), dir())
            .prop_map(|(a, b, d)| Op::Move(a, b, d))
            .boxed(),
        idx.clone().prop_map(Op::Orient).boxed(),
        idx.clone().prop_map(Op::ToFloat).boxed(),
        idx.clone().prop_map(Op::Embed).boxed(),
        idx.clone().prop_map(Op::BreakTab).boxed(),
        (idx.clone(), idx.clone(), dir())
            .prop_map(|(a, b, d)| Op::Join(a, b, d))
            .boxed(),
        Just(Op::NewTab).boxed(),
        (0u8..4).prop_map(Op::TabFocus).boxed(),
    ];
    if allow_shrink {
        v.push(
            (1u32..200, 1u32..60)
                .prop_map(|(c, r)| Op::WinResize(c, r))
                .boxed(),
        );
    }
    proptest::strategy::Union::new(v)
}

fn first_window(s: &Session<()>) -> Option<WindowId> {
    s.windows().next().map(|w| w.id)
}

fn run(s: &mut Session<()>, o: &Op) {
    let Some(w) = first_window(s) else { return };
    let panes = s.pane_ids();
    if panes.is_empty() {
        return;
    }
    let pick = |i: usize| panes[i % panes.len()];
    let tab_of = |s: &Session<()>, p| s.tab_of(p).unwrap();
    match *o {
        Op::Split(i, d, f, cells) => {
            let size = if cells {
                SplitSize::Cells(f % 40)
            } else {
                SplitSize::Fraction(f)
            };
            let _ = s.split_pane(pick(i), d, size, PaneSpec::default(), ());
        }
        Op::Close(i) => {
            let _ = s.close_pane(pick(i));
        }
        Op::Resize(i, d, a) => {
            let p = pick(i);
            let t = tab_of(s, p);
            let _ = s.tab_mut(t).unwrap().resize(p, d, a);
        }
        Op::Swap(a, b) => {
            let (p, q) = (pick(a), pick(b));
            if s.tab_of(p) == s.tab_of(q) {
                let t = tab_of(s, p);
                let _ = s.tab_mut(t).unwrap().swap(p, q);
            }
        }
        Op::SwapDir(a, d) => {
            let p = pick(a);
            let t = tab_of(s, p);
            let _ = s.tab_mut(t).unwrap().swap_dir(p, d);
        }
        Op::Rotate(f) => {
            let t = tab_of(s, pick(0));
            let _ = s.tab_mut(t).unwrap().rotate(f);
        }
        Op::Zoom => {
            let t = tab_of(s, pick(0));
            let _ = s.tab_mut(t).unwrap().zoom_toggle();
        }
        Op::Equalize => {
            let t = tab_of(s, pick(0));
            s.tab_mut(t).unwrap().equalize();
        }
        Op::Focus(k, d) => {
            let t = tab_of(s, pick(0));
            let mv = match k {
                0 => FocusMove::Dir(d),
                1 => FocusMove::Next,
                2 => FocusMove::Prev,
                _ => FocusMove::Last,
            };
            let tab = s.tab_mut(t).unwrap();
            if let Ok(p) = tab.focus_move(mv) {
                assert_eq!(tab.focused(), Some(p));
            }
        }
        Op::NextPreset => {
            let t = tab_of(s, pick(0));
            let _ = s.tab_mut(t).unwrap().next_preset();
        }
        Op::Combine(a, b) => {
            let (p, q) = (pick(a), pick(b));
            if s.tab_of(p) == s.tab_of(q) {
                let t = tab_of(s, p);
                let _ = s.tab_mut(t).unwrap().stack_combine(p, q);
            }
        }
        Op::StackAdd(a) => {
            let _ = s.stack_add_pane(pick(a), PaneSpec::default(), ());
        }
        Op::BreakOut(a, d) => {
            let p = pick(a);
            let t = tab_of(s, p);
            let _ = s.tab_mut(t).unwrap().stack_break_out(p, d);
        }
        Op::Move(a, b, d) => {
            let (p, q) = (pick(a), pick(b));
            if s.tab_of(p) == s.tab_of(q) {
                let t = tab_of(s, p);
                let _ = s.tab_mut(t).unwrap().move_pane(p, q, d);
            }
        }
        Op::Orient(a) => {
            let p = pick(a);
            let t = tab_of(s, p);
            let _ = s.tab_mut(t).unwrap().toggle_orient(p);
        }
        Op::ToFloat(a) => {
            let p = pick(a);
            let t = tab_of(s, p);
            let _ = s.tab_mut(t).unwrap().to_float(p, Rect::new(1, 1, 10, 5));
        }
        Op::Embed(a) => {
            let p = pick(a);
            let t = tab_of(s, p);
            let _ = s.tab_mut(t).unwrap().embed(p, None, Dir::Right);
        }
        Op::BreakTab(a) => {
            let _ = s.break_to_tab(pick(a));
        }
        Op::Join(a, b, d) => {
            let _ = s.join(pick(a), pick(b), d);
        }
        Op::NewTab => {
            let _ = s.new_tab(w, PaneSpec::default(), ());
        }
        Op::TabFocus(k) => {
            let t = match k {
                0 => TabTarget::Next,
                1 => TabTarget::Prev,
                2 => TabTarget::Last,
                _ => TabTarget::Index(0),
            };
            let _ = s.focus_tab(w, t);
        }
        Op::WinResize(c, r) => {
            let _ = s.resize_window(w, Size { cols: c, rows: r });
        }
    }
}

fn fresh(cols: u32, rows: u32) -> Session<()> {
    let mut s = Session::new("main");
    s.new_window(Size { cols, rows }, None, PaneSpec::default(), ());
    s
}

proptest! {
    #[test]
    fn random_ops_keep_model_well_formed(
        cols in 2u32..200, rows in 1u32..60,
        ops in proptest::collection::vec(op(true), 0..60),
    ) {
        let mut s = fresh(cols, rows);
        for o in &ops {
            if s.windows().next().is_none() {
                s.new_window(Size { cols, rows }, None, PaneSpec::default(), ());
            }
            run(&mut s, o);
            s.check_invariants().map_err(|e| TestCaseError::fail(format!("{e} after {o:?}")))?;
        }
    }

    #[test]
    fn minimums_hold_when_window_never_shrinks(
        cols in 2u32..200, rows in 1u32..60,
        ops in proptest::collection::vec(op(false), 0..60),
    ) {
        let mut s = fresh(cols, rows);
        for o in ops.iter() {
            if s.windows().next().is_none() {
                s.new_window(Size { cols, rows }, None, PaneSpec::default(), ());
            }
            run(&mut s, o);
            s.check_invariants().map_err(|e| TestCaseError::fail(format!("{e} after {o:?}")))?;
            for t in s.tab_ids() {
                for p in s.tab(t).unwrap().layout() {
                    if !p.header {
                        prop_assert!(p.rect.w >= MIN_COLS && p.rect.h >= MIN_ROWS, "{p:?} after {o:?}");
                    }
                }
            }
        }
    }

    #[test]
    fn directional_neighbours_are_symmetric(
        cols in 20u32..200, rows in 8u32..60,
        ops in proptest::collection::vec(op(false), 0..50),
    ) {
        let mut s = fresh(cols, rows);
        for o in ops.iter() {
            if s.windows().next().is_none() {
                s.new_window(Size { cols, rows }, None, PaneSpec::default(), ());
            }
            run(&mut s, o);
        }
        for t in s.tab_ids() {
            let tab = s.tab(t).unwrap();
            for a in tab.panes() {
                for d in Dir::ALL {
                    for b in tab.neighbours(a, d) {
                        prop_assert!(
                            tab.neighbours(b, d.opposite()).contains(&a),
                            "{a} -> {b} {d:?} not mirrored"
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn directional_focus_lands_on_a_neighbour(
        cols in 20u32..200, rows in 8u32..60,
        ops in proptest::collection::vec(op(false), 0..40),
        d in dir(),
    ) {
        let mut s = fresh(cols, rows);
        for o in ops.iter() {
            if s.windows().next().is_none() {
                s.new_window(Size { cols, rows }, None, PaneSpec::default(), ());
            }
            run(&mut s, o);
        }
        for t in s.tab_ids() {
            let tab = s.tab_mut(t).unwrap();
            let before = tab.focused();
            let Some(cur) = before else { continue };
            let cands = tab.neighbours(cur, d);
            match tab.focus_move(FocusMove::Dir(d)) {
                Ok(p) => prop_assert_eq!(Some(&p), cands.first()),
                Err(_) => prop_assert!(cands.is_empty()),
            }
        }
    }

    #[test]
    fn split_then_close_restores_geometry(
        cols in 8u32..200, rows in 4u32..60, d in dir(), f in 1u32..RATIO_ONE,
    ) {
        let mut s = fresh(cols, rows);
        let p = s.pane_ids()[0];
        let t = s.tab_of(p).unwrap();
        let before = s.tab(t).unwrap().layout();
        if let Ok(q) = s.split_pane(p, d, SplitSize::Fraction(f), PaneSpec::default(), ()) {
            s.check_invariants().unwrap();
            s.close_pane(q).unwrap();
            prop_assert_eq!(s.tab(t).unwrap().layout(), before);
        }
    }

    #[test]
    fn presets_tile_for_any_pane_count(n in 1usize..12, cols in 24u32..200, rows in 12u32..60) {
        let mut s = fresh(cols, rows);
        let mut last = s.pane_ids()[0];
        for _ in 1..n {
            last = s.split_pane(last, Dir::Right, SplitSize::Fraction(RATIO_ONE / 2), PaneSpec::default(), ())
                .or_else(|_| s.split_pane(last, Dir::Down, SplitSize::Fraction(RATIO_ONE / 2), PaneSpec::default(), ()))
                .unwrap_or(last);
        }
        let t = s.tab_of(last).unwrap();
        for p in Preset::ALL {
            let before = s.tab(t).unwrap().tiled_order();
            let tab = s.tab_mut(t).unwrap();
            if tab.apply_preset(p).is_ok() {
                tab.check_invariants().unwrap();
                prop_assert_eq!(tab.tiled_order().len(), before.len());
                prop_assert_eq!(tab.preset(), Some(p));
            }
        }
    }
}

// ------------------------------------------------------------ layout files

fn text() -> impl Strategy<Value = String> {
    proptest::collection::vec(any::<char>(), 0..8).prop_map(|v| v.into_iter().collect())
}

fn pane_node() -> impl Strategy<Value = PaneNode> {
    (
        proptest::option::of(text()),
        proptest::collection::vec(text(), 0..3),
        proptest::option::of(text()),
        any::<bool>(),
    )
        .prop_map(|(cwd, argv, profile, focus)| PaneNode {
            spec: PaneSpec { cwd, argv, profile },
            focus,
        })
}

fn orient() -> impl Strategy<Value = Orient> {
    prop_oneof![Just(Orient::Horizontal), Just(Orient::Vertical)]
}

fn node_spec() -> impl Strategy<Value = NodeSpec> {
    let leaf = prop_oneof![
        pane_node().prop_map(NodeSpec::Pane),
        (
            prop_oneof![Just(StackStyle::Vertical), Just(StackStyle::Horizontal)],
            proptest::collection::vec(pane_node(), 2..4),
            any::<usize>(),
        )
            .prop_map(|(style, panes, a)| NodeSpec::Stack {
                style,
                active: a % panes.len(),
                panes
            }),
    ];
    leaf.prop_recursive(4, 24, 2, |inner| {
        (orient(), 1u32..RATIO_ONE, inner.clone(), inner).prop_map(|(orient, ratio, a, b)| {
            NodeSpec::Split {
                orient,
                ratio,
                a: Box::new(a),
                b: Box::new(b),
            }
        })
    })
}

fn tab_spec() -> impl Strategy<Value = TabSpec> {
    let float = (
        0u32..50,
        0u32..50,
        0u32..80,
        0u32..40,
        any::<bool>(),
        any::<bool>(),
        pane_node(),
    )
        .prop_map(|(x, y, w, h, visible, pinned, pane)| FloatSpec {
            rect: Rect::new(x, y, w, h),
            visible,
            pinned,
            pane,
        });
    (
        text(),
        proptest::option::of(node_spec()),
        proptest::collection::vec(float, 0..3),
        pane_node(),
    )
        .prop_map(|(name, root, mut floats, extra)| {
            if root.is_none() && floats.is_empty() {
                floats.push(FloatSpec {
                    rect: Rect::new(0, 0, 5, 5),
                    visible: true,
                    pinned: false,
                    pane: extra,
                });
            }
            TabSpec { name, root, floats }
        })
}

fn layout_spec() -> impl Strategy<Value = LayoutSpec> {
    (text(), proptest::collection::vec(tab_spec(), 1..3))
        .prop_map(|(name, tabs)| LayoutSpec { name, tabs })
}

proptest! {
    #[test]
    fn layout_kdl_round_trip(ls in proptest::collection::vec(layout_spec(), 1..3)) {
        let text = write_layouts(&ls);
        prop_assert_eq!(parse_layouts(&text).unwrap(), ls);
    }

    #[test]
    fn applied_layouts_are_well_formed(l in layout_spec()) {
        let mut s: Session<()> = Session::new("main");
        let (w, ..) = s.new_window(Size { cols: 200, rows: 60 }, None, PaneSpec::default(), ());
        s.apply_layout(w, &l, |_, _| ()).unwrap();
        s.check_invariants().map_err(TestCaseError::fail)?;
        let cap = s.capture_layout(w, &l.name).unwrap();
        prop_assert_eq!(cap.tabs.len(), l.tabs.len() + 1);
        prop_assert_eq!(cap.tabs[1..].iter().map(TabSpec::pane_count).sum::<usize>(),
            l.tabs.iter().map(TabSpec::pane_count).sum::<usize>());
    }

    #[test]
    fn parser_never_panics(src in any::<String>(), cut in any::<usize>(), ls in proptest::collection::vec(layout_spec(), 1..2)) {
        let _ = parse_layouts(&src);
        let good = write_layouts(&ls);
        let chars: Vec<char> = good.chars().collect();
        let n = cut % (chars.len() + 1);
        let truncated: String = chars[..n].iter().collect();
        let _ = parse_layouts(&truncated);
        let mut dropped: Vec<char> = chars;
        if !dropped.is_empty() {
            dropped.remove(n % dropped.len());
        }
        let _ = parse_layouts(&dropped.into_iter().collect::<String>());
    }
}
