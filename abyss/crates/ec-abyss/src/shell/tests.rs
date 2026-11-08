// SPDX-License-Identifier: AGPL-3.0-only
//! Unit and end-to-end tests for the shell (COMP-05).

use super::toplevel::{MapTransition, Remembered, ToplevelMap};
use super::*;

#[test]
fn tiled_ignores_min_floating_honours_it() {
    let tile = Size::<i32, Logical>::from((720, 800));
    let min = Size::from((940, 500));
    let none = Size::from((0, 0));
    assert_eq!(clamp_to(tile, min, none, true), tile);
    assert_eq!(clamp_to(tile, min, none, false), Size::from((940, 800)));
    // max still wins for tiled windows.
    let max = Size::from((600, 0));
    assert_eq!(clamp_to(tile, min, max, true), Size::from((600, 800)));
}

#[test]
fn null_buffer_unmaps_then_next_commit_remaps() {
    let mut m = ToplevelMap::default();
    // Initial commit (no buffer), then the map commit.
    assert_eq!(m.step(false), None);
    assert_eq!(m.step(true), None);
    assert_eq!(m.step(true), None);
    // Null-buffer commit unmaps, exactly once.
    assert_eq!(m.step(false), Some(MapTransition::Unmapped));
    // The client's fresh initial commit re-places it, exactly once.
    assert_eq!(m.step(false), Some(MapTransition::Remap(None)));
    assert_eq!(m.step(false), None);
    assert_eq!(m.step(true), None);
    // And it can unmap again.
    assert_eq!(m.step(false), Some(MapTransition::Unmapped));
}

#[test]
fn remap_returns_the_remembered_placement_once() {
    let r = Remembered {
        output: 7,
        workspace: 2,
        rect: Rectangle::new(Point::from((196, 45)), Size::from((104, 104))),
    };
    let mut m = ToplevelMap::default();
    assert_eq!(m.step(true), None);
    assert_eq!(m.step(false), Some(MapTransition::Unmapped));
    // `unmap_toplevel` records where the window floated...
    m.placement = Some(r);
    // ...and the remap commit, buffer attached or not, hands it back.
    assert_eq!(m.step(true), Some(MapTransition::Remap(Some(r))));
    assert_eq!(m.placement, None);
    // A later unmap that recorded nothing (the window tiled) does not
    // resurrect the old rectangle.
    assert_eq!(m.step(false), Some(MapTransition::Unmapped));
    assert_eq!(m.step(false), Some(MapTransition::Remap(None)));
}

#[test]
fn never_mapped_toplevel_is_not_unmapped() {
    let mut m = ToplevelMap::default();
    for _ in 0..3 {
        assert_eq!(m.step(false), None);
    }
}

fn origin(mode: FloatingPlacement, pointer: (f64, f64), already: usize) -> (i32, i32) {
    let area = Rectangle::new(Point::from((100, 50)), Size::from((800, 600)));
    let size = Size::from((400, 300));
    let p = floating_origin(mode, area, size, Point::from(pointer), already);
    (p.x, p.y)
}

#[test]
fn centered_sits_in_the_middle_of_the_tiling_area() {
    // Offset by the area's own origin, so it centres on the output the
    // window belongs to and not on the compositor's global 0,0.
    assert_eq!(
        origin(FloatingPlacement::Centered, (0.0, 0.0), 0),
        (100 + 200, 50 + 150)
    );
}

#[test]
fn pointer_clamps_so_the_window_never_opens_partly_offscreen() {
    assert_eq!(origin(FloatingPlacement::Pointer, (300.0, 200.0), 0), (300, 200));
    // Past the far edge: pulled back so the whole window fits.
    assert_eq!(
        origin(FloatingPlacement::Pointer, (10_000.0, 10_000.0), 0),
        (100 + 800 - 400, 50 + 600 - 300)
    );
    // Before the near edge (pointer on another output): pushed in.
    assert_eq!(origin(FloatingPlacement::Pointer, (-500.0, -500.0), 0), (100, 50));
}

#[test]
fn cascade_steps_then_wraps_instead_of_piling_up() {
    assert_eq!(origin(FloatingPlacement::Cascade, (0.0, 0.0), 0), (100, 50));
    assert_eq!(origin(FloatingPlacement::Cascade, (0.0, 0.0), 1), (132, 82));
    // Room is (min(400, 300) / 32) = 9 steps, so the tenth window restarts
    // the diagonal rather than walking off the bottom-right.
    assert_eq!(origin(FloatingPlacement::Cascade, (0.0, 0.0), 9), (100, 50));
}

fn rr(x: i32, y: i32, w: i32, h: i32) -> Rectangle<i32, Logical> {
    Rectangle::new(Point::from((x, y)), Size::from((w, h)))
}

/// Two side-by-side tiles 1|2 on a 1000x500 area at the origin.
type Tiles = Vec<(u32, Rectangle<i32, Logical>)>;

fn two() -> (layout::Tree<u32>, Tiles) {
    let mut t = layout::Tree::new();
    t.insert_at(1, layout::Target::Root, layout::Side::Right, 1.0);
    t.insert_at(2, layout::Target::Leaf(&1), layout::Side::Right, 1.0);
    let tiles = t.radiant(rr(0, 0, 1000, 500), 0, 0);
    (t, tiles)
}

fn cand(key: DropKey, x: f64, y: f64) -> DropCandidate {
    DropCandidate {
        key,
        aim: Point::from((x, y)),
        ghost: rr(0, 0, 1, 1),
    }
}

#[test]
fn aim_drop_takes_the_nearest_candidate_and_stays_off_the_output() {
    let b = rr(0, 0, 1000, 500);
    let top = DropKey::Tile(0, layout::Zone::Top);
    let c = [cand(DropKey::Stay, 250.0, 250.0), cand(top, 750.0, 125.0)];
    assert_eq!(aim_drop(b, &c, DropKey::Stay, Point::from((700.0, 150.0))), top);
    assert_eq!(aim_drop(b, &c, top, Point::from((300.0, 240.0))), DropKey::Stay);
    assert_eq!(aim_drop(b, &c, top, Point::from((1500.0, 150.0))), DropKey::Stay);
    assert_eq!(aim_drop(b, &[], top, Point::from((10.0, 10.0))), DropKey::Stay);
}

#[test]
fn aim_drop_hysteresis_keeps_the_current_key_near_a_tie() {
    let b = rr(0, 0, 1000, 500);
    let (l, r) = (
        DropKey::Tile(0, layout::Zone::Left),
        DropKey::Tile(0, layout::Zone::Right),
    );
    let c = [cand(l, 400.0, 250.0), cand(r, 600.0, 250.0)];
    // 10 px nearer to `r` is inside the hysteresis: `l` holds.
    assert_eq!(aim_drop(b, &c, l, Point::from((505.0, 250.0))), l);
    // Well past it, `r` takes over, and then holds the same way back.
    assert_eq!(aim_drop(b, &c, l, Point::from((520.0, 250.0))), r);
    assert_eq!(aim_drop(b, &c, r, Point::from((495.0, 250.0))), r);
}

#[test]
fn a_window_over_its_own_tile_stays_and_over_b_lands_where_it_points() {
    let (t, tiles) = two();
    let area = rr(0, 0, 1000, 500);
    let c = drop_candidates(&t, &1, &tiles, area, 0, 0, 40);
    assert_eq!(c[0].key, DropKey::Stay, "home is first");
    assert_eq!(c[0].ghost, rr(0, 0, 500, 500));
    // Barely moved: home.
    assert_eq!(
        aim_drop(area, &c, DropKey::Stay, Point::from((270.0, 240.0))),
        DropKey::Stay
    );
    // Centred in B's upper area: a candidate whose landing contains it.
    let at = Point::from((750.0, 120.0));
    let key = aim_drop(area, &c, DropKey::Stay, at);
    let hit = c.iter().find(|x| x.key == key).expect("a candidate");
    assert!(
        hit.ghost.to_f64().contains(at),
        "{key:?} lands at {:?}",
        hit.ghost
    );
    // With A out, B's top would be the full-width top half, centred far
    // from (750, 120); B's right half (A and B swap sides) is nearer.
    assert_eq!(key, DropKey::Tile(1, layout::Zone::Right));
}

#[test]
fn apply_drop_side_centre_band_and_stale_target() {
    let (t, tiles) = two();
    let area = rr(0, 0, 1000, 500);

    // Bottom of 2: 2 over 1 on the right half.
    let mut d = t.clone();
    assert!(apply_drop(
        &mut d,
        &1,
        DropKey::Tile(1, layout::Zone::Bottom),
        &tiles
    ));
    let g = d.radiant(area, 0, 0);
    assert_eq!(g, vec![(2, rr(0, 0, 1000, 250)), (1, rr(0, 250, 1000, 250))]);

    // Centre swaps.
    let mut d = t.clone();
    assert!(apply_drop(
        &mut d,
        &1,
        DropKey::Tile(1, layout::Zone::Center),
        &tiles
    ));
    assert_eq!(d.windows(), vec![2, 1]);

    // Top band: a full-width row above the rest. With 2 over 3 left
    // behind, the root is already a column, so 1 joins it as a third row.
    let mut d = t.clone();
    d.insert_at(3, layout::Target::Leaf(&2), layout::Side::Bottom, 1.0);
    assert!(apply_drop(&mut d, &1, DropKey::Band(layout::Side::Top), &tiles));
    assert_eq!(d.windows(), vec![1, 2, 3]);
    assert_eq!(rect(&d, 1, area), rr(0, 0, 1000, 166));
    assert_eq!(rect(&d, 3, area).size.w, 1000);

    // A floating window (not in the tree) on a centre splits the target.
    let mut d = t.clone();
    assert!(apply_drop(
        &mut d,
        &9,
        DropKey::Tile(1, layout::Zone::Center),
        &tiles
    ));
    assert_eq!(d.windows(), vec![1, 2, 9]);

    // Target gone since the drag started: nothing changes.
    let mut d = t.clone();
    d.remove(&2);
    let before = d.windows();
    assert!(!apply_drop(
        &mut d,
        &1,
        DropKey::Tile(1, layout::Zone::Left),
        &tiles
    ));
    assert_eq!(d.windows(), before);
    assert!(!apply_drop(&mut d, &1, DropKey::Stay, &tiles));
}

#[test]
fn landing_output_is_the_other_output_under_the_window_centre() {
    // Two outputs side by side, the second one at x = 1000.
    let outs = [(1, rr(0, 0, 1000, 800)), (2, rr(1000, 0, 1920, 1080))];
    let p = |x: f64, y: f64| Point::from((x, y));
    // Centre over the other output lands there.
    assert_eq!(landing_output(&outs, 1, p(1500.0, 300.0)), Some(2));
    assert_eq!(landing_output(&outs, 2, p(200.0, 300.0)), Some(1));
    // Centre still on the source output stays put.
    assert_eq!(landing_output(&outs, 1, p(900.0, 300.0)), None);
    // Centre on no output at all stays put.
    assert_eq!(landing_output(&outs, 1, p(-50.0, 300.0)), None);
    assert_eq!(landing_output(&[], 1, p(0.0, 0.0)), None);
}

#[test]
fn drop_candidates_precompute_exactly_what_a_drop_would_do() {
    let (t, tiles) = two();
    let area = rr(0, 0, 1000, 500);
    let c = drop_candidates(&t, &1, &tiles, area, 0, 0, 40);
    // Home + 4 bands + 5 zones of tile 2; tile 1 is the dragged window's own.
    assert_eq!(c.len(), 10);
    assert!(c.iter().all(|x| !matches!(x.key, DropKey::Tile(0, _))));
    for x in c.iter().filter(|x| x.key != DropKey::Stay) {
        let mut d = t.clone();
        assert!(apply_drop(&mut d, &1, x.key, &tiles));
        assert_eq!(rect(&d, 1, area), x.ghost, "{:?}", x.key);
    }
    let swap = c.iter().find(|x| x.key == DropKey::Tile(1, layout::Zone::Center));
    assert_eq!(swap.map(|x| x.ghost), Some(rr(500, 0, 500, 500)));
    // A band is aimed at by its edge strip, not its landing rectangle.
    let left = c.iter().find(|x| x.key == DropKey::Band(layout::Side::Left));
    assert_eq!(left.map(|x| x.aim), Some(Point::from((20.0, 250.0))));
    // Band 0: no band targets at all.
    assert_eq!(drop_candidates(&t, &1, &tiles, area, 0, 0, 0).len(), 6);
}

fn rect(t: &layout::Tree<u32>, w: u32, area: Rectangle<i32, Logical>) -> Rectangle<i32, Logical> {
    t.radiant(area, 0, 0)
        .into_iter()
        .find(|(x, _)| *x == w)
        .unwrap()
        .1
}

/// Placement waits for the initial commit (COMP-05 §3/§4), driven end to end
/// by a real client: the bug was a toplevel tiled at `get_toplevel` time, which
/// squeezed its neighbour for one configure before rules moved it off.
mod initial_commit_placement {
    use super::*;
    use crate::shell::focus::state_tests::{
        client::{Client, Toplevel},
        harness, Harness,
    };

    fn window_of(h: &Harness, t: &Toplevel) -> Option<Window> {
        use wayland_client::Proxy;
        let id = t.surface.id().protocol_id();
        owned_windows(&h.state).into_iter().find(|w| {
            window_surface(w)
                .is_some_and(|s| smithay::reexports::wayland_server::Resource::id(&s).protocol_id() == id)
        })
    }

    fn floats(h: &Harness, window: &Window) -> bool {
        h.state.outputs.iter().any(|e| {
            e.workspaces
                .iter()
                .any(|ws| ws.floating.iter().any(|f| &f.window == window))
        })
    }

    /// A tiled neighbour, mapped and settled; returns it and its tile size.
    fn neighbour(h: &mut Harness, c: &mut Client) -> (Toplevel, (i32, i32)) {
        let n = c.create_toplevel(h);
        c.commit(h, &n.surface);
        c.attach(h, &n.surface);
        let size = *c
            .configured_sizes(&n.toplevel)
            .last()
            .expect("neighbour configured");
        let window = window_of(h, &n).expect("neighbour placed");
        assert!(!floats(h, &window), "the neighbour tiles");
        (n, size)
    }

    /// The shipped rules, exactly as `/etc/eclipse/abyss.kdl` states them.
    fn shipped_rules() -> Vec<crate::config::WindowRule> {
        let path = concat!(env!("CARGO_MANIFEST_DIR"), "/../../../packaging/etc/abyss.kdl");
        let text = std::fs::read_to_string(path).expect("shipped abyss.kdl");
        let errors = crate::config::Config::check_text(
            std::path::Path::new(path),
            crate::config::schema::Owner::Abyss,
            &text,
        );
        assert!(errors.is_empty(), "{errors:?}");
        crate::config::Config::load(Some(std::path::Path::new(path))).window_rules
    }

    #[test]
    fn tiled_windows_carry_the_tiled_states_and_floating_ones_do_not() {
        use smithay::reexports::wayland_protocols::xdg::shell::server::xdg_toplevel::State;
        let mut h = harness();
        let mut c = Client::connect(&mut h);
        let (n, _) = neighbour(&mut h, &mut c);
        let window = window_of(&h, &n).expect("placed");
        let tiled = |w: &Window| {
            w.toplevel().unwrap().with_pending_state(|s| {
                [
                    State::TiledLeft,
                    State::TiledRight,
                    State::TiledTop,
                    State::TiledBottom,
                ]
                .map(|t| s.states.contains(t))
            })
        };
        assert_eq!(tiled(&window), [true; 4], "a tile is tiled on every edge");

        crate::shell::focus::focus_window(&mut h.state, &window);
        crate::shell::toggle_floating(&mut h.state);
        assert!(floats(&h, &window), "toggled to floating");
        assert_eq!(tiled(&window), [false; 4], "floating clears the tiled states");
    }

    #[test]
    fn a_float_rule_on_app_id_never_squeezes_the_neighbour() {
        let mut h = harness();
        h.state.config.window_rules = shipped_rules();
        let mut c = Client::connect(&mut h);
        let (n, tile) = neighbour(&mut h, &mut c);

        // wl-copy's handshake: the role first, its identity after, as separate
        // requests; the compositor sees `get_toplevel` on its own.
        let t = c.create_toplevel(&mut h);
        assert!(
            window_of(&h, &t).is_none(),
            "nothing is placed before the initial commit"
        );
        t.toplevel.set_app_id("io.github.bugaevc.wl-clipboard".into());
        t.toplevel.set_title("wl-clipboard".into());
        c.commit(&mut h, &t.surface);
        let window = window_of(&h, &t).expect("placed at the initial commit");
        assert!(floats(&h, &window), "the shipped rule floats wl-clipboard");
        assert!(
            !c.configured_sizes(&t.toplevel).is_empty(),
            "initial configure sent"
        );
        c.attach(&mut h, &t.surface);
        assert!(floats(&h, &window), "still floating once mapped");

        let sizes = c.configured_sizes(&n.toplevel);
        assert!(
            sizes.iter().all(|&s| s == tile),
            "the neighbour was resized: {sizes:?}"
        );
    }

    /// S-06 §3.3 with no windowrules at all: a window that names itself
    /// only after it is placed gets the category default for its new id, not
    /// the one looked up for the empty id it mapped with.
    #[test]
    fn a_late_app_id_gets_its_category_default_without_rules() {
        let dir = std::env::temp_dir().join(format!("abyss-late-id-{}", std::process::id()));
        let apps = dir.join("applications");
        std::fs::create_dir_all(&apps).expect("apps dir");
        std::fs::write(
            apps.join("firefox.desktop"),
            "[Desktop Entry]\nType=Application\nCategories=Network;WebBrowser;\n",
        )
        .expect("desktop entry");
        rules::TEST_DATA_DIRS.with(|d| *d.borrow_mut() = Some(vec![dir.clone()]));

        let mut h = harness();
        assert!(h.state.config.window_rules.is_empty());
        let mut c = Client::connect(&mut h);
        let t = c.create_toplevel(&mut h);
        c.commit(&mut h, &t.surface);
        c.attach(&mut h, &t.surface);
        let window = window_of(&h, &t).expect("mapped");
        assert!(window.user_data().get::<rules::Placed>().is_some(), "settled");
        assert!(!rules::irreversible_capable_of(&window), "no id, no default");

        t.toplevel.set_app_id("firefox".into());
        c.commit(&mut h, &t.surface);
        assert!(
            rules::irreversible_capable_of(&window),
            "the default follows the late id"
        );

        rules::TEST_DATA_DIRS.with(|d| *d.borrow_mut() = None);
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn a_parented_toplevel_floats_without_resizing_the_neighbour() {
        let mut h = harness();
        let mut c = Client::connect(&mut h);
        let (n, tile) = neighbour(&mut h, &mut c);

        let t = c.create_toplevel(&mut h);
        t.toplevel.set_parent(Some(&n.toplevel));
        c.commit(&mut h, &t.surface);
        let window = window_of(&h, &t).expect("placed at the initial commit");
        assert!(floats(&h, &window), "a dialog floats by default");
        c.attach(&mut h, &t.surface);
        assert!(floats(&h, &window));

        let sizes = c.configured_sizes(&n.toplevel);
        assert!(
            sizes.iter().all(|&s| s == tile),
            "the neighbour was resized: {sizes:?}"
        );
    }

    /// `set_fullscreen` before the initial commit has no window to act on
    /// yet; it is parked and honoured when the commit places the window.
    #[test]
    fn fullscreen_asked_before_the_initial_commit_is_honoured_at_it() {
        let mut h = harness();
        let mut c = Client::connect(&mut h);
        let t = c.create_toplevel(&mut h);
        t.toplevel.set_fullscreen(None);
        c.pump(&mut h);
        assert!(window_of(&h, &t).is_none());
        c.commit(&mut h, &t.surface);
        let window = window_of(&h, &t).expect("placed at the initial commit");
        assert!(h.state.fullscreen.contains_key(&window), "fullscreen replayed");
        let output = h.state.outputs.focused().expect("output").output.clone();
        let full = h.state.space.output_geometry(&output).expect("mapped").size;
        assert_eq!(
            c.configured_sizes(&t.toplevel).last(),
            Some(&(full.w, full.h)),
            "the last configure before the map is the fullscreen size"
        );
    }

    /// Abyss has no maximized state: `set_maximized` is answered with a
    /// configure that does not carry it, and the window stays tiled.
    #[test]
    fn set_maximized_is_declined_and_the_window_stays_tiled() {
        let mut h = harness();
        let mut c = Client::connect(&mut h);
        let (n, tile) = neighbour(&mut h, &mut c);
        let window = window_of(&h, &n).expect("placed");
        let before = c.configured_sizes(&n.toplevel).len();

        n.toplevel.set_maximized();
        c.pump(&mut h);
        assert!(
            c.configured_sizes(&n.toplevel).len() > before,
            "the request is answered"
        );
        assert_eq!(c.maximized_configures(&n.toplevel), 0);
        assert_eq!(c.configured_sizes(&n.toplevel).last(), Some(&tile));
        assert!(!floats(&h, &window), "still tiled");

        n.toplevel.unset_maximized();
        c.pump(&mut h);
        assert_eq!(c.maximized_configures(&n.toplevel), 0);
        assert!(!floats(&h, &window));
    }

    /// Before the initial configure the request is just dropped from the
    /// pending state, so the first configure does not carry it either.
    #[test]
    fn set_maximized_before_the_initial_commit_is_dropped() {
        let mut h = harness();
        let mut c = Client::connect(&mut h);
        let t = c.create_toplevel(&mut h);
        t.toplevel.set_maximized();
        c.pump(&mut h);
        c.commit(&mut h, &t.surface);
        let window = window_of(&h, &t).expect("placed at the initial commit");
        assert_eq!(c.maximized_configures(&t.toplevel), 0);
        assert!(!floats(&h, &window));
    }

    #[test]
    fn a_toplevel_destroyed_before_its_initial_commit_never_touches_the_layout() {
        let mut h = harness();
        let mut c = Client::connect(&mut h);
        let (n, _) = neighbour(&mut h, &mut c);
        let before = c.configured_sizes(&n.toplevel).len();
        let focus = h.state.focus.clone();
        let _ = crate::ipc::capture::take();

        let t = c.create_toplevel(&mut h);
        t.toplevel.destroy();
        t.xdg.destroy();
        t.surface.destroy();
        c.pump(&mut h);

        assert_eq!(owned_windows(&h.state).len(), 1, "only the neighbour");
        assert_eq!(h.state.focus, focus, "focus never moved");
        assert_eq!(
            c.configured_sizes(&n.toplevel).len(),
            before,
            "the neighbour was not reconfigured"
        );
        let window_events: Vec<_> = crate::ipc::capture::take()
            .into_iter()
            .filter(|(kind, _)| kind == "window")
            .collect();
        assert!(window_events.is_empty(), "{window_events:?}");
    }

    // ---- fullscreen owns its workspace (COMP-05 §4, amendment C-19)

    /// A mapped toplevel that went fullscreen after its first buffer.
    fn fullscreen(h: &mut Harness, c: &mut Client) -> (Toplevel, Window) {
        let t = c.create_toplevel(h);
        c.commit(h, &t.surface);
        c.attach(h, &t.surface);
        t.toplevel.set_fullscreen(None);
        c.pump(h);
        let w = window_of(h, &t).expect("placed");
        assert!(h.state.fullscreen.contains_key(&w), "went fullscreen");
        (t, w)
    }

    /// The topmost window in the space.
    fn top(h: &Harness) -> Window {
        h.state
            .space
            .elements()
            .last()
            .cloned()
            .expect("something mapped")
    }

    fn focus(h: &mut Harness, w: &Window, raise: bool) {
        h.state.with_origin(crate::input::Origin::Physical, |s| {
            crate::shell::focus::focus_window_raising(s, w, raise)
        });
    }

    #[test]
    fn a_background_window_is_detached_from_its_own_workspace() {
        // The X11 fullscreen path reaches windows on unviewed workspaces
        // (`owned_windows`); acting on the viewed one left the window on its
        // own and copied it onto the viewed one, so it followed the human.
        let mut h = harness();
        let mut c = Client::connect(&mut h);
        let t = c.create_toplevel(&mut h);
        c.commit(&mut h, &t.surface);
        c.attach(&mut h, &t.surface);
        let w = window_of(&h, &t).expect("placed");
        let id = crate::shell::output_of_window(&h.state, &w).expect("on an output");

        crate::shell::switch_workspace(&mut h.state, 2);
        assert_eq!(crate::shell::home_workspace(&h.state, id, &w), 0);
        crate::shell::detach_from_layout(&mut h.state, id, &w);

        let ws = &h.state.outputs.get(id).expect("output").workspaces;
        assert!(!ws[0].holds_live(&w), "detached from its own workspace");
    }

    #[test]
    fn fullscreen_survives_switching_workspaces_away_and_back() {
        let mut h = harness();
        let mut c = Client::connect(&mut h);
        let n = c.create_toplevel(&mut h);
        c.commit(&mut h, &n.surface);
        c.attach(&mut h, &n.surface);
        let (_f, full) = fullscreen(&mut h, &mut c);

        crate::shell::switch_workspace(&mut h.state, 2);
        c.pump(&mut h);
        assert!(h.state.fullscreen.contains_key(&full), "kept while away");
        crate::shell::switch_workspace(&mut h.state, 1);
        c.pump(&mut h);
        assert!(h.state.fullscreen.contains_key(&full), "kept on return");
        assert_eq!(top(&h), full, "still on top on return");
    }

    #[test]
    fn a_new_window_takes_fullscreen_away() {
        let mut h = harness();
        let mut c = Client::connect(&mut h);
        let (_f, full) = fullscreen(&mut h, &mut c);

        let n = c.create_toplevel(&mut h);
        c.commit(&mut h, &n.surface);
        c.attach(&mut h, &n.surface);
        let new = window_of(&h, &n).expect("placed");

        assert!(!h.state.fullscreen.contains_key(&full), "fullscreen ended");
        assert_eq!(h.state.focus.as_ref(), Some(&new), "the new window has focus");
    }

    #[test]
    fn a_dialog_of_the_fullscreen_window_opens_above_it_and_keeps_it_fullscreen() {
        let mut h = harness();
        let mut c = Client::connect(&mut h);
        let (f, full) = fullscreen(&mut h, &mut c);

        let d = c.create_toplevel(&mut h);
        d.toplevel.set_parent(Some(&f.toplevel));
        c.commit(&mut h, &d.surface);
        c.attach(&mut h, &d.surface);
        let dialog = window_of(&h, &d).expect("placed");

        assert!(h.state.fullscreen.contains_key(&full), "still fullscreen");
        assert_eq!(top(&h), dialog, "the dialog is above its fullscreen parent");

        // Clicking back into the fullscreen window must not bury its dialog.
        focus(&mut h, &full, true);
        assert!(h.state.fullscreen.contains_key(&full));
        assert_eq!(top(&h), dialog, "the dialog stays above after refocus");
    }

    #[test]
    fn focusing_another_window_ends_fullscreen_but_passive_focus_does_not() {
        let mut h = harness();
        let mut c = Client::connect(&mut h);
        let (other_t, _) = neighbour(&mut h, &mut c);
        let other = window_of(&h, &other_t).expect("placed");
        let (_f, full) = fullscreen(&mut h, &mut c);

        // Focus-follows-mouse or a scene refresh: no restack, no change.
        focus(&mut h, &other, false);
        assert!(h.state.fullscreen.contains_key(&full), "passive focus keeps it");
        assert_eq!(top(&h), full, "and it stays on top");

        // A bind, a click on the taskbar, IPC: the human wants the other one.
        focus(&mut h, &other, true);
        assert!(!h.state.fullscreen.contains_key(&full), "fullscreen ended");
    }
}

mod held_layer_offset {
    use super::*;

    fn sz(w: i32, h: i32) -> Size<i32, Logical> {
        Size::from((w, h))
    }

    #[test]
    fn a_bottom_bar_that_grows_keeps_its_old_top_edge() {
        // smithay places it at 1040 - 40 = 1000; the drawn 30 px buffer
        // must stay at 1010, so the offset is +10.
        let anchor = Anchor::BOTTOM | Anchor::LEFT | Anchor::RIGHT;
        assert_eq!(held_offset(anchor, sz(1920, 40), sz(1920, 30)), (0, 10).into());
    }

    #[test]
    fn a_bottom_bar_that_shrinks_keeps_its_old_top_edge() {
        let anchor = Anchor::BOTTOM | Anchor::LEFT | Anchor::RIGHT;
        assert_eq!(held_offset(anchor, sz(1920, 30), sz(1920, 40)), (0, -10).into());
    }

    #[test]
    fn a_top_anchored_surface_is_never_held() {
        let anchor = Anchor::TOP | Anchor::LEFT | Anchor::RIGHT;
        assert_eq!(held_offset(anchor, sz(1920, 40), sz(1920, 30)), (0, 0).into());
    }

    #[test]
    fn a_right_anchored_surface_holds_horizontally() {
        let anchor = Anchor::RIGHT | Anchor::TOP | Anchor::BOTTOM;
        assert_eq!(held_offset(anchor, sz(300, 900), sz(200, 900)), (100, 0).into());
    }

    #[test]
    fn a_centred_surface_holds_its_centre() {
        // smithay: y = c - placed/2 = c - 20; held: y = c - acked/2 = c - 15.
        assert_eq!(
            held_offset(Anchor::empty(), sz(100, 40), sz(100, 30)),
            (0, 5).into()
        );
    }

    #[test]
    fn an_acked_size_equal_to_placed_is_no_offset() {
        let anchor = Anchor::BOTTOM | Anchor::LEFT | Anchor::RIGHT;
        assert_eq!(held_offset(anchor, sz(1920, 30), sz(1920, 30)), (0, 0).into());
    }
}

/// `mode "de"` (COMP-17 §2, COMP-05 §3.1, C-24): nothing tiles, and the flip
/// in either direction is live. Driven end to end by a real client.
mod de_mode {
    use super::*;
    use crate::config::Mode;
    use crate::shell::focus::state_tests::{
        client::{Client, Toplevel},
        harness, Harness,
    };

    fn window_of(h: &Harness, t: &Toplevel) -> Window {
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

    fn map(h: &mut Harness, c: &mut Client) -> (Toplevel, Window) {
        let t = c.create_toplevel(h);
        c.commit(h, &t.surface);
        c.attach(h, &t.surface);
        let w = window_of(h, &t);
        (t, w)
    }

    /// A dialog of `parent`, mapped.
    fn dialog(h: &mut Harness, c: &mut Client, parent: &Toplevel) -> (Toplevel, Window) {
        let t = c.create_toplevel(h);
        t.toplevel.set_parent(Some(&parent.toplevel));
        c.commit(h, &t.surface);
        c.attach(h, &t.surface);
        let w = window_of(h, &t);
        (t, w)
    }

    /// Reload with `mode` changed, through the same path a config reload takes.
    fn flip(h: &mut Harness, mode: Mode) {
        let mut next = h.state.config.clone();
        next.mode = mode;
        crate::config::apply_loaded(&mut h.state, next);
    }

    #[derive(Debug, PartialEq, Clone, Copy)]
    enum At {
        Tiled(u64, usize),
        Floating(u64, usize),
        Minimized(u64, usize),
        Nowhere,
    }

    fn at(h: &Harness, w: &Window) -> At {
        for e in h.state.outputs.iter() {
            for (i, ws) in e.workspaces.iter().enumerate() {
                if ws.tiled.contains(w) {
                    return At::Tiled(e.id, i);
                }
                if ws.floating.iter().any(|f| &f.window == w) {
                    return At::Floating(e.id, i);
                }
                if ws.is_minimized(w) {
                    return At::Minimized(e.id, i);
                }
            }
        }
        At::Nowhere
    }

    fn loc(h: &Harness, w: &Window) -> Point<i32, Logical> {
        h.state.space.element_geometry(w).expect("mapped").loc
    }

    fn focus(h: &mut Harness, w: &Window) {
        crate::shell::focus::focus_window(&mut h.state, w);
    }

    fn active(h: &Harness, id: u64) -> &workspace::Workspace {
        let e = h.state.outputs.get(id).expect("output");
        &e.workspaces[e.active]
    }

    #[test]
    fn entering_floats_every_tile_in_place_and_leaving_puts_them_back_with_weights() {
        let mut h = harness();
        let mut c = Client::connect(&mut h);
        let (_a, a) = map(&mut h, &mut c);
        let (_b, b) = map(&mut h, &mut c);
        let (_cc, w3) = map(&mut h, &mut c);
        let At::Tiled(id, _) = at(&h, &a) else {
            panic!("tiles")
        };
        h.state.outputs.get_mut(id).unwrap().workspaces[0]
            .tiled
            .set_weight(&b, 1.8);
        arrange(&mut h.state);
        let order = active(&h, id).tiled.windows();
        let before: Vec<_> = [&a, &b, &w3].iter().map(|w| loc(&h, w)).collect();

        flip(&mut h, Mode::De);
        for w in [&a, &b, &w3] {
            assert_eq!(at(&h, w), At::Floating(id, 0), "floated");
        }
        assert!(active(&h, id).tiled.windows().is_empty());
        let after: Vec<_> = [&a, &b, &w3].iter().map(|w| loc(&h, w)).collect();
        assert_eq!(before, after, "at the rect each tile had");

        flip(&mut h, Mode::Hybrid);
        assert_eq!(active(&h, id).tiled.windows(), order, "same tree, same order");
        assert_eq!(active(&h, id).tiled.weight_of(&b), Some(1.8));
        assert!(active(&h, id).floating.is_empty());
        let back: Vec<_> = [&a, &b, &w3].iter().map(|w| loc(&h, w)).collect();
        assert_eq!(before, back, "and in the same places");
        assert!(h.state.de.forget_is_clean());
    }

    #[test]
    fn every_workspace_and_output_is_lifted_not_only_the_active_one() {
        let mut h = harness();
        let mut c = Client::connect(&mut h);
        let (_a, a) = map(&mut h, &mut c);
        switch_workspace(&mut h.state, 3);
        let (_b, b) = map(&mut h, &mut c);
        switch_workspace(&mut h.state, 1);
        h.state.outputs.set_focused(h.b);
        let (_d, d) = map(&mut h, &mut c);
        let (ia, ib) = (h.a, h.b);
        assert_eq!(at(&h, &a), At::Tiled(ia, 0));
        assert_eq!(at(&h, &b), At::Tiled(ia, 2));
        assert_eq!(at(&h, &d), At::Tiled(ib, 0));

        flip(&mut h, Mode::De);
        assert_eq!(at(&h, &a), At::Floating(ia, 0));
        assert_eq!(at(&h, &b), At::Floating(ia, 2), "a hidden workspace too");
        assert_eq!(at(&h, &d), At::Floating(ib, 0), "the other output too");

        flip(&mut h, Mode::Wm);
        assert_eq!(at(&h, &a), At::Tiled(ia, 0));
        assert_eq!(at(&h, &b), At::Tiled(ia, 2));
        assert_eq!(at(&h, &d), At::Tiled(ib, 0));
    }

    #[test]
    fn wm_to_hybrid_touches_no_window() {
        let mut h = harness();
        let mut c = Client::connect(&mut h);
        let (_a, a) = map(&mut h, &mut c);
        let (_b, b) = map(&mut h, &mut c);
        focus(&mut h, &b);
        toggle_floating(&mut h.state);
        let (ta, tb) = (at(&h, &a), at(&h, &b));
        flip(&mut h, Mode::Wm);
        flip(&mut h, Mode::Hybrid);
        assert_eq!((at(&h, &a), at(&h, &b)), (ta, tb));
        assert!(matches!(tb, At::Floating(..)));
        assert!(h.state.de.forget_is_clean(), "nothing recorded");
    }

    #[test]
    fn a_window_that_floated_before_de_stays_floating_after() {
        let mut h = harness();
        let mut c = Client::connect(&mut h);
        let (_a, a) = map(&mut h, &mut c);
        let (_b, b) = map(&mut h, &mut c);
        focus(&mut h, &b);
        toggle_floating(&mut h.state);
        flip(&mut h, Mode::De);
        flip(&mut h, Mode::Hybrid);
        assert!(matches!(at(&h, &b), At::Floating(..)), "still floating");
        assert!(matches!(at(&h, &a), At::Tiled(..)));
    }

    #[test]
    fn new_windows_float_under_de_and_are_placed_by_default_on_leaving() {
        let mut h = harness();
        let mut c = Client::connect(&mut h);
        flip(&mut h, Mode::De);
        let (p, pw) = map(&mut h, &mut c);
        let (_d, dw) = dialog(&mut h, &mut c, &p);
        assert!(matches!(at(&h, &pw), At::Floating(..)), "a new window floats");
        assert!(matches!(at(&h, &dw), At::Floating(..)));
        flip(&mut h, Mode::Hybrid);
        assert!(matches!(at(&h, &pw), At::Tiled(..)), "default placement tiles it");
        assert!(matches!(at(&h, &dw), At::Floating(..)), "a dialog stays floating");
    }

    #[test]
    fn a_tile_rule_does_not_tile_under_de() {
        let mut h = harness();
        let mut c = Client::connect(&mut h);
        let rules = crate::config::parse_single_for_tests("windowrule \"tile\" { app-id \".*\"; }\n");
        assert!(rules.errors.is_empty(), "{:?}", rules.errors);
        h.state.config.window_rules = rules.window_rules;
        assert!(!h.state.config.window_rules.is_empty());
        // Control: under hybrid the rule tiles a dialog.
        let (p, _) = map(&mut h, &mut c);
        let (_d, dw) = dialog(&mut h, &mut c, &p);
        assert!(matches!(at(&h, &dw), At::Tiled(..)), "the rule tiles in hybrid");
        flip(&mut h, Mode::De);
        let (_d2, dw2) = dialog(&mut h, &mut c, &p);
        assert!(matches!(at(&h, &dw2), At::Floating(..)), "and does not under de");
    }

    #[test]
    fn tile_only_actions_do_nothing_under_de() {
        let mut h = harness();
        let mut c = Client::connect(&mut h);
        flip(&mut h, Mode::De);
        let (_a, a) = map(&mut h, &mut c);
        let (_b, b) = map(&mut h, &mut c);
        focus(&mut h, &b);
        toggle_floating(&mut h.state);
        assert!(
            matches!(at(&h, &b), At::Floating(..)),
            "toggle-floating never tiles"
        );
        toggle_layout(&mut h.state);
        assert!(h
            .state
            .outputs
            .iter()
            .all(|e| e.workspaces.iter().all(|w| w.layout.is_none())));
        adjust_priority(&mut h.state, 1);
        move_direction(&mut h.state, Direction::Left);
        assert!(matches!(at(&h, &a), At::Floating(..)) && matches!(at(&h, &b), At::Floating(..)));
        assert!(h.state.tile_drag.is_none());
    }

    #[test]
    fn a_minimized_tile_floats_when_restored_under_de_and_tiles_again_on_leaving() {
        let mut h = harness();
        let mut c = Client::connect(&mut h);
        let (_a, a) = map(&mut h, &mut c);
        let (_b, b) = map(&mut h, &mut c);
        focus(&mut h, &b);
        minimize_focused(&mut h.state);
        assert!(matches!(at(&h, &b), At::Minimized(..)));

        flip(&mut h, Mode::De);
        assert!(matches!(at(&h, &b), At::Minimized(..)), "still minimized");
        unminimize_window(&mut h.state, &b);
        assert!(matches!(at(&h, &b), At::Floating(..)), "restored floating");
        flip(&mut h, Mode::Hybrid);
        assert!(matches!(at(&h, &b), At::Tiled(..)) && matches!(at(&h, &a), At::Tiled(..)));

        // Minimized across the whole of de, and across a de-minimize.
        focus(&mut h, &b);
        minimize_focused(&mut h.state);
        flip(&mut h, Mode::De);
        flip(&mut h, Mode::Hybrid);
        assert!(matches!(at(&h, &b), At::Minimized(..)));
        unminimize_window(&mut h.state, &b);
        assert!(matches!(at(&h, &b), At::Tiled(..)), "restores into the layout");

        // A window floated under de and minimized there tiles on restore after.
        flip(&mut h, Mode::De);
        focus(&mut h, &b);
        minimize_focused(&mut h.state);
        flip(&mut h, Mode::Hybrid);
        unminimize_window(&mut h.state, &b);
        assert!(matches!(at(&h, &b), At::Tiled(..)));
    }

    #[test]
    fn a_window_minimized_after_the_flip_keeps_its_weight_on_leaving() {
        let mut h = harness();
        let mut c = Client::connect(&mut h);
        let (_a, a) = map(&mut h, &mut c);
        let (_b, b) = map(&mut h, &mut c);
        let (_cc, _w3) = map(&mut h, &mut c);
        let At::Tiled(id, _) = at(&h, &a) else { panic!() };
        h.state.outputs.get_mut(id).unwrap().workspaces[0]
            .tiled
            .set_weight(&b, 1.8);
        flip(&mut h, Mode::De);
        focus(&mut h, &b);
        minimize_focused(&mut h.state);
        flip(&mut h, Mode::Hybrid);
        unminimize_window(&mut h.state, &b);
        assert_eq!(at(&h, &b), At::Tiled(id, 0));
        let wt = h.state.outputs.get(id).unwrap().workspaces[0].tiled.weight_of(&b);
        assert_eq!(wt, Some(1.8));
    }

    #[test]
    fn moving_a_workspace_under_de_floats_what_it_carries_apart() {
        let mut h = harness();
        let mut c = Client::connect(&mut h);
        let (_a, a) = map(&mut h, &mut c);
        let (_b, b) = map(&mut h, &mut c);
        let (ia, ib) = (h.a, h.b);
        flip(&mut h, Mode::De);
        {
            let ws = &mut h.state.outputs.get_mut(ia).unwrap().workspaces[4];
            ws.pending.push(a.clone());
            ws.pending.push(b.clone());
        }
        for w in [&a, &b] {
            h.state.outputs.get_mut(ia).unwrap().workspaces[0]
                .floating
                .retain(|f| &f.window != w);
        }
        assert_eq!(move_workspace_to_output(&mut h.state, ia, 5, ib), Ok(true));
        for w in [&a, &b] {
            assert!(!matches!(at(&h, w), At::Tiled(..)), "nothing tiles under de");
        }
        let fl = &h.state.outputs.get(ib).unwrap().workspaces[4].floating;
        assert_eq!(fl.len(), 2);
        assert_ne!(fl[0].rect.loc, fl[1].rect.loc, "not stacked exactly");
    }

    #[test]
    fn the_super_held_tile_drag_never_starts_under_de() {
        let mut h = harness();
        let mut c = Client::connect(&mut h);
        flip(&mut h, Mode::De);
        let (_a, a) = map(&mut h, &mut c);
        // Force a tile in (a path nobody guarded): the drag must still refuse.
        let id = h.a;
        let area = Rectangle::from_size((800, 600).into());
        let ws = &mut h.state.outputs.get_mut(id).unwrap().workspaces[0];
        ws.floating.retain(|f| f.window != a);
        ws.tiled.insert(a.clone(), None, area, Point::default());
        let l = loc(&h, &a);
        drop_guides::drag_tile(&mut h.state, &a, l);
        assert!(h.state.tile_drag.is_none());
    }

    #[test]
    fn a_window_closed_or_moved_during_de_is_not_put_back_where_it_was() {
        let mut h = harness();
        let mut c = Client::connect(&mut h);
        let (_a, a) = map(&mut h, &mut c);
        let (_b, b) = map(&mut h, &mut c);
        let (_cc, w3) = map(&mut h, &mut c);
        let At::Tiled(id, _) = at(&h, &a) else { panic!() };
        flip(&mut h, Mode::De);
        unmap_window(&mut h.state, &w3);
        focus(&mut h, &b);
        move_to_workspace(&mut h.state, 4);
        assert_eq!(at(&h, &b), At::Floating(id, 3), "moves as a floating window");
        flip(&mut h, Mode::Hybrid);
        assert_eq!(at(&h, &w3), At::Nowhere);
        assert_eq!(at(&h, &a), At::Tiled(id, 0));
        assert_eq!(at(&h, &b), At::Tiled(id, 3), "tiles where it now lives");
        assert_eq!(
            h.state.outputs.get(id).unwrap().workspaces[0].tiled.windows(),
            vec![a.clone()]
        );
    }

    #[test]
    fn a_window_sent_to_the_other_output_in_de_floats_there_and_tiles_after() {
        let mut h = harness();
        let mut c = Client::connect(&mut h);
        let (_a, a) = map(&mut h, &mut c);
        let (_b, b) = map(&mut h, &mut c);
        flip(&mut h, Mode::De);
        focus(&mut h, &b);
        let (ia, ib) = (h.a, h.b);
        let number = h.state.outputs.get(ib).unwrap().number;
        move_to_output_workspace(&mut h.state, number);
        assert_eq!(at(&h, &b), At::Floating(ib, 0));
        flip(&mut h, Mode::Hybrid);
        assert_eq!(at(&h, &b), At::Tiled(ib, 0));
        assert_eq!(at(&h, &a), At::Tiled(ia, 0));
    }

    #[test]
    fn a_removed_output_keeps_its_windows_floating_then_tiles_them_after() {
        let mut h = harness();
        let mut c = Client::connect(&mut h);
        h.state.outputs.set_focused(h.b);
        let (_d, d) = map(&mut h, &mut c);
        let (_e, e) = map(&mut h, &mut c);
        let ia = h.a;
        flip(&mut h, Mode::De);
        crate::outputs::unregister(&mut h.state, h.b);
        arrange(&mut h.state);
        assert_eq!(at(&h, &d), At::Floating(ia, 0), "adopted floating");
        assert_eq!(at(&h, &e), At::Floating(ia, 0));
        flip(&mut h, Mode::Hybrid);
        assert_eq!(at(&h, &d), At::Tiled(ia, 0));
        assert_eq!(at(&h, &e), At::Tiled(ia, 0));
    }

    #[test]
    fn fullscreen_across_the_flip_comes_back_tiled() {
        let mut h = harness();
        let mut c = Client::connect(&mut h);
        let (_a, a) = map(&mut h, &mut c);
        let (b, bw) = map(&mut h, &mut c);
        b.toplevel.set_fullscreen(None);
        c.pump(&mut h);
        assert!(h.state.fullscreen.contains_key(&bw));

        flip(&mut h, Mode::De);
        assert!(h.state.fullscreen.contains_key(&bw), "still fullscreen");
        assert!(matches!(at(&h, &a), At::Floating(..)));
        // Unfullscreen under de floats; leaving then tiles it.
        b.toplevel.unset_fullscreen();
        c.pump(&mut h);
        assert!(!h.state.fullscreen.contains_key(&bw));
        assert!(matches!(at(&h, &bw), At::Floating(..)), "no tile under de");
        flip(&mut h, Mode::Hybrid);
        assert!(matches!(at(&h, &bw), At::Tiled(..)));

        // Still fullscreen when de ends: unfullscreen tiles it.
        b.toplevel.set_fullscreen(None);
        c.pump(&mut h);
        flip(&mut h, Mode::De);
        flip(&mut h, Mode::Hybrid);
        assert!(h.state.fullscreen.contains_key(&bw));
        b.toplevel.unset_fullscreen();
        c.pump(&mut h);
        assert!(matches!(at(&h, &bw), At::Tiled(..)));
    }

    #[test]
    fn a_floating_window_made_fullscreen_in_de_stays_floating_when_it_ends() {
        let mut h = harness();
        let mut c = Client::connect(&mut h);
        let (_a, _) = map(&mut h, &mut c);
        flip(&mut h, Mode::De);
        let (b, _) = map(&mut h, &mut c);
        // A dialog default-places as floating, so fullscreen one.
        let (p2, p2w) = dialog(&mut h, &mut c, &b);
        p2.toplevel.set_fullscreen(None);
        c.pump(&mut h);
        flip(&mut h, Mode::Hybrid);
        p2.toplevel.unset_fullscreen();
        c.pump(&mut h);
        assert!(matches!(at(&h, &p2w), At::Floating(..)));
    }

    // ---- maximize (M2: COMP-05 §3.1, C-24) ------------------------------

    use crate::shell::workspace::Placement;
    use smithay::reexports::wayland_protocols::xdg::shell::server::xdg_toplevel::State as XdgState;

    fn floating_of(h: &Harness, w: &Window) -> (Rectangle<i32, Logical>, Placement) {
        h.state
            .outputs
            .iter()
            .flat_map(|e| e.workspaces.iter())
            .flat_map(|ws| ws.floating.iter())
            .find(|f| &f.window == w)
            .map(|f| (f.rect, f.placement))
            .expect("floating")
    }

    fn bpx(h: &Harness) -> i32 {
        h.state.config.general.border_size
    }

    /// The frame a maximized window takes: the usable area inset by the outer gaps.
    fn lone(h: &Harness, id: u64) -> Rectangle<i32, Logical> {
        let output = h.state.outputs.get(id).expect("output").output.clone();
        layer_map_for_output(&output).arrange();
        tiling_area(&h.state, &output)
    }

    /// A de-mode window with a rectangle that is not the usable area.
    fn de_window(h: &mut Harness, c: &mut Client) -> (Toplevel, Window) {
        flip(h, Mode::De);
        let (t, w) = map(h, c);
        (t, w)
    }

    #[test]
    fn set_maximized_under_de_fills_the_usable_area_square_and_unmaximize_restores() {
        let mut h = harness();
        let mut c = Client::connect(&mut h);
        let (n, w) = de_window(&mut h, &mut c);
        let id = h.a;
        let (before, p) = floating_of(&h, &w);
        assert_eq!(p, Placement::Free);

        n.toplevel.set_maximized();
        c.pump(&mut h);
        let u = lone(&h, id);
        let (rect, p) = floating_of(&h, &w);
        assert!(matches!(p, Placement::Maximized { h: true, v: true, .. }));
        assert_eq!(
            rect, u,
            "the frame is the lone-tile frame: usable inset by the outer gaps"
        );
        assert_eq!(
            loc(&h, &w),
            Point::from((u.loc.x + bpx(&h), u.loc.y + bpx(&h))),
            "the content sits inside the border, as in a lone tile"
        );
        assert_eq!(
            c.configured_sizes(&n.toplevel).last(),
            Some(&(u.size.w - 2 * bpx(&h), u.size.h - 2 * bpx(&h)))
        );
        assert!(c.is_maximized(&n.toplevel), "the configure carries Maximized");
        assert!(maximize::is_maximized(&h.state, &w));

        n.toplevel.unset_maximized();
        c.pump(&mut h);
        let (rect, p) = floating_of(&h, &w);
        assert_eq!(p, Placement::Free);
        assert_eq!(rect, before, "the prior rectangle is back");
        assert!(!c.is_maximized(&n.toplevel));
    }

    /// Maximized under `de` is a lone tile to look at: the frame of the one
    /// window of a tiling workspace, the border inset kept, round corners, and
    /// nothing under the panel's exclusive zone.
    #[test]
    fn a_maximized_frame_is_the_lone_tile_frame_and_clears_the_panel() {
        let mut h = harness();
        let mut c = Client::connect(&mut h);
        let (n, w) = de_window(&mut h, &mut c);
        let (_panel, _layer) = c.map_panel(&mut h, 40);
        arrange(&mut h.state);
        n.toplevel.set_maximized();
        c.pump(&mut h);
        let id = h.a;
        let output = h.state.outputs.get(id).expect("output").output.clone();
        let (rect, _) = floating_of(&h, &w);
        let area = tiling_area(&h.state, &output);
        // The rect a lone window gets from the tiling layout under the same gaps.
        let mut tree = crate::shell::layout::Tree::<u32>::new();
        tree.insert(1, None, area, Point::default());
        let gaps = &h.state.config.general;
        let lone = tree.radiant(area, gaps.gaps_in, gaps.gaps_in_y());
        assert_eq!(rect, lone[0].1, "same rect as a lone tile");
        let usable = usable_area(&h.state, &output);
        assert!(usable.contains_rect(rect), "inside the usable area");
        let geo = h.state.space.output_geometry(&output).unwrap();
        let panel_zone = Rectangle::new(geo.loc, (geo.size.w, usable.loc.y - geo.loc.y).into());
        assert!(!rect.overlaps(panel_zone), "clear of the bar's exclusive zone");
    }

    #[test]
    fn maximize_is_declined_in_wm_and_hybrid_and_nothing_changes() {
        for mode in [Mode::Wm, Mode::Hybrid] {
            let mut h = harness();
            let mut c = Client::connect(&mut h);
            flip(&mut h, mode);
            let (n, w) = map(&mut h, &mut c);
            n.toplevel.set_maximized();
            c.pump(&mut h);
            assert_eq!(c.maximized_configures(&n.toplevel), 0, "{mode:?}");
            assert!(matches!(at(&h, &w), At::Tiled(..)), "{mode:?}: still tiled");
            assert!(!maximize::is_maximized(&h.state, &w));
            assert!(!mode.profile().maximize);
        }
        assert!(Mode::De.profile().maximize);
    }

    #[test]
    fn steam_style_startup_maximize_is_declined_in_wm_and_hybrid_and_honoured_in_de() {
        for (mode, honoured) in [(Mode::Wm, false), (Mode::Hybrid, false), (Mode::De, true)] {
            let mut h = harness();
            let mut c = Client::connect(&mut h);
            flip(&mut h, mode);
            let t = c.create_toplevel(&mut h);
            t.toplevel.set_maximized();
            c.pump(&mut h);
            c.commit(&mut h, &t.surface);
            c.attach(&mut h, &t.surface);
            let w = window_of(&h, &t);
            assert_eq!(c.is_maximized(&t.toplevel), honoured, "{mode:?}");
            assert_eq!(maximize::is_maximized(&h.state, &w), honoured, "{mode:?}");
            if honoured {
                let u = lone(&h, h.a);
                assert_eq!(floating_of(&h, &w).0, u);
                assert_eq!(
                    c.configured_sizes(&t.toplevel).last(),
                    Some(&(u.size.w - 2 * bpx(&h), u.size.h - 2 * bpx(&h)))
                );
            } else {
                assert_eq!(c.maximized_configures(&t.toplevel), 0, "{mode:?}");
            }
        }
    }

    #[test]
    fn the_x11_premap_decision_follows_the_mode_profile() {
        // `map_window_request`: parked request or property, honoured only where
        // the profile allows it.
        use crate::xwayland::xwm::premap_maximize;
        for mode in [Mode::Wm, Mode::Hybrid] {
            assert!(!premap_maximize(mode.profile().maximize, true, true));
            assert!(!premap_maximize(mode.profile().maximize, false, true));
        }
        assert!(premap_maximize(Mode::De.profile().maximize, true, false));
        assert!(premap_maximize(Mode::De.profile().maximize, false, true));
        assert!(!premap_maximize(Mode::De.profile().maximize, false, false));
    }

    #[test]
    fn horizontal_only_and_vertical_only_fill_one_axis() {
        let mut h = harness();
        let mut c = Client::connect(&mut h);
        let (n, w) = de_window(&mut h, &mut c);
        let id = h.a;
        let (before, _) = floating_of(&h, &w);
        let own = before;
        let u = lone(&h, id);

        assert!(maximize::set(&mut h.state, &w, Some((true, false))));
        let (rect, p) = floating_of(&h, &w);
        assert!(matches!(
            p,
            Placement::Maximized {
                h: true,
                v: false,
                ..
            }
        ));
        assert_eq!((rect.loc.x, rect.size.w), (u.loc.x, u.size.w));
        assert_eq!(
            (rect.loc.y, rect.size.h),
            (own.loc.y, own.size.h),
            "height untouched"
        );
        c.pump(&mut h);
        assert!(!c.is_maximized(&n.toplevel), "Maximized needs both axes");

        assert!(maximize::set(&mut h.state, &w, Some((false, true))));
        let (rect, _) = floating_of(&h, &w);
        assert_eq!((rect.loc.y, rect.size.h), (u.loc.y, u.size.h));
        assert_eq!((rect.loc.x, rect.size.w), (own.loc.x, own.size.w));

        assert!(maximize::set(&mut h.state, &w, None));
        assert_eq!(
            floating_of(&h, &w),
            (before, Placement::Free),
            "restores the original rect"
        );
    }

    #[test]
    fn fullscreen_wins_and_leaving_it_returns_to_maximized() {
        let mut h = harness();
        let mut c = Client::connect(&mut h);
        let (n, w) = de_window(&mut h, &mut c);
        let id = h.a;
        let (before, _) = floating_of(&h, &w);
        n.toplevel.set_maximized();
        c.pump(&mut h);
        n.toplevel.set_fullscreen(None);
        c.pump(&mut h);
        let output = h.state.outputs.get(id).unwrap().output.clone();
        let full = h.state.space.output_geometry(&output).unwrap();
        assert_eq!(floating_of(&h, &w).0, full, "fullscreen wins");
        assert!(!c.is_maximized(&n.toplevel), "told fullscreen, not maximized");

        n.toplevel.unset_fullscreen();
        c.pump(&mut h);
        let u = lone(&h, id);
        let (rect, p) = floating_of(&h, &w);
        assert!(p.is_fully_maximized(), "back to maximized");
        assert_eq!(rect, u);
        assert!(c.is_maximized(&n.toplevel));
        assert_eq!(
            c.configured_sizes(&n.toplevel).last(),
            Some(&(u.size.w - 2 * bpx(&h), u.size.h - 2 * bpx(&h)))
        );

        n.toplevel.unset_maximized();
        c.pump(&mut h);
        assert_eq!(
            floating_of(&h, &w).0,
            before,
            "fullscreen round trip lost no restore rect"
        );
    }

    #[test]
    fn unmaximizing_during_fullscreen_returns_to_the_old_rect() {
        let mut h = harness();
        let mut c = Client::connect(&mut h);
        let (n, w) = de_window(&mut h, &mut c);
        let (before, _) = floating_of(&h, &w);
        n.toplevel.set_maximized();
        c.pump(&mut h);
        n.toplevel.set_fullscreen(None);
        c.pump(&mut h);
        n.toplevel.unset_maximized();
        c.pump(&mut h);
        n.toplevel.unset_fullscreen();
        c.pump(&mut h);
        let (rect, p) = floating_of(&h, &w);
        assert_eq!(p, Placement::Free);
        assert_eq!(rect, before);
    }

    #[test]
    fn a_panel_appearing_or_resizing_is_followed_and_restore_survives_it() {
        let mut h = harness();
        // Centred, as a user who wrote `floating-placement "centered"` has it:
        // the de default is cascade, which starts at the corner a panel claims.
        h.state.config.general.floating_placement = FloatingPlacement::Centered;
        h.state.config.general.floating_placement_set = true;
        let mut c = Client::connect(&mut h);
        let (n, w) = de_window(&mut h, &mut c);
        let id = h.a;
        let (before, _) = floating_of(&h, &w);
        n.toplevel.set_maximized();
        c.pump(&mut h);
        let full = floating_of(&h, &w).0;

        let (panel, layer) = c.map_panel(&mut h, 40);
        arrange(&mut h.state);
        let u = lone(&h, id);
        assert_eq!(u.loc.y, full.loc.y + 40, "the panel claims the top strip");
        assert_eq!(floating_of(&h, &w).0, u, "the window follows the panel");
        assert_eq!(
            c.configured_sizes(&n.toplevel).last(),
            Some(&(u.size.w - 2 * bpx(&h), u.size.h - 2 * bpx(&h)))
        );

        layer.set_exclusive_zone(90);
        c.commit(&mut h, &panel);
        arrange(&mut h.state);
        let u2 = lone(&h, id);
        assert_eq!(u2.loc.y, full.loc.y + 90);
        assert_eq!(floating_of(&h, &w).0, u2);

        n.toplevel.unset_maximized();
        c.pump(&mut h);
        let (rect, _) = floating_of(&h, &w);
        assert_eq!(rect, before, "the pre-maximize rect, untouched by the panel");
    }

    #[test]
    fn an_output_resize_is_followed() {
        let mut h = harness();
        let mut c = Client::connect(&mut h);
        let (n, w) = de_window(&mut h, &mut c);
        n.toplevel.set_maximized();
        c.pump(&mut h);
        let output = h.state.outputs.get(h.a).unwrap().output.clone();
        let mode = smithay::output::Mode {
            size: (1000, 700).into(),
            refresh: 60_000,
        };
        output.change_current_state(Some(mode), None, None, None);
        arrange(&mut h.state);
        let u = lone(&h, h.a);
        let g = &h.state.config.general;
        assert_eq!(u.size, (1000 - 2 * g.gaps_out, 700 - 2 * g.gaps_out_y()).into());
        assert_eq!(floating_of(&h, &w).0, u);
    }

    #[test]
    fn a_removed_output_leaves_the_window_maximized_on_the_adopter() {
        let mut h = harness();
        let mut c = Client::connect(&mut h);
        h.state.outputs.set_focused(h.b);
        let (n, w) = de_window(&mut h, &mut c);
        // A rectangle that is not what the adopter would pick by default.
        let usable_b = lone(&h, h.b);
        let before = Rectangle::new(usable_b.loc + Point::from((57, 43)), (301, 203).into());
        for e in h.state.outputs.iter_mut() {
            for ws in e.workspaces.iter_mut() {
                for f in ws.floating.iter_mut().filter(|f| f.window == w) {
                    f.rect = before;
                }
            }
        }
        n.toplevel.set_maximized();
        c.pump(&mut h);
        assert_eq!(at(&h, &w), At::Floating(h.b, 0));
        let (ia, ib) = (h.a, h.b);
        let geo = |h: &Harness, id: u64| {
            let o = h.state.outputs.get(id).expect("output").output.clone();
            h.state.space.output_geometry(&o).expect("mapped")
        };
        let offset = geo(&h, ia).loc - geo(&h, ib).loc;
        crate::outputs::unregister(&mut h.state, ib);
        arrange(&mut h.state);
        assert_eq!(at(&h, &w), At::Floating(ia, 0), "adopted");
        let (rect, p) = floating_of(&h, &w);
        assert!(p.is_fully_maximized(), "still maximized");
        assert_eq!(rect, lone(&h, ia), "filling the adopter");
        assert!(c.is_maximized(&n.toplevel));
        // And it can still be restored, onto the adopter.
        n.toplevel.unset_maximized();
        c.pump(&mut h);
        let (rect, p) = floating_of(&h, &w);
        assert_eq!(p, Placement::Free);
        assert!(lone(&h, ia).contains_rect(rect), "restored inside the adopter");
        assert_eq!(
            rect,
            de::carried_rect(before, offset, lone(&h, ia)),
            "its own pre-maximize rectangle, not the default"
        );
    }

    #[test]
    fn minimize_and_restore_keeps_it_maximized() {
        let mut h = harness();
        let mut c = Client::connect(&mut h);
        let (n, w) = de_window(&mut h, &mut c);
        n.toplevel.set_maximized();
        c.pump(&mut h);
        focus(&mut h, &w);
        minimize_focused(&mut h.state);
        assert!(matches!(at(&h, &w), At::Minimized(..)));
        assert!(maximize::is_maximized(&h.state, &w), "kept while minimized");
        unminimize_window(&mut h.state, &w);
        c.pump(&mut h);
        let (rect, p) = floating_of(&h, &w);
        assert!(p.is_fully_maximized());
        assert_eq!(rect, lone(&h, h.a));
        assert!(c.is_maximized(&n.toplevel));
    }

    #[test]
    fn a_workspace_move_keeps_it_maximized() {
        let mut h = harness();
        let mut c = Client::connect(&mut h);
        let (n, w) = de_window(&mut h, &mut c);
        let id = h.a;
        let (before, _) = floating_of(&h, &w);
        n.toplevel.set_maximized();
        c.pump(&mut h);
        focus(&mut h, &w);
        move_to_workspace(&mut h.state, 4);
        c.pump(&mut h);
        assert_eq!(at(&h, &w), At::Floating(id, 3));
        let e = h.state.outputs.get(id).unwrap();
        if e.active != 3 {
            switch_workspace(&mut h.state, 4);
        }
        let (rect, p) = floating_of(&h, &w);
        assert!(p.is_fully_maximized(), "maximized there");
        assert_eq!(rect, lone(&h, id));
        n.toplevel.unset_maximized();
        c.pump(&mut h);
        assert_eq!(floating_of(&h, &w).0, before);
    }

    #[test]
    fn an_output_move_fills_the_other_outputs_usable_area() {
        let mut h = harness();
        let mut c = Client::connect(&mut h);
        let (n, w) = de_window(&mut h, &mut c);
        let (ia, ib) = (h.a, h.b);
        n.toplevel.set_maximized();
        c.pump(&mut h);
        // A panel on the first output only, so the two usable areas differ.
        let (_panel, _layer) = c.map_panel(&mut h, 30);
        focus(&mut h, &w);
        let number = h.state.outputs.get(ib).unwrap().number;
        move_to_output_workspace(&mut h.state, number);
        c.pump(&mut h);
        assert_eq!(at(&h, &w), At::Floating(ib, 0));
        let (rect, p) = floating_of(&h, &w);
        assert!(p.is_fully_maximized());
        assert_eq!(rect, lone(&h, ib));
        assert_ne!(lone(&h, ia).size, lone(&h, ib).size, "the panels differ");
    }

    #[test]
    fn a_client_move_or_resize_request_unmaximizes_first_keeping_the_pointer_inside() {
        let mut h = harness();
        let mut c = Client::connect(&mut h);
        let (n, w) = de_window(&mut h, &mut c);
        n.toplevel.set_maximized();
        c.pump(&mut h);
        let u = lone(&h, h.a);
        h.state.pointer_location = (u.loc.x as f64 + u.size.w as f64 * 0.75, u.loc.y as f64 + 10.0).into();
        let p = h.state.pointer_location;
        maximize::release_for_drag(&mut h.state, &w);
        c.pump(&mut h);
        let (rect, placement) = floating_of(&h, &w);
        assert_eq!(placement, Placement::Free);
        assert!(!c.is_maximized(&n.toplevel));
        assert!(
            rect.to_f64().contains(p),
            "the restored window {rect:?} is under the pointer {p:?}"
        );
        // And not still pinned to the whole area.
        assert!(rect.size.w < u.size.w);
    }

    #[test]
    fn a_drag_start_on_a_fullscreen_maximized_window_changes_nothing() {
        let mut h = harness();
        let mut c = Client::connect(&mut h);
        let (n, w) = de_window(&mut h, &mut c);
        n.toplevel.set_maximized();
        c.pump(&mut h);
        n.toplevel.set_fullscreen(None);
        c.pump(&mut h);
        let restore = h.state.fullscreen.get(&w).copied();
        h.state.pointer_location = (100.0, 100.0).into();
        maximize::release_for_drag(&mut h.state, &w);
        c.pump(&mut h);
        assert!(
            floating_of(&h, &w).1.is_maximized(),
            "placement kept under fullscreen"
        );
        assert_eq!(
            h.state.fullscreen.get(&w).copied(),
            restore,
            "restore rect untouched"
        );
    }

    #[test]
    fn resizing_a_fullscreen_maximized_window_keeps_the_maximize() {
        let mut h = harness();
        let mut c = Client::connect(&mut h);
        let (n, w) = de_window(&mut h, &mut c);
        n.toplevel.set_maximized();
        c.pump(&mut h);
        n.toplevel.set_fullscreen(None);
        c.pump(&mut h);
        let _ = crate::shell::resize_window(&mut h.state, &w, Some(300), Some(200));
        c.pump(&mut h);
        assert!(
            floating_of(&h, &w).1.is_maximized(),
            "leaving fullscreen must still return to maximized"
        );
    }

    #[test]
    fn a_maximized_window_with_a_max_size_reports_the_size_it_has() {
        let mut h = harness();
        let mut c = Client::connect(&mut h);
        let (n, w) = de_window(&mut h, &mut c);
        n.toplevel.set_max_size(400, 300);
        c.commit(&mut h, &n.surface);
        n.toplevel.set_maximized();
        c.pump(&mut h);
        let (rect, p) = floating_of(&h, &w);
        assert!(p.is_fully_maximized());
        assert_eq!((rect.size.w, rect.size.h), (400 + 2 * bpx(&h), 300 + 2 * bpx(&h)));
        assert_eq!(c.configured_sizes(&n.toplevel).last(), Some(&(400, 300)));
    }

    #[test]
    fn set_maximized_on_a_hidden_workspace_window_is_honoured() {
        let mut h = harness();
        let mut c = Client::connect(&mut h);
        let (n, w) = de_window(&mut h, &mut c);
        crate::shell::switch_workspace(&mut h.state, 2);
        assert!(!h.state.space.elements().any(|e| e == &w), "hidden");
        n.toplevel.set_maximized();
        c.pump(&mut h);
        assert!(floating_of(&h, &w).1.is_fully_maximized());
        assert!(c.is_maximized(&n.toplevel));
    }

    #[test]
    fn an_x11_style_exact_placement_or_an_ipc_resize_unmaximizes() {
        let mut h = harness();
        let mut c = Client::connect(&mut h);
        let (n, w) = de_window(&mut h, &mut c);
        n.toplevel.set_maximized();
        c.pump(&mut h);
        let want = Rectangle::new(Point::from((50, 60)), Size::from((300, 200)));
        place_at(&mut h.state, &w, want);
        c.pump(&mut h);
        let (_, p) = floating_of(&h, &w);
        assert_eq!(p, Placement::Free, "place_at is a move/resize");
        assert!(!c.is_maximized(&n.toplevel));

        n.toplevel.set_maximized();
        c.pump(&mut h);
        assert!(maximize::is_maximized(&h.state, &w));
        assert_eq!(resize_window(&mut h.state, &w, Some(320), None), Ok(true));
        assert_eq!(floating_of(&h, &w).1, Placement::Free);
    }

    #[test]
    fn alt_drag_unmaximizes_and_keeps_the_window_under_the_pointer() {
        let mut h = harness();
        let mut c = Client::connect(&mut h);
        let (n, w) = de_window(&mut h, &mut c);
        n.toplevel.set_maximized();
        c.pump(&mut h);
        let u = lone(&h, h.a);
        h.state.pointer_location = (u.loc.x as f64 + 100.0, u.loc.y as f64 + 100.0).into();
        let p = h.state.pointer_location;
        assert!(crate::input::grabs::start_move_at(&mut h.state, w.clone(), 0x110));
        let (rect, placement) = floating_of(&h, &w);
        assert_eq!(placement, Placement::Free);
        assert!(rect.to_f64().contains(p));
        c.pump(&mut h);
        assert!(!c.is_maximized(&n.toplevel));
    }

    #[test]
    fn leaving_de_restores_rects_and_tells_the_client_before_the_retile() {
        let mut h = harness();
        let mut c = Client::connect(&mut h);
        // A tile that becomes maximized under de goes back to its tile.
        let (_a, a) = map(&mut h, &mut c);
        let (b, bw) = map(&mut h, &mut c);
        let At::Tiled(id, _) = at(&h, &a) else { panic!() };
        let order = active(&h, id).tiled.windows();
        flip(&mut h, Mode::De);
        b.toplevel.set_maximized();
        c.pump(&mut h);
        assert!(c.is_maximized(&b.toplevel));
        flip(&mut h, Mode::Hybrid);
        c.pump(&mut h);
        assert!(!c.is_maximized(&b.toplevel), "the client is told");
        assert!(!maximize::is_maximized(&h.state, &bw));
        assert_eq!(active(&h, id).tiled.windows(), order, "re-tiled where it was");
        assert!(h.state.de.forget_is_clean());

        // A window that stays floating (a dialog) gets its old rect back.
        let (p, pw) = dialog(&mut h, &mut c, &b);
        flip(&mut h, Mode::De);
        let (before, _) = floating_of(&h, &pw);
        p.toplevel.set_maximized();
        c.pump(&mut h);
        assert_ne!(floating_of(&h, &pw).0, before);
        flip(&mut h, Mode::Wm);
        c.pump(&mut h);
        let (rect, placement) = floating_of(&h, &pw);
        assert_eq!(placement, Placement::Free);
        assert_eq!(rect, before);
        assert!(!c.is_maximized(&p.toplevel));
    }

    #[test]
    fn leaving_de_clears_a_minimized_or_fullscreen_maximize_too() {
        let mut h = harness();
        let mut c = Client::connect(&mut h);
        let (m, mw) = de_window(&mut h, &mut c);
        let (f, fw) = map(&mut h, &mut c);
        m.toplevel.set_maximized();
        c.pump(&mut h);
        focus(&mut h, &mw);
        minimize_focused(&mut h.state);
        f.toplevel.set_maximized();
        c.pump(&mut h);
        f.toplevel.set_fullscreen(None);
        c.pump(&mut h);
        flip(&mut h, Mode::Hybrid);
        assert!(!maximize::is_maximized(&h.state, &mw));
        assert!(!maximize::is_maximized(&h.state, &fw));
        unminimize_window(&mut h.state, &mw);
        // Opened under de, so default placement tiles it on leaving.
        assert!(
            matches!(at(&h, &mw), At::Tiled(..)),
            "no stale maximize holds it floating"
        );
        f.toplevel.unset_fullscreen();
        c.pump(&mut h);
        assert!(!c.is_maximized(&f.toplevel));
    }

    #[test]
    fn the_scene_state_bit_reports_maximized_under_de() {
        let mut h = harness();
        let mut c = Client::connect(&mut h);
        let (n, w) = de_window(&mut h, &mut c);
        let bit = ec_protocols::agent::server::eclipse_scene_v1::State::Maximized as u32;
        let handle = h.state.ipc.handle_for(&w) as u32;
        let states = |h: &Harness| crate::protocols::agent::facts(&h.state, handle, &w).states;
        assert_eq!(states(&h) & bit, 0);
        n.toplevel.set_maximized();
        c.pump(&mut h);
        c.commit(&mut h, &n.surface);
        assert!(
            w.toplevel()
                .unwrap()
                .current_state()
                .states
                .contains(XdgState::Maximized),
            "the client acked it"
        );
        assert_ne!(states(&h) & bit, 0, "reported maximized");
        n.toplevel.unset_maximized();
        c.pump(&mut h);
        c.commit(&mut h, &n.surface);
        assert_eq!(states(&h) & bit, 0);
    }

    #[test]
    fn the_toggle_maximize_bind_toggles_under_de_and_does_nothing_elsewhere() {
        let mut h = harness();
        let mut c = Client::connect(&mut h);
        let (n, w) = de_window(&mut h, &mut c);
        focus(&mut h, &w);
        maximize::toggle(&mut h.state);
        c.pump(&mut h);
        assert!(maximize::is_maximized(&h.state, &w));
        assert!(c.is_maximized(&n.toplevel));
        maximize::toggle(&mut h.state);
        c.pump(&mut h);
        assert!(!maximize::is_maximized(&h.state, &w));

        flip(&mut h, Mode::Hybrid);
        let (_b, bw) = map(&mut h, &mut c);
        focus(&mut h, &bw);
        maximize::toggle(&mut h.state);
        assert!(!maximize::is_maximized(&h.state, &bw));
        assert!(matches!(at(&h, &bw), At::Tiled(..)));
    }

    #[test]
    fn de_binds_super_page_up_to_toggle_maximize() {
        use crate::input::Action;
        let defaults = crate::config::mode_default_binds(Mode::De);
        assert!(defaults
            .iter()
            .any(|b| b.action == Action::ToggleMaximize && b.mods.logo && !b.mods.alt && !b.mods.shift));
        assert!(crate::config::mode_default_binds(Mode::Hybrid).is_empty());
    }
}
