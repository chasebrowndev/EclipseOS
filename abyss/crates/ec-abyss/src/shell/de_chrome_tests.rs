// SPDX-License-Identifier: AGPL-3.0-only
//! `mode "de"` window operations (M4): titlebar drag and double-click, the
//! resize band, z-bands, shade, sticky, the window menu and cascade placement.
//! Driven through the real input entry points (`pointer_moved`,
//! `pointer_button`, `keyboard_key`) wherever the behaviour is input.

use smithay::{
    backend::input::{ButtonState, KeyState},
    desktop::Window,
    input::keyboard::Keycode,
    input::pointer::{CursorIcon, CursorImageStatus},
    reexports::wayland_protocols::xdg::{
        decoration::zv1::server::zxdg_toplevel_decoration_v1::Mode as Deco,
        shell::server::xdg_toplevel::ResizeEdge,
    },
    utils::{Logical, Point, Rectangle},
};

use super::{
    edge, focus,
    focus::state_tests::{
        client::{Client, Toplevel},
        harness, Harness,
    },
    maximize, owned_windows,
    titlebar::{self, bar_geometry, ClickTracker, DOUBLE_CLICK_MS, DRAG_THRESHOLD},
    window_surface,
    winmenu::{self, Item},
    winops::{self, sort_by_band, Band},
};
use crate::{config::Mode, shell};

pub(super) const LEFT: u32 = 0x110;
pub(super) const RIGHT: u32 = 0x111;
const MIDDLE: u32 = 0x112;
// evdev codes; +8 is the xkb keycode.
const KEY_ESC: u32 = 1;
const KEY_ENTER: u32 = 28;
const KEY_UP: u32 = 103;
const KEY_DOWN: u32 = 108;
const KEY_END: u32 = 107;
const KEY_A: u32 = 30;

// ---------------------------------------------------------------- helpers

pub(super) fn window_of(h: &Harness, t: &Toplevel) -> Window {
    use wayland_client::Proxy;
    let id = t.surface.id().protocol_id();
    owned_windows(&h.state)
        .into_iter()
        .find(|w| {
            window_surface(w)
                .is_some_and(|s| smithay::reexports::wayland_server::Resource::id(&s).protocol_id() == id)
        })
        .expect("placed")
}

pub(super) fn flip(h: &mut Harness, mode: Mode) {
    let mut next = h.state.config.clone();
    next.mode = mode;
    crate::config::apply_loaded(&mut h.state, next);
}

pub(super) fn ssd(h: &mut Harness, w: &Window) {
    w.toplevel()
        .expect("xdg")
        .with_pending_state(|s| s.decoration_mode = Some(Deco::ServerSide));
    shell::arrange(&mut h.state);
}

pub(super) fn settle(h: &mut Harness, c: &mut Client, t: &Toplevel) {
    let (w, ht) = *c.configured_sizes(&t.toplevel).last().expect("configured");
    c.attach_sized(h, &t.surface, w, ht);
}

/// A decorated window under `de`, drawn at the size it was given.
pub(super) fn de_window(h: &mut Harness, c: &mut Client) -> (Toplevel, Window) {
    flip(h, Mode::De);
    let t = c.create_toplevel(h);
    c.commit(h, &t.surface);
    c.attach(h, &t.surface);
    let w = window_of(h, &t);
    ssd(h, &w);
    c.pump(h);
    settle(h, c, &t);
    (t, w)
}

pub(super) fn content(h: &Harness, w: &Window) -> Rectangle<i32, Logical> {
    h.state.space.element_geometry(w).expect("mapped")
}

pub(super) fn frame(h: &Harness, w: &Window) -> Rectangle<i32, Logical> {
    let bar = h.state.config.decoration.titlebar_height;
    titlebar::outer_of_content(content(h, w), h.state.config.general.border_size, bar)
}

/// The stored frame (what placement works on); the content the space reports
/// lags until the client draws its new size.
pub(super) fn float_rect(h: &Harness, w: &Window) -> Rectangle<i32, Logical> {
    h.state
        .outputs
        .iter()
        .flat_map(|e| e.workspaces.iter())
        .flat_map(|ws| ws.floating.iter())
        .find(|f| &f.window == w)
        .map(|f| f.rect)
        .expect("floating")
}

pub(super) fn move_to(h: &mut Harness, p: Point<f64, Logical>) {
    h.state.pointer_moved(p, 0);
}

pub(super) fn click(h: &mut Harness, button: u32, state: ButtonState) {
    h.state.pointer_button(button, state, 0);
}

pub(super) fn key(h: &mut Harness, code: u32) {
    h.state.keyboard_key(Keycode::new(code + 8), KeyState::Pressed, 1);
    h.state
        .keyboard_key(Keycode::new(code + 8), KeyState::Released, 2);
}

pub(super) fn pt(x: f64, y: f64) -> Point<f64, Logical> {
    Point::from((x, y))
}

/// A point on `w`'s bar, `frac` of the way along, clear of the buttons.
pub(super) fn on_bar_at(h: &Harness, w: &Window, frac: f64) -> Point<f64, Logical> {
    let bar = bar_geometry(&h.state, w).expect("has a bar");
    let usable = f64::from(bar.size.w) - 3.0 * f64::from(bar.size.h + 8) - 12.0;
    pt(
        f64::from(bar.loc.x) + 6.0 + usable * frac,
        f64::from(bar.loc.y) + f64::from(bar.size.h) / 2.0,
    )
}

pub(super) fn centre(r: Rectangle<i32, Logical>) -> Point<f64, Logical> {
    pt(
        f64::from(r.loc.x) + f64::from(r.size.w) / 2.0,
        f64::from(r.loc.y) + f64::from(r.size.h) / 2.0,
    )
}

fn order(h: &Harness) -> Vec<Window> {
    h.state.space.elements().cloned().collect()
}

fn index(h: &Harness, w: &Window) -> usize {
    order(h).iter().position(|e| e == w).expect("mapped")
}

fn holders(h: &Harness, w: &Window) -> usize {
    h.state
        .outputs
        .iter()
        .flat_map(|e| e.workspaces.iter())
        .filter(|ws| ws.holds(w))
        .count()
}

/// Three decorated windows, bottom to top.
fn three(h: &mut Harness, c: &mut Client) -> [(Toplevel, Window); 3] {
    let a = de_window(h, c);
    let b = de_window(h, c);
    let d = de_window(h, c);
    [a, b, d]
}

// --------------------------------------------------------------- z-bands

#[test]
fn bands_sort_stably_below_then_normal_then_above() {
    let mut v = vec![
        ('a', Band::Above),
        ('b', Band::Normal),
        ('c', Band::Below),
        ('d', Band::Normal),
        ('e', Band::Above),
        ('f', Band::Below),
    ];
    sort_by_band(&mut v, |t| t.1);
    let names: String = v.iter().map(|t| t.0).collect();
    assert_eq!(names, "cfbdae", "each band keeps its own order");
}

#[test]
fn raising_a_normal_window_never_crosses_a_band() {
    // The workspace raises by moving to the end of the stack; the layout then
    // sorts by band. Whatever was raised last, the bands hold.
    let mut stack = vec![
        ('a', Band::Above),
        ('b', Band::Normal),
        ('c', Band::Below),
        ('d', Band::Normal),
    ];
    for raised in ['b', 'd', 'b', 'c', 'a'] {
        let i = stack.iter().position(|t| t.0 == raised).unwrap();
        let e = stack.remove(i);
        stack.push(e);
        sort_by_band(&mut stack, |t| t.1);
        let bands: Vec<Band> = stack.iter().map(|t| t.1).collect();
        assert!(bands.windows(2).all(|w| w[0] <= w[1]), "{raised}: {bands:?}");
    }
}

#[test]
fn keep_above_and_below_hold_in_the_real_stack() {
    let mut h = harness();
    let mut c = Client::connect(&mut h);
    let [(_, a), (_, b), (_, d)] = three(&mut h, &mut c);
    assert!(index(&h, &a) < index(&h, &b) && index(&h, &b) < index(&h, &d));

    assert!(winops::set_keep_above(&mut h.state, &a, true));
    assert!(
        index(&h, &a) > index(&h, &b) && index(&h, &a) > index(&h, &d),
        "a is over both"
    );
    // Raising a normal window does not cross it.
    focus::focus_window_raising(&mut h.state, &b, true);
    assert!(index(&h, &b) < index(&h, &a));
    focus::focus_window_raising(&mut h.state, &d, true);
    assert!(index(&h, &d) < index(&h, &a));

    assert!(winops::set_keep_below(&mut h.state, &d, true));
    assert!(
        index(&h, &d) < index(&h, &b) && index(&h, &d) < index(&h, &a),
        "d is under both"
    );
    // Raising the below window does not lift it over the normal one.
    focus::focus_window_raising(&mut h.state, &d, true);
    assert!(index(&h, &d) < index(&h, &b));
    assert_eq!(h.state.winops.band(&a), Band::Above);
    assert_eq!(h.state.winops.band(&d), Band::Below);
}

#[test]
fn above_and_below_exclude_each_other() {
    let mut h = harness();
    let mut c = Client::connect(&mut h);
    let (_, w) = de_window(&mut h, &mut c);
    winops::set_keep_above(&mut h.state, &w, true);
    winops::set_keep_below(&mut h.state, &w, true);
    assert_eq!(h.state.winops.band(&w), Band::Below);
    winops::set_keep_above(&mut h.state, &w, true);
    assert_eq!(h.state.winops.band(&w), Band::Above);
    assert!(winops::toggle_keep_above(&mut h.state, &w));
    assert_eq!(h.state.winops.band(&w), Band::Normal);
}

#[test]
fn a_band_survives_minimize_and_a_workspace_move() {
    let mut h = harness();
    let mut c = Client::connect(&mut h);
    let [(_, a), (_, b), _] = three(&mut h, &mut c);
    winops::set_keep_above(&mut h.state, &a, true);
    shell::minimize_window(&mut h.state, &a);
    assert_eq!(h.state.winops.band(&a), Band::Above, "minimize keeps it");
    shell::unminimize_window(&mut h.state, &a);
    assert!(index(&h, &a) > index(&h, &b), "and it is over the others again");

    shell::move_window_to_workspace(&mut h.state, &a, 3);
    assert_eq!(h.state.winops.band(&a), Band::Above, "a workspace move keeps it");
}

#[test]
fn flags_go_when_the_window_closes_and_when_de_is_left() {
    let mut h = harness();
    let mut c = Client::connect(&mut h);
    let [(_, a), (_, b), _] = three(&mut h, &mut c);
    winops::set_keep_above(&mut h.state, &a, true);
    winops::set_sticky(&mut h.state, &b, true);
    winops::set_shaded(&mut h.state, &b, true);
    shell::unmap_window(&mut h.state, &a);
    assert_eq!(h.state.winops.band(&a), Band::Normal, "closing forgets it");
    flip(&mut h, Mode::Hybrid);
    assert!(h.state.winops.is_empty(), "leaving de clears every flag");
    assert!(!h.state.winops.is_sticky(&b) && !h.state.winops.is_shaded(&b));
    // And they cannot be set outside de.
    assert!(!winops::set_keep_above(&mut h.state, &b, true));
    assert!(!winops::set_sticky(&mut h.state, &b, true));
    assert!(!winops::set_shaded(&mut h.state, &b, true));
}

// ----------------------------------------------------------------- shade

#[test]
fn shade_rolls_up_and_back_without_a_configure() {
    let mut h = harness();
    let mut c = Client::connect(&mut h);
    let (t, w) = de_window(&mut h, &mut c);
    let before = c.configured_sizes(&t.toplevel).len();
    let rect = frame(&h, &w);
    assert!(winops::set_shaded(&mut h.state, &w, true));
    c.pump(&mut h);
    assert!(h.state.winops.is_shaded(&w));
    assert!(titlebar::bar_of(&w).unwrap().shaded, "the renderer is told");
    let sizes = c.configured_sizes(&t.toplevel);
    assert_eq!(
        sizes.len(),
        before,
        "the client is sent nothing when it is shaded"
    );
    assert!(sizes.iter().all(|(w, h)| *w > 0 && *h > 0), "never a zero size");
    assert_eq!(frame(&h, &w), rect, "the frame is kept for the way back");

    assert!(winops::set_shaded(&mut h.state, &w, false));
    c.pump(&mut h);
    assert!(!titlebar::bar_of(&w).unwrap().shaded);
    assert_eq!(
        c.configured_sizes(&t.toplevel).len(),
        before,
        "nor when it is unshaded"
    );
}

#[test]
fn a_shaded_window_takes_no_input_in_its_content_but_its_bar_still_works() {
    let mut h = harness();
    let mut c = Client::connect(&mut h);
    let (_, w) = de_window(&mut h, &mut c);
    let inside = centre(content(&h, &w));
    assert!(shell::surface_under(&h.state, inside).is_some(), "live before");
    winops::set_shaded(&mut h.state, &w, true);
    assert!(
        shell::surface_under(&h.state, inside).is_none(),
        "the rolled-up content is not there"
    );
    assert!(winops::element_under(&h.state, inside).is_none());
    assert!(titlebar::bar_at(&h.state, on_bar_at(&h, &w, 0.2)).is_some());
    winops::set_shaded(&mut h.state, &w, false);
    assert!(shell::surface_under(&h.state, inside).is_some(), "and back");
}

#[test]
fn a_shaded_window_cannot_be_resized() {
    let mut h = harness();
    let mut c = Client::connect(&mut h);
    let (_, w) = de_window(&mut h, &mut c);
    winops::set_shaded(&mut h.state, &w, true);
    assert!(!winops::can_resize(&h.state, &w));
    assert!(shell::resize_window(&mut h.state, &w, Some(300), Some(200)).is_err());
    assert!(!crate::input::grabs::start_resize_at(
        &mut h.state,
        w.clone(),
        LEFT
    ));
    assert!(!crate::input::grabs::start_resize_edges(
        &mut h.state,
        w.clone(),
        LEFT,
        ResizeEdge::Right
    ));
    let f = frame(&h, &w);
    let p = pt(f64::from(f.loc.x + f.size.w) + 3.0, f64::from(f.loc.y) + 80.0);
    assert!(edge::band_at(&h.state, p).is_none(), "no band either");
}

#[test]
fn maximize_and_fullscreen_restore_a_shaded_window() {
    let mut h = harness();
    let mut c = Client::connect(&mut h);
    let (t, w) = de_window(&mut h, &mut c);
    winops::set_shaded(&mut h.state, &w, true);
    assert!(maximize::set(&mut h.state, &w, Some((true, true))));
    assert!(!h.state.winops.is_shaded(&w), "maximize unshades");

    maximize::set(&mut h.state, &w, None);
    winops::set_shaded(&mut h.state, &w, true);
    t.toplevel.set_fullscreen(None);
    c.pump(&mut h);
    assert!(h.state.fullscreen.contains_key(&w));
    assert!(!h.state.winops.is_shaded(&w), "fullscreen unshades");
    // A fullscreen window has no bar to roll up to.
    assert!(!winops::set_shaded(&mut h.state, &w, true));
}

#[test]
fn a_window_with_no_bar_cannot_be_shaded() {
    let mut h = harness();
    let mut c = Client::connect(&mut h);
    flip(&mut h, Mode::De);
    let t = c.create_toplevel(&mut h);
    c.commit(&mut h, &t.surface);
    c.attach(&mut h, &t.surface);
    let w = window_of(&h, &t);
    // Never negotiated server-side decoration: it draws its own.
    assert!(!winops::set_shaded(&mut h.state, &w, true));
}

// ---------------------------------------------------------------- sticky

#[test]
fn a_sticky_window_is_shown_on_every_workspace_without_copies() {
    let mut h = harness();
    let mut c = Client::connect(&mut h);
    let [(_, a), (_, b), _] = three(&mut h, &mut c);
    winops::set_sticky(&mut h.state, &a, true);
    for ws in [2, 3, 1] {
        shell::switch_workspace(&mut h.state, ws);
        assert!(
            h.state.space.elements().any(|e| e == &a),
            "shown on workspace {ws}"
        );
        assert_eq!(holders(&h, &a), 1, "one entry, never a copy");
        assert!(
            !h.state.space.elements().any(|e| e == &b) || ws == 1,
            "a normal window stays home"
        );
    }
    assert!(h.state.space.elements().any(|e| e == &b), "back home it is there");
}

#[test]
fn closing_a_sticky_window_leaves_nothing_behind() {
    let mut h = harness();
    let mut c = Client::connect(&mut h);
    let (_, a) = de_window(&mut h, &mut c);
    winops::set_sticky(&mut h.state, &a, true);
    shell::switch_workspace(&mut h.state, 4);
    shell::unmap_window(&mut h.state, &a);
    assert!(!h.state.winops.is_sticky(&a));
    assert_eq!(holders(&h, &a), 0);
    assert!(!h.state.space.elements().any(|e| e == &a));
    shell::switch_workspace(&mut h.state, 1);
    assert!(!h.state.space.elements().any(|e| e == &a));
}

#[test]
fn a_sticky_window_minimizes_and_returns_on_whatever_workspace_is_showing() {
    let mut h = harness();
    let mut c = Client::connect(&mut h);
    let (_, a) = de_window(&mut h, &mut c);
    winops::set_sticky(&mut h.state, &a, true);
    shell::minimize_window(&mut h.state, &a);
    shell::switch_workspace(&mut h.state, 5);
    assert_eq!(holders(&h, &a), 1);
    assert!(shell::is_minimized(&h.state, &a));
    shell::unminimize_window(&mut h.state, &a);
    assert!(
        h.state.space.elements().any(|e| e == &a),
        "restored where the human is"
    );
    assert!(h.state.winops.is_sticky(&a));
}

#[test]
fn a_sticky_window_moved_to_another_output_leaves_no_copy_behind() {
    let mut h = harness();
    let mut c = Client::connect(&mut h);
    let (_, a) = de_window(&mut h, &mut c);
    focus::focus_window(&mut h.state, &a);
    winops::set_sticky(&mut h.state, &a, true);
    let target = h.state.outputs.get(h.b).unwrap().number;
    shell::move_to_output_workspace(&mut h.state, target);
    assert_eq!(holders(&h, &a), 1);
    assert_eq!(shell::output_of_window(&h.state, &a), Some(h.b));
    assert!(h.state.winops.is_sticky(&a), "the state went with it");
    shell::switch_workspace(&mut h.state, 6);
    // Switching the focused output (B now) keeps it showing there.
    let focused = h.state.outputs.focused().map(|e| e.id);
    if focused == Some(h.b) {
        assert!(h.state.space.elements().any(|e| e == &a));
    }
    assert_eq!(holders(&h, &a), 1);
}

#[test]
fn a_floating_window_released_over_the_other_output_moves_there() {
    let mut h = harness();
    let mut c = Client::connect(&mut h);
    let (_, w) = de_window(&mut h, &mut c);
    let b_geo = h
        .state
        .space
        .output_geometry(&h.state.outputs.get(h.b).unwrap().output)
        .unwrap();
    // Dragged past the edge of A: the centre is on B, the rectangle overhangs.
    let at = Rectangle::new(
        Point::from((b_geo.loc.x - 100, b_geo.loc.y + 100)),
        smithay::utils::Size::from((400, 300)),
    );
    shell::place_at(&mut h.state, &w, at);
    crate::input::grabs::finish_move(&mut h.state, &w);
    assert_eq!(shell::output_of_window(&h.state, &w), Some(h.b));
    assert_eq!(h.state.outputs.focused().map(|e| e.id), Some(h.b));
    let now = content(&h, &w);
    assert!(now.loc.x >= b_geo.loc.x, "clamped onto B");
    assert!(now.loc.y >= b_geo.loc.y);
    assert!(now.loc.x + now.size.w <= b_geo.loc.x + b_geo.size.w);
    assert_eq!(holders(&h, &w), 1);
}

#[test]
fn a_press_on_a_client_side_windows_strip_drags_it_and_a_click_does_not() {
    let mut h = harness();
    let mut c = Client::connect(&mut h);
    flip(&mut h, Mode::De);
    let t = c.create_toplevel(&mut h);
    c.commit(&mut h, &t.surface);
    c.attach(&mut h, &t.surface);
    c.pump(&mut h);
    settle(&mut h, &mut c, &t);
    let w = window_of(&h, &t);
    let before = content(&h, &w);
    let strip = before.loc.to_f64() + Point::from((50.0, 10.0));
    // Below the strip nothing is armed.
    move_to(&mut h, before.loc.to_f64() + Point::from((50.0, 40.0)));
    click(&mut h, LEFT, ButtonState::Pressed);
    assert!(h.state.strip_drag.is_none());
    click(&mut h, LEFT, ButtonState::Released);
    // A click on the strip moves nothing.
    move_to(&mut h, strip);
    click(&mut h, LEFT, ButtonState::Pressed);
    assert!(h.state.strip_drag.is_some());
    click(&mut h, LEFT, ButtonState::Released);
    assert!(h.state.strip_drag.is_none());
    assert_eq!(content(&h, &w), before);
    // Dragging past the threshold moves the window with the pointer.
    move_to(&mut h, strip);
    click(&mut h, LEFT, ButtonState::Pressed);
    move_to(&mut h, pt(strip.x + 60.0, strip.y + 40.0));
    assert!(h.state.pointer_grab_active, "the existing move grab");
    assert_eq!(content(&h, &w).loc, before.loc + Point::from((60, 40)));
    click(&mut h, LEFT, ButtonState::Released);
    assert!(!h.state.pointer_grab_active);
}

#[test]
fn choosing_a_desktop_for_a_sticky_window_ends_its_stickiness() {
    let mut h = harness();
    let mut c = Client::connect(&mut h);
    let (_, a) = de_window(&mut h, &mut c);
    winops::set_sticky(&mut h.state, &a, true);
    assert!(winmenu::open_window_menu(&mut h.state, &a, Point::from((50, 50))));
    key(&mut h, KEY_END);
    key(&mut h, KEY_UP);
    key(&mut h, KEY_ENTER); // Move to desktop
    key(&mut h, KEY_ENTER); // the first desktop offered
    assert!(!h.state.winmenu.is_open());
    assert!(!h.state.winops.is_sticky(&a));
    assert_eq!(holders(&h, &a), 1);
}

// ------------------------------------------------------------ double click

#[test]
fn two_presses_inside_the_interval_and_slop_are_a_double_click() {
    let mut t = ClickTracker::<u32>::default();
    assert!(!t.press(1, 1000, pt(10.0, 10.0)), "the first press is never one");
    assert!(
        t.press(1, 1000 + DOUBLE_CLICK_MS, pt(12.0, 9.0)),
        "right at the limit"
    );
    assert!(
        !t.press(1, 1000 + DOUBLE_CLICK_MS + 10, pt(12.0, 9.0)),
        "the pair was used up"
    );
}

#[test]
fn a_late_far_or_foreign_second_press_starts_over() {
    let mut t = ClickTracker::<u32>::default();
    t.press(1, 0, pt(0.0, 0.0));
    assert!(!t.press(1, DOUBLE_CLICK_MS + 1, pt(0.0, 0.0)), "too late");
    // That late press is itself the new first press.
    assert!(t.press(1, DOUBLE_CLICK_MS + 50, pt(1.0, 1.0)));
    t.press(1, 2000, pt(0.0, 0.0));
    assert!(!t.press(1, 2100, pt(40.0, 0.0)), "too far");
    t.press(1, 3000, pt(0.0, 0.0));
    assert!(!t.press(2, 3100, pt(0.0, 0.0)), "another window");
    assert!(
        !t.press(2, 3000 - 1, pt(0.0, 0.0)),
        "a clock that went back is not a double"
    );
}

#[test]
fn double_clicking_the_bar_toggles_maximize() {
    let mut h = harness();
    let mut c = Client::connect(&mut h);
    let (_, w) = de_window(&mut h, &mut c);
    let p = on_bar_at(&h, &w, 0.3);
    move_to(&mut h, p);
    click(&mut h, LEFT, ButtonState::Pressed);
    click(&mut h, LEFT, ButtonState::Released);
    assert!(!maximize::is_maximized(&h.state, &w), "one click does not");
    click(&mut h, LEFT, ButtonState::Pressed);
    click(&mut h, LEFT, ButtonState::Released);
    assert!(maximize::is_maximized(&h.state, &w), "two do");
    // The bar moved to the top of the area; a second pair restores.
    let p = on_bar_at(&h, &w, 0.3);
    move_to(&mut h, p);
    click(&mut h, LEFT, ButtonState::Pressed);
    click(&mut h, LEFT, ButtonState::Released);
    click(&mut h, LEFT, ButtonState::Pressed);
    click(&mut h, LEFT, ButtonState::Released);
    assert!(!maximize::is_maximized(&h.state, &w));
}

// -------------------------------------------------------------- bar drag

#[test]
fn dragging_the_bar_moves_the_window_with_the_pointer() {
    let mut h = harness();
    let mut c = Client::connect(&mut h);
    let (_, w) = de_window(&mut h, &mut c);
    let before = content(&h, &w);
    let p = on_bar_at(&h, &w, 0.3);
    move_to(&mut h, p);
    click(&mut h, LEFT, ButtonState::Pressed);
    // Under the threshold nothing moves.
    move_to(&mut h, pt(p.x + DRAG_THRESHOLD - 1.0, p.y));
    assert_eq!(content(&h, &w), before);
    assert!(!h.state.pointer_grab_active);
    // Past it, the window follows the whole way from where the press was.
    move_to(&mut h, pt(p.x + 60.0, p.y + 40.0));
    assert!(h.state.pointer_grab_active, "the existing move grab");
    let now = content(&h, &w);
    assert_eq!(now.loc, before.loc + Point::from((60, 40)));
    assert_eq!(now.size, before.size);
    click(&mut h, LEFT, ButtonState::Released);
    assert!(!h.state.pointer_grab_active, "the release ends the grab");
    assert!(
        h.state.bar_ui.swallowed_is_clear(),
        "no stale swallow can eat a later release"
    );
}

#[test]
fn dragging_a_maximized_window_restores_it_under_the_pointer_along_the_bar() {
    let mut h = harness();
    let mut c = Client::connect(&mut h);
    let (t, w) = de_window(&mut h, &mut c);
    maximize::set(&mut h.state, &w, Some((true, true)));
    c.pump(&mut h);
    settle(&mut h, &mut c, &t);
    let bar = bar_geometry(&h.state, &w).unwrap();
    let frac = 0.25;
    let px = f64::from(bar.loc.x) + f64::from(bar.size.w) * frac;
    let py = f64::from(bar.loc.y) + 8.0;
    move_to(&mut h, pt(px, py));
    click(&mut h, LEFT, ButtonState::Pressed);
    move_to(&mut h, pt(px + 10.0, py + 10.0));
    assert!(
        !maximize::is_maximized(&h.state, &w),
        "the drag ended the maximize"
    );
    c.pump(&mut h);
    settle(&mut h, &mut c, &t);
    let new_bar = bar_geometry(&h.state, &w).unwrap();
    let at = (px + 10.0 - f64::from(new_bar.loc.x)) / f64::from(new_bar.size.w);
    assert!(
        (at - frac).abs() < 0.02,
        "same place along the bar: {at} vs {frac}"
    );
    let dy = py + 10.0 - f64::from(new_bar.loc.y);
    assert!(
        (0.0..f64::from(new_bar.size.h)).contains(&dy),
        "still on the bar: {dy}"
    );
    click(&mut h, LEFT, ButtonState::Released);
}

#[test]
fn a_middle_press_on_the_bar_does_nothing_and_a_right_press_opens_the_menu() {
    let mut h = harness();
    let mut c = Client::connect(&mut h);
    let (t, w) = de_window(&mut h, &mut c);
    let p = on_bar_at(&h, &w, 0.3);
    move_to(&mut h, p);
    click(&mut h, MIDDLE, ButtonState::Pressed);
    click(&mut h, MIDDLE, ButtonState::Released);
    c.pump(&mut h);
    assert!(!h.state.winmenu.is_open());
    assert!(!maximize::is_maximized(&h.state, &w) && !c.closed(&t.toplevel));
    assert!(!h.state.pointer_grab_active);

    click(&mut h, RIGHT, ButtonState::Pressed);
    assert!(h.state.winmenu.is_open(), "right opens the window menu");
    assert_eq!(h.state.winmenu.window(), Some(&w));
    click(&mut h, RIGHT, ButtonState::Released);
    assert!(h.state.winmenu.is_open(), "its release does not dismiss it");
}

// ------------------------------------------------------------ resize band

fn outside_right(h: &Harness, w: &Window) -> Point<f64, Logical> {
    let f = frame(h, w);
    pt(
        f64::from(f.loc.x + f.size.w) + 3.0,
        f64::from(f.loc.y + f.size.h / 2),
    )
}

#[test]
fn hovering_the_band_sets_the_resize_cursor_and_leaving_restores_it() {
    let mut h = harness();
    let mut c = Client::connect(&mut h);
    let (_, w) = de_window(&mut h, &mut c);
    // Park the window so there is room around it.
    let u = h
        .state
        .space
        .output_geometry(&h.state.outputs.get(h.a).unwrap().output)
        .unwrap();
    shell::place_at(
        &mut h.state,
        &w,
        Rectangle::new(Point::from((u.loc.x + 100, u.loc.y + 100)), (300, 200).into()),
    );
    c.pump(&mut h);
    let p = outside_right(&h, &w);
    move_to(&mut h, p);
    assert_eq!(h.state.edge.hovered(), Some(ResizeEdge::Right));
    assert!(matches!(
        h.state.cursor_status,
        CursorImageStatus::Named(CursorIcon::EResize)
    ));
    assert!(
        h.state.seat.get_pointer().unwrap().current_focus().is_none(),
        "no client is under a band"
    );

    move_to(&mut h, pt(p.x + 40.0, p.y));
    assert_eq!(h.state.edge.hovered(), None);
    assert!(!h.state.edge.cursor_is_ours());
    assert!(matches!(
        h.state.cursor_status,
        CursorImageStatus::Named(CursorIcon::Default)
    ));
}

#[test]
fn pressing_in_the_band_resizes_with_the_cursor_until_the_release() {
    let mut h = harness();
    let mut c = Client::connect(&mut h);
    let (t, w) = de_window(&mut h, &mut c);
    let u = h
        .state
        .space
        .output_geometry(&h.state.outputs.get(h.a).unwrap().output)
        .unwrap();
    shell::place_at(
        &mut h.state,
        &w,
        Rectangle::new(Point::from((u.loc.x + 100, u.loc.y + 100)), (300, 200).into()),
    );
    c.pump(&mut h);
    settle(&mut h, &mut c, &t);
    let before = float_rect(&h, &w);
    let p = outside_right(&h, &w);
    move_to(&mut h, p);
    click(&mut h, LEFT, ButtonState::Pressed);
    assert!(h.state.pointer_grab_active, "the existing resize grab");
    move_to(&mut h, pt(p.x + 50.0, p.y));
    let now = float_rect(&h, &w);
    assert_eq!(now.size.w, before.size.w + 50);
    assert_eq!(now.size.h, before.size.h, "only the dragged edge moved");
    assert_eq!(now.loc, before.loc);
    assert!(
        matches!(
            h.state.cursor_status,
            CursorImageStatus::Named(CursorIcon::EResize)
        ),
        "the cursor stays during the grab"
    );
    click(&mut h, LEFT, ButtonState::Released);
    assert!(!h.state.pointer_grab_active);
    assert!(
        matches!(
            h.state.cursor_status,
            CursorImageStatus::Named(CursorIcon::Default)
        ),
        "and goes back when the grab ends"
    );
    assert!(!h.state.edge.cursor_is_ours());
}

#[test]
fn corners_resize_both_ways() {
    let mut h = harness();
    let mut c = Client::connect(&mut h);
    let (t, w) = de_window(&mut h, &mut c);
    let u = h
        .state
        .space
        .output_geometry(&h.state.outputs.get(h.a).unwrap().output)
        .unwrap();
    shell::place_at(
        &mut h.state,
        &w,
        Rectangle::new(Point::from((u.loc.x + 100, u.loc.y + 100)), (300, 200).into()),
    );
    c.pump(&mut h);
    settle(&mut h, &mut c, &t);
    let before = float_rect(&h, &w);
    let f = frame(&h, &w);
    let p = pt(
        f64::from(f.loc.x + f.size.w) + 3.0,
        f64::from(f.loc.y + f.size.h) + 3.0,
    );
    move_to(&mut h, p);
    assert_eq!(h.state.edge.hovered(), Some(ResizeEdge::BottomRight));
    click(&mut h, LEFT, ButtonState::Pressed);
    move_to(&mut h, pt(p.x + 30.0, p.y + 20.0));
    let now = float_rect(&h, &w);
    assert_eq!((now.size.w, now.size.h), (before.size.w + 30, before.size.h + 20));
    click(&mut h, LEFT, ButtonState::Released);
}

#[test]
fn no_band_for_maximized_fullscreen_or_client_side_windows() {
    let mut h = harness();
    let mut c = Client::connect(&mut h);
    let (t, w) = de_window(&mut h, &mut c);
    let u = h
        .state
        .space
        .output_geometry(&h.state.outputs.get(h.a).unwrap().output)
        .unwrap();
    shell::place_at(
        &mut h.state,
        &w,
        Rectangle::new(Point::from((u.loc.x + 100, u.loc.y + 100)), (300, 200).into()),
    );
    c.pump(&mut h);
    let p = outside_right(&h, &w);
    assert_eq!(
        edge::band_at(&h.state, p).map(|(_, e)| e),
        Some(ResizeEdge::Right)
    );

    // Client-side decoration: keeps its own edges.
    w.toplevel()
        .unwrap()
        .with_pending_state(|s| s.decoration_mode = Some(Deco::ClientSide));
    shell::arrange(&mut h.state);
    assert!(
        edge::band_at(&h.state, p).is_none(),
        "client-side windows are not given a band"
    );
    ssd(&mut h, &w);
    assert!(edge::band_at(&h.state, p).is_some());

    maximize::set(&mut h.state, &w, Some((true, true)));
    let f = frame(&h, &w);
    let outside = pt(f64::from(f.loc.x + f.size.w) + 3.0, f64::from(f.loc.y + 50));
    assert!(edge::band_at(&h.state, outside).is_none(), "maximized: no band");
    maximize::set(&mut h.state, &w, None);

    t.toplevel.set_fullscreen(None);
    c.pump(&mut h);
    let f = frame(&h, &w);
    let outside = pt(f64::from(f.loc.x + f.size.w) + 3.0, f64::from(f.loc.y + 50));
    assert!(edge::band_at(&h.state, outside).is_none(), "fullscreen: no band");
}

#[test]
fn a_window_stacked_above_covers_a_lower_windows_band() {
    let mut h = harness();
    let mut c = Client::connect(&mut h);
    let (_, a) = de_window(&mut h, &mut c);
    let (_, b) = de_window(&mut h, &mut c);
    let u = h
        .state
        .space
        .output_geometry(&h.state.outputs.get(h.a).unwrap().output)
        .unwrap();
    shell::place_at(
        &mut h.state,
        &a,
        Rectangle::new(Point::from((u.loc.x + 100, u.loc.y + 100)), (300, 200).into()),
    );
    // b sits across a's right-hand band.
    shell::place_at(
        &mut h.state,
        &b,
        Rectangle::new(Point::from((u.loc.x + 380, u.loc.y + 100)), (300, 200).into()),
    );
    c.pump(&mut h);
    focus::focus_window_raising(&mut h.state, &b, true);
    let p = outside_right(&h, &a);
    let hit = edge::band_at(&h.state, p);
    assert!(
        hit.is_none() || hit.is_some_and(|(w, _)| w != a),
        "b is over a's band"
    );
}

// ------------------------------------------------------------ window menu

#[test]
fn escape_dismisses_the_menu_and_no_keyboard_grab_remains() {
    let mut h = harness();
    let mut c = Client::connect(&mut h);
    let (_, w) = de_window(&mut h, &mut c);
    assert!(winmenu::open_window_menu(&mut h.state, &w, Point::from((50, 50))));
    assert!(h.state.winmenu.is_open());
    key(&mut h, KEY_A);
    assert!(h.state.winmenu.is_open(), "other keys are eaten, not forwarded");
    key(&mut h, KEY_ESC);
    assert!(!h.state.winmenu.is_open());
    let keyboard = h.state.seat.get_keyboard().unwrap();
    assert!(!keyboard.is_grabbed(), "the menu never held a keyboard grab");
    assert_eq!(h.state.focus.as_ref(), Some(&w), "focus is where it was");
}

#[test]
fn a_press_outside_dismisses_the_menu_and_goes_no_further() {
    let mut h = harness();
    let mut c = Client::connect(&mut h);
    let (_, w) = de_window(&mut h, &mut c);
    assert!(winmenu::open_window_menu(&mut h.state, &w, Point::from((50, 50))));
    let far = pt(700.0, 550.0);
    move_to(&mut h, far);
    click(&mut h, LEFT, ButtonState::Pressed);
    assert!(!h.state.winmenu.is_open());
    click(&mut h, LEFT, ButtonState::Released);
    assert!(!h.state.pointer_grab_active);
}

#[test]
fn a_focus_change_dismisses_the_menu() {
    let mut h = harness();
    let mut c = Client::connect(&mut h);
    let (_, a) = de_window(&mut h, &mut c);
    let (_, b) = de_window(&mut h, &mut c);
    focus::focus_window(&mut h.state, &a);
    assert!(winmenu::open_window_menu(&mut h.state, &a, Point::from((50, 50))));
    focus::focus_window(&mut h.state, &b);
    assert!(!h.state.winmenu.is_open());
}

#[test]
fn the_session_lock_dismisses_the_menu_and_it_never_opens_under_it() {
    let mut h = harness();
    let mut c = Client::connect(&mut h);
    let (_, w) = de_window(&mut h, &mut c);
    assert!(winmenu::open_window_menu(&mut h.state, &w, Point::from((50, 50))));
    h.state.engage_lock();
    assert!(!h.state.winmenu.is_open(), "locking closes it");
    assert!(!winmenu::open_window_menu(
        &mut h.state,
        &w,
        Point::from((50, 50))
    ));
    assert!(!h.state.winmenu.is_open());
    // A right press cannot get one either (it never reaches the bar).
    let p = on_bar_at(&h, &w, 0.3);
    move_to(&mut h, p);
    click(&mut h, RIGHT, ButtonState::Pressed);
    assert!(!h.state.winmenu.is_open());
}

#[test]
fn the_menu_opens_only_under_de() {
    let mut h = harness();
    let mut c = Client::connect(&mut h);
    let (_, w) = de_window(&mut h, &mut c);
    flip(&mut h, Mode::Hybrid);
    assert!(!winmenu::open_window_menu(
        &mut h.state,
        &w,
        Point::from((50, 50))
    ));
    flip(&mut h, Mode::De);
    assert!(winmenu::open_window_menu(&mut h.state, &w, Point::from((50, 50))));
    flip(&mut h, Mode::Wm);
    assert!(!h.state.winmenu.is_open(), "leaving de closes it");
}

#[test]
fn a_client_request_needs_a_live_press_serial() {
    let mut h = harness();
    let mut c = Client::connect(&mut h);
    let (t, w) = de_window(&mut h, &mut c);
    let surface = w.toplevel().unwrap().clone();
    let _ = t;
    // No press has been sent to the client, so any serial is a forgery.
    winmenu::show_for_client(
        &mut h.state,
        &surface,
        smithay::utils::Serial::from(4242),
        Point::from((5, 5)),
    );
    assert!(!h.state.winmenu.is_open());
}

#[test]
fn arrows_and_enter_run_the_row() {
    let mut h = harness();
    let mut c = Client::connect(&mut h);
    let (_, w) = de_window(&mut h, &mut c);
    assert!(winmenu::open_window_menu(&mut h.state, &w, Point::from((50, 50))));
    key(&mut h, KEY_DOWN);
    key(&mut h, KEY_ENTER);
    assert!(!h.state.winmenu.is_open());
    assert!(shell::is_minimized(&h.state, &w), "the first row is Minimize");
}

#[test]
fn the_menu_toggles_keep_above_and_the_rest() {
    let mut h = harness();
    let mut c = Client::connect(&mut h);
    let (_, w) = de_window(&mut h, &mut c);
    let items = |h: &Harness| h.state.winmenu.model().unwrap().items().to_vec();
    let pick = |h: &mut Harness, item: Item| {
        assert!(winmenu::open_window_menu(&mut h.state, &w, Point::from((50, 50))));
        let at = items(h).iter().position(|i| *i == item).unwrap();
        let row = h.state.winmenu.view().unwrap().row_rect_local(at);
        let origin = h.state.winmenu.view().unwrap().origin;
        move_to(
            h,
            pt(f64::from(origin.x + 20), f64::from(origin.y + row.loc.y + 5)),
        );
        click(h, LEFT, ButtonState::Pressed);
        click(h, LEFT, ButtonState::Released);
        assert!(!h.state.winmenu.is_open(), "a choice closes the menu");
    };
    pick(&mut h, Item::KeepAbove);
    assert_eq!(h.state.winops.band(&w), Band::Above);
    pick(&mut h, Item::KeepBelow);
    assert_eq!(h.state.winops.band(&w), Band::Below);
    pick(&mut h, Item::Sticky);
    assert!(h.state.winops.is_sticky(&w));
    pick(&mut h, Item::Shade);
    assert!(h.state.winops.is_shaded(&w));
    pick(&mut h, Item::Maximize);
    assert!(maximize::is_maximized(&h.state, &w) && !h.state.winops.is_shaded(&w));
}

#[test]
fn move_to_desktop_goes_through_the_workspace_list() {
    let mut h = harness();
    let mut c = Client::connect(&mut h);
    let (_, w) = de_window(&mut h, &mut c);
    assert!(winmenu::open_window_menu(&mut h.state, &w, Point::from((50, 50))));
    key(&mut h, KEY_END);
    key(&mut h, KEY_UP);
    key(&mut h, KEY_ENTER);
    assert_eq!(h.state.winmenu.model().unwrap().page(), winmenu::Page::Desktops);
    key(&mut h, KEY_ENTER);
    assert!(!h.state.winmenu.is_open());
    let e = h.state.outputs.get(h.a).unwrap();
    assert!(e.workspaces[1].holds(&w), "the first other desktop");
}

#[test]
fn a_menu_row_with_no_titlebar_is_not_offered_to_shade() {
    let mut h = harness();
    let mut c = Client::connect(&mut h);
    flip(&mut h, Mode::De);
    let t = c.create_toplevel(&mut h);
    c.commit(&mut h, &t.surface);
    c.attach(&mut h, &t.surface);
    let w = window_of(&h, &t);
    assert!(winmenu::open_window_menu(&mut h.state, &w, Point::from((50, 50))));
    let m = h.state.winmenu.model().unwrap();
    let shade = m.items().iter().position(|i| *i == Item::Shade).unwrap();
    assert!(!m.view().rows[shade].enabled);
}

#[test]
fn the_menu_is_never_in_a_capture_pass() {
    // `render/scan_tests.rs` keeps `capture.rs` from naming the menu and checks
    // every backend splices it in itself. Here: the state the capture pass reads
    // holds the open menu nowhere it could be listed.
    let src = include_str!("../render/capture.rs");
    assert!(!src.contains("winmenu") && !src.contains("menu_elements"));
}

// -------------------------------------------------------------- cascade

#[test]
fn new_windows_cascade_under_de_and_wrap_on_the_usable_area() {
    let mut h = harness();
    let mut c = Client::connect(&mut h);
    flip(&mut h, Mode::De);
    // The mode default is applied by the loader; a live flip keeps the value.
    h.state.config.general.floating_placement = crate::config::FloatingPlacement::Cascade;
    let id = h.a;
    let area = {
        let o = h.state.outputs.get(id).unwrap().output.clone();
        smithay::desktop::layer_map_for_output(&o).arrange();
        shell::usable_area(&h.state, &o)
    };
    let mut locs = Vec::new();
    for _ in 0..24 {
        let t = c.create_toplevel(&mut h);
        c.commit(&mut h, &t.surface);
        c.attach(&mut h, &t.surface);
        let w = window_of(&h, &t);
        let f = h
            .state
            .outputs
            .get(id)
            .unwrap()
            .workspaces
            .iter()
            .flat_map(|ws| ws.floating.iter())
            .find(|f| f.window == w)
            .map(|f| f.rect)
            .unwrap();
        locs.push(f.loc);
        assert!(
            area.contains_rect(f) || f.loc.x >= area.loc.x,
            "stays on the usable area: {f:?}"
        );
    }
    assert_ne!(locs[0], locs[1], "the second does not sit on the first");
    assert_eq!(locs[1] - locs[0], Point::from((32, 32)));
    assert!(
        locs.windows(2).any(|p| p[1].x < p[0].x),
        "the diagonal wraps instead of walking off: {locs:?}"
    );
}

// ------------------------------------------------- review fixes (de-m4)

/// A dialog of `parent`, decorated like the others.
fn dialog_of(h: &mut Harness, c: &mut Client, parent: &Toplevel) -> (Toplevel, Window) {
    let t = c.create_toplevel(h);
    t.toplevel.set_parent(Some(&parent.toplevel));
    c.commit(h, &t.surface);
    c.attach(h, &t.surface);
    let w = window_of(h, &t);
    ssd(h, &w);
    c.pump(h);
    (t, w)
}

#[test]
fn a_dialog_takes_its_parents_band() {
    let mut h = harness();
    let mut c = Client::connect(&mut h);
    let (pt_, p) = de_window(&mut h, &mut c);
    let (_, other) = de_window(&mut h, &mut c);
    let (_, d) = dialog_of(&mut h, &mut c, &pt_);

    // Keep-above parent: its dialog is not hidden behind it, whatever else
    // was raised since.
    assert!(winops::set_keep_above(&mut h.state, &p, true));
    focus::focus_window_raising(&mut h.state, &other, true);
    assert_eq!(winops::effective_band(&h.state, &d), Band::Above);
    assert!(index(&h, &d) > index(&h, &p), "the dialog is over its parent");
    assert!(index(&h, &d) > index(&h, &other), "and over the normal windows");

    // Keep-below parent: its dialog does not rise over the normal windows.
    assert!(winops::set_keep_below(&mut h.state, &p, true));
    focus::focus_window_raising(&mut h.state, &other, true);
    assert_eq!(winops::effective_band(&h.state, &d), Band::Below);
    assert!(index(&h, &d) < index(&h, &other));
    assert!(index(&h, &p) < index(&h, &d), "still over its parent");

    // Its own band wins over the parent's.
    assert!(winops::set_keep_above(&mut h.state, &d, true));
    assert_eq!(winops::effective_band(&h.state, &d), Band::Above);
}

#[test]
fn a_band_survives_an_output_move() {
    let mut h = harness();
    let mut c = Client::connect(&mut h);
    let [(_, a), (_, b), _] = three(&mut h, &mut c);
    focus::focus_window(&mut h.state, &a);
    winops::set_keep_above(&mut h.state, &a, true);
    let target = h.state.outputs.get(h.b).unwrap().number;
    shell::move_to_output_workspace(&mut h.state, target);
    assert_eq!(shell::output_of_window(&h.state, &a), Some(h.b));
    assert_eq!(h.state.winops.band(&a), Band::Above, "the output move keeps it");
    assert_eq!(holders(&h, &a), 1);
    // A window arriving after it does not cover it.
    shell::move_window_to_workspace(&mut h.state, &b, 1);
    shell::arrange(&mut h.state);
    assert_eq!(winops::effective_band(&h.state, &a), Band::Above);
}

#[test]
fn an_x11_window_that_asked_for_a_position_keeps_it_on_the_area() {
    let area = Rectangle::new(Point::from((0, 0)), (1000, 800).into());
    let size = (300, 200).into();
    assert_eq!(
        shell::x11_hinted_origin(Point::from((120, 340)), size, area),
        Point::from((120, 340))
    );
    assert_eq!(
        shell::x11_hinted_origin(Point::from((900, 790)), size, area),
        Point::from((700, 600)),
        "held to the area"
    );
    assert_eq!(
        shell::x11_hinted_origin(Point::from((-50, -9)), size, area),
        Point::from((0, 0))
    );
}

#[test]
fn closing_a_window_in_its_band_restores_the_cursor() {
    let mut h = harness();
    let mut c = Client::connect(&mut h);
    let (_, w) = de_window(&mut h, &mut c);
    let u = h
        .state
        .space
        .output_geometry(&h.state.outputs.get(h.a).unwrap().output)
        .unwrap();
    shell::place_at(
        &mut h.state,
        &w,
        Rectangle::new(Point::from((u.loc.x + 100, u.loc.y + 100)), (300, 200).into()),
    );
    c.pump(&mut h);
    let p = outside_right(&h, &w);
    move_to(&mut h, p);
    assert!(h.state.edge.cursor_is_ours());
    shell::unmap_window(&mut h.state, &w);
    move_to(&mut h, pt(p.x + 1.0, p.y));
    assert!(
        !h.state.edge.cursor_is_ours(),
        "the stuck resize cursor is put back"
    );
    assert!(matches!(
        h.state.cursor_status,
        CursorImageStatus::Named(CursorIcon::Default)
    ));
}

#[test]
fn outside_de_a_resize_does_not_touch_the_cursor() {
    use smithay::input::SeatHandler;
    let mut h = harness();
    flip(&mut h, Mode::Hybrid);
    edge::show_cursor(&mut h.state, ResizeEdge::Right);
    assert!(!h.state.edge.cursor_is_ours());
    assert!(matches!(
        h.state.cursor_status,
        CursorImageStatus::Named(CursorIcon::Default)
    ));
    // A client's own resize shape is drawn as the arrow it always was.
    let seat = h.state.seat.clone();
    h.state
        .cursor_image(&seat, CursorImageStatus::Named(CursorIcon::EwResize));
    assert!(matches!(
        h.state.cursor_status,
        CursorImageStatus::Named(CursorIcon::Default)
    ));
    flip(&mut h, Mode::De);
    h.state
        .cursor_image(&seat, CursorImageStatus::Named(CursorIcon::EwResize));
    assert!(matches!(
        h.state.cursor_status,
        CursorImageStatus::Named(CursorIcon::EwResize)
    ));
}

#[test]
fn a_shaded_windows_hidden_content_does_not_block_the_band_under_it() {
    let mut h = harness();
    let mut c = Client::connect(&mut h);
    let (tb, b) = de_window(&mut h, &mut c);
    let (ta, a) = de_window(&mut h, &mut c);
    let u = h
        .state
        .space
        .output_geometry(&h.state.outputs.get(h.a).unwrap().output)
        .unwrap();
    shell::place_at(
        &mut h.state,
        &b,
        Rectangle::new(Point::from((u.loc.x + 100, u.loc.y + 200)), (300, 200).into()),
    );
    // a is a tall window whose body covers b's right-hand band.
    shell::place_at(
        &mut h.state,
        &a,
        Rectangle::new(Point::from((u.loc.x + 380, u.loc.y + 20)), (300, 500).into()),
    );
    c.pump(&mut h);
    settle(&mut h, &mut c, &tb);
    settle(&mut h, &mut c, &ta);
    focus::focus_window_raising(&mut h.state, &a, true);
    let p = outside_right(&h, &b);
    assert!(
        edge::band_at(&h.state, p).is_none_or(|(w, _)| w != b),
        "unshaded a covers it"
    );
    assert!(winops::set_shaded(&mut h.state, &a, true));
    assert_eq!(
        edge::band_at(&h.state, p).map(|(w, e)| (w == b, e)),
        Some((true, ResizeEdge::Right)),
        "a's hidden body no longer does"
    );
}

#[test]
fn a_shaded_windows_content_is_not_an_agent_hit_target() {
    let mut h = harness();
    let mut c = Client::connect(&mut h);
    let (_, w) = de_window(&mut h, &mut c);
    let p = centre(content(&h, &w));
    assert!(winops::element_under(&h.state, p).is_some_and(|(e, _)| e == &w));
    winops::set_shaded(&mut h.state, &w, true);
    assert!(winops::element_under(&h.state, p).is_none());
}

#[test]
fn pressing_the_resize_band_focuses_and_raises_the_window() {
    let mut h = harness();
    let mut c = Client::connect(&mut h);
    let (_, a) = de_window(&mut h, &mut c);
    let (_, b) = de_window(&mut h, &mut c);
    let u = h
        .state
        .space
        .output_geometry(&h.state.outputs.get(h.a).unwrap().output)
        .unwrap();
    shell::place_at(
        &mut h.state,
        &a,
        Rectangle::new(Point::from((u.loc.x + 100, u.loc.y + 100)), (300, 200).into()),
    );
    shell::place_at(
        &mut h.state,
        &b,
        Rectangle::new(Point::from((u.loc.x + 700, u.loc.y + 100)), (300, 200).into()),
    );
    c.pump(&mut h);
    focus::focus_window(&mut h.state, &b);
    assert_eq!(h.state.focus.as_ref(), Some(&b));
    let p = outside_right(&h, &a);
    move_to(&mut h, p);
    click(&mut h, LEFT, ButtonState::Pressed);
    assert!(h.state.pointer_grab_active, "it resizes");
    assert_eq!(h.state.focus.as_ref(), Some(&a), "and focuses");
    assert_eq!(index(&h, &a), order(&h).len() - 1, "and raises");
}
