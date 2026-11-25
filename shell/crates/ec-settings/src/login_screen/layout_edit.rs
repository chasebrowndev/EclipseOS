// SPDX-License-Identifier: AGPL-3.0-only
//! The login layout editor's rules, pure: what a drag, a resize, an add or a
//! remove does to a [`Layout`]. No iced, no I/O. The view asks; this answers
//! with a layout that always passes [`ec_layout::validate`], or `None` and the
//! view springs the tile back.
//!
//! Every position and size comes out of [`ec_layout::place`] or
//! [`ec_layout::resize`]; nothing here does its own overlap arithmetic.

use ec_layout::{
    place_with, resize_anchored_with, validate, validate_with, Content, Kind, Layout, Rect, LOCK_KINDS,
    LOCK_REQUIRED, REQUIRED,
};

/// What a content-sized tile is measured for in the editor: a laptop, a mouse
/// and a keyboard (the builder's "Several" sample). The greeter shows however many
/// devices it finds, up to what fits at the tile's place (`ec_layout::fit`).
pub const CONTENT: Content = Content::PREVIEW;

/// `l` with every content-sized tile's stored size set to what [`CONTENT`]
/// needs, so the editor can read a tile's rectangle straight off the layout.
/// The stored size is never read by the greeter; this only keeps it honest.
pub fn normalized(l: Layout) -> Layout {
    normalized_for(Screen::Login, l)
}

/// [`normalized`] for `screen`'s content.
pub fn normalized_for(screen: Screen, mut l: Layout) -> Layout {
    for t in &mut l.tiles {
        t.rect = t.extent(screen.content());
    }
    l
}

/// The tiles a person may add or remove, in the order the tray offers them.
/// Users and Password are required and never appear here; NowPlaying needs a
/// session, which the login screen does not have, so it is never offered.
pub const OPTIONAL: [Kind; 5] = [
    Kind::Power,
    Kind::Battery,
    Kind::Wifi,
    Kind::Clock,
    Kind::Greeting,
];

/// Which screen's layout is being edited. The two share the engine and the
/// widget; they differ in what they offer, what they require, and what they draw.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Screen {
    Login,
    Lock,
}

impl Screen {
    /// The tiles a person may add or remove, in tray order.
    pub fn optional(self) -> &'static [Kind] {
        match self {
            Screen::Login => &OPTIONAL,
            // Password is required; everything else the lock draws is optional.
            Screen::Lock => &[Kind::Battery, Kind::Wifi, Kind::Clock, Kind::Greeting],
        }
    }

    /// What a content-sized tile is measured for. The login editor previews a
    /// laptop, a mouse and a keyboard; the lock draws its Battery at one
    /// gauge's footprint (what its reader normalises a stored size to).
    pub fn content(self) -> Content {
        match self {
            Screen::Login => CONTENT,
            Screen::Lock => Content::MIN,
        }
    }

    pub fn required(self) -> &'static [Kind] {
        match self {
            Screen::Login => &REQUIRED,
            Screen::Lock => &LOCK_REQUIRED,
        }
    }

    /// Is this a layout this screen may draw? The lock's editor writes only
    /// kinds the lock draws (the lock itself would ignore the rest).
    pub fn accepts(self, l: &Layout) -> bool {
        match self {
            Screen::Login => validate(l).is_ok(),
            Screen::Lock => {
                validate_with(l, &LOCK_REQUIRED).is_ok()
                    && l.tiles.iter().all(|t| LOCK_KINDS.contains(&t.kind))
            }
        }
    }
}

/// Which part of a tile is held to resize it: an edge or a corner. The edge
/// the handle is on moves; the opposite one stays, so a west or north drag
/// resizes from the far side.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Handle {
    North,
    South,
    East,
    West,
    NorthEast,
    NorthWest,
    SouthEast,
    SouthWest,
}

impl Handle {
    #[cfg(test)]
    pub const ALL: [Handle; 8] = [
        Handle::North,
        Handle::South,
        Handle::East,
        Handle::West,
        Handle::NorthEast,
        Handle::NorthWest,
        Handle::SouthEast,
        Handle::SouthWest,
    ];

    pub fn west(self) -> bool {
        matches!(self, Handle::West | Handle::NorthWest | Handle::SouthWest)
    }

    pub fn east(self) -> bool {
        matches!(self, Handle::East | Handle::NorthEast | Handle::SouthEast)
    }

    pub fn north(self) -> bool {
        matches!(self, Handle::North | Handle::NorthWest | Handle::NorthEast)
    }

    pub fn south(self) -> bool {
        matches!(self, Handle::South | Handle::SouthWest | Handle::SouthEast)
    }
}

/// May this tile be taken off the screen?
pub fn removable(screen: Screen, kind: Kind) -> bool {
    screen.optional().contains(&kind) && !screen.required().contains(&kind) && !kind.needs_session()
}

/// The optional tiles not on the screen, in tray order.
pub fn offered(screen: Screen, l: &Layout) -> Vec<Kind> {
    screen
        .optional()
        .iter()
        .copied()
        .filter(|k| l.get(*k).is_none())
        .collect()
}

/// The size a tile arrives at, in cells. Never below its minimum.
pub fn default_size(screen: Screen, kind: Kind) -> (i32, i32) {
    if let Some(size) = kind.intrinsic_size(screen.content()) {
        return size;
    }
    let (w, h) = match kind {
        Kind::Clock => (8, 3),
        Kind::Greeting => (8, 1),
        Kind::Power => (6, 1),
        Kind::Battery => (4, 2),
        Kind::Wifi => (4, 3),
        Kind::Users => (8, 4),
        Kind::Password => (8, 2),
        Kind::NowPlaying => (8, 3),
    };
    let (mw, mh) = kind.min_size();
    (w.max(mw), h.max(mh))
}

/// Where a tile likes to sit, so taking it off and putting it back brings it
/// home when there is room.
pub fn home(screen: Screen, kind: Kind) -> (i32, i32) {
    if screen == Screen::Lock {
        // Around the lock's built-in arrangement (`ec_lock::face::builtin`).
        return match kind {
            Kind::Clock => (6, 3),
            Kind::Greeting => (8, 7),
            Kind::Password => (8, 8),
            Kind::Battery => (18, 1),
            _ => (18, 5),
        };
    }
    match kind {
        Kind::Clock => (8, 1),
        Kind::Greeting => (8, 4),
        Kind::Users => (8, 5),
        Kind::Password => (8, 9),
        Kind::Power => (9, 12),
        Kind::Battery => (17, 1),
        Kind::Wifi => (17, 8),
        Kind::NowPlaying => (8, 1),
    }
}

/// `l` with `kind` at `rect`, if that is a layout the greeter accepts.
fn with(screen: Screen, l: &Layout, kind: Kind, rect: Rect) -> Option<Layout> {
    let mut m = l.clone();
    m.set(kind, rect);
    let m = normalized_for(screen, m);
    (screen.accepts(&m) && previews_clear(screen, &m)).then_some(m)
}

/// Every tile, content-sized ones at [`CONTENT`], is on the grid and clear of
/// the others: what the editor draws is what it accepted.
fn previews_clear(screen: Screen, l: &Layout) -> bool {
    l.tiles.iter().enumerate().all(|(i, t)| {
        let r = t.extent(screen.content());
        r.in_grid()
            && l.tiles[..i]
                .iter()
                .all(|o| !o.extent(screen.content()).overlaps(r))
    })
}

/// Where `kind` lands when its top-left is dragged to cell `(x, y)`: the
/// nearest free spot at its current size. `None` when it is not on the
/// screen or has no room.
pub fn drag_to(screen: Screen, l: &Layout, kind: Kind, x: i32, y: i32) -> Option<Rect> {
    let cur = l.get(kind)?;
    place_with(l, kind, Rect::new(x, y, cur.w, cur.h), screen.content())
}

/// Where `kind` lands when `handle` is dragged to cell `(px, py)`: the held
/// edge (or corner) goes to that grid line (exclusive for the east and south
/// edges, so a tile spanning cells 8..16 has its east edge at 16), and the edge
/// opposite it stays. An axis the handle does not own is untouched.
///
/// A content-sized tile has no size to change: `None`.
pub fn resize_to(screen: Screen, l: &Layout, kind: Kind, handle: Handle, px: i32, py: i32) -> Option<Rect> {
    if kind.is_intrinsic() {
        return None;
    }
    let cur = l.get(kind)?;
    let (own_x, own_y) = (handle.west() || handle.east(), handle.north() || handle.south());
    let (mut px, mut py) = (px, py);
    loop {
        let (mut x0, mut x1) = (cur.x, cur.x + cur.w);
        let (mut y0, mut y1) = (cur.y, cur.y + cur.h);
        if handle.west() {
            x0 = px;
        }
        if handle.east() {
            x1 = px;
        }
        if handle.north() {
            y0 = py;
        }
        if handle.south() {
            y1 = py;
        }
        let r = resize_anchored_with(
            l,
            kind,
            Rect::new(x0, y0, x1 - x0, y1 - y0),
            handle.west(),
            handle.north(),
            screen.content(),
        )?;
        // `ec_layout` clears a neighbour by shrinking whichever side is
        // longer, which for an edge handle can be the axis it does not own.
        // Pull the held edge back toward the tile until only its own axis
        // changes.
        let kept = if own_x && !own_y {
            (r.y, r.h) == (cur.y, cur.h)
        } else if own_y && !own_x {
            (r.x, r.w) == (cur.x, cur.w)
        } else {
            true
        };
        if kept {
            return Some(r);
        }
        let edge = if own_x {
            if handle.west() {
                cur.x
            } else {
                cur.x + cur.w
            }
        } else if handle.north() {
            cur.y
        } else {
            cur.y + cur.h
        };
        let held = if own_x { &mut px } else { &mut py };
        if *held == edge {
            return Some(cur);
        }
        *held += (edge - *held).signum();
    }
}

/// `l` with `kind` moved to `rect`. `None` if the result would not validate.
pub fn commit(screen: Screen, l: &Layout, kind: Kind, rect: Rect) -> Option<Layout> {
    l.get(kind)?;
    with(screen, l, kind, rect)
}

/// `kind` added at its default size, as near cell `at` as there is room (or
/// near its home when `at` is `None`). `None` for a kind that cannot be added
/// or when the screen has no room for it.
pub fn add(screen: Screen, l: &Layout, kind: Kind, at: Option<(i32, i32)>) -> Option<Layout> {
    if !screen.optional().contains(&kind) || l.get(kind).is_some() {
        return None;
    }
    let (w, h) = default_size(screen, kind);
    let (x, y) = at.unwrap_or_else(|| home(screen, kind));
    let rect = place_with(l, kind, Rect::new(x, y, w, h), screen.content())?;
    with(screen, l, kind, rect)
}

/// `l` without `kind`. `None` for a required or session-only kind, or one
/// that is not there.
pub fn remove(screen: Screen, l: &Layout, kind: Kind) -> Option<Layout> {
    if !removable(screen, kind) || l.get(kind).is_none() {
        return None;
    }
    let mut m = l.clone();
    m.remove(kind).then_some(m)
}

#[cfg(test)]
mod tests {
    use super::*;
    use ec_layout::{COLS, ROWS};

    fn ok(l: &Layout) {
        assert_eq!(validate(l), Ok(()), "{l:?}");
    }

    #[test]
    fn required_tiles_cannot_be_removed_and_are_never_offered() {
        let l = Layout::default();
        for k in REQUIRED {
            assert!(!removable(Screen::Login, k));
            assert_eq!(remove(Screen::Login, &l, k), None);
            assert!(!OPTIONAL.contains(&k));
        }
    }

    #[test]
    fn now_playing_is_never_addable_or_offered() {
        let l = Layout { tiles: vec![] };
        assert!(!OPTIONAL.contains(&Kind::NowPlaying));
        assert_eq!(add(Screen::Login, &l, Kind::NowPlaying, None), None);
        assert!(!offered(Screen::Login, &Layout::default()).contains(&Kind::NowPlaying));
        assert!(!removable(Screen::Login, Kind::NowPlaying));
    }

    #[test]
    fn the_default_offers_what_it_lacks() {
        assert_eq!(
            offered(Screen::Login, &Layout::default()),
            vec![Kind::Battery, Kind::Wifi]
        );
    }

    #[test]
    fn add_lands_valid_and_home_when_free() {
        let l = Layout::default();
        let m = add(Screen::Login, &l, Kind::Battery, None).unwrap();
        ok(&m);
        let (w, h) = default_size(Screen::Login, Kind::Battery);
        assert_eq!(m.get(Kind::Battery), Some(Rect::new(17, 1, w, h)));
        assert_eq!(add(Screen::Login, &m, Kind::Battery, None), None, "no duplicates");
    }

    #[test]
    fn add_at_a_busy_cell_goes_to_the_nearest_free_one() {
        let l = Layout::default();
        let m = add(Screen::Login, &l, Kind::Wifi, Some((9, 6))).unwrap(); // on top of Users
        ok(&m);
        assert!(m.get(Kind::Wifi).unwrap().in_grid());
    }

    #[test]
    fn add_with_no_room_is_refused() {
        let mut l = Layout { tiles: vec![] };
        l.set(Kind::Users, Rect::new(0, 0, COLS, 7));
        l.set(Kind::Password, Rect::new(0, 7, COLS, 7));
        ok(&l);
        assert_eq!(add(Screen::Login, &l, Kind::Clock, None), None);
    }

    #[test]
    fn remove_then_add_round_trips_to_the_default() {
        let l = Layout::default();
        let m = remove(Screen::Login, &l, Kind::Power).unwrap();
        ok(&m);
        assert_eq!(offered(Screen::Login, &m)[0], Kind::Power);
        assert_eq!(remove(Screen::Login, &m, Kind::Power), None);
        let back = add(Screen::Login, &m, Kind::Power, None).unwrap();
        assert_eq!(back.get(Kind::Power), l.get(Kind::Power));
    }

    #[test]
    fn a_drag_always_commits_a_valid_layout_or_nothing() {
        let l = Layout::default();
        for kind in [
            Kind::Clock,
            Kind::Users,
            Kind::Password,
            Kind::Power,
            Kind::Greeting,
        ] {
            for x in -3..COLS + 3 {
                for y in -3..ROWS + 3 {
                    if let Some(r) = drag_to(Screen::Login, &l, kind, x, y) {
                        ok(&commit(Screen::Login, &l, kind, r).expect("place returned a valid spot"));
                    }
                }
            }
        }
    }

    #[test]
    fn a_drag_onto_a_neighbour_is_pushed_clear() {
        let l = Layout::default();
        let r = drag_to(Screen::Login, &l, Kind::Clock, 8, 6).unwrap();
        ok(&commit(Screen::Login, &l, Kind::Clock, r).unwrap());
    }

    #[test]
    fn a_drag_keeps_the_size() {
        let l = Layout::default();
        let cur = l.get(Kind::Users).unwrap();
        let r = drag_to(Screen::Login, &l, Kind::Users, 0, 0).unwrap();
        assert_eq!((r.w, r.h), (cur.w, cur.h));
    }

    #[test]
    fn resize_keeps_the_opposite_edge_and_the_axes_the_handle_does_not_own() {
        let l = Layout::default();
        let cur = l.get(Kind::Power).unwrap();
        let r = resize_to(Screen::Login, &l, Kind::Power, Handle::East, cur.x + 3, cur.y + 9).unwrap();
        assert_eq!((r.x, r.y, r.h), (cur.x, cur.y, cur.h));
        let r = resize_to(Screen::Login, &l, Kind::Clock, Handle::South, 99, 3).unwrap();
        assert_eq!(r.w, l.get(Kind::Clock).unwrap().w);
        // West and north drags keep the east and south edges.
        let u = l.get(Kind::Users).unwrap();
        let r = resize_to(Screen::Login, &l, Kind::Users, Handle::West, u.x - 3, 0).unwrap();
        assert_eq!((r.x + r.w, r.y, r.h), (u.x + u.w, u.y, u.h));
        assert!(r.x < u.x);
        let r = resize_to(Screen::Login, &l, Kind::Password, Handle::North, 0, 8).unwrap();
        let pw = l.get(Kind::Password).unwrap();
        assert_eq!((r.y + r.h, r.x, r.w), (pw.y + pw.h, pw.x, pw.w));
        let r = resize_to(
            Screen::Login,
            &l,
            Kind::Users,
            Handle::NorthWest,
            u.x - 2,
            u.y - 1,
        )
        .unwrap();
        assert_eq!((r.x + r.w, r.y + r.h), (u.x + u.w, u.y + u.h));
    }

    #[test]
    fn every_edge_and_corner_drag_stays_valid_and_above_the_minimum() {
        let l = Layout::default();
        for kind in [
            Kind::Clock,
            Kind::Users,
            Kind::Password,
            Kind::Power,
            Kind::Greeting,
        ] {
            let (mw, mh) = kind.min_size();
            for h in Handle::ALL {
                for px in -3..COLS + 4 {
                    for py in -3..ROWS + 4 {
                        if let Some(r) = resize_to(Screen::Login, &l, kind, h, px, py) {
                            ok(&commit(Screen::Login, &l, kind, r)
                                .unwrap_or_else(|| panic!("{kind:?} {h:?} {px},{py} {r:?}")));
                            assert!(r.w >= mw && r.h >= mh, "{kind:?} {h:?} {r:?}");
                            assert!(r.in_grid());
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn an_edge_drag_only_ever_changes_its_own_axis() {
        let l = Layout::default();
        for kind in [
            Kind::Clock,
            Kind::Users,
            Kind::Password,
            Kind::Power,
            Kind::Greeting,
        ] {
            let cur = l.get(kind).unwrap();
            for h in [Handle::North, Handle::South, Handle::East, Handle::West] {
                for p in -3..COLS + 4 {
                    let r = resize_to(Screen::Login, &l, kind, h, p, p).unwrap_or(cur);
                    if matches!(h, Handle::North | Handle::South) {
                        assert_eq!((r.x, r.w), (cur.x, cur.w), "{kind:?} {h:?} {p}");
                    } else {
                        assert_eq!((r.y, r.h), (cur.y, cur.h), "{kind:?} {h:?} {p}");
                    }
                }
            }
        }
    }

    #[test]
    fn a_drag_past_the_far_side_stops_at_the_minimum_not_inside_out() {
        let l = Layout::default();
        let u = l.get(Kind::Users).unwrap();
        let (mw, _) = Kind::Users.min_size();
        let r = resize_to(Screen::Login, &l, Kind::Users, Handle::West, u.x + u.w + 5, 0).unwrap();
        assert_eq!(r.w, mw);
        assert_eq!(r.x + r.w, u.x + u.w, "the east edge held");
        let r = resize_to(Screen::Login, &l, Kind::Users, Handle::East, u.x - 5, 0).unwrap();
        assert_eq!((r.x, r.w), (u.x, mw));
    }

    #[test]
    fn resize_is_clamped_by_neighbours() {
        // Pulling Clock's south edge into Users stops short of it.
        let l = Layout::default();
        let r = resize_to(Screen::Login, &l, Kind::Clock, Handle::South, 0, 12).unwrap();
        let users = l.get(Kind::Users).unwrap();
        assert!(r.y + r.h <= users.y);
    }

    #[test]
    fn nothing_moves_a_tile_that_is_not_there() {
        let l = Layout::default();
        assert_eq!(drag_to(Screen::Login, &l, Kind::Wifi, 0, 0), None);
        assert_eq!(
            resize_to(Screen::Login, &l, Kind::Wifi, Handle::SouthEast, 5, 5),
            None
        );
        assert_eq!(commit(Screen::Login, &l, Kind::Wifi, Rect::new(0, 0, 4, 3)), None);
    }

    #[test]
    fn commit_refuses_an_overlap() {
        let l = Layout::default();
        assert_eq!(
            commit(Screen::Login, &l, Kind::Clock, Rect::new(8, 5, 8, 3)),
            None
        );
    }

    #[test]
    fn every_default_size_meets_its_minimum() {
        for k in ec_layout::ALL {
            let (w, h) = default_size(Screen::Login, k);
            let (mw, mh) = k.min_size();
            assert!(w >= mw && h >= mh, "{k:?}");
        }
    }

    #[test]
    fn a_content_tile_is_added_at_its_intrinsic_size_and_cannot_be_resized() {
        let m = add(Screen::Login, &Layout::default(), Kind::Battery, None).unwrap();
        let (w, h) = Kind::Battery.intrinsic_size(CONTENT).unwrap();
        let r = m.get(Kind::Battery).unwrap();
        assert_eq!((r.w, r.h), (w, h));
        for hd in Handle::ALL {
            for px in -2..COLS + 3 {
                for py in -2..ROWS + 3 {
                    assert_eq!(
                        resize_to(Screen::Login, &m, Kind::Battery, hd, px, py),
                        None,
                        "{hd:?}"
                    );
                }
            }
        }
    }

    #[test]
    fn dragging_a_content_tile_keeps_its_size_and_never_overlaps() {
        let m = add(Screen::Login, &Layout::default(), Kind::Battery, None).unwrap();
        let (w, h) = Kind::Battery.intrinsic_size(CONTENT).unwrap();
        for x in -3..COLS + 3 {
            for y in -3..ROWS + 3 {
                let r = drag_to(Screen::Login, &m, Kind::Battery, x, y).expect("there is room");
                assert_eq!((r.w, r.h), (w, h));
                let c = commit(Screen::Login, &m, Kind::Battery, r).expect("a placed spot commits");
                ok(&c);
                assert!(previews_clear(Screen::Login, &c));
            }
        }
    }

    #[test]
    fn other_tiles_keep_clear_of_the_previewed_size_not_the_stored_one() {
        let m = add(Screen::Login, &Layout::default(), Kind::Battery, None).unwrap();
        let b = m.get(Kind::Battery).unwrap();
        for x in 0..COLS {
            for y in 0..ROWS {
                if let Some(r) = drag_to(Screen::Login, &m, Kind::Clock, x, y) {
                    assert!(!r.overlaps(b), "{r:?} on {b:?}");
                }
            }
        }
    }

    #[test]
    fn a_hand_made_commit_that_would_overlap_the_preview_is_refused() {
        let m = add(Screen::Login, &Layout::default(), Kind::Battery, None).unwrap();
        // Clock is 8..16 wide; a battery at 14 spans 14..18, over it.
        assert_eq!(
            commit(Screen::Login, &m, Kind::Battery, Rect::new(14, 1, 4, 2)),
            None
        );
        // At 16 it touches and fits, and a stored size of 9x9 is replaced.
        let c = commit(Screen::Login, &m, Kind::Battery, Rect::new(16, 1, 9, 9)).unwrap();
        assert_eq!(c.get(Kind::Battery), Some(Rect::new(16, 1, 4, 2)));
    }

    #[test]
    fn an_old_file_battery_is_normalised_to_the_intrinsic_size() {
        let mut l = Layout::default();
        l.set(Kind::Battery, Rect::new(17, 1, 8, 5));
        let n = normalized(l);
        assert_eq!(n.get(Kind::Battery), Some(Rect::new(17, 1, 4, 2)));
        ok(&n);
    }

    #[test]
    fn the_lock_offers_only_what_it_draws_and_requires_only_the_field() {
        let l = Layout::lock_default();
        assert!(Screen::Lock.accepts(&l));
        assert_eq!(
            offered(Screen::Lock, &l),
            vec![Kind::Battery, Kind::Wifi],
            "Clock and Greeting are placed; Users, Power and NowPlaying are never offered"
        );
        for k in [Kind::Users, Kind::Power, Kind::NowPlaying] {
            assert_eq!(add(Screen::Lock, &Layout::lock_default(), k, None), None, "{k:?}");
        }
        assert!(!removable(Screen::Lock, Kind::Password));
        assert_eq!(remove(Screen::Lock, &l, Kind::Password), None);
        assert!(removable(Screen::Lock, Kind::Clock));
    }

    #[test]
    fn a_lock_add_and_remove_round_trips_and_stays_valid() {
        let l = Layout::lock_default();
        let m = add(Screen::Lock, &l, Kind::Battery, None).unwrap();
        assert!(Screen::Lock.accepts(&m));
        assert_eq!(remove(Screen::Lock, &m, Kind::Battery), Some(l));
    }

    #[test]
    fn every_lock_kind_is_drawable_by_the_lock() {
        // The editor's tray and the lock's own tile set cannot drift apart.
        for k in ec_layout::LOCK_KINDS {
            assert!(
                Screen::Lock.required().contains(&k) || Screen::Lock.optional().contains(&k),
                "{k:?} is drawn by the lock but not editable"
            );
        }
    }
}
