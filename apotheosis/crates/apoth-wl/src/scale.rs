// SPDX-License-Identifier: AGPL-3.0-only
//! Scale math (T-09 §3, §4; UI-48). Fractional scale is in 120ths:
//! buffer = round(logical × scale), viewport destination = logical.

/// Denominator of `wp_fractional_scale_v1.preferred_scale`.
pub const DENOM: u32 = 120;
const MAX_SCALE_120: u32 = 16 * DENOM;

/// A surface scale in 120ths. Never zero.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Scale(u32);

impl Scale {
    /// Scale 1.0.
    pub const ONE: Scale = Scale(DENOM);

    /// From the protocol value. `0` is invalid; values above 16x are clamped.
    pub fn from_120ths(v: u32) -> Option<Scale> {
        (v > 0).then(|| Scale(v.min(MAX_SCALE_120)))
    }

    /// From an integer `wl_output` / `preferred_buffer_scale` factor (clamped to 1..=16).
    pub fn from_integer(n: i32) -> Scale {
        Scale((n.clamp(1, 16) as u32) * DENOM)
    }

    /// The protocol value.
    pub fn as_120ths(self) -> u32 {
        self.0
    }

    /// As a float (for display and shaders only, never for sizing).
    pub fn as_f64(self) -> f64 {
        f64::from(self.0) / f64::from(DENOM)
    }

    /// True when the scale is a whole number.
    pub fn is_integer(self) -> bool {
        self.0.is_multiple_of(DENOM)
    }

    /// The integer factor to give `wl_surface.set_buffer_scale`: the scale, rounded up.
    pub fn integer_ceil(self) -> i32 {
        self.0.div_ceil(DENOM) as i32
    }

    /// Buffer extent for one logical extent: round-half-up of `logical × scale`.
    pub fn to_buffer(self, logical: u32) -> u32 {
        ((u64::from(logical) * u64::from(self.0) + u64::from(DENOM / 2)) / u64::from(DENOM)) as u32
    }

    /// Buffer size for a logical size.
    pub fn to_buffer_size(self, logical: (u32, u32)) -> (u32, u32) {
        (self.to_buffer(logical.0), self.to_buffer(logical.1))
    }

    /// Logical extent for a buffer extent: round-half-up of `buffer / scale`.
    pub fn to_logical(self, buffer: u32) -> u32 {
        let s = u64::from(self.0);
        ((u64::from(buffer) * u64::from(DENOM) + s / 2) / s) as u32
    }

    /// True when `logical` maps to a whole number of buffer pixels, so a surface of that
    /// logical size is pixel-exact with no rounding (the pixel-crisp condition).
    pub fn is_exact(self, logical: u32) -> bool {
        (u64::from(logical) * u64::from(self.0)) % u64::from(DENOM) == 0
    }
}

impl Default for Scale {
    fn default() -> Self {
        Scale::ONE
    }
}

/// How a window learns and applies its scale.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ScaleMode {
    /// `wp_fractional_scale_v1` + `wp_viewporter`.
    Fractional,
    /// Integer `wl_output` / `preferred_buffer_scale` with `set_buffer_scale`.
    Integer,
}

/// Fractional needs both globals; otherwise the integer fallback.
pub fn choose_mode(have_fractional: bool, have_viewporter: bool) -> ScaleMode {
    if have_fractional && have_viewporter {
        ScaleMode::Fractional
    } else {
        ScaleMode::Integer
    }
}

/// The integer scale: the compositor's `preferred_buffer_scale` when it sent one, else the
/// largest scale among outputs the surface has entered, else 1. (The compositor's choice
/// already reflects the output with the largest overlap, T-09 §4.)
pub fn integer_scale(preferred_buffer: Option<i32>, entered_outputs: &[i32]) -> i32 {
    if let Some(p) = preferred_buffer {
        return p.clamp(1, 16);
    }
    entered_outputs
        .iter()
        .copied()
        .max()
        .unwrap_or(1)
        .clamp(1, 16)
}

/// The scale a window renders at.
pub fn effective(mode: ScaleMode, fractional_pref: Option<Scale>, integer: i32) -> Scale {
    match mode {
        ScaleMode::Fractional => fractional_pref.unwrap_or(Scale::ONE),
        ScaleMode::Integer => Scale::from_integer(integer),
    }
}

/// The grid that fits a buffer: whole cells, the remainder is padding (T-09 §4).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct GridFit {
    /// Columns.
    pub cols: u32,
    /// Rows.
    pub rows: u32,
    /// Horizontal remainder in buffer pixels.
    pub pad_x: u32,
    /// Vertical remainder in buffer pixels.
    pub pad_y: u32,
}

/// Fit whole cells (sizes in buffer pixels) into a buffer. Zero cell sizes yield an empty fit.
pub fn fit_grid(buffer_w: u32, buffer_h: u32, cell_w: u32, cell_h: u32) -> GridFit {
    if cell_w == 0 || cell_h == 0 {
        return GridFit {
            cols: 0,
            rows: 0,
            pad_x: buffer_w,
            pad_y: buffer_h,
        };
    }
    GridFit {
        cols: buffer_w / cell_w,
        rows: buffer_h / cell_h,
        pad_x: buffer_w % cell_w,
        pad_y: buffer_h % cell_h,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(v: u32) -> Scale {
        Scale::from_120ths(v).unwrap()
    }

    #[test]
    fn standard_scales_round_trip_on_even_sizes() {
        for (v, logical, buffer) in [
            (120, 800, 800),
            (150, 800, 1000),
            (180, 800, 1200),
            (240, 800, 1600),
        ] {
            assert_eq!(s(v).to_buffer(logical), buffer);
            assert_eq!(s(v).to_logical(buffer), logical);
            assert!(s(v).is_exact(logical));
        }
    }

    #[test]
    fn rounding_is_half_up() {
        // 801 x 1.5 = 1201.5 -> 1202; 1001 x 1.25 = 1251.25 -> 1251
        assert_eq!(s(180).to_buffer(801), 1202);
        assert_eq!(s(150).to_buffer(1001), 1251);
        assert!(!s(180).is_exact(801));
    }

    #[test]
    fn invalid_and_clamped() {
        assert!(Scale::from_120ths(0).is_none());
        assert_eq!(Scale::from_120ths(u32::MAX).unwrap().as_120ths(), 16 * 120);
        assert_eq!(Scale::from_integer(0), Scale::ONE);
        assert_eq!(Scale::from_integer(99).as_120ths(), 16 * 120);
    }

    #[test]
    fn integer_ceil_and_flags() {
        assert_eq!(s(150).integer_ceil(), 2);
        assert_eq!(s(120).integer_ceil(), 1);
        assert!(s(240).is_integer());
        assert!(!s(150).is_integer());
        assert!((s(150).as_f64() - 1.25).abs() < 1e-9);
    }

    #[test]
    fn mode_needs_both_globals() {
        assert_eq!(choose_mode(true, true), ScaleMode::Fractional);
        assert_eq!(choose_mode(true, false), ScaleMode::Integer);
        assert_eq!(choose_mode(false, true), ScaleMode::Integer);
    }

    #[test]
    fn integer_scale_preference_order() {
        assert_eq!(integer_scale(Some(2), &[1, 3]), 2);
        assert_eq!(integer_scale(None, &[1, 3, 2]), 3);
        assert_eq!(integer_scale(None, &[]), 1);
        assert_eq!(integer_scale(Some(0), &[]), 1);
    }

    #[test]
    fn effective_scale_by_mode() {
        assert_eq!(effective(ScaleMode::Fractional, Some(s(150)), 2), s(150));
        assert_eq!(effective(ScaleMode::Fractional, None, 2), Scale::ONE);
        assert_eq!(
            effective(ScaleMode::Integer, Some(s(150)), 2),
            Scale::from_integer(2)
        );
    }

    #[test]
    fn grid_snaps_to_whole_cells() {
        let g = fit_grid(1003, 605, 10, 20);
        assert_eq!(
            g,
            GridFit {
                cols: 100,
                rows: 30,
                pad_x: 3,
                pad_y: 5
            }
        );
        let z = fit_grid(100, 50, 0, 10);
        assert_eq!((z.cols, z.rows, z.pad_x, z.pad_y), (0, 0, 100, 50));
    }
}
