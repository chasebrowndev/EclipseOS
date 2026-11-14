// SPDX-License-Identifier: AGPL-3.0-only
//! Cell geometry shared by the whole model. Everything is in character cells.

/// Minimum pane width in columns (T-13 §3).
pub const MIN_COLS: u32 = 2;
/// Minimum pane height in rows (T-13 §3).
pub const MIN_ROWS: u32 = 1;

/// A size in cells.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct Size {
    /// Columns.
    pub cols: u32,
    /// Rows.
    pub rows: u32,
}

/// A rectangle in cells, origin top-left.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct Rect {
    /// Left column.
    pub x: u32,
    /// Top row.
    pub y: u32,
    /// Width in columns.
    pub w: u32,
    /// Height in rows.
    pub h: u32,
}

impl Rect {
    /// Build a rectangle.
    pub const fn new(x: u32, y: u32, w: u32, h: u32) -> Self {
        Self { x, y, w, h }
    }

    /// The rectangle `0,0` of the given size.
    pub const fn from_size(s: Size) -> Self {
        Self::new(0, 0, s.cols, s.rows)
    }

    /// First column past the right edge.
    pub const fn right(&self) -> u32 {
        self.x + self.w
    }

    /// First row past the bottom edge.
    pub const fn bottom(&self) -> u32 {
        self.y + self.h
    }

    /// True when the rectangle has no cells.
    pub const fn is_empty(&self) -> bool {
        self.w == 0 || self.h == 0
    }

    /// Number of cells.
    pub const fn area(&self) -> u64 {
        self.w as u64 * self.h as u64
    }

    /// True when the two rectangles share at least one cell.
    pub fn overlaps(&self, o: &Rect) -> bool {
        !self.is_empty()
            && !o.is_empty()
            && self.x < o.right()
            && o.x < self.right()
            && self.y < o.bottom()
            && o.y < self.bottom()
    }

    /// True when `o` lies entirely inside `self`.
    pub fn contains_rect(&self, o: &Rect) -> bool {
        o.x >= self.x && o.y >= self.y && o.right() <= self.right() && o.bottom() <= self.bottom()
    }
}

/// Split orientation. `Horizontal` places the two children side by side.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Orient {
    /// Children left | right.
    Horizontal,
    /// Children top / bottom.
    Vertical,
}

impl Orient {
    /// The other orientation.
    pub const fn flip(self) -> Self {
        match self {
            Orient::Horizontal => Orient::Vertical,
            Orient::Vertical => Orient::Horizontal,
        }
    }
}

/// A direction for focus, resize, split and move.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Dir {
    /// Towards column 0.
    Left,
    /// Away from column 0.
    Right,
    /// Towards row 0.
    Up,
    /// Away from row 0.
    Down,
}

impl Dir {
    /// All four directions.
    pub const ALL: [Dir; 4] = [Dir::Left, Dir::Right, Dir::Up, Dir::Down];

    /// The opposite direction.
    pub const fn opposite(self) -> Self {
        match self {
            Dir::Left => Dir::Right,
            Dir::Right => Dir::Left,
            Dir::Up => Dir::Down,
            Dir::Down => Dir::Up,
        }
    }

    /// The split orientation that separates panes along this direction.
    pub const fn axis(self) -> Orient {
        match self {
            Dir::Left | Dir::Right => Orient::Horizontal,
            Dir::Up | Dir::Down => Orient::Vertical,
        }
    }

    /// True for `Right` and `Down` (the second child of a split).
    pub const fn is_forward(self) -> bool {
        matches!(self, Dir::Right | Dir::Down)
    }
}
