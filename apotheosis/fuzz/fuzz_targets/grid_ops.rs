// SPDX-License-Identifier: AGPL-3.0-only
//! apoth-grid: random edit sequences. No panic; dimensions stay fixed; history stays within
//! its line limit; every cell is readable afterwards.
#![no_main]
use apoth_grid::{CellAttrs, Color, Glyph, Limits, Region, Screen, ScreenConfig};
use libfuzzer_sys::arbitrary::{Result, Unstructured};
use libfuzzer_sys::fuzz_target;

fn go(u: &mut Unstructured<'_>) -> Result<()> {
    let rows = u.int_in_range(1..=40usize)?;
    let cols = u.int_in_range(1..=80usize)?;
    let hist = u.int_in_range(0..=50usize)?;
    let cfg = ScreenConfig {
        history: u.arbitrary()?,
        limits: Limits { bytes: 64 << 20, lines: Some(hist) },
    };
    let mut s = Screen::new(rows, cols, cfg);
    let attrs = CellAttrs::default();
    let bg = Color::Default;
    for _ in 0..256 {
        if u.is_empty() {
            break;
        }
        let y = u.int_in_range(0..=rows - 1)?;
        let x = u.int_in_range(0..=cols - 1)?;
        match u.int_in_range(0..=8u8)? {
            0 => {
                let n = u.int_in_range(0..=16usize)?;
                let mut run = Vec::with_capacity(n);
                for _ in 0..n {
                    let cp = u.int_in_range(0x20..=0x10ffffu32)?;
                    let cp = char::from_u32(cp).map_or(0x41, |c| c as u32);
                    run.push(Glyph { cp, width: u.int_in_range(1..=2u8)? });
                }
                s.print_run(y, x, &run, &attrs);
            }
            1 => {
                let x1 = u.int_in_range(x..=cols - 1)?;
                s.erase_cells(y, x, x1, bg, u.arbitrary()?);
            }
            2 => {
                let y1 = u.int_in_range(y..=rows - 1)?;
                s.erase_rows(y, y1, bg, u.arbitrary()?);
            }
            3 => s.insert_chars(y, x, u.int_in_range(0..=100usize)?, u.int_in_range(x..=cols - 1)?, bg),
            4 => s.delete_chars(y, x, u.int_in_range(0..=100usize)?, u.int_in_range(x..=cols - 1)?, bg),
            5..=8 => {
                let bottom = u.int_in_range(y..=rows - 1)?;
                let right = u.int_in_range(x..=cols - 1)?;
                let region = Region { top: y, bottom, left: x, right };
                let n = u.int_in_range(0..=64usize)?;
                match u.int_in_range(0..=3u8)? {
                    0 => s.scroll_up(region, n, bg),
                    1 => s.scroll_down(region, n, bg),
                    2 => s.delete_lines(region, n, bg),
                    _ => s.insert_lines(region, n, bg),
                }
            }
            _ => {}
        }
    }
    assert_eq!((s.rows(), s.cols()), (rows, cols));
    assert!(s.history_rows() <= hist, "history over its line limit");
    let mut buf = Vec::new();
    for y in 0..rows {
        for x in 0..cols {
            let _ = s.cell(y, x);
            s.cell_text(y, x, &mut buf);
            buf.clear();
        }
    }
    Ok(())
}

fuzz_target!(|data: &[u8]| {
    let _ = go(&mut Unstructured::new(data));
});
