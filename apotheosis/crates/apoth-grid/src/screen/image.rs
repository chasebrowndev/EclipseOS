// SPDX-License-Identifier: AGPL-3.0-only
//! A screen as data for the mux snapshot (T-21 §3, ADR 0123): its pages exactly as they are
//! (styles, links and clusters are page-local, so a page is already its own serialisation)
//! plus the few counters that decide what later output does — the page template the next
//! page is cut from, the change sequence and the next page serial.
//!
//! [`Screen::from_image`] takes data from another process and trusts none of it: every
//! page is rebuilt through [`Page::from_bytes`] (which validates and audits it), the
//! template must be a spec [`PageSpec::is_valid`] accepts, and the page list must add up to
//! the active area. Tracked pins are not part of an image: each view tracks its own.

use super::*;

/// The non-page part of a screen image.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ScreenMeta {
    /// Active rows.
    pub rows: usize,
    /// Columns.
    pub cols: usize,
    /// Keeps history (primary) or not (alternate).
    pub history: bool,
    /// Scrollback limits.
    pub limits: Limits,
    /// The template new pages are cut from (grows and shrinks with use).
    pub spec: PageSpec,
    /// The row change sequence.
    pub seq: u64,
    /// The serial the next page gets.
    pub next_serial: u64,
}

/// Why an image was refused. Carries no content.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ImageError {
    /// A page did not validate.
    Page,
    /// The template, sizes or page list do not fit together.
    Shape,
}

impl std::fmt::Display for ImageError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            ImageError::Page => "a snapshot page is corrupt",
            ImageError::Shape => "a snapshot screen does not fit together",
        })
    }
}

impl std::error::Error for ImageError {}

impl Screen {
    /// The counters a snapshot carries with the pages.
    #[must_use]
    pub fn image_meta(&self) -> ScreenMeta {
        ScreenMeta {
            rows: self.rows,
            cols: self.cols,
            history: self.history,
            limits: self.limits,
            spec: self.spec,
            seq: self.seq,
            next_serial: self.next_serial,
        }
    }

    /// The pages, oldest history first; the active area is the tail.
    pub fn image_pages(&self) -> impl ExactSizeIterator<Item = &Page> {
        self.pages.iter()
    }

    /// Rebuild a screen from [`Screen::image_meta`] and page bytes. Everything is
    /// checked; the result is a screen whose later output behaves exactly as the original's.
    pub fn from_image(meta: ScreenMeta, pages: Vec<Vec<u8>>) -> Result<Screen, ImageError> {
        let ScreenMeta {
            rows,
            cols,
            history,
            limits,
            spec,
            seq,
            next_serial,
        } = meta;
        if !(1..=u16::MAX as usize).contains(&rows)
            || !(1..=u16::MAX as usize).contains(&cols)
            || rows * cols > MAX_CELLS
            || !spec.is_valid()
            || spec.cols != cols
            || pages.is_empty()
        {
            return Err(ImageError::Shape);
        }
        let mut s = Screen {
            id: NEXT_SCREEN_ID.fetch_add(1, Ordering::Relaxed),
            epoch: 0,
            rows,
            cols,
            history,
            limits,
            spec,
            ceil: Ceil::MAX,
            cap: None,
            pages: Vec::with_capacity(pages.len()),
            pool: PagePool::default(),
            next_serial,
            seq,
            total_rows: 0,
            bytes_total: 0,
            pins: PinTable::default(),
            pending: None,
            force_full: true,
            cursor_dirty: false,
            stats: Stats::default(),
        };
        let mut serials = std::collections::HashSet::with_capacity(pages.len());
        for buf in pages {
            let p = Page::from_bytes(buf).map_err(|_| ImageError::Page)?;
            if p.cols() != cols
                || p.used_rows() == 0
                || p.serial() >= next_serial
                || !serials.insert(p.serial())
            {
                return Err(ImageError::Shape);
            }
            s.total_rows += p.used_rows();
            s.bytes_total += s.page_logical_bytes(&p);
            s.pages.push(p);
        }
        if s.total_rows < rows || (!history && s.total_rows != rows) {
            return Err(ImageError::Shape);
        }
        Ok(s)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn image(s: &Screen) -> (ScreenMeta, Vec<Vec<u8>>) {
        (
            s.image_meta(),
            s.image_pages().map(|p| p.as_bytes().to_vec()).collect(),
        )
    }

    fn text(s: &Screen) -> Vec<String> {
        let mut out = Vec::new();
        let mut buf = Vec::new();
        for abs in 0..s.total_rows {
            let mut row = String::new();
            for x in 0..s.cols {
                buf.clear();
                s.cell_abs(abs, x, &mut buf);
                row.extend(buf.iter().filter_map(|&c| char::from_u32(c)));
            }
            out.push(row);
        }
        out
    }

    fn filled(history: bool) -> Screen {
        let mut s = Screen::new(
            4,
            10,
            ScreenConfig {
                history,
                limits: Limits::default(),
            },
        );
        let bold = CellAttrs {
            style: Style {
                fg: Color::Rgb(1, 2, 3),
                ..Style::DEFAULT
            },
            link: Some(("id", "https://example.org/")),
            ..CellAttrs::default()
        };
        for i in 0..40u32 {
            let g = [Glyph {
                cp: 'a' as u32 + i % 26,
                width: 1,
            }];
            s.print_run(3, (i % 10) as usize, &g, &bold);
            if i % 10 == 9 {
                s.scroll_up(Region::full(4, 10), 1, Color::Default);
            }
        }
        s
    }

    #[test]
    fn a_screen_round_trips_through_its_image() {
        for history in [true, false] {
            let s = filled(history);
            let (meta, pages) = image(&s);
            let t = Screen::from_image(meta, pages).unwrap();
            t.check_invariants().unwrap();
            assert_eq!(t.image_meta(), s.image_meta());
            assert_eq!(text(&t), text(&s));
            assert_eq!(t.history_rows(), s.history_rows());
            assert_eq!(t.link_at(2, 0), s.link_at(2, 0));
        }
    }

    #[test]
    fn later_output_matches_after_a_round_trip() {
        let mut s = filled(true);
        let (meta, pages) = image(&s);
        let mut t = Screen::from_image(meta, pages).unwrap();
        for scr in [&mut s, &mut t] {
            for _ in 0..300 {
                scr.print_run(
                    3,
                    0,
                    &[Glyph {
                        cp: 'z' as u32,
                        width: 1,
                    }],
                    &CellAttrs::default(),
                );
                scr.scroll_up(Region::full(4, 10), 1, Color::Default);
            }
        }
        assert_eq!(image(&s), image(&t));
    }

    #[test]
    fn bad_images_are_refused() {
        let s = filled(true);
        let (meta, pages) = image(&s);
        let shape = |m: ScreenMeta, p: Vec<Vec<u8>>| Screen::from_image(m, p).err();
        assert_eq!(shape(meta, Vec::new()), Some(ImageError::Shape));
        assert_eq!(
            shape(ScreenMeta { rows: 0, ..meta }, pages.clone()),
            Some(ImageError::Shape)
        );
        assert_eq!(
            shape(ScreenMeta { cols: 11, ..meta }, pages.clone()),
            Some(ImageError::Shape)
        );
        // More active rows than the pages hold.
        assert_eq!(
            shape(ScreenMeta { rows: 5000, ..meta }, pages.clone()),
            Some(ImageError::Shape)
        );
        // A serial the screen would hand out again.
        assert_eq!(
            shape(
                ScreenMeta {
                    next_serial: 1,
                    ..meta
                },
                pages.clone()
            ),
            Some(ImageError::Shape)
        );
        // A template that would allocate without bound.
        let huge = PageSpec {
            rows_cap: 65_535,
            ..meta.spec
        };
        let wide = ScreenMeta {
            spec: PageSpec {
                cols: 65_535,
                ..huge
            },
            ..meta
        };
        assert_eq!(shape(wide, pages.clone()), Some(ImageError::Shape));
        // The same page twice.
        let mut twice = pages.clone();
        twice.push(pages[0].clone());
        assert_eq!(shape(meta, twice), Some(ImageError::Shape));
        // A flipped byte anywhere is refused or yields a screen that audits.
        for i in (0..pages[0].len()).step_by(97) {
            let mut p = pages.clone();
            p[0][i] ^= 0x5a;
            if let Ok(t) = Screen::from_image(meta, p) {
                t.check_invariants().unwrap();
            }
        }
    }
}
