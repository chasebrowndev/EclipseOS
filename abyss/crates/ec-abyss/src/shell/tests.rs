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
}
