// SPDX-License-Identifier: AGPL-3.0-only
//! The window switcher, the overview, hot corners and hover previews (M7).
//! Driven through the real input entry points (`keyboard_key`, `pointer_moved`,
//! `pointer_button`) wherever the behaviour is input, and with real clients in
//! the harness. There is no GL context here, so what is asserted about
//! drawing is the plan the backends draw from (`overview::frame`): which cards
//! exist, what each shows, which windows may reach a texture.

use std::time::{Duration, Instant};

use serde_json::{json, Value};
use smithay::{
    backend::input::{ButtonState, KeyState},
    desktop::Window,
    input::keyboard::Keycode,
    utils::{Logical, Point, Rectangle},
};

use ec_abyss_render::overview::Body;

use super::{
    corners::{Arm, Corners},
    frame, preview, switcher,
    thumbs::{self, Face},
    Frame,
};
use crate::{
    config::Mode,
    ipc::methods::dispatch,
    shell::{
        self,
        focus::state_tests::{
            client::{Client, Toplevel},
            harness, Harness,
        },
    },
};

const LEFT: u32 = 0x110;
const MIDDLE: u32 = 0x112;
// evdev codes; +8 is the xkb keycode.
const KEY_ESC: u32 = 1;
const KEY_TAB: u32 = 15;
const KEY_W: u32 = 17;
const KEY_LEFTSHIFT: u32 = 42;
const KEY_LEFTALT: u32 = 56;
const KEY_LEFTMETA: u32 = 125;
const KEY_RIGHT: u32 = 106;

fn down(h: &mut Harness, code: u32) {
    h.state.keyboard_key(Keycode::new(code + 8), KeyState::Pressed, 1);
}

fn up(h: &mut Harness, code: u32) {
    h.state
        .keyboard_key(Keycode::new(code + 8), KeyState::Released, 2);
}

fn tap(h: &mut Harness, code: u32) {
    down(h, code);
    up(h, code);
}

fn pt(x: f64, y: f64) -> Point<f64, Logical> {
    Point::from((x, y))
}

fn centre(r: Rectangle<i32, Logical>) -> Point<f64, Logical> {
    pt(
        f64::from(r.loc.x) + f64::from(r.size.w) / 2.0,
        f64::from(r.loc.y) + f64::from(r.size.h) / 2.0,
    )
}

fn move_to(h: &mut Harness, p: Point<f64, Logical>) {
    h.state.pointer_moved(p, 0);
}

fn click(h: &mut Harness, button: u32, state: ButtonState) {
    h.state.pointer_button(button, state, 0);
}

fn click_at(h: &mut Harness, p: Point<f64, Logical>) {
    move_to(h, p);
    click(h, LEFT, ButtonState::Pressed);
    click(h, LEFT, ButtonState::Released);
}

fn flip(h: &mut Harness, mode: Mode) {
    let mut next = h.state.config.clone();
    next.mode = mode;
    // `Config::load` seats the mode's default binds; a flip in a test does not
    // load, so seat them the same way.
    if mode == Mode::De {
        next.binds.extend(crate::config::mode_default_binds(Mode::De));
    }
    crate::config::apply_loaded(&mut h.state, next);
}

fn window_of(h: &Harness, t: &Toplevel) -> Window {
    use wayland_client::Proxy;
    let id = t.surface.id().protocol_id();
    shell::owned_windows(&h.state)
        .into_iter()
        .find(|w| {
            shell::window_surface(w)
                .is_some_and(|s| smithay::reexports::wayland_server::Resource::id(&s).protocol_id() == id)
        })
        .expect("placed")
}

/// A mapped window of `size` titled `title`, focused (the map focuses it).
fn win_sized(h: &mut Harness, c: &mut Client, title: &str, size: (i32, i32)) -> (Toplevel, Window) {
    let t = c.create_toplevel(h);
    t.toplevel.set_title(title.into());
    c.commit(h, &t.surface);
    c.attach_sized(h, &t.surface, size.0, size.1);
    let w = window_of(h, &t);
    c.pump(h);
    (t, w)
}

fn win(h: &mut Harness, c: &mut Client, title: &str) -> (Toplevel, Window) {
    win_sized(h, c, title, (200, 150))
}

fn de() -> (Harness, Client) {
    let mut h = harness();
    flip(&mut h, Mode::De);
    let c = Client::connect(&mut h);
    (h, c)
}

fn active(h: &Harness) -> usize {
    h.state.outputs.focused().expect("output").active
}

fn frame_of(h: &Harness) -> Frame {
    frame(&h.state).expect("something is open")
}

fn handle(h: &mut Harness, w: &Window) -> u64 {
    h.state.ipc.handle_for(w)
}

fn desk_of(h: &Harness, w: &Window) -> usize {
    h.state
        .outputs
        .iter()
        .find_map(|e| e.workspaces.iter().position(|ws| ws.holds(w)))
        .expect("on a desktop")
}

fn secret(h: &mut Harness, w: &Window) {
    let s = shell::window_surface(w).expect("surface");
    h.state.sensitive.insert(s);
}

/// The text a frame would put on screen, for "this title is never drawn".
fn drawn_text(f: &Frame) -> String {
    format!("{:?}", f.view)
}

// -------------------------------------------------------------------- switcher

#[test]
fn the_order_is_most_recent_first_across_desktops_with_minimized_windows_last() {
    let (mut h, mut c) = de();
    let (_, a) = win(&mut h, &mut c, "a");
    let (_, b) = win(&mut h, &mut c, "b");
    let (_, cc) = win(&mut h, &mut c, "c");
    shell::focus_window(&mut h.state, &a);
    shell::focus_window(&mut h.state, &b);
    shell::focus_window(&mut h.state, &cc);
    let id = h.a;
    assert_eq!(switcher::mru(&h.state, id), [cc.clone(), b.clone(), a.clone()]);
    // A window on another desktop, focused last, leads; the first desktop
    // follows in its own order.
    shell::switch_workspace(&mut h.state, 2);
    let (_, d) = win(&mut h, &mut c, "d");
    assert_eq!(
        switcher::mru(&h.state, id),
        [d.clone(), cc.clone(), b.clone(), a.clone()]
    );
    // Back on the first desktop (which focuses `c`, its head): windows
    // interleave by their own recency, so `d` falls between `c` and `b`.
    shell::switch_workspace(&mut h.state, 1);
    shell::focus_window(&mut h.state, &a);
    assert_eq!(
        switcher::mru(&h.state, id),
        [a.clone(), cc.clone(), d.clone(), b.clone()]
    );
    // Minimized windows go to the end, wherever they were.
    shell::minimize_window(&mut h.state, &cc);
    assert_eq!(switcher::mru(&h.state, id), [a, d, b, cc]);
}

#[test]
fn mru_interleaves_windows_of_different_desktops_by_their_own_recency() {
    let (mut h, mut c) = de();
    let id = h.a;
    let (_, a) = win(&mut h, &mut c, "a");
    shell::switch_workspace(&mut h.state, 2);
    let (_, b) = win(&mut h, &mut c, "b");
    shell::switch_workspace(&mut h.state, 1);
    shell::focus_window(&mut h.state, &a);
    let (_, cc) = win(&mut h, &mut c, "c");
    // Desktop 1 holds a and c, desktop 2 holds b: by desktop stamp the order
    // would be c, a, b; by window it is c, b, a.
    shell::switch_workspace(&mut h.state, 2);
    shell::focus_window(&mut h.state, &b);
    shell::switch_workspace(&mut h.state, 1);
    shell::focus_window(&mut h.state, &cc);
    assert_eq!(switcher::mru(&h.state, id), [cc, b, a]);
}

#[test]
fn alt_tab_opens_on_the_previous_window_and_releasing_alt_activates_it() {
    let (mut h, mut c) = de();
    let (_, a) = win(&mut h, &mut c, "a");
    let (_, b) = win(&mut h, &mut c, "b");
    shell::focus_window(&mut h.state, &b);
    down(&mut h, KEY_LEFTALT);
    tap(&mut h, KEY_TAB);
    assert!(h.state.switcher.is_open());
    assert_eq!(h.state.switcher.selected_window(), Some(a.clone()));
    assert_eq!(h.state.switcher.windows(), [b.clone(), a.clone()]);
    assert_eq!(
        h.state.focus.as_ref(),
        Some(&b),
        "nothing changes while it is open"
    );
    up(&mut h, KEY_LEFTALT);
    assert!(!h.state.switcher.is_open());
    assert_eq!(h.state.focus.as_ref(), Some(&a));
    // Tapping again goes back.
    down(&mut h, KEY_LEFTALT);
    tap(&mut h, KEY_TAB);
    up(&mut h, KEY_LEFTALT);
    assert_eq!(h.state.focus.as_ref(), Some(&b));
}

#[test]
fn from_a_desktop_with_no_focused_window_the_first_press_is_the_most_recent() {
    let (mut h, mut c) = de();
    let (_, _a) = win(&mut h, &mut c, "a");
    let (_, b) = win(&mut h, &mut c, "b");
    shell::switch_workspace(&mut h.state, 4);
    assert!(h.state.focus.is_none());
    down(&mut h, KEY_LEFTALT);
    tap(&mut h, KEY_TAB);
    assert_eq!(h.state.switcher.selected_window(), Some(b.clone()));
    up(&mut h, KEY_LEFTALT);
    assert_eq!(h.state.focus.as_ref(), Some(&b));
    assert_eq!(active(&h), 0);
}

#[test]
fn tab_steps_while_alt_is_held_and_shift_reverses() {
    let (mut h, mut c) = de();
    let (_, a) = win(&mut h, &mut c, "a");
    let (_, b) = win(&mut h, &mut c, "b");
    let (_, cc) = win(&mut h, &mut c, "c");
    shell::focus_window(&mut h.state, &a);
    shell::focus_window(&mut h.state, &b);
    shell::focus_window(&mut h.state, &cc);
    // Order: c, b, a.
    down(&mut h, KEY_LEFTALT);
    tap(&mut h, KEY_TAB);
    assert_eq!(h.state.switcher.selected(), Some(1));
    tap(&mut h, KEY_TAB);
    assert_eq!(h.state.switcher.selected(), Some(2));
    tap(&mut h, KEY_TAB);
    assert_eq!(h.state.switcher.selected(), Some(0), "wraps");
    down(&mut h, KEY_LEFTSHIFT);
    tap(&mut h, KEY_TAB);
    assert_eq!(h.state.switcher.selected(), Some(2), "shift goes back");
    up(&mut h, KEY_LEFTSHIFT);
    assert!(
        h.state.switcher.is_open(),
        "letting go of shift alone does not activate"
    );
    tap(&mut h, KEY_RIGHT);
    assert_eq!(h.state.switcher.selected(), Some(0), "the arrow keys step too");
    up(&mut h, KEY_LEFTALT);
    assert_eq!(h.state.focus.as_ref(), Some(&cc));
}

#[test]
fn alt_shift_tab_opens_on_the_least_recently_used_window() {
    let (mut h, mut c) = de();
    let (_, a) = win(&mut h, &mut c, "a");
    let (_, b) = win(&mut h, &mut c, "b");
    let (_, cc) = win(&mut h, &mut c, "c");
    shell::focus_window(&mut h.state, &a);
    shell::focus_window(&mut h.state, &b);
    shell::focus_window(&mut h.state, &cc);
    down(&mut h, KEY_LEFTALT);
    down(&mut h, KEY_LEFTSHIFT);
    tap(&mut h, KEY_TAB);
    assert!(h.state.switcher.is_open());
    assert_eq!(h.state.switcher.selected_window(), Some(a.clone()));
    up(&mut h, KEY_LEFTSHIFT);
    up(&mut h, KEY_LEFTALT);
    assert_eq!(h.state.focus.as_ref(), Some(&a));
}

#[test]
fn activating_switches_desktop_and_unminimizes() {
    let (mut h, mut c) = de();
    let (_, a) = win(&mut h, &mut c, "a");
    shell::switch_workspace(&mut h.state, 3);
    let (_, b) = win(&mut h, &mut c, "b");
    assert_eq!(active(&h), 2);
    down(&mut h, KEY_LEFTALT);
    tap(&mut h, KEY_TAB);
    up(&mut h, KEY_LEFTALT);
    assert_eq!(active(&h), 0, "its desktop came forward");
    assert_eq!(h.state.focus.as_ref(), Some(&a));
    // A minimized window: the last in the list, chosen with shift.
    shell::minimize_window(&mut h.state, &a);
    assert!(shell::is_minimized(&h.state, &a));
    shell::switch_workspace(&mut h.state, 3);
    shell::focus_window(&mut h.state, &b);
    down(&mut h, KEY_LEFTALT);
    down(&mut h, KEY_LEFTSHIFT);
    tap(&mut h, KEY_TAB);
    assert_eq!(h.state.switcher.selected_window(), Some(a.clone()));
    up(&mut h, KEY_LEFTSHIFT);
    up(&mut h, KEY_LEFTALT);
    assert!(!shell::is_minimized(&h.state, &a));
    assert_eq!(active(&h), 0);
    assert_eq!(h.state.focus.as_ref(), Some(&a));
}

#[test]
fn escape_cancels_with_nothing_changed_and_no_keyboard_grab_remains() {
    let (mut h, mut c) = de();
    let (_, a) = win(&mut h, &mut c, "a");
    let (_, b) = win(&mut h, &mut c, "b");
    let before = (h.state.focus.clone(), active(&h));
    down(&mut h, KEY_LEFTALT);
    tap(&mut h, KEY_TAB);
    assert!(h.state.switcher.is_open());
    tap(&mut h, KEY_ESC);
    assert!(!h.state.switcher.is_open());
    up(&mut h, KEY_LEFTALT);
    assert_eq!((h.state.focus.clone(), active(&h)), before);
    assert_eq!(h.state.focus.as_ref(), Some(&b));
    assert!(!h.state.seat.get_keyboard().unwrap().is_grabbed());
    let _ = a;
}

#[test]
fn a_window_closing_mid_switch_drops_out_without_picking_another() {
    let (mut h, mut c) = de();
    let (_, a) = win(&mut h, &mut c, "a");
    let (tb, b) = win(&mut h, &mut c, "b");
    let (_, cc) = win(&mut h, &mut c, "c");
    let (_, d) = win(&mut h, &mut c, "d");
    for w in [&a, &b, &cc, &d] {
        shell::focus_window(&mut h.state, w);
    }
    // Order: d, c, b, a.
    down(&mut h, KEY_LEFTALT);
    tap(&mut h, KEY_TAB);
    tap(&mut h, KEY_TAB);
    assert_eq!(h.state.switcher.selected_window(), Some(b.clone()));
    // The selected window closes.
    tb.toplevel.destroy();
    tb.xdg.destroy();
    tb.surface.destroy();
    c.pump(&mut h);
    assert!(h.state.switcher.is_open());
    assert_eq!(h.state.switcher.windows(), [d.clone(), cc.clone(), a.clone()]);
    assert_eq!(
        h.state.switcher.selected_window(),
        Some(a.clone()),
        "the selection stays at its place: the window that followed"
    );
    up(&mut h, KEY_LEFTALT);
    assert_eq!(h.state.focus.as_ref(), Some(&a));
}

#[test]
fn closing_every_other_window_closes_the_switcher_cleanly() {
    let (mut h, mut c) = de();
    let (ta, _) = win(&mut h, &mut c, "a");
    let (tb, _) = win(&mut h, &mut c, "b");
    down(&mut h, KEY_LEFTALT);
    tap(&mut h, KEY_TAB);
    assert!(h.state.switcher.is_open());
    for t in [&ta, &tb] {
        t.toplevel.destroy();
        t.xdg.destroy();
        t.surface.destroy();
    }
    c.pump(&mut h);
    assert!(!h.state.switcher.is_open());
    up(&mut h, KEY_LEFTALT);
    assert!(!h.state.switcher.is_open());
}

#[test]
fn a_click_on_a_card_activates_it_and_a_click_outside_cancels() {
    let (mut h, mut c) = de();
    let (_, a) = win(&mut h, &mut c, "a");
    let (_, b) = win(&mut h, &mut c, "b");
    assert!(switcher::open(&mut h.state, false));
    let card = h.state.switcher.placed()[1].frame;
    move_to(&mut h, centre(card));
    assert_eq!(h.state.switcher.selected(), Some(1), "hover selects");
    click(&mut h, LEFT, ButtonState::Pressed);
    click(&mut h, LEFT, ButtonState::Released);
    assert!(!h.state.switcher.is_open());
    assert_eq!(h.state.focus.as_ref(), Some(&a));
    // Outside the panel: cancel.
    assert!(switcher::open(&mut h.state, false));
    click_at(&mut h, pt(2.0, 598.0));
    assert!(!h.state.switcher.is_open());
    assert_eq!(h.state.focus.as_ref(), Some(&a), "unchanged");
    let _ = b;
}

#[test]
fn a_switcher_opened_with_no_modifier_stays_until_enter() {
    let (mut h, mut c) = de();
    let (_, a) = win(&mut h, &mut c, "a");
    let (_, _b) = win(&mut h, &mut c, "b");
    assert!(switcher::open(&mut h.state, false));
    up(&mut h, KEY_LEFTALT);
    assert!(h.state.switcher.is_open(), "no chord to release");
    tap(&mut h, 28);
    assert!(!h.state.switcher.is_open());
    assert_eq!(h.state.focus.as_ref(), Some(&a));
}

#[test]
fn the_overlays_open_only_under_de_and_never_under_the_lock() {
    let (mut h, mut c) = de();
    let (_, w) = win(&mut h, &mut c, "a");
    let (_, _w2) = win(&mut h, &mut c, "b");
    flip(&mut h, Mode::Hybrid);
    assert!(!switcher::open(&mut h.state, false));
    assert!(!super::open(&mut h.state));
    assert_eq!(
        preview::show(&mut h.state, &w, Rectangle::new((0, 0).into(), (10, 10).into())),
        Err("previews need mode \"de\" and a live session")
    );
    flip(&mut h, Mode::De);
    h.state.engage_lock();
    assert!(!switcher::open(&mut h.state, false));
    assert!(!super::open(&mut h.state));
    assert!(preview::show(&mut h.state, &w, Rectangle::new((0, 0).into(), (10, 10).into())).is_err());
    assert!(!super::any_open(&h.state));
}

#[test]
fn locking_or_leaving_de_closes_each_overlay_and_drops_its_textures() {
    let (mut h, mut c) = de();
    let (_, w) = win(&mut h, &mut c, "a");
    let (_, _w2) = win(&mut h, &mut c, "b");
    let anchor = Rectangle::new((100, 560).into(), (80, 30).into());
    for kind in 0..3 {
        match kind {
            0 => assert!(switcher::open(&mut h.state, false)),
            1 => assert!(super::open(&mut h.state)),
            _ => assert_eq!(preview::show(&mut h.state, &w, anchor), Ok(true)),
        }
        assert!(super::any_open(&h.state), "kind {kind}");
        h.state.engage_lock();
        assert!(!super::any_open(&h.state), "the lock closes kind {kind}");
        assert!(h.state.thumbs.store.is_empty() && h.state.overlay_cache.is_empty());
        h.state.lock.locked = false;
        match kind {
            0 => assert!(switcher::open(&mut h.state, false)),
            1 => assert!(super::open(&mut h.state)),
            _ => assert_eq!(preview::show(&mut h.state, &w, anchor), Ok(true)),
        }
        flip(&mut h, Mode::Wm);
        assert!(!super::any_open(&h.state), "leaving de closes kind {kind}");
        flip(&mut h, Mode::De);
    }
}

#[test]
fn removing_the_output_closes_what_is_open_on_it() {
    let (mut h, mut c) = de();
    let (_, _w) = win(&mut h, &mut c, "a");
    let id = h.state.outputs.focused().unwrap().id;
    assert!(switcher::open(&mut h.state, false));
    crate::outputs::unregister(&mut h.state, id);
    assert!(!h.state.switcher.is_open());
    // The same for the overview, on whichever output now has the focus.
    assert!(super::open(&mut h.state));
    let id = h.state.overview.output().unwrap();
    crate::outputs::unregister(&mut h.state, id);
    assert!(!h.state.overview.is_open());
}

#[test]
fn opening_one_overlay_closes_the_others() {
    let (mut h, mut c) = de();
    let (_, w) = win(&mut h, &mut c, "a");
    let (_, _w2) = win(&mut h, &mut c, "b");
    let anchor = Rectangle::new((100, 560).into(), (80, 30).into());
    assert_eq!(preview::show(&mut h.state, &w, anchor), Ok(true));
    assert!(switcher::open(&mut h.state, false));
    assert!(h.state.switcher.is_open() && !h.state.preview.is_open());
    assert!(super::open(&mut h.state));
    assert!(h.state.overview.is_open() && !h.state.switcher.is_open());
    assert!(
        preview::show(&mut h.state, &w, anchor).is_err(),
        "a preview does not open over the switcher or the overview"
    );
    super::close(&mut h.state);
    assert_eq!(preview::show(&mut h.state, &w, anchor), Ok(true));
    assert!(super::open(&mut h.state));
    assert!(!h.state.preview.is_open());
}

#[test]
fn keys_go_to_the_overlay_and_not_to_a_client_and_no_grab_is_left() {
    let (mut h, mut c) = de();
    let (_, w) = win(&mut h, &mut c, "a");
    assert!(super::open(&mut h.state));
    tap(&mut h, 30); // a letter: filtered, not delivered
    assert!(h.state.overview.is_open());
    assert_eq!(h.state.overview.filter(), Some("a"));
    tap(&mut h, KEY_ESC);
    assert!(!h.state.overview.is_open());
    let keyboard = h.state.seat.get_keyboard().unwrap();
    assert!(!keyboard.is_grabbed(), "neither overlay holds a keyboard grab");
    assert_eq!(h.state.focus.as_ref(), Some(&w), "focus is where it was");
}

// -------------------------------------------------------------------- overview

#[test]
fn super_w_toggles_the_overview() {
    let (mut h, mut c) = de();
    let (_, _w) = win(&mut h, &mut c, "a");
    down(&mut h, KEY_LEFTMETA);
    tap(&mut h, KEY_W);
    assert!(h.state.overview.is_open());
    tap(&mut h, KEY_W);
    assert!(!h.state.overview.is_open(), "the same chord closes it");
    tap(&mut h, KEY_W);
    assert!(h.state.overview.is_open());
    up(&mut h, KEY_LEFTMETA);
    tap(&mut h, KEY_ESC);
    assert!(!h.state.overview.is_open());
}

#[test]
fn the_overview_lays_out_this_desktops_windows_without_overlap() {
    let (mut h, mut c) = de();
    let sizes = [(300, 200), (150, 400), (500, 120), (240, 240), (320, 180)];
    let mut wins = Vec::new();
    for (i, s) in sizes.iter().enumerate() {
        wins.push(win_sized(&mut h, &mut c, &format!("w{i}"), *s).1);
    }
    // One on another desktop is not in this one's overview.
    shell::switch_workspace(&mut h.state, 2);
    let (_, other) = win(&mut h, &mut c, "other");
    shell::switch_workspace(&mut h.state, 1);
    assert!(super::open(&mut h.state));
    let cards = h.state.overview.cards();
    assert_eq!(cards.len(), 5);
    assert!(!cards.contains(&other));
    let placed = h.state.overview.placed().to_vec();
    let out = h
        .state
        .space
        .output_geometry(&h.state.outputs.focused().unwrap().output)
        .unwrap();
    for (i, a) in placed.iter().enumerate() {
        assert!(out.contains_rect(a.frame), "{:?} is off the output", a.frame);
        for b in &placed[i + 1..] {
            assert!(!a.frame.overlaps(b.frame), "{:?} overlaps {:?}", a.frame, b.frame);
        }
    }
    for (p, w) in placed.iter().zip(&cards) {
        let b = w.bbox().size;
        let want = f64::from(b.w) / f64::from(b.h);
        let got = f64::from(p.thumb.size.w) / f64::from(p.thumb.size.h);
        assert!((got / want - 1.0).abs() < 0.05, "aspect {got} vs {want}");
    }
    // The strip is above the cards and holds all ten desktops.
    assert_eq!(h.state.overview.tiles().len(), 10);
    assert!(placed.iter().all(|p| p.frame.loc.y
        >= h.state
            .overview
            .tiles()
            .iter()
            .map(|t| t.loc.y + t.size.h)
            .max()
            .unwrap()));
    let _ = wins;
}

#[test]
fn typing_filters_the_cards_by_title_without_regard_to_case() {
    let (mut h, mut c) = de();
    let (_, alpha) = win(&mut h, &mut c, "Alpha notes");
    let (_, _beta) = win(&mut h, &mut c, "beta");
    let (_, gamma) = win(&mut h, &mut c, "ALPHABET soup");
    assert!(super::open(&mut h.state));
    assert_eq!(h.state.overview.cards().len(), 3);
    h.state.inject_text("alph").unwrap();
    let cards = h.state.overview.cards();
    assert_eq!(cards.len(), 2);
    assert!(cards.contains(&alpha) && cards.contains(&gamma));
    h.state.inject_text("abet").unwrap();
    assert_eq!(h.state.overview.cards(), std::slice::from_ref(&gamma));
    h.state.inject_key("BackSpace", &[]).unwrap();
    h.state.inject_key("BackSpace", &[]).unwrap();
    h.state.inject_key("BackSpace", &[]).unwrap();
    h.state.inject_key("BackSpace", &[]).unwrap();
    assert_eq!(h.state.overview.filter(), Some("alph"));
    assert_eq!(h.state.overview.cards().len(), 2);
    h.state.inject_text("zzz").unwrap();
    assert!(h.state.overview.cards().is_empty());
    // The filter pill is part of the frame, and shows what was typed.
    assert!(drawn_text(&frame_of(&h)).contains("alphzzz"));
}

#[test]
fn a_click_on_a_card_focuses_it_and_closes_the_overview() {
    let (mut h, mut c) = de();
    let (_, a) = win(&mut h, &mut c, "a");
    let (_, b) = win(&mut h, &mut c, "b");
    assert_eq!(h.state.focus.as_ref(), Some(&b));
    assert!(super::open(&mut h.state));
    let k = h.state.overview.cards().iter().position(|w| *w == a).unwrap();
    let frame = h.state.overview.placed()[k].frame;
    click_at(&mut h, centre(frame));
    assert!(!h.state.overview.is_open());
    assert_eq!(h.state.focus.as_ref(), Some(&a));
}

#[test]
fn a_click_on_nothing_closes_the_overview_and_changes_nothing() {
    let (mut h, mut c) = de();
    let (_, _a) = win(&mut h, &mut c, "a");
    let (_, b) = win(&mut h, &mut c, "b");
    assert!(super::open(&mut h.state));
    click_at(&mut h, pt(3.0, 590.0));
    assert!(!h.state.overview.is_open());
    assert_eq!(h.state.focus.as_ref(), Some(&b));
}

#[test]
fn a_middle_click_closes_the_window() {
    let (mut h, mut c) = de();
    let (ta, a) = win(&mut h, &mut c, "a");
    let (_, _b) = win(&mut h, &mut c, "b");
    assert!(super::open(&mut h.state));
    let k = h.state.overview.cards().iter().position(|w| *w == a).unwrap();
    let at = centre(h.state.overview.placed()[k].frame);
    move_to(&mut h, at);
    click(&mut h, MIDDLE, ButtonState::Pressed);
    click(&mut h, MIDDLE, ButtonState::Released);
    c.pump(&mut h);
    assert!(c.closed(&ta.toplevel), "the window was asked to close");
    assert!(h.state.overview.is_open(), "the overview stays");
}

#[test]
fn a_click_on_a_desktop_in_the_strip_switches_to_it() {
    let (mut h, mut c) = de();
    let (_, _a) = win(&mut h, &mut c, "a");
    assert!(super::open(&mut h.state));
    let tile = h.state.overview.tiles()[3];
    click_at(&mut h, centre(tile));
    assert_eq!(active(&h), 3);
    assert!(h.state.overview.is_open(), "it stays, showing that desktop");
    assert_eq!(h.state.overview.desktop(), Some(3));
    assert!(h.state.overview.cards().is_empty());
}

#[test]
fn dragging_a_card_onto_a_desktop_moves_the_window_there() {
    let (mut h, mut c) = de();
    let (_, a) = win(&mut h, &mut c, "a");
    let (_, b) = win(&mut h, &mut c, "b");
    assert!(super::open(&mut h.state));
    let k = h.state.overview.cards().iter().position(|w| *w == a).unwrap();
    let from = centre(h.state.overview.placed()[k].frame);
    let tile = h.state.overview.tiles()[5];
    let to = centre(tile);
    move_to(&mut h, from);
    click(&mut h, LEFT, ButtonState::Pressed);
    for i in 1..=6 {
        let f = f64::from(i) / 6.0;
        move_to(
            &mut h,
            pt(from.x + (to.x - from.x) * f, from.y + (to.y - from.y) * f),
        );
    }
    assert!(h.state.overview.dragging());
    assert!(
        frame_of(&h).view.tiles[5].target,
        "the desktop under the dragged card is lit as the target"
    );
    click(&mut h, LEFT, ButtonState::Released);
    assert_eq!(desk_of(&h, &a), 5, "moved through move_window_to_workspace");
    assert_eq!(desk_of(&h, &b), 0, "the other window stayed");
    assert!(!h.state.overview.dragging());
}

#[test]
fn dropping_a_dragged_card_anywhere_else_changes_nothing() {
    let (mut h, mut c) = de();
    let (_, a) = win(&mut h, &mut c, "a");
    let (_, _b) = win(&mut h, &mut c, "b");
    assert!(super::open(&mut h.state));
    let k = h.state.overview.cards().iter().position(|w| *w == a).unwrap();
    let from = centre(h.state.overview.placed()[k].frame);
    move_to(&mut h, from);
    click(&mut h, LEFT, ButtonState::Pressed);
    move_to(&mut h, pt(from.x + 30.0, from.y + 40.0));
    click(&mut h, LEFT, ButtonState::Released);
    assert_eq!(desk_of(&h, &a), 0);
    assert!(h.state.overview.is_open());
}

#[test]
fn a_window_closing_under_the_overview_rebuilds_it() {
    let (mut h, mut c) = de();
    let (ta, _a) = win(&mut h, &mut c, "a");
    let (_, _b) = win(&mut h, &mut c, "b");
    assert!(super::open(&mut h.state));
    assert_eq!(h.state.overview.cards().len(), 2);
    ta.toplevel.destroy();
    ta.xdg.destroy();
    ta.surface.destroy();
    c.pump(&mut h);
    assert!(h.state.overview.is_open());
    assert_eq!(h.state.overview.cards().len(), 1);
}

// --------------------------------------------------------------------- corners

const DWELL: Duration = Duration::from_millis(150);
const COOL: Duration = Duration::from_millis(750);

#[test]
fn a_corner_fires_once_after_its_dwell_and_leaving_cancels_it() {
    let mut c = Corners::default();
    let t0 = Instant::now();
    let Arm::Wait(d, serial) = c.enter(1, 0, DWELL, COOL, t0) else {
        panic!("a dwell is waited out");
    };
    assert_eq!(d, DWELL);
    assert_eq!(c.armed(), Some(0));
    assert_eq!(
        c.enter(1, 0, DWELL, COOL, t0),
        Arm::Nothing,
        "still there: no re-arm"
    );
    // The timer fires with the pointer still in the corner.
    assert_eq!(c.due(serial, Some((1, 0)), t0 + DWELL), Some(0));
    assert_eq!(c.due(serial, Some((1, 0)), t0 + DWELL), None, "once");
    // A pointer that left before the timer: nothing.
    let mut c = Corners::default();
    let Arm::Wait(_, serial) = c.enter(1, 0, DWELL, COOL, t0) else {
        panic!()
    };
    assert_eq!(c.due(serial, None, t0 + DWELL), None);
    assert_eq!(c.due(serial, Some((1, 0)), t0 + DWELL), None, "and it is spent");
    // A timer from an earlier arming is stale.
    let mut c = Corners::default();
    let Arm::Wait(_, first) = c.enter(1, 0, DWELL, COOL, t0) else {
        panic!()
    };
    let Arm::Wait(_, second) = c.enter(1, 1, DWELL, COOL, t0) else {
        panic!()
    };
    assert_ne!(first, second);
    assert_eq!(c.due(first, Some((1, 1)), t0 + DWELL), None);
    assert_eq!(c.due(second, Some((1, 1)), t0 + DWELL), Some(1));
}

#[test]
fn no_dwell_fires_at_once() {
    let mut c = Corners::default();
    assert_eq!(c.enter(1, 0, Duration::ZERO, COOL, Instant::now()), Arm::Now(0));
}

#[test]
fn the_cooldown_holds_every_corner_after_one_fires() {
    let mut c = Corners::default();
    let t0 = Instant::now();
    assert_eq!(c.enter(1, 0, Duration::ZERO, COOL, t0), Arm::Now(0));
    // Leave (a fresh state machine would be reset by `disarm`; do it the
    // same way the pointer path does) and come back, in the cooldown: nothing.
    let loop_ = smithay::reexports::calloop::EventLoop::<crate::state::AbyssState>::try_new().unwrap();
    c.disarm(&loop_.handle());
    assert_eq!(
        c.enter(1, 2, DWELL, COOL, t0 + Duration::from_millis(300)),
        Arm::Nothing,
        "another corner, in the cooldown"
    );
    c.disarm(&loop_.handle());
    assert!(matches!(
        c.enter(1, 0, DWELL, COOL, t0 + COOL + Duration::from_millis(1)),
        Arm::Wait(..)
    ));
}

#[test]
fn only_outer_corners_have_a_rectangle() {
    let (h, _c) = de();
    let rects = h.state.corners.rects();
    // Two outputs side by side: each has four corners, minus the two that
    // meet along the shared edge on each.
    let on = |id: u64| rects.iter().filter(|r| r.output == id).count();
    assert_eq!(on(h.a) + on(h.b), 4 + 4 - 4, "{rects:?}");
    for r in rects {
        let geo = h
            .state
            .space
            .output_geometry(&h.state.outputs.get(r.output).unwrap().output)
            .unwrap();
        assert!(geo.contains_rect(r.rect));
    }
    // The top-left of the first output is a corner.
    let first = h
        .state
        .space
        .output_geometry(&h.state.outputs.get(h.a).unwrap().output)
        .unwrap();
    assert!(h
        .state
        .corners
        .at(pt(f64::from(first.loc.x) + 1.0, f64::from(first.loc.y) + 1.0))
        .is_some());
}

#[test]
fn a_dwelling_pointer_in_the_top_left_opens_the_overview_and_others_do_nothing() {
    let (mut h, mut c) = de();
    let (_, _w) = win(&mut h, &mut c, "a");
    // The default: the top-left corner is the overview, after the dwell.
    move_to(&mut h, pt(400.0, 300.0));
    move_to(&mut h, pt(1.0, 1.0));
    assert_eq!(h.state.corners.armed(), Some(0), "armed, waiting out the dwell");
    assert!(!h.state.overview.is_open(), "it does not fire before the dwell");
    let serial = corner_serial(&h);
    super::corners::fire(&mut h.state, serial);
    assert!(h.state.overview.is_open());
    // Escape, then another corner: off by default.
    tap(&mut h, KEY_ESC);
    h.state.config.general.hot_corner_dwell_ms = 0;
    let geo = h
        .state
        .space
        .output_geometry(&h.state.outputs.get(h.a).unwrap().output)
        .unwrap();
    move_to(
        &mut h,
        pt(
            f64::from(geo.loc.x + geo.size.w) - 2.0,
            f64::from(geo.loc.y) + 1.0,
        ),
    );
    assert!(!h.state.overview.is_open(), "top-right is off");
}

fn corner_serial(h: &Harness) -> u64 {
    h.state.corners.serial_for_test()
}

#[test]
fn a_dwell_timer_that_fires_while_suppressed_does_not_leave_the_corner_dead() {
    let (mut h, mut c) = de();
    let (_, _w) = win(&mut h, &mut c, "a");
    h.state.config.general.hot_corner_dwell_ms = 150;
    move_to(&mut h, pt(400.0, 300.0));
    move_to(&mut h, pt(1.0, 1.0));
    assert_eq!(h.state.corners.armed(), Some(0));
    let serial = corner_serial(&h);
    // A grab starts before the timer runs.
    h.state.pointer_grab_active = true;
    super::corners::fire(&mut h.state, serial);
    assert!(!h.state.overview.is_open());
    h.state.pointer_grab_active = false;
    // The pointer is still in the corner: the next motion arms it afresh.
    move_to(&mut h, pt(2.0, 2.0));
    assert_eq!(h.state.corners.armed(), Some(0));
    let serial = corner_serial(&h);
    super::corners::fire(&mut h.state, serial);
    assert!(h.state.overview.is_open());
}

#[test]
fn a_zero_dwell_corner_fires_on_entry_and_the_cooldown_stops_a_second_one() {
    let (mut h, mut c) = de();
    let (_, _w) = win(&mut h, &mut c, "a");
    h.state.config.general.hot_corner_dwell_ms = 0;
    move_to(&mut h, pt(400.0, 300.0));
    move_to(&mut h, pt(1.0, 1.0));
    assert!(h.state.overview.is_open());
    tap(&mut h, KEY_ESC);
    // Out and back in within the cooldown: nothing.
    move_to(&mut h, pt(400.0, 300.0));
    move_to(&mut h, pt(1.0, 1.0));
    assert!(!h.state.overview.is_open());
}

#[test]
fn a_switch_windows_corner_opens_the_switcher() {
    let (mut h, mut c) = de();
    let (_, _a) = win(&mut h, &mut c, "a");
    let (_, _b) = win(&mut h, &mut c, "b");
    h.state.config.general.hot_corners[1] = crate::config::HotCorner::SwitchWindows;
    h.state.config.general.hot_corner_dwell_ms = 0;
    let geo = h
        .state
        .space
        .output_geometry(&h.state.outputs.get(h.a).unwrap().output)
        .unwrap();
    // The top-right of the first output meets the second one: not a corner.
    let right = pt(
        f64::from(geo.loc.x + geo.size.w) - 1.0,
        f64::from(geo.loc.y) + 1.0,
    );
    move_to(&mut h, right);
    assert!(
        !h.state.switcher.is_open(),
        "an edge shared with another output is not a corner"
    );
    // The far side of the second output is.
    let other = h
        .state
        .space
        .output_geometry(&h.state.outputs.get(h.b).unwrap().output)
        .unwrap();
    move_to(
        &mut h,
        pt(
            f64::from(other.loc.x + other.size.w) - 1.0,
            f64::from(other.loc.y) + 1.0,
        ),
    );
    assert!(h.state.switcher.is_open());
}

#[test]
fn hot_corners_are_suppressed_by_fullscreen_grabs_the_lock_and_other_overlays() {
    let (mut h, mut c) = de();
    let (_, w) = win(&mut h, &mut c, "a");
    h.state.config.general.hot_corner_dwell_ms = 0;
    let away = |h: &mut Harness| move_to(h, pt(400.0, 300.0));
    let corner = |h: &mut Harness| move_to(h, pt(1.0, 1.0));
    // A grab (a button held starts one).
    h.state.pointer_grab_active = true;
    away(&mut h);
    corner(&mut h);
    assert!(!h.state.overview.is_open(), "during a grab");
    h.state.pointer_grab_active = false;
    // Fullscreen on the output.
    shell::focus_window(&mut h.state, &w);
    shell::toggle_fullscreen(&mut h.state);
    away(&mut h);
    corner(&mut h);
    assert!(!h.state.overview.is_open(), "during fullscreen");
    shell::toggle_fullscreen(&mut h.state);
    // The lock.
    h.state.engage_lock();
    away(&mut h);
    corner(&mut h);
    assert!(
        !h.state.overview.is_open() && h.state.corners.armed().is_none(),
        "under the lock"
    );
    h.state.lock.locked = false;
    // Another overlay already up.
    assert!(switcher::open(&mut h.state, false));
    away(&mut h);
    corner(&mut h);
    assert!(h.state.switcher.is_open() && !h.state.overview.is_open());
    // And outside de.
    super::close(&mut h.state);
    flip(&mut h, Mode::Hybrid);
    away(&mut h);
    corner(&mut h);
    assert!(!h.state.overview.is_open());
}

// --------------------------------------------------------------------- preview

fn show_params(id: u64) -> Value {
    json!({"id": id, "x": 300.0, "y": 560.0, "width": 120.0, "height": 30.0})
}

#[test]
fn a_preview_is_placed_beside_its_anchor_and_stays_on_the_output() {
    let (mut h, mut c) = de();
    let (_, w) = win_sized(&mut h, &mut c, "a", (400, 300));
    let out = h
        .state
        .space
        .output_geometry(&h.state.outputs.get(h.a).unwrap().output)
        .unwrap();
    let anchor = Rectangle::new((300, 560).into(), (120, 30).into());
    assert_eq!(preview::show(&mut h.state, &w, anchor), Ok(true));
    let placed = h.state.preview.placed().unwrap();
    assert!(out.contains_rect(placed.frame));
    assert!(
        placed.frame.loc.y + placed.frame.size.h <= anchor.loc.y,
        "a chip at the bottom gets a preview above it"
    );
    // At the corner of the output it is clamped.
    let corner = Rectangle::new((out.loc.x + out.size.w - 40, 570).into(), (40, 30).into());
    assert_eq!(preview::show(&mut h.state, &w, corner), Ok(true));
    assert!(out.contains_rect(h.state.preview.placed().unwrap().frame));
    assert_eq!(h.state.preview.anchor(), Some(corner));
}

#[test]
fn a_preview_ends_by_hide_by_the_window_closing_by_focus_by_the_lock_and_by_idling() {
    let (mut h, mut c) = de();
    let (_, a) = win(&mut h, &mut c, "a");
    let (tb, b) = win(&mut h, &mut c, "b");
    let anchor = Rectangle::new((300, 560).into(), (120, 30).into());
    // hide
    assert_eq!(preview::show(&mut h.state, &a, anchor), Ok(true));
    assert!(preview::hide(&mut h.state));
    assert!(!h.state.preview.is_open());
    assert!(!preview::hide(&mut h.state), "nothing to hide");
    // focus moves to the window
    assert_eq!(preview::show(&mut h.state, &a, anchor), Ok(true));
    shell::focus_window(&mut h.state, &b);
    assert!(h.state.preview.is_open(), "focus moving elsewhere leaves it");
    shell::focus_window(&mut h.state, &a);
    assert!(!h.state.preview.is_open(), "focus moving to its window ends it");
    // the window closing
    shell::focus_window(&mut h.state, &a);
    assert_eq!(preview::show(&mut h.state, &b, anchor), Ok(true));
    tb.toplevel.destroy();
    tb.xdg.destroy();
    tb.surface.destroy();
    c.pump(&mut h);
    assert!(!h.state.preview.is_open());
    // the lock
    assert_eq!(preview::show(&mut h.state, &a, anchor), Ok(true));
    h.state.engage_lock();
    assert!(!h.state.preview.is_open());
    h.state.lock.locked = false;
    // the idle timeout: only the showing it belongs to
    assert_eq!(preview::show(&mut h.state, &a, anchor), Ok(true));
    let first = h.state.preview.serial();
    assert_eq!(preview::show(&mut h.state, &a, anchor), Ok(true));
    let second = h.state.preview.serial();
    assert_ne!(first, second);
    preview::idle_expired(&mut h.state, first);
    assert!(h.state.preview.is_open(), "an earlier showing's timer is stale");
    preview::idle_expired(&mut h.state, second);
    assert!(!h.state.preview.is_open());
    assert_eq!(preview::IDLE, Duration::from_secs(10));
}

#[test]
fn the_preview_methods_validate_their_parameters_and_answer_through_the_handler() {
    let (mut h, mut c) = de();
    let (_, a) = win(&mut h, &mut c, "a");
    let id = handle(&mut h, &a);
    let call = |h: &mut Harness, m: &str, p: Value| {
        dispatch(&mut h.state, 0, m, &p).map_err(|e| (e.code, e.message))
    };
    let ok = call(&mut h, "show_window_preview", show_params(id)).expect("shown");
    assert_eq!(ok, json!({"ok": true, "shown": true}));
    assert!(h.state.preview.is_open());
    let bad = |p: Value| p;
    for p in [
        json!({"id": id, "x": 1.0, "y": 1.0, "width": 0.0, "height": 5.0}),
        json!({"id": id, "x": 1.0, "y": 1.0, "width": 5.0}),
        json!({"id": id, "x": "a", "y": 1.0, "width": 5.0, "height": 5.0}),
        json!({"id": 9999, "x": 1.0, "y": 1.0, "width": 5.0, "height": 5.0}),
        json!({"id": id, "x": 1.0, "y": 1.0, "width": 5.0, "height": 5.0, "extra": 1}),
        json!({"id": id, "x": 1e12, "y": 1.0, "width": 5.0, "height": 5.0}),
    ] {
        assert!(call(&mut h, "show_window_preview", bad(p)).is_err());
    }
    let hid = call(&mut h, "hide_window_preview", Value::Null).expect("hidden");
    assert_eq!(hid, json!({"ok": true, "hidden": true}));
    let again = call(&mut h, "hide_window_preview", Value::Null).expect("still ok");
    assert_eq!(again, json!({"ok": true, "hidden": false}));
    assert!(call(&mut h, "hide_window_preview", json!({"x": 1})).is_err());
    // Not under de, not under the lock.
    h.state.engage_lock();
    assert!(call(&mut h, "show_window_preview", show_params(id)).is_err());
    h.state.lock.locked = false;
    flip(&mut h, Mode::Hybrid);
    assert!(call(&mut h, "show_window_preview", show_params(id)).is_err());
}

// ------------------------------------------------------------------- redaction

/// Two windows, the second made secret by `how`; both opened in `which`.
fn secret_and_plain(how: &str) -> (Harness, Client, Window, Window) {
    let (mut h, mut c) = de();
    let (t, s) = win(&mut h, &mut c, "TOP-SECRET-LEDGER");
    let (_, p) = win(&mut h, &mut c, "public notes");
    match how {
        "sensitive" => secret(&mut h, &s),
        _ => {
            t.toplevel.set_app_id("vault.app".into());
            c.pump(&mut h);
            h.state.config.capture.redact_app_id.push("vault.app".into());
        }
    }
    (h, c, s, p)
}

fn assert_secret_never_shows(h: &mut Harness, secret: &Window, plain: &Window) {
    let sid = handle(h, secret);
    let pid = handle(h, plain);
    let f = frame_of(h);
    let by = |id: u64| f.view.cards.iter().find(|c| c.id == id);
    let card = by(sid).expect("a redacted window still has a placeholder card");
    assert_eq!(card.body, Body::Hidden, "no pixels");
    assert!(!card.wants_thumb());
    assert_eq!(card.title, None, "no title");
    let shown = f.wanted.iter().find(|w| w.handle == sid).expect("planned");
    assert_eq!(shown.shown.face, Face::Hidden);
    let (draw, drop) = thumbs::split(&f.wanted);
    assert!(!draw.iter().any(|w| w.handle == sid), "never handed to the GPU");
    assert!(drop.contains(&sid), "and its texture is dropped");
    assert!(
        !drawn_text(&f).contains("TOP-SECRET-LEDGER"),
        "the title is nowhere in what is drawn"
    );
    // The other window is drawn normally.
    let pc = by(pid).expect("plain card");
    assert!(matches!(pc.body, Body::Pixels { .. }) && pc.title.as_deref() == Some("public notes"));
    assert!(draw.iter().any(|w| w.handle == pid));
}

#[test]
fn a_secret_window_never_yields_pixels_or_a_title_in_the_switcher_the_overview_or_a_preview() {
    for how in ["sensitive", "app-id"] {
        let (mut h, _c, s, p) = secret_and_plain(how);
        // The switcher.
        assert!(switcher::open(&mut h.state, false), "{how}");
        assert_secret_never_shows(&mut h, &s, &p);
        super::close(&mut h.state);
        // The overview.
        assert!(super::open(&mut h.state), "{how}");
        assert_secret_never_shows(&mut h, &s, &p);
        super::close(&mut h.state);
        // The preview: the secret one is a blank card with no title.
        let anchor = Rectangle::new((300, 560).into(), (120, 30).into());
        assert_eq!(preview::show(&mut h.state, &s, anchor), Ok(true), "{how}");
        let f = frame_of(&h);
        assert_eq!(f.view.cards.len(), 1);
        assert_eq!(f.view.cards[0].body, Body::Hidden);
        assert_eq!(f.view.cards[0].title, None);
        assert!(thumbs::split(&f.wanted).0.is_empty());
        assert!(!drawn_text(&f).contains("TOP-SECRET-LEDGER"));
        // A window that is not secret previews with its pixels and title.
        assert_eq!(preview::show(&mut h.state, &p, anchor), Ok(true));
        let f = frame_of(&h);
        assert!(matches!(f.view.cards[0].body, Body::Pixels { .. }));
        assert_eq!(f.view.cards[0].title.as_deref(), Some("public notes"));
    }
}

#[test]
fn the_decision_is_made_before_any_texture_work() {
    let (mut h, _c, s, p) = secret_and_plain("sensitive");
    let hidden = thumbs::shown(&h.state, &s).expect("a card");
    assert_eq!((hidden.face.clone(), hidden.title.clone()), (Face::Hidden, None));
    let clear = thumbs::shown(&h.state, &p).expect("a card");
    assert!(matches!(clear.face, Face::Pixels { .. }));
    assert_eq!(clear.title.as_deref(), Some("public notes"));
    // The ratchet: lifting the mark takes effect on the very next decision,
    // and setting it back hides the window again at once.
    h.state.sensitive.clear();
    assert!(matches!(
        thumbs::shown(&h.state, &s).unwrap().face,
        Face::Pixels { .. }
    ));
    secret(&mut h, &s);
    assert_eq!(thumbs::shown(&h.state, &s).unwrap().face, Face::Hidden);
}

#[test]
fn typing_never_finds_a_redacted_window_by_its_title() {
    let (mut h, _c, s, p) = secret_and_plain("sensitive");
    assert!(super::open(&mut h.state));
    assert_eq!(h.state.overview.cards().len(), 2);
    h.state.inject_text("ledger").unwrap();
    assert!(
        h.state.overview.cards().is_empty(),
        "a title that may not be shown must not be matched either"
    );
    for _ in 0..6 {
        h.state.inject_key("BackSpace", &[]).unwrap();
    }
    h.state.inject_text("public").unwrap();
    assert_eq!(h.state.overview.cards(), [p]);
    let _ = s;
}

#[test]
fn a_window_that_becomes_secret_with_the_overview_open_is_hidden_at_once() {
    let (mut h, mut c) = de();
    let (_, w) = win(&mut h, &mut c, "payroll");
    assert!(super::open(&mut h.state));
    assert!(matches!(frame_of(&h).view.cards[0].body, Body::Pixels { .. }));
    secret(&mut h, &w);
    let f = frame_of(&h);
    assert_eq!(f.view.cards[0].body, Body::Hidden);
    assert_eq!(f.view.cards[0].title, None);
    assert!(!drawn_text(&f).contains("payroll"));
}
