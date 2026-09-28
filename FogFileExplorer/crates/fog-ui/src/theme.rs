// SPDX-License-Identifier: AGPL-3.0-only

//! Every colour and size fog-ui draws with, in one place so M2 can replace
//! this module with the EclipseOS theme (FOG §Visual design) without touching
//! the views. Values follow the house style: warm near-black, white text at
//! three alphas, one gold accent that marks only the focused cursor, hard
//! edges.

use iced::Color;

/// `#rrggbb` at full opacity.
const fn rgb(r: u8, g: u8, b: u8) -> Color {
    Color::from_rgb8(r, g, b)
}

/// White at `a`.
const fn white(a: f32) -> Color {
    Color {
        r: 1.0,
        g: 1.0,
        b: 1.0,
        a,
    }
}

pub mod color {
    use super::{rgb, white, Color};

    /// Window base. Warm, never blue-grey.
    pub const BASE: Color = rgb(0x0b, 0x09, 0x06);
    /// Path bar and status line ground: one step above the list.
    pub const CHROME: Color = rgb(0x12, 0x10, 0x0b);
    /// Rule between the chrome rows and the list. Grounds and rules are
    /// opaque: iced blends in linear light, where a small white alpha over
    /// near-black lands far brighter than it reads on paper.
    pub const RULE: Color = rgb(0x2a, 0x27, 0x21);

    pub const TEXT: Color = white(1.0);
    pub const TEXT_SECONDARY: Color = white(0.64);
    pub const TEXT_TERTIARY: Color = white(0.40);

    /// Ground of the active tab: the tab opens onto the pane below it.
    pub const TAB_ACTIVE: Color = CHROME;
    /// Ground of the tab strip and idle tabs: a step below the chrome.
    pub const TAB_STRIP: Color = rgb(0x0e, 0x0c, 0x08);

    /// The single accent. It marks the cursor of whichever region has the
    /// keyboard (the list, the places, the palette, the path caret) and
    /// nothing else; the other regions' cursors go [`NEUTRAL`].
    pub const ACCENT: Color = rgb(0xf2, 0xc3, 0x3c);
    /// Accent as text, lifted for small sizes.
    pub const ACCENT_TEXT: Color = rgb(0xf5, 0xcf, 0x5c);
    /// Ground under the selected row.
    pub const ACCENT_FILL: Color = rgb(0x21, 0x1b, 0x0b);

    /// Errors and "fogd not running". Deliberately unlike the accent.
    pub const DANGER: Color = rgb(0xe0, 0x55, 0x3f);
    /// Warm neutral: the palette's "primary", which iced paints on the
    /// hovered or dragged scrollbar. Not the accent — the selection keeps
    /// the only gold.
    pub const NEUTRAL: Color = rgb(0x96, 0x91, 0x8a);
    pub const OK: Color = rgb(0x7f, 0xae, 0x5e);
    /// Ground under marked rows and under an unfocused cursor: neutral, so
    /// a selection never competes with the one gold value.
    pub const MARK_FILL: Color = rgb(0x1b, 0x19, 0x15);
    /// Ground of the job tray: the tab strip's lower step, so the tray
    /// reads as its own band between the list and the status line.
    pub const TRAY: Color = TAB_STRIP;
    /// A progress bar's empty track and its filled part. Neutral: a job's
    /// progress never takes the gold.
    pub const TRACK: Color = RULE;
    pub const PROGRESS: Color = NEUTRAL;
    /// Dims the window under the palette and the dialogs.
    pub const SCRIM: Color = Color {
        r: 0.0,
        g: 0.0,
        b: 0.0,
        a: 0.78,
    };
}

/// The iced theme, for the widgets fog-ui does not style itself (the list's
/// scrollbar): generated from the colours above.
pub fn iced_theme() -> iced::Theme {
    iced::Theme::custom(
        "Fog",
        iced::theme::Palette {
            background: color::BASE,
            text: color::TEXT,
            primary: color::NEUTRAL,
            success: color::OK,
            warning: color::DANGER,
            danger: color::DANGER,
        },
    )
}

pub mod size {
    /// Row height of the file list, in logical pixels.
    pub const ROW_H: f32 = 24.0;
    /// Body and data text.
    pub const TEXT: f32 = 13.0;
    /// The status line and the kind tags: smaller, quieter.
    pub const TEXT_SMALL: f32 = 11.5;
    /// Horizontal padding of every row and chrome line.
    pub const PAD_X: f32 = 14.0;
    /// Vertical padding of the path bar and status line.
    pub const CHROME_Y: f32 = 9.0;
    /// The accent bar at the left edge of the selected row.
    pub const BAR_W: f32 = 3.0;
    /// One device-independent pixel: rules.
    pub const HAIRLINE: f32 = 1.0;
    /// The right-aligned data columns of the list.
    pub const SIZE_W: f32 = 80.0;
    pub const DATE_W: f32 = 132.0;
    pub const TYPE_W: f32 = 64.0;
    /// iced's embedded scrollbar width: the column header leaves it free so
    /// its labels sit over the list's columns.
    pub const SCROLLBAR_W: f32 = 10.0;
    /// Column header height: tighter than a row, it is a caption.
    pub const HEADER_H: f32 = 22.0;
    /// Sidebar width, and the gap above each of its sections.
    pub const SIDEBAR_W: f32 = 184.0;
    pub const SECTION_GAP: f32 = 16.0;
    /// Tab strip height and the widest a tab grows.
    pub const TAB_H: f32 = 28.0;
    pub const TAB_MAX_W: f32 = 200.0;
    /// The palette overlay: width, distance from the top, rows shown.
    pub const PALETTE_W: f32 = 560.0;
    pub const PALETTE_TOP: f32 = 64.0;
    pub const PALETTE_ROWS: usize = 12;
    /// Text caret in the path bar and palette.
    pub const CARET_W: f32 = 2.0;
    /// Advance of one monospace glyph, in ems: turns a width into a
    /// character budget for eliding the path bar. Slightly generous, so an
    /// estimate never clips the current folder.
    pub const MONO_ADVANCE: f32 = 0.62;
    /// Gap between status line segments.
    pub const GAP: f32 = 18.0;
    /// The job tray: rows shown before it scrolls with its cursor, the
    /// kind column, the byte or file count column, the ETA column, and the
    /// height of each job's progress bar.
    pub const TRAY_ROWS: usize = 4;
    pub const KIND_W: f32 = 72.0;
    pub const AMOUNT_W: f32 = 168.0;
    pub const ETA_W: f32 = 72.0;
    pub const PROGRESS_H: f32 = 2.0;
    /// The trash view's "from" column: the folder an item was deleted from.
    pub const FROM_W: f32 = 220.0;
    /// The conflict and delete dialogs.
    pub const DIALOG_W: f32 = 560.0;
    /// Initial window size.
    pub const WINDOW_W: f32 = 960.0;
    pub const WINDOW_H: f32 = 640.0;
}
