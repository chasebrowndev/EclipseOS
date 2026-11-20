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

/// Content-sized tiles (ADR 0126, amendment 2026-11-19). The size of a Glance
/// such as the battery row is decided by what it shows, not stored: the
/// operator picks only where its top-left sits. Pixel numbers are for a
/// reference output of [`REF_W`] x [`REF_H`]; the footprint in cells is that
/// size over the reference cell, rounded up, so it does not depend on the real
/// output and a layout that validates here fits every output.
pub mod intrinsic {
    /// The output the cell sizes below are measured against.
    pub const REF_W: i32 = 1920;
    pub const REF_H: i32 = 1080;
    /// One gauge: its square, the numeral line under it (with its gap), and
    /// the spacing between gauges across and down.
    pub const GAUGE_PX: i32 = 76;
    pub const NUMERAL_PX: i32 = 28;
    pub const GAUGE_GAP_PX: i32 = 8;
    pub const ROW_GAP_PX: i32 = 12;
    /// The pane's inner margin on every side.
    pub const PAD_PX: i32 = 16;
    /// The clear space a tile leaves on each side of its cells (the greeter's
    /// `TILE_GAP`).
    pub const TILE_GAP_PX: i32 = 8;
    /// Gauges per row, rows, and so the most devices a tile shows.
    pub const MAX_PER_ROW: usize = 4;
    pub const MAX_ROWS: usize = 2;
    pub const MAX_DEVICES: usize = MAX_PER_ROW * MAX_ROWS;
    /// A gauge cell's height: the square and its numeral.
    pub const CELL_H_PX: i32 = GAUGE_PX + NUMERAL_PX;

    /// `(columns, rows)` of gauges for `devices`, filling a row before
    /// wrapping and capped at [`MAX_DEVICES`]. `(0, 0)` for none.
    pub const fn grid(devices: usize) -> (i32, i32) {
        let n = if devices > MAX_DEVICES {
            MAX_DEVICES
        } else {
            devices
        };
        if n == 0 {
            return (0, 0);
        }
        let cols = if n > MAX_PER_ROW { MAX_PER_ROW } else { n };
        (cols as i32, n.div_ceil(MAX_PER_ROW) as i32)
    }

    /// The pane's size in reference pixels for `devices`; `(0, 0)` for none.
    pub const fn px(devices: usize) -> (i32, i32) {
        let (c, r) = grid(devices);
        if c == 0 {
            return (0, 0);
        }
        (
            2 * PAD_PX + c * GAUGE_PX + (c - 1) * GAUGE_GAP_PX,
            2 * PAD_PX + r * CELL_H_PX + (r - 1) * ROW_GAP_PX,
        )
    }

    /// Cells needed for `px` reference pixels plus the clear space either side,
    /// across `n` cells spanning `total` pixels.
    pub const fn cells(px: i32, n: i32, total: i32) -> i32 {
        let need = (px + 2 * TILE_GAP_PX) * n;
        (need + total - 1) / total
    }
}

/// What a content-sized tile measures itself from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Content {
    /// Batteries to show.
    pub devices: usize,
}

impl Content {
    /// The least a tile can be: one gauge. What [`validate`] holds a layout to.
    pub const MIN: Content = Content { devices: 1 };
    /// What the settings editor shows (a laptop, a mouse and a keyboard).
    pub const PREVIEW: Content = Content { devices: 3 };
}

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
            // Content-sized: never read (see `is_intrinsic`).
            Kind::Battery => (1, 1),
            Kind::Wifi => (2, 1),
            Kind::NowPlaying => (6, 2),
        }
    }

    /// Is the size derived from the content rather than stored? A tile of such
    /// a kind keeps only its position; `w` and `h` in a file or a [`Rect`] are
    /// ignored, and resizing it is refused.
    pub const fn is_intrinsic(self) -> bool {
        matches!(self, Kind::Battery)
    }

    /// The footprint in cells of an intrinsic kind showing `c`: `(0, 0)` when
    /// there is nothing to show, `None` for a kind whose size is stored.
    pub const fn intrinsic_size(self, c: Content) -> Option<(i32, i32)> {
        match self {
            Kind::Battery => {
                let (w, h) = intrinsic::px(c.devices);
                if w == 0 {
                    return Some((0, 0));
                }
                Some((
                    intrinsic::cells(w, COLS, intrinsic::REF_W),
                    intrinsic::cells(h, ROWS, intrinsic::REF_H),
                ))
            }
            _ => None,
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

    /// No area: a content-sized tile with nothing to show.
    pub const fn is_empty(self) -> bool {
        self.w <= 0 || self.h <= 0
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

impl Tile {
    /// The cells the tile occupies showing `c`: its stored rectangle, or for a
    /// content-sized kind its stored top-left with the size the content needs.
    pub fn extent(&self, c: Content) -> Rect {
        match self.kind.intrinsic_size(c) {
            Some((w, h)) => Rect::new(self.rect.x, self.rect.y, w, h),
            None => self.rect,
        }
    }
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

    /// Where `kind` sits showing `c` (see [`Tile::extent`]).
    pub fn extent(&self, kind: Kind, c: Content) -> Option<Rect> {
        self.tiles.iter().find(|t| t.kind == kind).map(|t| t.extent(c))
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
            .find(|t| t.extent(Content::MIN).overlaps(p))
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
            // A content-sized tile's w and h are not read: old files carry them.
            let rect = match kind.intrinsic_size(Content::MIN) {
                Some((w, h)) => Rect::new(rect.x, rect.y, w, h),
                None => rect,
            };
            tiles.push(Tile { kind, rect });
        }
        Ok(Layout { tiles })
    }
}

/// Is this layout one the greeter may draw? Checked on read and again by the
/// root helper before anything is written.
pub fn validate(l: &Layout) -> Result<(), Error> {
    let c = Content::MIN;
    for (i, t) in l.tiles.iter().enumerate() {
        if l.tiles[..i].iter().any(|o| o.kind == t.kind) {
            return Err(Error::Duplicate(t.kind));
        }
        let r = t.extent(c);
        if !r.in_grid() {
            return Err(Error::OutOfGrid(t.kind));
        }
        let (mw, mh) = t.kind.min_size();
        if !t.kind.is_intrinsic() && (r.w < mw || r.h < mh) {
            return Err(Error::TooSmall(t.kind));
        }
        if let Some(o) = l.tiles[..i].iter().find(|o| o.extent(c).overlaps(r)) {
            return Err(Error::Overlap(o.kind, t.kind));
        }
    }
    REQUIRED
        .into_iter()
        .find(|k| l.get(*k).is_none())
        .map_or(Ok(()), |k| Err(Error::Missing(k)))
}

fn free(l: &Layout, kind: Kind, r: Rect, c: Content) -> bool {
    r.in_grid()
        && !l
            .tiles
            .iter()
            .any(|t| t.kind != kind && !t.extent(c).is_empty() && t.extent(c).overlaps(r))
}

/// Where `kind` lands when dragged so its corner is at `want`'s: the nearest
/// grid position at its current size that overlaps nothing. `None` when the
/// layout has no room for it anywhere; the editor then drops the move.
/// A content-sized kind ignores `want`'s size and takes its own.
pub fn place(l: &Layout, kind: Kind, want: Rect) -> Option<Rect> {
    place_with(l, kind, want, Content::MIN)
}

/// [`place`] with content-sized tiles (this one and its neighbours) measured
/// for `c`, as an editor that previews more than one device does.
pub fn place_with(l: &Layout, kind: Kind, want: Rect, c: Content) -> Option<Rect> {
    let (mw, mh) = kind.min_size();
    let (w, h) = match kind.intrinsic_size(c) {
        Some((w, h)) if w > 0 => (w, h),
        Some(_) => kind.intrinsic_size(Content::MIN)?,
        None => (want.w.clamp(mw, COLS), want.h.clamp(mh, ROWS)),
    };
    let mut best: Option<(i64, Rect)> = None;
    for y in 0..=ROWS - h {
        for x in 0..=COLS - w {
            let r = Rect::new(x, y, w, h);
            if !free(l, kind, r, c) {
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

/// Where a content-sized tile draws at run time and how many of `devices` it
/// shows. The tile asks for the footprint of all of them (up to the most a
/// tile holds); if that overlaps a neighbour or leaves the grid it gives up
/// one device at a time, so the result is the most that fit, deterministically.
/// `None` when there is nothing to show or not even one gauge fits. A
/// stored-size kind returns its rectangle and `0`.
pub fn fit(l: &Layout, kind: Kind, devices: usize) -> Option<(Rect, usize)> {
    let t = l.tiles.iter().find(|t| t.kind == kind)?;
    if !kind.is_intrinsic() {
        return Some((t.rect, 0));
    }
    (1..=devices.min(intrinsic::MAX_DEVICES)).rev().find_map(|n| {
        let c = Content { devices: n };
        let r = t.extent(c);
        free(l, kind, r, Content::MIN).then_some((r, n))
    })
}

/// `kind` resized to `want` keeping its top-left corner: shrunk (never below
/// its minimum) until it overlaps nothing. `None` if even the minimum does.
///
/// A content-sized kind cannot be resized: `None`.
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
    resize_anchored_with(l, kind, want, fix_right, fix_bottom, Content::MIN)
}

/// [`resize_anchored`] with content-sized neighbours measured for `c`.
pub fn resize_anchored_with(
    l: &Layout,
    kind: Kind,
    want: Rect,
    fix_right: bool,
    fix_bottom: bool,
    c: Content,
) -> Option<Rect> {
    if kind.is_intrinsic() {
        return None;
    }
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
        if free(l, kind, r, c) {
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

    // ---- content-sized tiles ------------------------------------------------

    fn with_battery(x: i32, y: i32) -> Layout {
        let mut l = Layout::default();
        l.set(Kind::Battery, Rect::new(x, y, 99, 99));
        l
    }

    #[test]
    fn the_footprint_follows_the_device_count_and_wraps_at_four() {
        let size = |n| Kind::Battery.intrinsic_size(Content { devices: n }).unwrap();
        assert_eq!(size(0), (0, 0));
        assert_eq!(size(1), (2, 2));
        assert_eq!(size(3), (4, 2));
        assert_eq!(size(4), (5, 2));
        assert_eq!(size(5), (5, 4), "the fifth device starts a second row");
        assert_eq!(size(8), (5, 4));
        assert_eq!(size(40), size(8), "capped at what a tile holds");
        assert_eq!(intrinsic::grid(4), (4, 1));
        assert_eq!(intrinsic::grid(6), (4, 2));
        assert_eq!(Kind::Clock.intrinsic_size(Content::MIN), None);
    }

    #[test]
    fn the_footprint_always_holds_the_content_px() {
        for n in 1..=intrinsic::MAX_DEVICES {
            let (w, h) = intrinsic::px(n);
            let (cw, ch) = Kind::Battery.intrinsic_size(Content { devices: n }).unwrap();
            let (cell_w, cell_h) = (
                intrinsic::REF_W as f32 / COLS as f32,
                intrinsic::REF_H as f32 / ROWS as f32,
            );
            assert!(
                cw as f32 * cell_w >= (w + 2 * intrinsic::TILE_GAP_PX) as f32,
                "{n} wide"
            );
            assert!(
                ch as f32 * cell_h >= (h + 2 * intrinsic::TILE_GAP_PX) as f32,
                "{n} tall"
            );
        }
    }

    #[test]
    fn old_files_keep_the_position_and_lose_the_size() {
        let l = Layout::from_lines(
            "tile.users = \"8 5 8 4\"\ntile.password = \"8 9 8 2\"\ntile.battery = \"17 1 8 5\"\n",
        )
        .unwrap();
        let r = l.get(Kind::Battery).unwrap();
        assert_eq!((r.x, r.y), (17, 1));
        assert_eq!((r.w, r.h), Kind::Battery.intrinsic_size(Content::MIN).unwrap());
        assert_eq!(validate(&l), Ok(()));
        // The new text still has four numbers, so the older readers parse it.
        assert!(l.to_lines().contains("tile.battery = \"17 1 2 2\""));
    }

    #[test]
    fn validate_ignores_the_stored_size_and_uses_the_intrinsic_one() {
        // A stored 1x1 or 20x20 is the same tile: only the position counts.
        for (w, h) in [(1, 1), (20, 20)] {
            let mut l = Layout::default();
            l.set(Kind::Battery, Rect::new(18, 1, w, h));
            assert_eq!(validate(&l), Ok(()), "{w}x{h}");
        }
        // Two cells wide does not fit starting at column 23.
        let l = with_battery(23, 1);
        assert_eq!(validate(&l), Err(Error::OutOfGrid(Kind::Battery)));
        let l = with_battery(18, 13);
        assert_eq!(validate(&l), Err(Error::OutOfGrid(Kind::Battery)));
    }

    #[test]
    fn overlap_uses_the_intrinsic_size_not_the_stored_one() {
        // Clock is 8..16 x 1..4. A 2x2 battery at x=16 touches; at x=15 overlaps.
        let l = with_battery(16, 1);
        assert_eq!(validate(&l), Ok(()));
        let l = with_battery(15, 1);
        assert!(matches!(validate(&l), Err(Error::Overlap(..))));
        // A stored size that would overlap does not, if the intrinsic one clears.
        let mut l = Layout::default();
        l.set(Kind::Battery, Rect::new(16, 1, 8, 13));
        assert_eq!(validate(&l), Ok(()));
    }

    #[test]
    fn place_gives_a_content_tile_its_own_size() {
        let l = with_battery(18, 1);
        let r = place(&l, Kind::Battery, Rect::new(18, 6, 9, 9)).unwrap();
        assert_eq!((r.w, r.h), (2, 2));
        let r = place_with(&l, Kind::Battery, Rect::new(18, 6, 9, 9), Content::PREVIEW).unwrap();
        assert_eq!((r.w, r.h), (4, 2));
        // Neighbours of an intrinsic tile are measured at the same content.
        let mut m = l.clone();
        m.set(Kind::Battery, Rect::new(18, 1, 4, 2));
        let c = place_with(&m, Kind::Clock, Rect::new(16, 1, 8, 3), Content::PREVIEW).unwrap();
        assert!(!c.overlaps(Rect::new(18, 1, 4, 2)));
    }

    #[test]
    fn resizing_a_content_tile_is_refused() {
        let l = with_battery(18, 1);
        assert_eq!(resize(&l, Kind::Battery, Rect::new(18, 1, 5, 5)), None);
        assert_eq!(
            resize_anchored_with(
                &l,
                Kind::Battery,
                Rect::new(18, 1, 5, 5),
                true,
                true,
                Content::PREVIEW
            ),
            None
        );
        assert!(Kind::Battery.is_intrinsic() && !Kind::Wifi.is_intrinsic());
    }

    #[test]
    fn fit_remeasures_as_devices_come_and_go() {
        let l = with_battery(18, 1);
        assert_eq!(fit(&l, Kind::Battery, 0), None, "nothing to show draws nothing");
        assert_eq!(fit(&l, Kind::Battery, 1), Some((Rect::new(18, 1, 2, 2), 1)));
        assert_eq!(fit(&l, Kind::Battery, 3), Some((Rect::new(18, 1, 4, 2), 3)));
        assert_eq!(fit(&l, Kind::Battery, 6), Some((Rect::new(18, 1, 5, 4), 6)));
        assert_eq!(fit(&l, Kind::Battery, 99).map(|f| f.1), Some(8));
    }

    #[test]
    fn fit_gives_up_devices_until_it_clears_a_neighbour_or_the_edge() {
        // At column 20 four columns reach the grid's edge (24): 3 gauges are 4
        // cells, 4 gauges are 5.
        let l = with_battery(20, 1);
        assert_eq!(fit(&l, Kind::Battery, 4), Some((Rect::new(20, 1, 4, 2), 3)));
        // Two rows reach row 14 from row 11: only one row fits.
        let l = with_battery(18, 11);
        assert_eq!(fit(&l, Kind::Battery, 6), Some((Rect::new(18, 11, 5, 2), 4)));
        // A neighbour below the first row: Power at 18,3 stops the second row.
        let mut l = with_battery(18, 1);
        l.set(Kind::Power, Rect::new(18, 3, 6, 1));
        assert_eq!(fit(&l, Kind::Battery, 8), Some((Rect::new(18, 1, 5, 2), 4)));
        // Not even one gauge: nothing is drawn.
        let mut l = with_battery(18, 1);
        l.set(Kind::Power, Rect::new(18, 2, 6, 1));
        assert_eq!(fit(&l, Kind::Battery, 3), None);
        // Deterministic.
        assert_eq!(fit(&l, Kind::Battery, 3), fit(&l, Kind::Battery, 3));
    }

    #[test]
    fn an_empty_content_tile_overlaps_nothing() {
        let mut l = with_battery(8, 6); // inside Users
        l.set(Kind::Battery, Rect::new(9, 6, 0, 0));
        let m = Content { devices: 0 };
        assert!(l.extent(Kind::Battery, m).unwrap().is_empty());
        let r = place_with(&l, Kind::Users, Rect::new(8, 5, 8, 4), m).unwrap();
        assert_eq!(r, Rect::new(8, 5, 8, 4));
    }

    #[test]
    fn hit_uses_the_intrinsic_footprint() {
        let l = with_battery(18, 1);
        assert_eq!(l.hit(19, 2), Some(Kind::Battery));
        assert_eq!(l.hit(20, 1), None, "the stored 99x99 is not the tile");
    }
}
