// SPDX-License-Identifier: AGPL-3.0-only
//! Tile layout on a snapped grid. Pure: no iced, no I/O, no allocation beyond
//! the tile list, so a root helper and a greeter can both link it.
//!
//! The grid is [`COLS`] x [`ROWS`] cells whatever the output size; cells scale
//! to the output ([`Rect::to_px`]), so a layout fits any aspect ratio. Tiles
//! never overlap and never leave the grid. [`place`] and [`resize`] are how an
//! editor moves a tile: they always return a valid spot or `None`.
//! [`validate`] is the gate for anything read from disk.

pub const COLS: i32 = 24;
pub const ROWS: i32 = 14;

/// Every kind appears at most once; [`REQUIRED`] kinds must appear.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Kind {
    Clock,
    Greeting,
    Users,
    Password,
    Power,
    // Glances: small live-info panes (ADR 0126).
    Battery,
    Wifi,
    NowPlaying,
}

pub const ALL: [Kind; 8] = [
    Kind::Clock,
    Kind::Greeting,
    Kind::Users,
    Kind::Password,
    Kind::Power,
    Kind::Battery,
    Kind::Wifi,
    Kind::NowPlaying,
];

/// A layout without these cannot log anyone in.
pub const REQUIRED: [Kind; 2] = [Kind::Users, Kind::Password];

impl Kind {
    /// The key used in the settings file.
    pub const fn name(self) -> &'static str {
        match self {
            Kind::Clock => "clock",
            Kind::Greeting => "greeting",
            Kind::Users => "users",
            Kind::Password => "password",
            Kind::Power => "power",
            Kind::Battery => "battery",
            Kind::Wifi => "wifi",
            Kind::NowPlaying => "now_playing",
        }
    }

    pub fn from_name(s: &str) -> Option<Kind> {
        ALL.into_iter().find(|k| k.name() == s)
    }

    /// Smallest `(w, h)` in cells at which the tile is usable.
    pub const fn min_size(self) -> (i32, i32) {
        match self {
            Kind::Clock => (4, 2),
            Kind::Greeting => (4, 1),
            Kind::Users => (4, 3),
            Kind::Password => (6, 2),
            Kind::Power => (3, 1),
            Kind::Battery => (3, 1),
            Kind::Wifi => (2, 1),
            Kind::NowPlaying => (6, 2),
        }
    }

    /// Needs the user's session (its bus), so it cannot appear before login.
    pub const fn needs_session(self) -> bool {
        matches!(self, Kind::NowPlaying)
    }
}

/// Grid cells. `w` and `h` are positive in a valid tile.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rect {
    pub x: i32,
    pub y: i32,
    pub w: i32,
    pub h: i32,
}

impl Rect {
    pub const fn new(x: i32, y: i32, w: i32, h: i32) -> Self {
        Rect { x, y, w, h }
    }

    /// Strict overlap: tiles that only share an edge do not overlap.
    pub const fn overlaps(self, o: Rect) -> bool {
        self.x < o.x + o.w && o.x < self.x + self.w && self.y < o.y + o.h && o.y < self.y + self.h
    }

    pub const fn in_grid(self) -> bool {
        self.x >= 0
            && self.y >= 0
            && self.w > 0
            && self.h > 0
            && self.x + self.w <= COLS
            && self.y + self.h <= ROWS
    }

    /// `(x, y, w, h)` in pixels on a `width` x `height` output, with `gap`
    /// pixels of breathing room taken off every side so neighbours never touch.
    pub fn to_px(self, width: u32, height: u32, gap: u32) -> (u32, u32, u32, u32) {
        let edge = |c: i32, n: i32, total: u32| (c as i64 * total as i64 / n as i64) as u32;
        let (x0, x1) = (edge(self.x, COLS, width), edge(self.x + self.w, COLS, width));
        let (y0, y1) = (edge(self.y, ROWS, height), edge(self.y + self.h, ROWS, height));
        let g = gap.min((x1 - x0) / 2).min((y1 - y0) / 2);
        (x0 + g, y0 + g, x1 - x0 - 2 * g, y1 - y0 - 2 * g)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Tile {
    pub kind: Kind,
    pub rect: Rect,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Layout {
    pub tiles: Vec<Tile>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    Duplicate(Kind),
    Missing(Kind),
    OutOfGrid(Kind),
    TooSmall(Kind),
    Overlap(Kind, Kind),
}

impl Default for Layout {
    /// What the greeter draws when nothing is configured or the file is bad.
    fn default() -> Self {
        let t = |kind, x, y, w, h| Tile {
            kind,
            rect: Rect::new(x, y, w, h),
        };
        Layout {
            tiles: vec![
                t(Kind::Clock, 8, 1, 8, 3),
                t(Kind::Greeting, 8, 4, 8, 1),
                t(Kind::Users, 8, 5, 8, 4),
                t(Kind::Password, 8, 9, 8, 2),
                t(Kind::Power, 9, 12, 6, 1),
            ],
        }
    }
}

impl Layout {
    pub fn get(&self, kind: Kind) -> Option<Rect> {
        self.tiles.iter().find(|t| t.kind == kind).map(|t| t.rect)
    }

    /// Set (or add) a tile without checking it; call [`validate`] after.
    pub fn set(&mut self, kind: Kind, rect: Rect) {
        match self.tiles.iter_mut().find(|t| t.kind == kind) {
            Some(t) => t.rect = rect,
            None => self.tiles.push(Tile { kind, rect }),
        }
    }

    /// Remove an optional tile. Required kinds cannot be removed.
    pub fn remove(&mut self, kind: Kind) -> bool {
        if REQUIRED.contains(&kind) {
            return false;
        }
        self.tiles.retain(|t| t.kind != kind);
        true
    }

    /// The topmost tile under a grid cell.
    pub fn hit(&self, x: i32, y: i32) -> Option<Kind> {
        let p = Rect::new(x, y, 1, 1);
        self.tiles
            .iter()
            .rev()
            .find(|t| t.rect.overlaps(p))
            .map(|t| t.kind)
    }
}

/// The most bytes [`Layout::from_lines`] will look at: the settings files'
/// own cap, so a layout file can never be a way to make a reader allocate.
pub const MAX_LINES_BYTES: usize = 4096;

/// Why [`Layout::from_lines`] refused a text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ParseError {
    /// Longer than [`MAX_LINES_BYTES`].
    TooLong,
    /// Not `key = value`, or a blank line.
    Line,
    /// A key that is not `tile.<kind>`.
    Key,
    /// The same kind twice.
    Duplicate,
    /// A value that is not `"x y w h"` in plain decimal cells.
    Value,
}

/// `"x y w h"`: four plain decimal cells, single spaces, nothing else.
fn parse_cells(s: &str) -> Option<Rect> {
    let mut it = s.split(' ').map(|n| {
        let plain = !n.is_empty() && n.len() <= 3 && n.bytes().all(|b| b.is_ascii_digit());
        plain.then(|| n.parse::<i32>().ok()).flatten()
    });
    let r = Rect::new(it.next()??, it.next()??, it.next()??, it.next()??);
    it.next().is_none().then_some(r)
}

impl Layout {
    /// The `tile.<kind> = "x y w h"` lines of the settings files, one per
    /// tile, each ending in a newline.
    pub fn to_lines(&self) -> String {
        let mut out = String::new();
        for t in &self.tiles {
            let r = t.rect;
            out += &format!("tile.{} = \"{} {} {} {}\"\n", t.kind.name(), r.x, r.y, r.w, r.h);
        }
        out
    }

    /// Parse the lines [`Layout::to_lines`] writes. Strict: an unknown or
    /// repeated key, a bad number, a blank line or a text over
    /// [`MAX_LINES_BYTES`] fails the whole text. Parsing is not validation:
    /// run [`validate`] on the result before drawing it.
    pub fn from_lines(text: &str) -> Result<Layout, ParseError> {
        if text.len() > MAX_LINES_BYTES {
            return Err(ParseError::TooLong);
        }
        let mut tiles: Vec<Tile> = Vec::new();
        for line in text.lines() {
            let (key, value) = line.split_once(" = ").ok_or(ParseError::Line)?;
            let kind = key
                .strip_prefix("tile.")
                .and_then(Kind::from_name)
                .ok_or(ParseError::Key)?;
            if tiles.iter().any(|t| t.kind == kind) {
                return Err(ParseError::Duplicate);
            }
            let rect = value
                .strip_prefix('"')
                .and_then(|v| v.strip_suffix('"'))
                .and_then(parse_cells)
                .ok_or(ParseError::Value)?;
            tiles.push(Tile { kind, rect });
        }
        Ok(Layout { tiles })
    }
}

/// Is this layout one the greeter may draw? Checked on read and again by the
/// root helper before anything is written.
pub fn validate(l: &Layout) -> Result<(), Error> {
    for (i, t) in l.tiles.iter().enumerate() {
        if l.tiles[..i].iter().any(|o| o.kind == t.kind) {
            return Err(Error::Duplicate(t.kind));
        }
        if !t.rect.in_grid() {
            return Err(Error::OutOfGrid(t.kind));
        }
        let (mw, mh) = t.kind.min_size();
        if t.rect.w < mw || t.rect.h < mh {
            return Err(Error::TooSmall(t.kind));
        }
        if let Some(o) = l.tiles[..i].iter().find(|o| o.rect.overlaps(t.rect)) {
            return Err(Error::Overlap(o.kind, t.kind));
        }
    }
    REQUIRED
        .into_iter()
        .find(|k| l.get(*k).is_none())
        .map_or(Ok(()), |k| Err(Error::Missing(k)))
}

fn free(l: &Layout, kind: Kind, r: Rect) -> bool {
    r.in_grid() && !l.tiles.iter().any(|t| t.kind != kind && t.rect.overlaps(r))
}

/// Where `kind` lands when dragged so its corner is at `want`'s: the nearest
/// grid position at its current size that overlaps nothing. `None` when the
/// layout has no room for it anywhere; the editor then drops the move.
pub fn place(l: &Layout, kind: Kind, want: Rect) -> Option<Rect> {
    let (mw, mh) = kind.min_size();
    let (w, h) = (want.w.clamp(mw, COLS), want.h.clamp(mh, ROWS));
    let mut best: Option<(i64, Rect)> = None;
    for y in 0..=ROWS - h {
        for x in 0..=COLS - w {
            let r = Rect::new(x, y, w, h);
            if !free(l, kind, r) {
                continue;
            }
            let (dx, dy) = ((x - want.x) as i64, (y - want.y) as i64);
            let d = dx * dx + dy * dy;
            // Scan order breaks ties: lower y, then lower x.
            if best.is_none_or(|(b, _)| d < b) {
                best = Some((d, r));
            }
        }
    }
    best.map(|(_, r)| r)
}

/// `kind` resized to `want` keeping its top-left corner: shrunk (never below
/// its minimum) until it overlaps nothing. `None` if even the minimum does.
pub fn resize(l: &Layout, kind: Kind, want: Rect) -> Option<Rect> {
    resize_anchored(l, kind, want, false, false)
}

/// [`resize`] for a drag of any edge. `want` is the whole rectangle the handle
/// asks for. When `fix_right` (a west-edge drag) the right edge is the one that
/// stays put as the tile shrinks to clear a neighbour, and likewise `fix_bottom`
/// for a north-edge drag; otherwise the left or top edge stays.
pub fn resize_anchored(
    l: &Layout,
    kind: Kind,
    want: Rect,
    fix_right: bool,
    fix_bottom: bool,
) -> Option<Rect> {
    let (mw, mh) = kind.min_size();
    let (right, bottom) = (want.x + want.w, want.y + want.h);
    let (mut x, mut y) = (want.x.clamp(0, COLS - mw), want.y.clamp(0, ROWS - mh));
    let (mut w, mut h) = ((right - x).clamp(mw, COLS - x), (bottom - y).clamp(mh, ROWS - y));
    if fix_right {
        x = (right.min(COLS) - w).max(0);
    }
    if fix_bottom {
        y = (bottom.min(ROWS) - h).max(0);
    }
    loop {
        let r = Rect::new(x, y, w, h);
        if free(l, kind, r) {
            return Some(r);
        }
        if w > mw && w >= h {
            w -= 1;
            x += fix_right as i32;
        } else if h > mh {
            h -= 1;
            y += fix_bottom as i32;
        } else if w > mw {
            w -= 1;
            x += fix_right as i32;
        } else {
            return None;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_default_is_valid() {
        assert_eq!(validate(&Layout::default()), Ok(()));
    }

    #[test]
    fn required_tiles_cannot_be_removed() {
        let mut l = Layout::default();
        assert!(!l.remove(Kind::Password));
        assert!(l.remove(Kind::Clock));
        assert_eq!(validate(&l), Ok(()));
    }

    #[test]
    fn validate_rejects_each_fault() {
        let mut l = Layout::default();
        l.set(Kind::Clock, Rect::new(8, 5, 8, 3));
        assert!(matches!(validate(&l), Err(Error::Overlap(..))));

        let mut l = Layout::default();
        l.set(Kind::Power, Rect::new(22, 12, 3, 1));
        assert_eq!(validate(&l), Err(Error::OutOfGrid(Kind::Power)));

        let mut l = Layout::default();
        l.set(Kind::Password, Rect::new(8, 9, 2, 2));
        assert_eq!(validate(&l), Err(Error::TooSmall(Kind::Password)));

        let mut l = Layout::default();
        l.tiles.push(Tile {
            kind: Kind::Clock,
            rect: Rect::new(0, 0, 4, 2),
        });
        assert_eq!(validate(&l), Err(Error::Duplicate(Kind::Clock)));

        let mut l = Layout::default();
        l.tiles.retain(|t| t.kind != Kind::Users);
        assert_eq!(validate(&l), Err(Error::Missing(Kind::Users)));
    }

    #[test]
    fn a_free_drop_lands_where_asked() {
        let l = Layout::default();
        let r = place(&l, Kind::Clock, Rect::new(0, 0, 8, 3)).unwrap();
        assert_eq!(r, Rect::new(0, 0, 8, 3));
    }

    #[test]
    fn a_drop_on_a_neighbour_is_pushed_to_the_nearest_free_spot() {
        let l = Layout::default();
        let want = Rect::new(8, 6, 8, 3); // on top of Users
        let r = place(&l, Kind::Clock, want).unwrap();
        let mut m = l.clone();
        m.set(Kind::Clock, r);
        assert_eq!(validate(&m), Ok(()));
    }

    #[test]
    fn a_drop_off_the_grid_is_pulled_back() {
        let l = Layout::default();
        let r = place(&l, Kind::Power, Rect::new(40, 30, 3, 1)).unwrap();
        assert!(r.in_grid());
    }

    #[test]
    fn a_full_grid_has_no_room() {
        let mut l = Layout { tiles: vec![] };
        l.set(Kind::Users, Rect::new(0, 0, 24, 7));
        l.set(Kind::Password, Rect::new(0, 7, 24, 7));
        assert_eq!(place(&l, Kind::Clock, Rect::new(0, 0, 4, 2)), None);
    }

    #[test]
    fn resize_shrinks_until_it_clears_a_neighbour() {
        let l = Layout::default();
        let r = resize(&l, Kind::Clock, Rect::new(8, 1, 8, 8)).unwrap();
        assert_eq!(r.x, 8);
        let mut m = l.clone();
        m.set(Kind::Clock, r);
        assert_eq!(validate(&m), Ok(()));
    }

    #[test]
    fn a_west_drag_keeps_the_right_edge() {
        let l = Layout::default();
        // Users sits at 8,5 8x4; pulling its west edge into Clock/Greeting's column
        // is fine (nothing left of it), but shrinking from the left keeps x+w.
        let r = resize_anchored(&l, Kind::Users, Rect::new(2, 5, 14, 4), true, false).unwrap();
        assert_eq!((r.x + r.w, r.y, r.h), (16, 5, 4));
        let mut m = l.clone();
        m.set(Kind::Users, r);
        assert_eq!(validate(&m), Ok(()));
        // A north drag into the Greeting tile shrinks from the top, keeping the bottom.
        let r = resize_anchored(&l, Kind::Users, Rect::new(8, 2, 8, 7), false, true).unwrap();
        assert_eq!(r.y + r.h, 9);
        m.set(Kind::Users, r);
        assert_eq!(validate(&m), Ok(()));
    }

    #[test]
    fn resize_never_goes_below_the_minimum() {
        let l = Layout::default();
        let r = resize(&l, Kind::Users, Rect::new(8, 5, 1, 1)).unwrap();
        assert_eq!((r.w, r.h), Kind::Users.min_size());
    }

    #[test]
    fn pixels_leave_a_gap_and_cover_the_output() {
        let (x, y, w, h) = Rect::new(0, 0, COLS, ROWS).to_px(1920, 1080, 8);
        assert_eq!((x, y, w, h), (8, 8, 1904, 1064));
        let a = Rect::new(0, 0, 12, 14).to_px(1000, 700, 6);
        let b = Rect::new(12, 0, 12, 14).to_px(1000, 700, 6);
        assert!(a.0 + a.2 < b.0, "gap between neighbours");
    }

    #[test]
    fn kind_names_round_trip_and_are_unique() {
        for k in ALL {
            assert_eq!(Kind::from_name(k.name()), Some(k));
        }
        assert!(Kind::NowPlaying.needs_session() && !Kind::Battery.needs_session());
    }

    #[test]
    fn lines_round_trip() {
        let l = Layout::default();
        assert_eq!(Layout::from_lines(&l.to_lines()), Ok(l));
    }

    #[test]
    fn lines_refuse_anything_loose() {
        let bad = [
            "tile.clock = \"1 2 3\"",
            "tile.clock = \"1 2 3 4 5\"",
            "tile.clock = \"1  2 3 4\"",
            "tile.clock = \"-1 0 4 2\"",
            "tile.clock = \"+1 0 4 2\"",
            "tile.clock = \"0 0 4 99999\"",
            "tile.clock = 0 0 4 2",
            "tile.bogus = \"0 0 4 2\"",
            "clock = \"0 0 4 2\"",
            "tile.clock = \"0 0 4 2\"\ntile.clock = \"0 4 4 2\"",
            "tile.clock = \"0 0 4 2\"\n\ntile.wifi = \"0 4 2 1\"",
            "# note",
        ];
        for b in bad {
            assert!(Layout::from_lines(b).is_err(), "{b:?}");
        }
        let long = "tile.clock = \"0 0 4 2\"\n".repeat(200);
        assert_eq!(Layout::from_lines(&long), Err(ParseError::TooLong));
    }

    #[test]
    fn empty_text_is_an_empty_layout_that_does_not_validate() {
        let l = Layout::from_lines("").unwrap();
        assert!(l.tiles.is_empty() && validate(&l).is_err());
    }

    #[test]
    fn hit_finds_the_tile_under_a_cell() {
        let l = Layout::default();
        assert_eq!(l.hit(9, 10), Some(Kind::Password));
        assert_eq!(l.hit(0, 0), None);
    }
}
