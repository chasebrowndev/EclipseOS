// SPDX-License-Identifier: AGPL-3.0-only
//! The desktop grid, the window menu on a minimized window and scripted drags
//! (M6): the control-socket methods are driven through `ipc::methods::dispatch`
//! and the drag through the same `pointer_moved` / `pointer_button` entry
//! points a device uses.

use serde_json::{json, Value};
use smithay::desktop::Window;

use super::{
    de_chrome_tests::{de_window, flip, float_rect, on_bar_at, LEFT},
    desktop::Grid,
    focus::state_tests::{
        client::{Client, Toplevel},
        harness, Harness,
    },
    maximize, quicktile,
    winmenu::{Item, Page},
};
use crate::{config::Mode, ipc::methods::dispatch, shell};

/// `(code, message)` for a refusal: `RpcError` has no `Debug`.
type Refused = (i32, String);

fn call(h: &mut Harness, method: &str, p: Value) -> Result<Value, Refused> {
    dispatch(&mut h.state, 0, method, &p).map_err(|e| (e.code, e.message))
}

fn handle(h: &mut Harness, w: &Window) -> u64 {
    h.state.ipc.handle_for(w)
}

fn active(h: &Harness) -> usize {
    h.state.outputs.focused().expect("output").active + 1
}

fn window_on(h: &mut Harness, c: &mut Client, ws: usize) -> (Toplevel, Window) {
    shell::switch_workspace(&mut h.state, ws);
    de_window(h, c)
}

/// Params for a chip near the bottom edge of the focused output.
fn chip(h: &Harness, id: u64) -> Value {
    let e = h.state.outputs.focused().unwrap();
    let g = h.state.space.output_geometry(&e.output).unwrap();
    json!({"id": id, "x": g.loc.x + 40, "y": g.loc.y + g.size.h - 10})
}

fn open_chip(h: &mut Harness, id: u64) -> Result<Value, Refused> {
    let p = chip(h, id);
    call(h, "open_window_menu", p)
}

fn items(h: &Harness) -> Vec<Item> {
    h.state.winmenu.model().expect("open").items().to_vec()
}

fn enabled(h: &Harness, item: Item) -> bool {
    let m = h.state.winmenu.model().expect("open");
    let i = m.items().iter().position(|x| *x == item).expect("row");
    m.view().rows[i].enabled
}

// ------------------------------------------------------------------ grid

#[test]
fn the_grid_reports_shape_active_desktop_and_counts_in_every_mode() {
    let mut h = harness();
    let mut c = Client::connect(&mut h);
    let (_, a) = window_on(&mut h, &mut c, 3);
    let (_, _b) = de_window(&mut h, &mut c);
    shell::minimize_window(&mut h.state, &a);
    for mode in [Mode::Wm, Mode::Hybrid, Mode::De] {
        flip(&mut h, mode);
        let v = call(&mut h, "get_desktop_grid", json!({})).expect("a read works in every mode");
        assert_eq!(
            (v["rows"].clone(), v["columns"].clone()),
            (json!(2), json!(5)),
            "{mode:?}"
        );
        assert_eq!(v["directional"], json!(mode == Mode::De), "{mode:?}");
        assert_eq!(v["outputs"][0]["active"], json!(3));
        let d = v["desktops"].as_array().expect("list");
        assert_eq!(d.len(), shell::workspace::COUNT, "only the human workspaces");
        assert_eq!(
            d[2],
            json!({"index": 3, "occupied": true, "windows": 2}),
            "minimized counts"
        );
        assert_eq!(d[0], json!({"index": 1, "occupied": false, "windows": 0}));
        // Counts only: nothing about the windows themselves.
        assert!(!v.to_string().contains("title") && !v.to_string().contains("app_id"));
    }
    assert!(call(&mut h, "get_desktop_grid", json!({"x": 1})).is_err());
}

#[test]
fn the_row_count_reshapes_the_grid() {
    let mut h = harness();
    let mut next = h.state.config.clone();
    next.general.desktop_rows = 3;
    crate::config::apply_loaded(&mut h.state, next);
    let v = call(&mut h, "get_desktop_grid", json!({})).unwrap();
    assert_eq!((v["rows"].clone(), v["columns"].clone()), (json!(3), json!(4)));
    assert_eq!(Grid::of(&h.state), Grid::new(3));
}

#[test]
fn switching_by_direction_walks_the_grid_wraps_and_is_de_only() {
    let mut h = harness();
    flip(&mut h, Mode::De);
    let go = |h: &mut Harness, d: &str| call(h, "switch_desktop_direction", json!({"direction": d}));
    assert_eq!(active(&h), 1);
    let r = go(&mut h, "right").unwrap();
    assert_eq!((active(&h), r["changed"].clone()), (2, json!(true)));
    assert_eq!(go(&mut h, "down").map(|_| active(&h)).unwrap(), 7);
    assert_eq!(go(&mut h, "up").map(|_| active(&h)).unwrap(), 2);
    go(&mut h, "left").unwrap();
    assert_eq!(
        go(&mut h, "left").map(|_| active(&h)).unwrap(),
        5,
        "wraps within the row"
    );
    // Without wrap an edge is a quiet no-op, not an error.
    let mut next = h.state.config.clone();
    next.general.desktop_wrap = false;
    crate::config::apply_loaded(&mut h.state, next);
    shell::switch_workspace(&mut h.state, 1);
    let r = go(&mut h, "left").unwrap();
    assert_eq!((active(&h), r["changed"].clone()), (1, json!(false)));
    assert!(go(&mut h, "sideways").is_err());
    assert!(call(&mut h, "switch_desktop_direction", json!({})).is_err());

    for mode in [Mode::Wm, Mode::Hybrid] {
        flip(&mut h, mode);
        shell::switch_workspace(&mut h.state, 4);
        let e = go(&mut h, "right").expect_err("de only");
        assert!(e.1.contains("de only"), "{}", e.1);
        assert_eq!(active(&h), 4, "{mode:?} changed nothing");
    }
}

#[test]
fn a_directional_switch_goes_the_workspace_way_sticky_windows_included() {
    let mut h = harness();
    let mut c = Client::connect(&mut h);
    let (_, home) = de_window(&mut h, &mut c);
    let (_, sticky) = de_window(&mut h, &mut c);
    crate::shell::winops::set_sticky(&mut h.state, &sticky, true);
    call(&mut h, "switch_desktop_direction", json!({"direction": "right"})).unwrap();
    assert_eq!(active(&h), 2);
    assert!(
        h.state.space.elements().any(|e| e == &sticky),
        "sticky came along"
    );
    assert!(!h.state.space.elements().any(|e| e == &home));
    assert_eq!(
        h.state.focus.as_ref(),
        Some(&sticky),
        "focus lands on what is shown"
    );
    call(&mut h, "switch_desktop_direction", json!({"direction": "left"})).unwrap();
    assert!(h.state.space.elements().any(|e| e == &home) && h.state.space.elements().any(|e| e == &sticky));
}

#[test]
fn the_bind_actions_switch_and_carry_the_focused_window_under_de_only() {
    use crate::input::Direction;
    let mut h = harness();
    let mut c = Client::connect(&mut h);
    let (_, w) = de_window(&mut h, &mut c);
    assert!(shell::desktop::switch(&mut h.state, Direction::Down));
    assert_eq!(active(&h), 6);
    assert!(shell::desktop::switch(&mut h.state, Direction::Up));
    assert!(shell::desktop::move_with(&mut h.state, Direction::Right));
    assert_eq!(active(&h), 2, "it goes there with the window");
    assert!(h.state.space.elements().any(|e| e == &w));
    assert_eq!(h.state.focus.as_ref(), Some(&w));
    // Even with follow-window-to-workspace off, the chord names both.
    let mut next = h.state.config.clone();
    next.general.follow_window_to_workspace = false;
    crate::config::apply_loaded(&mut h.state, next);
    assert!(shell::desktop::move_with(&mut h.state, Direction::Right));
    assert_eq!(active(&h), 3);
    assert!(h.state.space.elements().any(|e| e == &w));
    flip(&mut h, Mode::Wm);
    assert!(!shell::desktop::switch(&mut h.state, Direction::Right));
    assert!(!shell::desktop::move_with(&mut h.state, Direction::Right));
    assert_eq!(active(&h), 3);
}

// ----------------------------------------------------------- window menu

#[test]
fn open_window_menu_clamps_to_the_output_and_refuses_what_it_should() {
    let mut h = harness();
    let mut c = Client::connect(&mut h);
    let (_, w) = de_window(&mut h, &mut c);
    let id = handle(&mut h, &w);
    let out = {
        let e = h.state.outputs.focused().unwrap();
        h.state.space.output_geometry(&e.output).unwrap()
    };
    assert!(call(&mut h, "open_window_menu", json!({"id": 9999, "x": 5, "y": 5})).is_err());
    assert!(call(&mut h, "open_window_menu", json!({"id": id, "x": 5})).is_err());
    assert!(!h.state.winmenu.is_open());
    // A wild anchor lands on the output, and the panel stays inside it.
    let r = call(
        &mut h,
        "open_window_menu",
        json!({"id": id, "x": 99999, "y": -4000}),
    )
    .unwrap();
    assert_eq!(r["x"], json!(out.loc.x + out.size.w - 1));
    assert_eq!(r["y"], json!(out.loc.y));
    assert!(h.state.winmenu.is_open() && h.state.winmenu.window() == Some(&w));
    let panel = h.state.winmenu.view().unwrap().rect();
    assert!(out.contains_rect(panel), "{panel:?} outside {out:?}");
    // Locked: refused, and the menu already open is not a way around it.
    h.state.winmenu.close();
    h.state.lock.locked = true;
    let e = call(&mut h, "open_window_menu", json!({"id": id, "x": 50, "y": 50}))
        .err()
        .unwrap();
    assert_eq!(e.0, ec_abyss_wire::rpc::DENIED);
    assert!(!h.state.winmenu.is_open());
    h.state.lock.locked = false;
    // Not de: refused, nothing opens.
    for mode in [Mode::Wm, Mode::Hybrid] {
        flip(&mut h, mode);
        let e = call(&mut h, "open_window_menu", json!({"id": id, "x": 50, "y": 50}))
            .err()
            .unwrap();
        assert!(e.1.contains("de only"), "{mode:?}");
        assert!(!h.state.winmenu.is_open());
    }
}

#[test]
fn a_minimized_window_has_a_menu_whose_rows_make_sense() {
    let mut h = harness();
    let mut c = Client::connect(&mut h);
    let (_, w) = de_window(&mut h, &mut c);
    let id = handle(&mut h, &w);
    shell::minimize_window(&mut h.state, &w);
    assert!(shell::is_minimized(&h.state, &w));
    open_chip(&mut h, id).expect("a chip can open it");
    assert!(h.state.winmenu.is_open());
    let m = h.state.winmenu.model().unwrap();
    assert_eq!(m.view().rows[0].label, "Unminimize");
    assert!(!enabled(&h, Item::Maximize) && !enabled(&h, Item::Shade));
    for item in [
        Item::KeepAbove,
        Item::KeepBelow,
        Item::Sticky,
        Item::MoveTo,
        Item::Close,
    ] {
        assert!(enabled(&h, item), "{item:?}");
    }
    // Opening did not bring it back or focus it.
    assert!(shell::is_minimized(&h.state, &w) && h.state.focus.as_ref() != Some(&w));
    // A layout pass keeps the menu: minimized is the state it opened in.
    shell::arrange(&mut h.state);
    assert!(h.state.winmenu.is_open());
}

#[test]
fn the_rows_on_a_minimized_window_do_what_they_say() {
    let mut h = harness();
    let mut c = Client::connect(&mut h);
    let (_, w) = de_window(&mut h, &mut c);
    let id = handle(&mut h, &w);
    shell::minimize_window(&mut h.state, &w);
    let open = |h: &mut Harness| {
        open_chip(h, id).expect("opens");
    };
    let press = |h: &mut Harness, item: Item| {
        let m = h.state.winmenu.model().unwrap();
        let i = m.items().iter().position(|x| *x == item).unwrap();
        let r = m.view().row_rect_local(i);
        let o = m.view().origin;
        h.state.pointer_moved(
            super::de_chrome_tests::pt(f64::from(o.x + 12), f64::from(o.y + r.loc.y + 3)),
            0,
        );
        h.state
            .pointer_button(LEFT, smithay::backend::input::ButtonState::Pressed, 0);
        h.state
            .pointer_button(LEFT, smithay::backend::input::ButtonState::Released, 0);
    };
    // Keep above and on all desktops are remembered for the way back.
    open(&mut h);
    press(&mut h, Item::KeepAbove);
    assert!(h.state.winops.band(&w) == crate::shell::winops::Band::Above);
    assert!(shell::is_minimized(&h.state, &w), "still away");
    open(&mut h);
    press(&mut h, Item::Sticky);
    assert!(h.state.winops.is_sticky(&w) && shell::is_minimized(&h.state, &w));
    // Move to desktop 5 keeps it minimized, in exactly one place, and ends the
    // stickiness like it does for a shown window.
    open(&mut h);
    press(&mut h, Item::MoveTo);
    assert_eq!(h.state.winmenu.model().unwrap().page(), Page::Desktops);
    press(&mut h, Item::Desktop(4));
    assert!(!h.state.winmenu.is_open());
    assert!(shell::is_minimized(&h.state, &w));
    let e = h.state.outputs.focused().unwrap();
    let homes: Vec<usize> = (0..shell::workspace::COUNT)
        .filter(|i| e.workspaces[*i].holds(&w))
        .collect();
    assert_eq!(homes, [4], "one holder, on desktop 5");
    assert!(!h.state.space.elements().any(|x| x == &w));
    // Unminimize brings it back onto the desktop it was sent to.
    open(&mut h);
    press(&mut h, Item::Minimize);
    assert!(!shell::is_minimized(&h.state, &w));
    // Restoring onto another desktop does not steal the screen.
    assert_eq!(active(&h), 1);
    assert!(h.state.outputs.focused().unwrap().workspaces[4].holds_live(&w));
    shell::switch_workspace(&mut h.state, 5);
    assert!(h.state.space.elements().any(|x| x == &w));
}

#[test]
fn close_on_a_minimized_window_asks_it_to_close() {
    let mut h = harness();
    let mut c = Client::connect(&mut h);
    let (t, w) = de_window(&mut h, &mut c);
    let id = handle(&mut h, &w);
    shell::minimize_window(&mut h.state, &w);
    open_chip(&mut h, id).unwrap();
    assert!(items(&h).contains(&Item::Close));
    h.state.winmenu.close();
    shell::close_window(&mut h.state, &w);
    c.pump(&mut h);
    assert!(c.closed(&t.toplevel));
}

#[test]
fn restoring_the_window_another_way_closes_its_menu() {
    let mut h = harness();
    let mut c = Client::connect(&mut h);
    let (_, w) = de_window(&mut h, &mut c);
    let id = handle(&mut h, &w);
    shell::minimize_window(&mut h.state, &w);
    open_chip(&mut h, id).unwrap();
    shell::unminimize_window(&mut h.state, &w);
    assert!(!h.state.winmenu.is_open());
}

#[test]
fn a_menu_asked_for_a_minimized_window_from_another_output_is_refused() {
    let mut h = harness();
    let mut c = Client::connect(&mut h);
    let (_, w) = de_window(&mut h, &mut c);
    let id = handle(&mut h, &w);
    shell::minimize_window(&mut h.state, &w);
    let out = {
        let e = h.state.outputs.focused().unwrap();
        h.state.space.output_geometry(&e.output).unwrap()
    };
    let e = call(
        &mut h,
        "open_window_menu",
        json!({"id": id, "x": out.loc.x + out.size.w + 500, "y": 10}),
    )
    .expect_err("off its output");
    assert_eq!(e.0, ec_abyss_wire::rpc::INVALID_PARAMS);
    assert!(!h.state.winmenu.is_open());
}

// ------------------------------------------------------------------ drag

fn drag(h: &mut Harness, p: Value) -> Result<Value, Refused> {
    call(h, "drag_at", p)
}

#[test]
fn a_scripted_drag_on_a_titlebar_moves_the_window_and_leaves_no_state() {
    let mut h = harness();
    let mut c = Client::connect(&mut h);
    let (_, w) = de_window(&mut h, &mut c);
    h.state.bar_ui.forget(&w);
    let before = shell::stored_content(&h.state, &w).expect("floating");
    let p = on_bar_at(&h, &w, 0.3);
    let r = drag(
        &mut h,
        json!({"x": p.x, "y": p.y, "to_x": p.x + 120.0, "to_y": p.y + 70.0}),
    )
    .unwrap();
    assert_eq!(r["holding"], json!(false));
    let now = shell::stored_content(&h.state, &w).expect("floating");
    assert_eq!(now.loc, before.loc + smithay::utils::Point::from((120, 70)));
    assert!(!h.state.pointer_grab_active, "the release ended the grab");
    assert!(h.state.drag_hold.is_none());
    assert!(h.state.bar_ui.swallowed_is_clear());
}

#[test]
fn a_scripted_drag_to_the_left_edge_tiles_and_out_again_restores() {
    let mut h = harness();
    let mut c = Client::connect(&mut h);
    let (t, w) = de_window(&mut h, &mut c);
    h.state.bar_ui.forget(&w);
    let free = float_rect(&h, &w);
    let out = {
        let e = h.state.outputs.focused().unwrap();
        h.state.space.output_geometry(&e.output).unwrap()
    };
    let p = on_bar_at(&h, &w, 0.3);
    drag(
        &mut h,
        json!({"x": p.x, "y": p.y, "to_x": out.loc.x, "to_y": out.loc.y + out.size.h / 2}),
    )
    .unwrap();
    assert_eq!(quicktile::zone_of(&h.state, &w), Some(quicktile::Zone::Left));
    assert!(h.state.tile_preview.is_none());
    c.pump(&mut h);
    super::de_chrome_tests::settle(&mut h, &mut c, &t);
    h.state.bar_ui.forget(&w);
    let p = on_bar_at(&h, &w, 0.4);
    drag(
        &mut h,
        json!({"x": p.x, "y": p.y, "to_x": p.x + 300.0, "to_y": p.y + 200.0}),
    )
    .unwrap();
    assert_eq!(quicktile::zone_of(&h.state, &w), None);
    assert_eq!(float_rect(&h, &w).size, free.size, "the old size is back");
    let top = on_bar_at(&h, &w, 0.4);
    drag(
        &mut h,
        json!({"x": top.x, "y": top.y, "to_x": out.loc.x + 400, "to_y": out.loc.y}),
    )
    .unwrap();
    assert!(maximize::is_maximized(&h.state, &w));
}

#[test]
fn hold_ms_keeps_the_button_down_until_it_is_released_or_cancelled() {
    let mut h = harness();
    let mut c = Client::connect(&mut h);
    let (_, w) = de_window(&mut h, &mut c);
    h.state.bar_ui.forget(&w);
    let p = on_bar_at(&h, &w, 0.3);
    let go = |h: &mut Harness| {
        drag(
            h,
            json!({"x": p.x, "y": p.y, "to_x": p.x + 90.0, "to_y": p.y + 60.0, "hold_ms": 5000}),
        )
    };
    let r = go(&mut h).unwrap();
    assert_eq!(r["holding"], json!(true));
    assert!(h.state.pointer_grab_active, "still dragging mid-hold");
    assert!(h.state.drag_hold.is_some());
    // One hold at a time; a second call is refused and touches nothing.
    assert!(go(&mut h).is_err());
    assert!(h.state.pointer_grab_active);
    // An unrelated window going away leaves the hold alone.
    let (_, other) = de_window(&mut h, &mut c);
    shell::unmap_window(&mut h.state, &other);
    assert!(h.state.drag_hold.is_some() && h.state.pointer_grab_active);
    // The dragged window going away lets go.
    shell::unmap_window(&mut h.state, &w);
    assert!(
        h.state.drag_hold.is_none() && !h.state.pointer_grab_active,
        "released cleanly"
    );
    // So does the lock.
    let (_, w) = de_window(&mut h, &mut c);
    h.state.bar_ui.forget(&w);
    let p = on_bar_at(&h, &w, 0.3);
    drag(
        &mut h,
        json!({"x": p.x, "y": p.y, "to_x": p.x + 30.0, "to_y": p.y + 30.0, "hold_ms": 5000}),
    )
    .unwrap();
    assert!(h.state.drag_hold.is_some());
    h.state.engage_lock();
    assert!(h.state.drag_hold.is_none(), "the lock ended the hold");
    // The timer's own path releases too.
    h.state.lock.locked = false;
    h.state.bar_ui.forget(&w);
    let p = on_bar_at(&h, &w, 0.3);
    drag(
        &mut h,
        json!({"x": p.x, "y": p.y, "to_x": p.x + 30.0, "to_y": p.y + 30.0, "hold_ms": 5000}),
    )
    .unwrap();
    h.state.finish_drag_hold();
    assert!(h.state.drag_hold.is_none() && !h.state.pointer_grab_active);
}

#[test]
fn drag_parameters_are_validated_before_anything_moves() {
    let mut h = harness();
    let at = h.state.pointer_location;
    for bad in [
        json!({"x": 1, "y": 1, "to_x": 2}),
        json!({"x": 1, "y": 1, "to_x": 2, "to_y": 2, "button": "back"}),
        json!({"x": 1, "y": 1, "to_x": 2, "to_y": 2, "steps": 0}),
        json!({"x": 1, "y": 1, "to_x": 2, "to_y": 2, "steps": 201}),
        json!({"x": 1, "y": 1, "to_x": 2, "to_y": 2, "hold_ms": 30001}),
        json!({"x": 1, "y": 1, "to_x": 2, "to_y": 2, "modifiers": ["hyper"]}),
        json!({"x": 1, "y": 1, "to_x": 2, "to_y": 2, "extra": 1}),
    ] {
        assert!(drag(&mut h, bad.clone()).is_err(), "{bad}");
    }
    assert_eq!(
        h.state.pointer_location, at,
        "a refused drag never moved the pointer"
    );
    assert!(h.state.drag_hold.is_none());
}

#[test]
fn a_scripted_drag_is_tagged_scripted_and_a_protected_surface_never_takes_it() {
    // The tag is set around the whole call, exactly as for `click_at`.
    let src = include_str!("../ipc/methods.rs");
    assert!(src.contains("\"drag_at\" => scripted(state, |s| drag_at(s, params))"));
    assert!(!crate::input::Origin::Scripted.reaches_protected());
}

#[test]
fn a_cancelled_hold_leaves_no_timer_to_release_the_next_one() {
    let mut h = harness();
    let mut c = Client::connect(&mut h);
    let (_, w) = de_window(&mut h, &mut c);
    h.state.bar_ui.forget(&w);
    let p = on_bar_at(&h, &w, 0.3);
    let hold = |h: &mut Harness, ms: u64| {
        drag(
            h,
            json!({"x": p.x, "y": p.y, "to_x": p.x + 20.0, "to_y": p.y + 20.0, "hold_ms": ms}),
        )
        .unwrap()
    };
    // A short hold, cancelled; then a long one that its timer must not touch.
    hold(&mut h, 40);
    h.state.cancel_drag_hold();
    h.state.bar_ui.forget(&w);
    hold(&mut h, 5000);
    std::thread::sleep(std::time::Duration::from_millis(120));
    h.dispatch();
    assert!(h.state.drag_hold.is_some(), "the first hold's timer was removed");
    h.state.cancel_drag_hold();
    // And a real expiry releases.
    h.state.bar_ui.forget(&w);
    hold(&mut h, 30);
    std::thread::sleep(std::time::Duration::from_millis(100));
    h.dispatch();
    assert!(h.state.drag_hold.is_none() && !h.state.pointer_grab_active);
}

#[test]
fn a_scripted_drag_is_refused_whole_under_the_lock() {
    let mut h = harness();
    h.state.lock.locked = true;
    let at = h.state.pointer_location;
    let r = drag(
        &mut h,
        json!({"x": 1, "y": 1, "to_x": 50, "to_y": 50, "modifiers": ["ctrl"]}),
    );
    assert!(r.is_err());
    assert_eq!(h.state.pointer_location, at);
    assert!(h.state.drag_hold.is_none());
}
