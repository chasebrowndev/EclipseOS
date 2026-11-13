// SPDX-License-Identifier: AGPL-3.0-only
//! Inputs a `Term` needs that are not in the byte stream (spec gap: T-03 does not say how
//! they arrive; they are plain construction parameters so `feed` stays pure).

use std::time::Duration;

use apoth_grid::Limits;
use apoth_unicode::Widths;

/// 8-bit RGB.
pub type Rgb = (u8, u8, u8);

/// Cursor shape and blink (DECSCUSR).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CursorStyle {
    /// 0 block, 1 underline, 2 bar.
    pub shape: u8,
    /// Blinking.
    pub blink: bool,
}

/// Construction parameters of a `Term`.
#[derive(Clone, Debug)]
pub struct TermConfig {
    /// Initial rows.
    pub rows: usize,
    /// Initial columns.
    pub cols: usize,
    /// `render.sync-timeout`, clamped to 100..=5000 ms.
    pub sync_timeout: Duration,
    /// `clipboard.osc52-max` in bytes of OSC payload.
    pub osc52_max: usize,
    /// OSC 52 write allowed (default allow for local and remote, T-18 §3).
    pub osc52_write: bool,
    /// Width policy (T-04 §3).
    pub widths: Widths,
    /// Application SemVer for DA2 and XTVERSION.
    pub version: (u32, u32, u32),
    /// DA1 advertises sixel (attribute 4); false until M25.
    pub sixel_advertised: bool,
    /// Cell size in physical pixels (`w`, `h`) for `CSI 14/16 t` and mode 2048.
    pub cell_px: (u32, u32),
    /// Scrollback limits of the primary screen.
    pub scrollback: Limits,
    /// Title stack depth (spec gap; xterm's 10).
    pub title_stack_max: usize,
    /// Default foreground, background, cursor colour.
    pub fg: Rgb,
    /// Default background.
    pub bg: Rgb,
    /// Cursor colour.
    pub cursor_color: Rgb,
    /// Dark colour scheme (answers `CSI ? 996 n`).
    pub dark: bool,
    /// Cursor style at power-on.
    pub cursor: CursorStyle,
}

impl Default for TermConfig {
    fn default() -> Self {
        let v = env!("CARGO_PKG_VERSION");
        let mut it = v
            .split(['.', '-', '+'])
            .map(|p| p.parse::<u32>().unwrap_or(0));
        let version = (
            it.next().unwrap_or(0),
            it.next().unwrap_or(0),
            it.next().unwrap_or(0),
        );
        TermConfig {
            rows: 24,
            cols: 80,
            sync_timeout: Duration::from_millis(1000),
            osc52_max: 1 << 20,
            osc52_write: true,
            widths: Widths::default(),
            version,
            sixel_advertised: false,
            cell_px: (8, 16),
            scrollback: Limits::default(),
            title_stack_max: 10,
            fg: (0xe5, 0xe5, 0xe5),
            bg: (0, 0, 0),
            cursor_color: (0xe5, 0xe5, 0xe5),
            dark: true,
            cursor: CursorStyle {
                shape: 0,
                blink: false,
            },
        }
    }
}

impl TermConfig {
    /// `sync_timeout` clamped to the documented range.
    pub fn sync_timeout_clamped(&self) -> Duration {
        self.sync_timeout
            .clamp(Duration::from_millis(100), Duration::from_millis(5000))
    }
}
