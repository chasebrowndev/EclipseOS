// SPDX-License-Identifier: AGPL-3.0-only
//! Quick Tile, edge tiling and snapping under `mode "de"` (M5): the placement
//! and its transitions, the Super+arrow rule, the tiled states, the drag paths
//! (driven through the real `pointer_moved` / `pointer_button` entry points)
//! and the snap rules.

use smithay::{
    backend::input::{ButtonState, KeyState},
    desktop::{layer_map_for_output, Window},
    input::keyboard::Keycode,
    reexports::wayland_protocols::xdg::shell::server::xdg_toplevel::State,
    utils::{Logical, Point, Rectangle},
};

use super::{
    de_chrome_tests::{click, content, de_window, flip, float_rect, move_to, on_bar_at, pt, settle, LEFT},
    focus,
    focus::state_tests::{
        client::{Client, Toplevel},
        harness, Harness,
    },
    maximize,
    quicktile::{self, EdgeTarget, Zone},
    titlebar, winops,
    workspace::Placement,
};
use crate::{config::Mode, input::Direction, shell};

const KEY_LEFTSHIFT: u32 = 42;

fn usable_of(h: &Harness, id: u64) -> Rectangle<i32, Logical> {
    let o = h.state.outputs.get(id).expect("output").output.clone();
    layer_map_for_output(&o).arrange();
    shell::usable_area(&h.state, &o)
}

fn placement(h: &Harness, w: &Window) -> Placement {
    maximize::placement_of(&h.state, w)
}

fn tiled_states(w: &Window) -> [bool; 4] {
    w.toplevel().unwrap().with_pending_state(|s| {
        [
            State::TiledLeft,
            State::TiledRight,
            State::TiledTop,
            State::TiledBottom,
        ]
        .map(|t| s.states.contains(t))
    })
}

fn focus_on(h: &mut Harness, w: &Window) {
    focus::focus_window(&mut h.state, w);
}

fn tile(h: &mut Harness, w: &Window, dir: Direction) {
    focus_on(h, w);
    quicktile::bind(&mut h.state, dir);
}

fn bar_of(h: &Harness, w: &Window) -> i32 {
    titlebar::bar_height(&h.state, w)
}

/// A decorated window under `de`, shrunk so several fit on an output, and put
/// at the frame rectangle `at`.
fn small_window(h: &mut Harness, c: &mut Client, at: Rectangle<i32, Logical>) -> (Toplevel, Window) {
    let (t, w) = de_window(h, c);
    shell::resize_window(&mut h.state, &w, Some(at.size.w), Some(at.size.h)).unwrap();
    c.pump(h);
    settle(h, c, &t);
    let bar = bar_of(h, &w);
    let border = h.state.config.general.border_size;
    shell::place_at(&mut h.state, &w, titlebar::content_of_outer(at, border, bar));
    c.pump(h);
    settle(h, c, &t);
    (t, w)
}

/// Put `w` back at the frame `at`.
fn put_frame(h: &mut Harness, w: &Window, at: Rectangle<i32, Logical>) {
    let bar = bar_of(h, w);
    let border = h.state.config.general.border_size;
    shell::place_at(&mut h.state, w, titlebar::content_of_outer(at, border, bar));
}

/// Drag `w` by its bar. Two presses in a row would be a double-click
/// (maximize), so the click tracker is cleared first.
fn drag_bar(h: &mut Harness, w: &Window, dx: f64, dy: f64) {
    h.state.bar_ui.forget(w);
    let p = on_bar_at(h, w, 0.3);
    move_to(h, p);
    click(h, LEFT, ButtonState::Pressed);
    move_to(h, pt(p.x + dx, p.y + dy));
    click(h, LEFT, ButtonState::Released);
}

fn shift(h: &mut Harness, down: bool) {
    let state = if down {
        KeyState::Pressed
    } else {
        KeyState::Released
    };
    h.state.keyboard_key(Keycode::new(KEY_LEFTSHIFT + 8), state, 1);
}

// ------------------------------------------------------------- geometry

#[test]
fn a_tile_fills_its_zone_with_the_frame_and_the_content_follows() {
    let mut h = harness();
    let mut c = Client::connect(&mut h);
    let (t, w) = de_window(&mut h, &mut c);
    let before = float_rect(&h, &w);
    tile(&mut h, &w, Direction::Left);
    c.pump(&mut h);
    let id = shell::output_of_window(&h.state, &w).unwrap();
    let zone = Zone::Left.rect(usable_of(&h, id));
    assert_eq!(
        float_rect(&h, &w),
        zone,
        "the frame, titlebar included, is the zone"
    );
    let Placement::QuickTile { zone: z, restore } = placement(&h, &w) else {
        panic!("not tiled");
    };
    assert_eq!((z, restore), (Zone::Left, before));
    // The client is configured for the frame less the bar, with no border.
    let bar = bar_of(&h, &w);
    assert_eq!(
        c.configured_sizes(&t.toplevel).last().copied(),
        Some((zone.size.w, zone.size.h - bar))
    );
    settle(&mut h, &mut c, &t);
    assert_eq!(content(&h, &w), titlebar::content_of_outer(zone, 0, bar));
}

#[test]
fn zones_follow_an_output_resize_and_every_zone_fills_its_quarter() {
    let mut h = harness();
    let mut c = Client::connect(&mut h);
    let (_, w) = de_window(&mut h, &mut c);
    tile(&mut h, &w, Direction::Right);
    tile(&mut h, &w, Direction::Down);
    let id = shell::output_of_window(&h.state, &w).unwrap();
    assert_eq!(float_rect(&h, &w), Zone::BottomRight.rect(usable_of(&h, id)));
    let o = h.state.outputs.get(id).unwrap().output.clone();
    let mode = smithay::output::Mode {
        size: (1200, 900).into(),
        refresh: 60_000,
    };
    o.change_current_state(Some(mode), None, None, None);
    shell::arrange(&mut h.state);
    let u = usable_of(&h, id);
    assert_eq!(u.size, (1200, 900).into());
    assert_eq!(
        float_rect(&h, &w),
        Zone::BottomRight.rect(u),
        "followed the new area"
    );
}

// ------------------------------------------------------------ the states

#[test]
fn tiled_states_follow_the_zone_and_clear_on_leaving() {
    let mut h = harness();
    let mut c = Client::connect(&mut h);
    let (_, w) = de_window(&mut h, &mut c);
    assert_eq!(tiled_states(&w), [false; 4]);
    tile(&mut h, &w, Direction::Left);
    assert_eq!(
        tiled_states(&w),
        [true, false, true, true],
        "left half: left, top, bottom"
    );
    tile(&mut h, &w, Direction::Up);
    assert_eq!(tiled_states(&w), [true, false, true, false], "top-left quarter");
    tile(&mut h, &w, Direction::Right);
    assert_eq!(tiled_states(&w), [true, true, true, false], "top half");
    assert!(
        !w.toplevel()
            .unwrap()
            .with_pending_state(|s| s.states.contains(State::Maximized)),
        "a tile is not maximized"
    );
    tile(&mut h, &w, Direction::Down);
    assert_eq!(placement(&h, &w), Placement::Free);
    assert_eq!(tiled_states(&w), [false; 4], "cleared on leaving the placement");
}

// ------------------------------------------------------- the key rule

#[test]
fn super_arrows_compose_step_back_and_return_to_the_free_rectangle() {
    let mut h = harness();
    let mut c = Client::connect(&mut h);
    let (_, w) = de_window(&mut h, &mut c);
    let before = float_rect(&h, &w);
    let zone = |h: &Harness| quicktile::zone_of(&h.state, &w);
    tile(&mut h, &w, Direction::Left);
    assert_eq!(zone(&h), Some(Zone::Left));
    tile(&mut h, &w, Direction::Up);
    assert_eq!(
        zone(&h),
        Some(Zone::TopLeft),
        "Left then Up is the top-left quarter"
    );
    tile(&mut h, &w, Direction::Left);
    assert_eq!(zone(&h), Some(Zone::TopLeft), "the same way again does nothing");
    tile(&mut h, &w, Direction::Right);
    assert_eq!(zone(&h), Some(Zone::Top), "the opposite direction steps back");
    tile(&mut h, &w, Direction::Down);
    assert_eq!(zone(&h), None, "and out of the last tile to floating");
    assert_eq!(float_rect(&h, &w), before, "the free rectangle comes back");

    // Up from a free window is the top half; the other order composes too.
    tile(&mut h, &w, Direction::Up);
    assert_eq!(zone(&h), Some(Zone::Top));
    tile(&mut h, &w, Direction::Right);
    assert_eq!(zone(&h), Some(Zone::TopRight));
    tile(&mut h, &w, Direction::Down);
    assert_eq!(zone(&h), Some(Zone::Right));
    tile(&mut h, &w, Direction::Left);
    assert_eq!(zone(&h), None);
    assert_eq!(float_rect(&h, &w), before);
}

#[test]
fn the_tile_bind_does_nothing_outside_de_and_while_fullscreen() {
    let mut h = harness();
    let mut c = Client::connect(&mut h);
    let (_, w) = de_window(&mut h, &mut c);
    flip(&mut h, Mode::Hybrid);
    let tiled_before = shell::is_floating(&h.state, &w);
    tile(&mut h, &w, Direction::Left);
    assert_eq!(quicktile::zone_of(&h.state, &w), None);
    assert_eq!(shell::is_floating(&h.state, &w), tiled_before, "no layout change");

    flip(&mut h, Mode::De);
    let (_, f) = de_window(&mut h, &mut c);
    focus_on(&mut h, &f);
    shell::toggle_fullscreen(&mut h.state);
    quicktile::bind(&mut h.state, Direction::Left);
    assert_eq!(
        quicktile::zone_of(&h.state, &f),
        None,
        "fullscreen wins over the bind"
    );
}

// ------------------------------------------------------- transitions

#[test]
fn a_tile_survives_minimize_a_workspace_move_and_a_rearrange() {
    let mut h = harness();
    let mut c = Client::connect(&mut h);
    let (_, w) = de_window(&mut h, &mut c);
    tile(&mut h, &w, Direction::Right);
    let id = shell::output_of_window(&h.state, &w).unwrap();
    let want = Zone::Right.rect(usable_of(&h, id));

    shell::minimize_window(&mut h.state, &w);
    shell::unminimize_window(&mut h.state, &w);
    assert_eq!(quicktile::zone_of(&h.state, &w), Some(Zone::Right));
    assert_eq!(float_rect(&h, &w), want, "minimize and back");

    shell::move_window_to_workspace(&mut h.state, &w, 3);
    shell::arrange(&mut h.state);
    assert_eq!(quicktile::zone_of(&h.state, &w), Some(Zone::Right));
    assert_eq!(float_rect(&h, &w), want, "moved to workspace 3");

    for _ in 0..3 {
        shell::arrange(&mut h.state);
    }
    assert_eq!(float_rect(&h, &w), want, "arrange is idempotent");
}

#[test]
fn removing_an_output_moves_the_tile_to_the_next_one_keeping_its_zone() {
    let mut h = harness();
    let mut c = Client::connect(&mut h);
    h.state.outputs.set_focused(h.b);
    let (_, w) = de_window(&mut h, &mut c);
    tile(&mut h, &w, Direction::Up);
    tile(&mut h, &w, Direction::Right);
    assert_eq!(shell::output_of_window(&h.state, &w), Some(h.b));
    let ia = h.a;
    crate::outputs::unregister(&mut h.state, h.b);
    shell::arrange(&mut h.state);
    assert_eq!(shell::output_of_window(&h.state, &w), Some(ia));
    assert_eq!(quicktile::zone_of(&h.state, &w), Some(Zone::TopRight));
    assert_eq!(float_rect(&h, &w), Zone::TopRight.rect(usable_of(&h, ia)));
    assert!(placement(&h, &w)
        .restore_rect()
        .is_some_and(|r| usable_of(&h, ia).contains_rect(r)));
}

#[test]
fn an_output_move_by_bind_keeps_the_zone_on_the_new_output() {
    let mut h = harness();
    let mut c = Client::connect(&mut h);
    let (_, w) = de_window(&mut h, &mut c);
    tile(&mut h, &w, Direction::Left);
    tile(&mut h, &w, Direction::Down);
    focus_on(&mut h, &w);
    let ib = h.b;
    let number = h.state.outputs.get(ib).unwrap().number;
    shell::move_to_output_workspace(&mut h.state, number);
    assert_eq!(shell::output_of_window(&h.state, &w), Some(ib));
    assert_eq!(quicktile::zone_of(&h.state, &w), Some(Zone::BottomLeft));
    assert_eq!(float_rect(&h, &w), Zone::BottomLeft.rect(usable_of(&h, ib)));
}

#[test]
fn fullscreen_wins_over_a_tile_and_returns_to_it() {
    let mut h = harness();
    let mut c = Client::connect(&mut h);
    let (_, w) = de_window(&mut h, &mut c);
    tile(&mut h, &w, Direction::Left);
    let id = shell::output_of_window(&h.state, &w).unwrap();
    let want = Zone::Left.rect(usable_of(&h, id));
    focus_on(&mut h, &w);
    shell::toggle_fullscreen(&mut h.state);
    assert!(h.state.fullscreen.contains_key(&w));
    assert_eq!(
        quicktile::zone_of(&h.state, &w),
        Some(Zone::Left),
        "the tile is kept"
    );
    assert_eq!(
        tiled_states(&w),
        [false; 4],
        "a fullscreen window is not told it is tiled"
    );
    shell::toggle_fullscreen(&mut h.state);
    assert!(!h.state.fullscreen.contains_key(&w));
    assert_eq!(quicktile::zone_of(&h.state, &w), Some(Zone::Left));
    assert_eq!(float_rect(&h, &w), want, "back on the tile");
    assert_eq!(tiled_states(&w), [true, false, true, true]);
}

#[test]
fn maximize_replaces_a_tile_and_restore_gives_back_the_original_free_rect() {
    let mut h = harness();
    let mut c = Client::connect(&mut h);
    let (_, w) = de_window(&mut h, &mut c);
    let before = float_rect(&h, &w);
    tile(&mut h, &w, Direction::Left);
    assert!(maximize::set(&mut h.state, &w, Some((true, true))));
    let id = shell::output_of_window(&h.state, &w).unwrap();
    assert!(maximize::is_maximized(&h.state, &w));
    let o = h.state.outputs.get(id).expect("output").output.clone();
    assert_eq!(float_rect(&h, &w), shell::tiling_area(&h.state, &o));
    assert_eq!(tiled_states(&w), [false; 4]);
    assert!(maximize::set(&mut h.state, &w, None));
    assert_eq!(placement(&h, &w), Placement::Free);
    assert_eq!(float_rect(&h, &w), before, "the free rect, not the tile rect");

    // And the other way: a tile over a maximize also restores to the free rect.
    assert!(maximize::set(&mut h.state, &w, Some((true, true))));
    tile(&mut h, &w, Direction::Right);
    assert_eq!(quicktile::zone_of(&h.state, &w), Some(Zone::Right));
    assert!(!maximize::is_maximized(&h.state, &w));
    tile(&mut h, &w, Direction::Left);
    assert_eq!(placement(&h, &w), Placement::Free);
    assert_eq!(float_rect(&h, &w), before);
}

#[test]
fn shade_keep_above_and_sticky_keep_working_on_a_tile() {
    let mut h = harness();
    let mut c = Client::connect(&mut h);
    let (_, w) = de_window(&mut h, &mut c);
    let (_, other) = de_window(&mut h, &mut c);
    tile(&mut h, &w, Direction::Right);
    let rect = float_rect(&h, &w);
    assert!(winops::set_shaded(&mut h.state, &w, true));
    assert_eq!(
        quicktile::zone_of(&h.state, &w),
        Some(Zone::Right),
        "shading keeps the tile"
    );
    assert_eq!(float_rect(&h, &w), rect);
    assert!(winops::set_shaded(&mut h.state, &w, false));
    assert_eq!(float_rect(&h, &w), rect, "unshade restores on the tile");
    assert!(winops::set_keep_above(&mut h.state, &w, true));
    assert!(winops::set_sticky(&mut h.state, &w, true));
    focus_on(&mut h, &other);
    shell::arrange(&mut h.state);
    let order: Vec<Window> = h.state.space.elements().cloned().collect();
    let (iw, io) = (
        order.iter().position(|e| e == &w).unwrap(),
        order.iter().position(|e| e == &other).unwrap(),
    );
    assert!(iw > io, "a keep-above tile stays over the focused window");
    shell::switch_workspace(&mut h.state, 2);
    assert!(h.state.space.elements().any(|e| e == &w), "sticky");
    assert_eq!(quicktile::zone_of(&h.state, &w), Some(Zone::Right));
    // A tile unshades a rolled-up window, as maximize does.
    shell::switch_workspace(&mut h.state, 1);
    assert!(winops::set_shaded(&mut h.state, &w, true));
    tile(&mut h, &w, Direction::Up);
    assert!(!h.state.winops.is_shaded(&w));
}

#[test]
fn leaving_de_clears_the_tile_restores_the_rect_and_tells_the_client() {
    let mut h = harness();
    let mut c = Client::connect(&mut h);
    flip(&mut h, Mode::De);
    let t = c.create_toplevel(&mut h);
    // A fixed-size window stays floating when `de` is left.
    t.toplevel.set_min_size(300, 200);
    t.toplevel.set_max_size(300, 200);
    c.commit(&mut h, &t.surface);
    c.attach(&mut h, &t.surface);
    let w = super::de_chrome_tests::window_of(&h, &t);
    let before = float_rect(&h, &w);
    tile(&mut h, &w, Direction::Left);
    assert_eq!(tiled_states(&w), [true, false, true, true]);
    flip(&mut h, Mode::Hybrid);
    assert_eq!(placement(&h, &w), Placement::Free, "no placement outside de");
    assert_eq!(tiled_states(&w), [false; 4], "the client is told it is not tiled");
    if shell::is_floating(&h.state, &w) {
        assert_eq!(float_rect(&h, &w), before, "the old rectangle back");
    }
}

// ----------------------------------------------------------- dragging

#[test]
fn dragging_a_tile_out_restores_its_size_under_the_pointer_along_the_bar() {
    let mut h = harness();
    let mut c = Client::connect(&mut h);
    let (t, w) = de_window(&mut h, &mut c);
    let before = float_rect(&h, &w);
    tile(&mut h, &w, Direction::Left);
    c.pump(&mut h);
    settle(&mut h, &mut c, &t);
    let bar = titlebar::bar_geometry(&h.state, &w).unwrap();
    let (px, py) = (
        f64::from(bar.loc.x) + f64::from(bar.size.w) * 0.25,
        f64::from(bar.loc.y) + 8.0,
    );
    // Where the pointer is along the bar when the drag begins (it has moved a
    // few px since the press): that proportion is kept.
    let frac = (px + 10.0 - f64::from(bar.loc.x)) / f64::from(bar.size.w);
    move_to(&mut h, pt(px, py));
    click(&mut h, LEFT, ButtonState::Pressed);
    move_to(&mut h, pt(px + 10.0, py + 10.0));
    assert_eq!(placement(&h, &w), Placement::Free, "the drag ended the tile");
    assert_eq!(float_rect(&h, &w).size, before.size, "the previous size");
    c.pump(&mut h);
    settle(&mut h, &mut c, &t);
    let nb = titlebar::bar_geometry(&h.state, &w).unwrap();
    let at = (px + 10.0 - f64::from(nb.loc.x)) / f64::from(nb.size.w);
    assert!(
        (at - frac).abs() < 0.02,
        "same place along the bar: {at} vs {frac}"
    );
    let dy = py + 10.0 - f64::from(nb.loc.y);
    assert!(
        (0.0..f64::from(nb.size.h)).contains(&dy),
        "still on the bar: {dy}"
    );
    assert_eq!(tiled_states(&w), [false; 4]);
    click(&mut h, LEFT, ButtonState::Released);
}

#[test]
fn dragging_to_a_screen_edge_previews_the_zone_and_tiles_on_release() {
    let mut h = harness();
    let mut c = Client::connect(&mut h);
    let (_, w) = small_window(
        &mut h,
        &mut c,
        Rectangle::new(Point::from((300, 200)), (200, 150).into()),
    );
    let id = shell::output_of_window(&h.state, &w).unwrap();
    let u = usable_of(&h, id);
    let g = h
        .state
        .space
        .output_geometry(&h.state.outputs.get(id).unwrap().output)
        .unwrap();
    let p = on_bar_at(&h, &w, 0.3);
    move_to(&mut h, p);
    click(&mut h, LEFT, ButtonState::Pressed);
    move_to(&mut h, pt(p.x + 6.0, p.y + 6.0));
    assert!(h.state.tile_preview.is_none(), "mid-screen: no preview");
    // Left edge, middle: the left half.
    move_to(&mut h, pt(f64::from(g.loc.x), f64::from(g.loc.y + g.size.h / 2)));
    let pv = h.state.tile_preview.clone().expect("preview");
    assert_eq!(pv.target, EdgeTarget::Tile(Zone::Left));
    assert_eq!(pv.rect, Zone::Left.rect(u));
    // Moving away cancels it.
    move_to(&mut h, pt(f64::from(g.loc.x + 300), f64::from(g.loc.y + 300)));
    assert!(h.state.tile_preview.is_none());
    // A corner is a quarter, the top edge maximizes.
    move_to(&mut h, pt(f64::from(g.loc.x), f64::from(g.loc.y)));
    assert_eq!(
        h.state.tile_preview.as_ref().unwrap().target,
        EdgeTarget::Tile(Zone::TopLeft)
    );
    move_to(&mut h, pt(f64::from(g.loc.x + 300), f64::from(g.loc.y)));
    let pv = h.state.tile_preview.clone().expect("top");
    assert_eq!((pv.target, pv.rect), (EdgeTarget::Maximize, u));
    // Releasing on a side tiles.
    move_to(&mut h, pt(f64::from(g.loc.x), f64::from(g.loc.y + g.size.h / 2)));
    click(&mut h, LEFT, ButtonState::Released);
    assert!(h.state.tile_preview.is_none(), "the preview goes with the drop");
    assert_eq!(quicktile::zone_of(&h.state, &w), Some(Zone::Left));
    assert_eq!(float_rect(&h, &w), Zone::Left.rect(u));
}

#[test]
fn a_touchpad_drag_previews_from_the_dragged_point_not_the_stale_pointer() {
    let mut h = harness();
    let mut c = Client::connect(&mut h);
    let (_, w) = small_window(
        &mut h,
        &mut c,
        Rectangle::new(Point::from((300, 200)), (200, 150).into()),
    );
    let id = shell::output_of_window(&h.state, &w).unwrap();
    let u = usable_of(&h, id);
    let g = h
        .state
        .space
        .output_geometry(&h.state.outputs.get(id).unwrap().output)
        .unwrap();
    let p = on_bar_at(&h, &w, 0.3);
    move_to(&mut h, p);
    crate::input::grabs::begin_gesture_drag_for_test(&mut h.state, &w);
    // The pointer sits mid-screen and never moves; the fingers carry the
    // window to the left edge.
    let to_edge = f64::from(g.loc.x) - p.x;
    crate::input::grabs::update_drag_gesture(&mut h.state, pt(to_edge / 2.0, 0.0));
    assert!(h.state.tile_preview.is_none(), "halfway: no preview");
    crate::input::grabs::update_drag_gesture(&mut h.state, pt(to_edge / 2.0, 0.0));
    let pv = h.state.tile_preview.clone().expect("preview at the edge");
    assert_eq!(pv.target, EdgeTarget::Tile(Zone::Left));
    crate::input::grabs::end_drag_gesture(&mut h.state, false);
    assert_eq!(quicktile::zone_of(&h.state, &w), Some(Zone::Left));
    assert_eq!(float_rect(&h, &w), Zone::Left.rect(u));
    assert!(
        ec_abyss_render::userdata::is_quick_tiled(&w),
        "drawn by placement"
    );
    quicktile::set(&mut h.state, &w, None);
    assert!(!ec_abyss_render::userdata::is_quick_tiled(&w));
}

#[test]
fn releasing_at_the_top_edge_maximizes() {
    let mut h = harness();
    let mut c = Client::connect(&mut h);
    let (_, w) = small_window(
        &mut h,
        &mut c,
        Rectangle::new(Point::from((300, 200)), (200, 150).into()),
    );
    let id = shell::output_of_window(&h.state, &w).unwrap();
    let g = h
        .state
        .space
        .output_geometry(&h.state.outputs.get(id).unwrap().output)
        .unwrap();
    let p = on_bar_at(&h, &w, 0.3);
    move_to(&mut h, p);
    click(&mut h, LEFT, ButtonState::Pressed);
    move_to(&mut h, pt(f64::from(g.loc.x + 300), f64::from(g.loc.y)));
    click(&mut h, LEFT, ButtonState::Released);
    assert!(maximize::is_maximized(&h.state, &w));
}

#[test]
fn an_edge_shared_by_two_outputs_does_not_trigger_but_the_outer_edges_do() {
    let mut h = harness();
    let mut c = Client::connect(&mut h);
    let (_, w) = small_window(
        &mut h,
        &mut c,
        Rectangle::new(Point::from((300, 200)), (200, 150).into()),
    );
    let id = shell::output_of_window(&h.state, &w).unwrap();
    let og = |h: &Harness, i: u64| {
        h.state
            .space
            .output_geometry(&h.state.outputs.get(i).unwrap().output)
            .unwrap()
    };
    let (g, other) = (og(&h, id), og(&h, if id == h.a { h.b } else { h.a }));
    let p = on_bar_at(&h, &w, 0.3);
    move_to(&mut h, p);
    click(&mut h, LEFT, ButtonState::Pressed);
    move_to(&mut h, pt(p.x + 6.0, p.y + 6.0));
    // Whichever side the other output is on, that edge does not trigger...
    let (shared, outer) = if other.loc.x >= g.loc.x + g.size.w {
        (g.loc.x + g.size.w - 1, g.loc.x)
    } else {
        (g.loc.x, g.loc.x + g.size.w - 1)
    };
    move_to(&mut h, pt(f64::from(shared), f64::from(g.loc.y + g.size.h / 2)));
    assert!(h.state.tile_preview.is_none(), "a shared edge never triggers");
    // ...and the opposite, outer, one does.
    move_to(&mut h, pt(f64::from(outer), f64::from(g.loc.y + g.size.h / 2)));
    assert!(h.state.tile_preview.is_some(), "an outer edge does");
    click(&mut h, LEFT, ButtonState::Released);
}

#[test]
fn edge_tiling_can_be_switched_off() {
    let mut h = harness();
    let mut c = Client::connect(&mut h);
    let (_, w) = small_window(
        &mut h,
        &mut c,
        Rectangle::new(Point::from((300, 200)), (200, 150).into()),
    );
    h.state.config.general.edge_tile = false;
    let id = shell::output_of_window(&h.state, &w).unwrap();
    let g = h
        .state
        .space
        .output_geometry(&h.state.outputs.get(id).unwrap().output)
        .unwrap();
    let p = on_bar_at(&h, &w, 0.3);
    move_to(&mut h, p);
    click(&mut h, LEFT, ButtonState::Pressed);
    move_to(&mut h, pt(f64::from(g.loc.x + 300), f64::from(g.loc.y)));
    assert!(h.state.tile_preview.is_none());
    click(&mut h, LEFT, ButtonState::Released);
    assert_eq!(placement(&h, &w), Placement::Free);
}

// ------------------------------------------------------------- snapping

#[test]
fn a_move_snaps_to_the_usable_area_and_shift_turns_it_off() {
    let mut h = harness();
    let mut c = Client::connect(&mut h);
    let (_, w) = small_window(
        &mut h,
        &mut c,
        Rectangle::new(Point::from((300, 200)), (200, 150).into()),
    );
    let id = shell::output_of_window(&h.state, &w).unwrap();
    let u = usable_of(&h, id);
    let start = float_rect(&h, &w);
    // Release 6 px short of the left edge: it snaps flush.
    drag_bar(&mut h, &w, f64::from(u.loc.x + 6 - start.loc.x), 0.0);
    assert_eq!(float_rect(&h, &w).loc.x, u.loc.x, "snapped to the area edge");
    // The same drag with Shift held does not snap.
    put_frame(&mut h, &w, start);
    c.pump(&mut h);
    shift(&mut h, true);
    drag_bar(&mut h, &w, f64::from(u.loc.x + 6 - start.loc.x), 0.0);
    shift(&mut h, false);
    assert_eq!(float_rect(&h, &w).loc.x, u.loc.x + 6, "Shift moves freely");
    // And the flag turns it off outright.
    put_frame(&mut h, &w, start);
    h.state.config.general.snap = false;
    drag_bar(&mut h, &w, f64::from(u.loc.x + 6 - start.loc.x), 0.0);
    assert_eq!(float_rect(&h, &w).loc.x, u.loc.x + 6);
}

fn right_of(r: Rectangle<i32, Logical>) -> i32 {
    r.loc.x + r.size.w
}

#[test]
fn a_move_snaps_to_another_windows_frame_and_ignores_the_ones_that_do_not_count() {
    let mut h = harness();
    let mut c = Client::connect(&mut h);
    let (_, b) = small_window(
        &mut h,
        &mut c,
        Rectangle::new(Point::from((420, 100)), (200, 150).into()),
    );
    let (_, a) = small_window(
        &mut h,
        &mut c,
        Rectangle::new(Point::from((60, 300)), (200, 150).into()),
    );
    let id = shell::output_of_window(&h.state, &a).unwrap();
    let u = usable_of(&h, id);
    let (fa, fb) = (float_rect(&h, &a), float_rect(&h, &b));
    // Move `a` right so its right edge ends 6 px left of `b`'s left edge, and
    // far enough below that only the horizontal axis can snap... they overlap
    // vertically within the snap distance of nothing else.
    let dx = f64::from(fb.loc.x - 6 - right_of(fa));
    let set_back = |h: &mut Harness| put_frame(h, &a, fa);
    // `a` sits below `b` here, so lift it into b's rows first.
    let lift = f64::from(fb.loc.y - fa.loc.y);
    drag_bar(&mut h, &a, dx, lift);
    assert_eq!(
        right_of(float_rect(&h, &a)),
        fb.loc.x,
        "flush against the other window"
    );
    assert!(u.contains_rect(float_rect(&h, &a)));

    // A minimized window is not there to snap to.
    set_back(&mut h);
    shell::minimize_window(&mut h.state, &b);
    drag_bar(&mut h, &a, dx, lift);
    assert_eq!(right_of(float_rect(&h, &a)), fb.loc.x - 6, "minimized: ignored");
    shell::unminimize_window(&mut h.state, &b);

    // A rolled-up window is ignored too.
    set_back(&mut h);
    assert!(winops::set_shaded(&mut h.state, &b, true));
    drag_bar(&mut h, &a, dx, lift);
    assert_eq!(right_of(float_rect(&h, &a)), fb.loc.x - 6, "shaded: ignored");
    winops::set_shaded(&mut h.state, &b, false);

    // So is one on another workspace.
    set_back(&mut h);
    h.state.config.general.follow_window_to_workspace = false;
    shell::move_window_to_workspace(&mut h.state, &b, 4);
    drag_bar(&mut h, &a, dx, lift);
    assert_eq!(
        right_of(float_rect(&h, &a)),
        fb.loc.x - 6,
        "other workspace: ignored"
    );
}

#[test]
fn a_resize_snaps_the_moving_edges() {
    let mut h = harness();
    let mut c = Client::connect(&mut h);
    let (_, w) = small_window(
        &mut h,
        &mut c,
        Rectangle::new(Point::from((300, 200)), (200, 150).into()),
    );
    let id = shell::output_of_window(&h.state, &w).unwrap();
    let u = usable_of(&h, id);
    let f = float_rect(&h, &w);
    // The band is just outside the right edge of the frame.
    let from = pt(f64::from(right_of(f)) + 2.0, f64::from(f.loc.y + f.size.h / 2));
    move_to(&mut h, from);
    click(&mut h, LEFT, ButtonState::Pressed);
    assert!(h.state.pointer_grab_active, "the band started a resize");
    let target = u.loc.x + u.size.w - 7;
    move_to(&mut h, pt(from.x + f64::from(target - right_of(f)), from.y));
    click(&mut h, LEFT, ButtonState::Released);
    assert_eq!(
        right_of(float_rect(&h, &w)),
        u.loc.x + u.size.w,
        "snapped to the area edge"
    );
    assert_eq!(float_rect(&h, &w).loc, f.loc, "the fixed edges stayed");
}

fn zone_of_win(h: &Harness, w: &Window) -> Option<Zone> {
    quicktile::zone_of(&h.state, w)
}

/// Every pair of tiled windows on the output is disjoint.
fn assert_no_tile_overlap(h: &Harness, ws: &[&Window]) {
    let id = shell::output_of_window(&h.state, ws[0]).unwrap();
    let usable = usable_of(h, id);
    for (i, a) in ws.iter().enumerate() {
        for b in &ws[i + 1..] {
            if let (Some(za), Some(zb)) = (zone_of_win(h, a), zone_of_win(h, b)) {
                assert!(
                    !za.rect(usable).overlaps(zb.rect(usable)),
                    "{za:?} overlaps {zb:?}"
                );
            }
        }
    }
}

#[test]
fn tiling_onto_an_occupied_zone_swaps_the_occupant_to_the_free_side() {
    let mut h = harness();
    let mut c = Client::connect(&mut h);
    let (_ta, a) = de_window(&mut h, &mut c);
    let (_tb, b) = de_window(&mut h, &mut c);
    tile(&mut h, &a, Direction::Left);
    tile(&mut h, &b, Direction::Right);
    assert_eq!(zone_of_win(&h, &a), Some(Zone::Left));
    assert_eq!(zone_of_win(&h, &b), Some(Zone::Right));
    // b goes to the occupied left half (as an edge drop would): a is swapped over.
    quicktile::set(&mut h.state, &b, Some(Zone::Left));
    assert_eq!(zone_of_win(&h, &b), Some(Zone::Left));
    assert_eq!(zone_of_win(&h, &a), Some(Zone::Right));
    assert_no_tile_overlap(&h, &[&a, &b]);
}

#[test]
fn tiling_onto_a_half_reflows_quarter_occupants_and_a_third_window_never_overlaps() {
    let mut h = harness();
    let mut c = Client::connect(&mut h);
    let (_ta, a) = de_window(&mut h, &mut c);
    let (_tb, b) = de_window(&mut h, &mut c);
    let (_tc, w) = de_window(&mut h, &mut c);
    quicktile::set(&mut h.state, &a, Some(Zone::TopLeft));
    quicktile::set(&mut h.state, &b, Some(Zone::BottomLeft));
    quicktile::set(&mut h.state, &w, Some(Zone::Left));
    assert_eq!(zone_of_win(&h, &w), Some(Zone::Left));
    assert_eq!(zone_of_win(&h, &a), Some(Zone::TopRight));
    assert_eq!(zone_of_win(&h, &b), Some(Zone::BottomRight));
    assert_no_tile_overlap(&h, &[&a, &b, &w]);
    // Nowhere free: a full-height tile over a half and two quarters on the
    // right frees nothing, so the occupants go back to floating.
    quicktile::set(&mut h.state, &w, Some(Zone::Top));
    assert_no_tile_overlap(&h, &[&a, &b, &w]);
}

#[test]
fn a_client_moveresize_needs_a_floating_window_and_a_held_button_on_it() {
    use smithay::reexports::wayland_protocols::xdg::shell::server::xdg_toplevel::ResizeEdge;
    let mut h = harness();
    let mut c = Client::connect(&mut h);
    let (_t, w) = de_window(&mut h, &mut c);
    let r = content(&h, &w);
    let inside = pt(
        r.loc.x as f64 + r.size.w as f64 / 2.0,
        r.loc.y as f64 + r.size.h as f64 / 2.0,
    );
    move_to(&mut h, inside);

    // No button held: refused.
    assert!(!crate::input::grabs::start_client_moveresize(
        &mut h.state,
        w.clone(),
        None
    ));

    click(&mut h, LEFT, ButtonState::Pressed);
    // Held on the window, floating: a move starts, and replaces the click grab.
    assert!(crate::input::grabs::start_client_moveresize(
        &mut h.state,
        w.clone(),
        None
    ));
    assert!(h.state.pointer_grab_active);
    click(&mut h, LEFT, ButtonState::Released);
    assert!(!h.state.pointer_grab_active);

    // A resize with an explicit edge.
    click(&mut h, LEFT, ButtonState::Pressed);
    assert!(crate::input::grabs::start_client_moveresize(
        &mut h.state,
        w.clone(),
        Some(ResizeEdge::BottomRight)
    ));
    click(&mut h, LEFT, ButtonState::Released);

    // Not over the window: refused.
    click(&mut h, LEFT, ButtonState::Pressed);
    let far = pt(1.0, 1.0);
    h.state.pointer_location = far;
    let (_t2, other) = de_window(&mut h, &mut c);
    assert!(!crate::input::grabs::start_client_moveresize(
        &mut h.state,
        other,
        None
    ));
    click(&mut h, LEFT, ButtonState::Released);
}
