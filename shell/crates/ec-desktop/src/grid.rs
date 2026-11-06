// SPDX-License-Identifier: AGPL-3.0-only
//! Where cells are on a surface: pure geometry, in the surface's logical
//! pixels, from `ec_ui::tokens::desktop`. No iced widgets here, only its
//! `Point`/`Rectangle`/`Size`, so the hit tests are unit-tested.

use ec_ui::tokens::desktop;
use iced::{Point, Rectangle, Size};

use crate::layout::{Cell, Grid};

/// The grid a surface of `size` offers: as many whole cells as fit inside
/// the margin, and at least one.
pub fn grid_for(size: Size) -> Grid {
    let fit = |extent: f32, cell: f32| (((extent - 2.0 * desktop::MARGIN) / cell).floor().max(1.0)) as u32;
    Grid {
        cols: fit(size.width, desktop::CELL_W),
        rows: fit(size.height, desktop::CELL_H),
    }
}

/// The rectangle of `cell`.
pub fn cell_rect(cell: Cell) -> Rectangle {
    Rectangle {
        x: desktop::MARGIN + cell.col as f32 * desktop::CELL_W,
        y: desktop::MARGIN + cell.row as f32 * desktop::CELL_H,
        width: desktop::CELL_W,
        height: desktop::CELL_H,
    }
}

/// The cell under `p`, kept inside `grid`: a drop beyond an edge lands in the
/// nearest cell, never off the screen.
pub fn cell_at(p: Point, grid: Grid) -> Cell {
    let at = |v: f32, cell: f32, count: u32| {
        let i = ((v - desktop::MARGIN) / cell).floor();
        i.clamp(0.0, count.saturating_sub(1) as f32) as u32
    };
    Cell {
        col: at(p.x, desktop::CELL_W, grid.cols),
        row: at(p.y, desktop::CELL_H, grid.rows),
    }
}

/// The rectangle with corners `a` and `b`, whichever way round.
pub fn band_rect(a: Point, b: Point) -> Rectangle {
    Rectangle {
        x: a.x.min(b.x),
        y: a.y.min(b.y),
        width: (a.x - b.x).abs(),
        height: (a.y - b.y).abs(),
    }
}

/// Which of `rects` the band touches (indices, in order).
pub fn band_hits(band: Rectangle, rects: &[Rectangle]) -> Vec<usize> {
    rects
        .iter()
        .enumerate()
        .filter(|(_, r)| band.intersects(r))
        .map(|(i, _)| i)
        .collect()
}

/// How wide `c` is in average-letter units: a rough face metric, enough to
/// decide where a label breaks without measuring text in the model.
fn units(c: char) -> f32 {
    match c {
        'i' | 'j' | 'l' | 't' | 'f' | 'I' | '.' | ',' | ':' | ';' | '\'' | '!' | '|' | ' ' | '(' | ')'
        | '-' => 0.5,
        'm' | 'w' | 'M' | 'W' | '@' => 1.5,
        c if c.is_uppercase() => 1.15,
        _ => 1.0,
    }
}

fn width_of(s: &str) -> f32 {
    s.chars().map(units).sum()
}

/// `label` cut to `desktop::LABEL_LINES` lines of `desktop::LABEL_UNITS`: broken
/// at spaces where it can, mid-word where it must, and the last line ends in
/// an ellipsis when something was left out. Lines are joined with `\n`.
pub fn label(label: &str) -> String {
    let budget = desktop::LABEL_UNITS;
    let mut lines: Vec<String> = Vec::new();
    let mut cur = String::new();
    let words: Vec<String> = label.split_whitespace().map(str::to_owned).collect();
    for w in words {
        let mut w = w;
        loop {
            let joined = if cur.is_empty() {
                width_of(&w)
            } else {
                width_of(&cur) + 0.5 + width_of(&w)
            };
            if joined <= budget {
                if !cur.is_empty() {
                    cur.push(' ');
                }
                cur.push_str(&w);
                break;
            }
            if !cur.is_empty() && width_of(&w) <= budget {
                lines.push(std::mem::take(&mut cur));
                continue;
            }
            // Split the word at the budget.
            if !cur.is_empty() {
                lines.push(std::mem::take(&mut cur));
            }
            // Prefer to break after a separator, else mid-word.
            let mut taken = String::new();
            let mut left = String::new();
            let mut at = None;
            let mut run = 0.0;
            for (i, c) in w.char_indices() {
                run += units(c);
                if run > budget {
                    break;
                }
                if matches!(c, '-' | '_' | '.') && i + c.len_utf8() < w.len() {
                    at = Some(i + c.len_utf8());
                }
            }
            match at {
                Some(i) => {
                    taken.push_str(&w[..i]);
                    left.push_str(&w[i..]);
                }
                None => {
                    for c in w.chars() {
                        if left.is_empty() && width_of(&taken) + units(c) <= budget {
                            taken.push(c);
                        } else {
                            left.push(c);
                        }
                    }
                }
            }
            if taken.is_empty() {
                // One glyph wider than a line: take it anyway.
                let mut it = w.chars();
                taken.extend(it.next());
                left = it.collect();
            }
            lines.push(taken);
            if left.is_empty() {
                break;
            }
            w = left;
        }
    }
    if !cur.is_empty() {
        lines.push(cur);
    }
    if lines.len() > desktop::LABEL_LINES {
        lines.truncate(desktop::LABEL_LINES);
        if let Some(last) = lines.last_mut() {
            while width_of(last) + 1.0 > budget {
                last.pop();
            }
            last.push('…');
        }
    }
    lines.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(x: f32, y: f32) -> Point {
        Point::new(x, y)
    }

    #[test]
    fn the_grid_is_whole_cells_inside_the_margin() {
        let g = grid_for(Size::new(1920.0, 1036.0));
        assert_eq!(
            g.cols,
            ((1920.0 - 2.0 * desktop::MARGIN) / desktop::CELL_W) as u32
        );
        assert_eq!(
            g.rows,
            ((1036.0 - 2.0 * desktop::MARGIN) / desktop::CELL_H) as u32
        );
        // The last cell ends inside the margin.
        let last = cell_rect(Cell {
            col: g.cols - 1,
            row: g.rows - 1,
        });
        assert!(last.x + last.width <= 1920.0 - desktop::MARGIN);
        assert!(last.y + last.height <= 1036.0 - desktop::MARGIN);
    }

    #[test]
    fn a_tiny_surface_still_has_one_cell() {
        assert_eq!(grid_for(Size::new(10.0, 10.0)), Grid { cols: 1, rows: 1 });
    }

    #[test]
    fn a_point_finds_its_cell_and_is_clamped_to_the_grid() {
        let g = Grid { cols: 4, rows: 3 };
        let r = cell_rect(Cell { col: 2, row: 1 });
        assert_eq!(cell_at(p(r.x + 1.0, r.y + 1.0), g), Cell { col: 2, row: 1 });
        assert_eq!(cell_at(p(-50.0, -50.0), g), Cell { col: 0, row: 0 });
        assert_eq!(cell_at(p(9999.0, 9999.0), g), Cell { col: 3, row: 2 });
    }

    #[test]
    fn a_band_hits_every_cell_it_touches_whichever_way_it_is_dragged() {
        let rects: Vec<_> = (0..3).map(|c| cell_rect(Cell { col: c, row: 0 })).collect();
        let a = p(rects[0].x + 10.0, rects[0].y + 10.0);
        let b = p(rects[1].x + 10.0, rects[1].y + 60.0);
        assert_eq!(band_hits(band_rect(a, b), &rects), [0, 1]);
        assert_eq!(band_hits(band_rect(b, a), &rects), [0, 1]);
        // A band in the gap between margin and the first cell touches nothing.
        let none = band_rect(p(0.0, 0.0), p(desktop::MARGIN - 1.0, desktop::MARGIN - 1.0));
        assert!(band_hits(none, &rects).is_empty());
    }

    #[test]
    fn short_labels_stay_whole() {
        assert_eq!(label("notes.txt"), "notes.txt");
        assert_eq!(label("  Home  "), "Home");
    }

    #[test]
    fn a_label_wraps_at_spaces_in_two_lines() {
        assert_eq!(label("Quarterly budget draft"), "Quarterly\nbudget draft");
    }

    #[test]
    fn a_word_too_wide_for_a_line_is_split() {
        let out = label("aaaaaaaaaaaaaaaaaaaaaaaaa");
        let lines: Vec<_> = out.split('\n').collect();
        assert_eq!(lines.len(), 2);
        assert!(lines[1].ends_with('…'), "{out:?}");
    }

    #[test]
    fn a_long_name_is_clamped_to_two_lines_with_an_ellipsis() {
        let out = label("a really quite exceptionally long file name for a desktop icon.txt");
        let lines: Vec<_> = out.split('\n').collect();
        assert_eq!(lines.len(), desktop::LABEL_LINES);
        assert!(lines[1].ends_with('…'));
        assert!(lines.iter().all(|l| width_of(l) <= desktop::LABEL_UNITS));
    }

    #[test]
    fn a_long_name_breaks_after_a_separator_when_it_can() {
        assert_eq!(label("budget-2026-final.ods"), "budget-2026-\nfinal.ods");
    }

    #[test]
    fn an_empty_label_is_empty() {
        assert_eq!(label(""), "");
    }
}
