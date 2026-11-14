// SPDX-License-Identifier: AGPL-3.0-only
//! Inputs a `Term` needs that are not in the byte stream (spec gap: T-03 does not say how
//! they arrive; they are plain construction parameters so `feed` stays pure).

use std::time::Duration;

use apoth_grid::Limits;
use apoth_unicode::Widths;

/// 8-bit RGB.
pub type Rgb = (u8, u8, u8);

/// Sequence classes the pane's policy denies (T-18 §3). A denied class has no observable
/// effect and produces no reply (T-18 §11 policy conformance). Main computes the word from
/// the sequence policy table and the pane's remote state; the io thread installs it with
/// [`crate::Term::set_deny`] between feeds, so `feed` stays deterministic for a given word.
pub mod deny {
    /// `title-set`: OSC 0/1/2 and the title stack pop.
    pub const TITLE_SET: u32 = 1 << 0;
    /// `title-report`: `CSI 20/21 t` (the reply is always empty when allowed).
    pub const TITLE_REPORT: u32 = 1 << 1;
    /// `window-ops-report`: `CSI 14/16/18/19 t`, title stack 22/23.
    pub const WINDOW_REPORT: u32 = 1 << 2;
    /// `colour`: OSC 4, 10-19, 104, 110-119 set and query.
    pub const COLOUR: u32 = 1 << 3;
    /// `notification`: OSC 9, 99, 777.
    pub const NOTIFICATION: u32 = 1 << 4;
    /// `hyperlink`: OSC 8.
    pub const HYPERLINK: u32 = 1 << 5;
    /// `clipboard-write`: OSC 52 set.
    pub const CLIPBOARD_WRITE: u32 = 1 << 6;
    /// `clipboard-read`: OSC 52 query (no request event is raised).
    pub const CLIPBOARD_READ: u32 = 1 << 7;
    /// `primary-selection`: OSC 52 naming `p` or `s`.
    pub const PRIMARY: u32 = 1 << 8;
    /// `user-vars`: OSC 1337 SetUserVar.
    pub const USER_VAR: u32 = 1 << 9;
    /// `graphics-direct`: kitty graphics (all of it) and sixel, XTSMGRAPHICS.
    pub const GRAPHICS_DIRECT: u32 = 1 << 10;
    /// `graphics-file` denied: file, temp-file and shm media have no effect, no reply.
    pub const GRAPHICS_FILE: u32 = 1 << 11;
    /// `graphics-file` is `ask`: the term raises [`crate::EventSlot::graphics_file`] and
    /// waits for [`crate::Term::graphics_file_decision`].
    pub const GRAPHICS_FILE_ASK: u32 = 1 << 12;
}

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
    /// Images: caps, quotas, which protocols are on (`graphics.*`, T-15). DA1 advertises
    /// sixel (attribute 4) when `graphics.sixel` is on.
    pub graphics: crate::graphics::GraphicsConfig,
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
    /// ANSI 0-15 at power-on and after OSC 104 (the theme's, T-17 §2; xterm's by default).
    pub ansi: [Rgb; 16],
    /// Dark colour scheme (answers `CSI ? 996 n`).
    pub dark: bool,
    /// Cursor style at power-on.
    pub cursor: CursorStyle,
    /// Denied sequence classes ([`deny`] bits; 0 = everything allowed).
    pub deny: u32,
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
            graphics: crate::graphics::GraphicsConfig::default(),
            cell_px: (8, 16),
            scrollback: Limits::default(),
            title_stack_max: 10,
            fg: (0xe5, 0xe5, 0xe5),
            bg: (0, 0, 0),
            cursor_color: (0xe5, 0xe5, 0xe5),
            ansi: crate::color::XTERM_ANSI16,
            dark: true,
            cursor: CursorStyle {
                shape: 0,
                blink: false,
            },
            deny: 0,
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
