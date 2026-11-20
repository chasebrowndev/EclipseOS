// SPDX-License-Identifier: AGPL-3.0-only
//! The login layout editor's rules, pure: what a drag, a resize, an add or a
//! remove does to a [`Layout`]. No iced, no I/O. The view asks; this answers
//! with a layout that always passes [`ec_layout::validate`], or `None` and the
//! view springs the tile back.
//!
//! Every position and size comes out of [`ec_layout::place`] or
//! [`ec_layout::resize`]; nothing here does its own overlap arithmetic.

use ec_layout::{place, resize_anchored, validate, Kind, Layout, Rect, REQUIRED};

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
pub fn removable(kind: Kind) -> bool {
    OPTIONAL.contains(&kind) && !REQUIRED.contains(&kind) && !kind.needs_session()
}

/// The optional tiles not on the screen, in tray order.
pub fn offered(l: &Layout) -> Vec<Kind> {
    OPTIONAL.into_iter().filter(|k| l.get(*k).is_none()).collect()
}

/// The size a tile arrives at, in cells. Never below its minimum.
pub fn default_size(kind: Kind) -> (i32, i32) {
    let (w, h) = match kind {
        Kind::Clock => (8, 3),
        Kind::Greeting => (8, 1),
        Kind::Power => (6, 1),
        Kind::Battery => (6, 3),
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
pub fn home(kind: Kind) -> (i32, i32) {
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
fn with(l: &Layout, kind: Kind, rect: Rect) -> Option<Layout> {
    let mut m = l.clone();
    m.set(kind, rect);
    validate(&m).ok().map(|()| m)
}

/// Where `kind` lands when its top-left is dragged to cell `(x, y)`: the
/// nearest free spot at its current size. `None` when it is not on the
/// screen or has no room.
pub fn drag_to(l: &Layout, kind: Kind, x: i32, y: i32) -> Option<Rect> {
    let cur = l.get(kind)?;
    place(l, kind, Rect::new(x, y, cur.w, cur.h))
}

/// Where `kind` lands when `handle` is dragged to cell `(px, py)`: the held
/// edge (or corner) goes to that grid line (exclusive for the east and south
/// edges, so a tile spanning cells 8..16 has its east edge at 16), and the edge
/// opposite it stays. An axis the handle does not own is untouched.
pub fn resize_to(l: &Layout, kind: Kind, handle: Handle, px: i32, py: i32) -> Option<Rect> {
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
        let r = resize_anchored(
            l,
            kind,
            Rect::new(x0, y0, x1 - x0, y1 - y0),
            handle.west(),
            handle.north(),
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
pub fn commit(l: &Layout, kind: Kind, rect: Rect) -> Option<Layout> {
    l.get(kind)?;
    with(l, kind, rect)
}

/// `kind` added at its default size, as near cell `at` as there is room (or
/// near its home when `at` is `None`). `None` for a kind that cannot be added
/// or when the screen has no room for it.
pub fn add(l: &Layout, kind: Kind, at: Option<(i32, i32)>) -> Option<Layout> {
    if !OPTIONAL.contains(&kind) || l.get(kind).is_some() {
        return None;
    }
    let (w, h) = default_size(kind);
    let (x, y) = at.unwrap_or_else(|| home(kind));
    let rect = place(l, kind, Rect::new(x, y, w, h))?;
    with(l, kind, rect)
}

/// `l` without `kind`. `None` for a required or session-only kind, or one
/// that is not there.
pub fn remove(l: &Layout, kind: Kind) -> Option<Layout> {
    if !removable(kind) || l.get(kind).is_none() {
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
            assert!(!removable(k));
            assert_eq!(remove(&l, k), None);
            assert!(!OPTIONAL.contains(&k));
        }
    }

    #[test]
    fn now_playing_is_never_addable_or_offered() {
        let l = Layout { tiles: vec![] };
        assert!(!OPTIONAL.contains(&Kind::NowPlaying));
        assert_eq!(add(&l, Kind::NowPlaying, None), None);
        assert!(!offered(&Layout::default()).contains(&Kind::NowPlaying));
        assert!(!removable(Kind::NowPlaying));
    }

    #[test]
    fn the_default_offers_what_it_lacks() {
        assert_eq!(offered(&Layout::default()), vec![Kind::Battery, Kind::Wifi]);
    }

    #[test]
    fn add_lands_valid_and_home_when_free() {
        let l = Layout::default();
        let m = add(&l, Kind::Battery, None).unwrap();
        ok(&m);
        let (w, h) = default_size(Kind::Battery);
        assert_eq!(m.get(Kind::Battery), Some(Rect::new(17, 1, w, h)));
        assert_eq!(add(&m, Kind::Battery, None), None, "no duplicates");
    }

    #[test]
    fn add_at_a_busy_cell_goes_to_the_nearest_free_one() {
        let l = Layout::default();
        let m = add(&l, Kind::Wifi, Some((9, 6))).unwrap(); // on top of Users
        ok(&m);
        assert!(m.get(Kind::Wifi).unwrap().in_grid());
    }

    #[test]
    fn add_with_no_room_is_refused() {
        let mut l = Layout { tiles: vec![] };
        l.set(Kind::Users, Rect::new(0, 0, COLS, 7));
        l.set(Kind::Password, Rect::new(0, 7, COLS, 7));
        ok(&l);
        assert_eq!(add(&l, Kind::Clock, None), None);
    }

    #[test]
    fn remove_then_add_round_trips_to_the_default() {
        let l = Layout::default();
        let m = remove(&l, Kind::Power).unwrap();
        ok(&m);
        assert_eq!(offered(&m)[0], Kind::Power);
        assert_eq!(remove(&m, Kind::Power), None);
        let back = add(&m, Kind::Power, None).unwrap();
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
                    if let Some(r) = drag_to(&l, kind, x, y) {
                        ok(&commit(&l, kind, r).expect("place returned a valid spot"));
                    }
                }
            }
        }
    }

    #[test]
    fn a_drag_onto_a_neighbour_is_pushed_clear() {
        let l = Layout::default();
        let r = drag_to(&l, Kind::Clock, 8, 6).unwrap();
        ok(&commit(&l, Kind::Clock, r).unwrap());
    }

    #[test]
    fn a_drag_keeps_the_size() {
        let l = Layout::default();
        let cur = l.get(Kind::Users).unwrap();
        let r = drag_to(&l, Kind::Users, 0, 0).unwrap();
        assert_eq!((r.w, r.h), (cur.w, cur.h));
    }

    #[test]
    fn resize_keeps_the_opposite_edge_and_the_axes_the_handle_does_not_own() {
        let l = Layout::default();
        let cur = l.get(Kind::Power).unwrap();
        let r = resize_to(&l, Kind::Power, Handle::East, cur.x + 3, cur.y + 9).unwrap();
        assert_eq!((r.x, r.y, r.h), (cur.x, cur.y, cur.h));
        let r = resize_to(&l, Kind::Clock, Handle::South, 99, 3).unwrap();
        assert_eq!(r.w, l.get(Kind::Clock).unwrap().w);
        // West and north drags keep the east and south edges.
        let u = l.get(Kind::Users).unwrap();
        let r = resize_to(&l, Kind::Users, Handle::West, u.x - 3, 0).unwrap();
        assert_eq!((r.x + r.w, r.y, r.h), (u.x + u.w, u.y, u.h));
        assert!(r.x < u.x);
        let r = resize_to(&l, Kind::Password, Handle::North, 0, 8).unwrap();
        let pw = l.get(Kind::Password).unwrap();
        assert_eq!((r.y + r.h, r.x, r.w), (pw.y + pw.h, pw.x, pw.w));
        let r = resize_to(&l, Kind::Users, Handle::NorthWest, u.x - 2, u.y - 1).unwrap();
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
                        if let Some(r) = resize_to(&l, kind, h, px, py) {
                            ok(&commit(&l, kind, r)
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
                    let r = resize_to(&l, kind, h, p, p).unwrap_or(cur);
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
        let r = resize_to(&l, Kind::Users, Handle::West, u.x + u.w + 5, 0).unwrap();
        assert_eq!(r.w, mw);
        assert_eq!(r.x + r.w, u.x + u.w, "the east edge held");
        let r = resize_to(&l, Kind::Users, Handle::East, u.x - 5, 0).unwrap();
        assert_eq!((r.x, r.w), (u.x, mw));
    }

    #[test]
    fn resize_is_clamped_by_neighbours() {
        // Pulling Clock's south edge into Users stops short of it.
        let l = Layout::default();
        let r = resize_to(&l, Kind::Clock, Handle::South, 0, 12).unwrap();
        let users = l.get(Kind::Users).unwrap();
        assert!(r.y + r.h <= users.y);
    }

    #[test]
    fn nothing_moves_a_tile_that_is_not_there() {
        let l = Layout::default();
        assert_eq!(drag_to(&l, Kind::Wifi, 0, 0), None);
        assert_eq!(resize_to(&l, Kind::Wifi, Handle::SouthEast, 5, 5), None);
        assert_eq!(commit(&l, Kind::Wifi, Rect::new(0, 0, 4, 3)), None);
    }

    #[test]
    fn commit_refuses_an_overlap() {
        let l = Layout::default();
        assert_eq!(commit(&l, Kind::Clock, Rect::new(8, 5, 8, 3)), None);
    }

    #[test]
    fn every_default_size_meets_its_minimum() {
        for k in ec_layout::ALL {
            let (w, h) = default_size(k);
            let (mw, mh) = k.min_size();
            assert!(w >= mw && h >= mh, "{k:?}");
        }
    }
}
