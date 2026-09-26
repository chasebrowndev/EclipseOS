// SPDX-License-Identifier: AGPL-3.0-only
//! The style spec (`docs/STYLE.md`), transcribed once.
//!
//! Every colour, radius, size and weight the desktop uses lives here. The
//! point is not tidiness: it is that "warm near-black, never blue-grey" and
//! "one live yellow per pane" are design rules that only hold if there is a
//! single place they can be checked against.

use iced::{Color, Font};

/// `#rrggbb` at full opacity.
const fn rgb(hex: u32) -> Color {
    Color {
        r: ((hex >> 16) & 0xff) as f32 / 255.0,
        g: ((hex >> 8) & 0xff) as f32 / 255.0,
        b: (hex & 0xff) as f32 / 255.0,
        a: 1.0,
    }
}

/// White at `a`. The spec expresses nearly every surface and text colour this
/// way, because they all sit over the same warm base and over live wallpaper.
const fn white(a: f32) -> Color {
    Color {
        r: 1.0,
        g: 1.0,
        b: 1.0,
        a,
    }
}

/// `fg` composited over an opaque `bg` (Porter-Duff "over", assuming
/// `bg.a == 1.0`). Used once, for [`color::GLASS_DEEP_BACKED`]: a floating
/// surface's translucent tint is meant to sit over the compositor's blur
/// backdrop, but that backdrop is a purely compositor-side decision the
/// client is never told about (BLUR-01), so when it is off there is nothing
/// behind the tint. Pre-compositing it over an opaque token here gives the
/// same tint a floor that holds regardless.
const fn over_opaque(fg: Color, bg: Color) -> Color {
    let keep = 1.0 - fg.a;
    Color {
        r: fg.r * fg.a + bg.r * keep,
        g: fg.g * fg.a + bg.g * keep,
        b: fg.b * fg.a + bg.b * keep,
        a: 1.0,
    }
}

pub mod color {
    use super::{rgb, white, Color};

    /// Window and desktop base. Warm, never blue-grey.
    pub const BASE: Color = rgb(0x0b0906);
    /// Opaque surfaces behind glass, darkest to lightest.
    pub const SURFACE_0: Color = rgb(0x12100b);
    pub const SURFACE_1: Color = rgb(0x1a1712);
    pub const SURFACE_2: Color = rgb(0x1c1913);

    /// The glass fill. Thin on purpose — the depth comes from the
    /// compositor's blur underneath, not from this layer's opacity.
    pub const GLASS: Color = white(0.045);
    pub const GLASS_STRONG: Color = white(0.06);
    /// The ground of a surface that floats over live wallpaper with the
    /// compositor's blur behind it — a bar, a toast, the launcher.
    ///
    /// One warm translucent colour rather than a black scrim plus a white
    /// lift, because a container has exactly one background and the two
    /// layers composite to this. It is darker than [`GLASS`] on purpose: a
    /// pane's glass sits over a known base, this sits over whatever the
    /// wallpaper happens to be, and 11px mono has to stay legible on both.
    pub const GLASS_DEEP: Color = Color {
        a: 0.62,
        ..rgb(0x17140f)
    };
    /// [`GLASS_DEEP`], pre-composited over [`SURFACE_1`] so the result is
    /// opaque. Use this, not `GLASS_DEEP` directly, for a surface's own
    /// background — see the note on [`super::over_opaque`] (BLUR-01).
    pub const GLASS_DEEP_BACKED: Color = super::over_opaque(GLASS_DEEP, SURFACE_1);
    /// The ground under a floating sheet that must stay readable over any
    /// wallpaper — a context menu, a tray drawer. Nearly opaque on purpose:
    /// a menu is a mark rail, and a mark rail that lets the desktop through
    /// is a mark rail you cannot read.
    pub const MENU_GROUND: Color = rgb(0x17140f);
    pub const BORDER: Color = white(0.10);
    pub const BORDER_STRONG: Color = white(0.16);
    /// Hairline between rows in an inset list.
    pub const HAIRLINE: Color = white(0.055);
    /// Top edge highlight, the one thing that reads as a light source. `SOFT`
    /// and `STRONG` are the ends of the spec's `.07–.2` inset range: soft on
    /// a big calm surface, strong on a small chip that has to pop.
    pub const HIGHLIGHT: Color = white(0.12);
    pub const HIGHLIGHT_SOFT: Color = white(0.07);
    pub const HIGHLIGHT_STRONG: Color = white(0.20);

    /// Hover and press over glass. Depth on a glass surface comes from a fill
    /// *and* a border *and* a highlight moving together — a lone grey fill
    /// swap reads as a slab dropped on the surface, which is the exact bug
    /// this pair exists to avoid.
    pub const LIFT: Color = white(0.08);
    pub const LIFT_STRONG: Color = white(0.13);
    /// The hover *fill* for a cell that already has an outline.
    ///
    /// [`LIFT`] is white(.08) and [`BORDER`] is white(.10): on a chip that
    /// draws both, the fill arrives at almost exactly the outline's value and
    /// the chip reads as one flat lighter slab instead of an outlined shape
    /// with a wash inside it. A hover fill must stay clearly *under* the
    /// outline it sits within — dimmer, and more transparent — so that the
    /// outline keeps describing the shape and the fill only says "the pointer
    /// is here".
    pub const LIFT_SOFT: Color = white(0.035);
    /// Sidebar ground: darker than the panes it sits beside.
    pub const SIDEBAR: Color = Color {
        r: 0.0,
        g: 0.0,
        b: 0.0,
        a: 0.4,
    };

    /// The single accent. See `accent discipline` in the spec: it marks state
    /// and one live value per screen, and is never decorative.
    pub const ACCENT: Color = rgb(0xf2c33c);
    /// Accent as text — lifted, because #f2c33c on near-black is heavy at
    /// 12px.
    pub const ACCENT_TEXT: Color = rgb(0xf5cf5c);
    pub const ACCENT_FILL: Color = Color { a: 0.09, ..ACCENT };
    pub const ACCENT_BORDER: Color = Color { a: 0.28, ..ACCENT };
    /// The faintest accent ground — a wash under a whole row, not a fill.
    pub const ACCENT_WASH: Color = Color { a: 0.035, ..ACCENT };
    /// The pressed/active accent ground, one step above [`ACCENT_FILL`].
    pub const ACCENT_FILL_STRONG: Color = Color { a: 0.18, ..ACCENT };

    pub const TEXT: Color = white(1.0);
    pub const TEXT_SECONDARY: Color = white(0.64);
    pub const TEXT_TERTIARY: Color = white(0.40);
    /// Warm neutral for swatches and inert indicators.
    pub const NEUTRAL: Color = rgb(0x96918a);

    /// Off state of a toggle, and the inert bars of a chart.
    pub const CONTROL_OFF: Color = white(0.13);
    pub const TRACK: Color = white(0.12);

    /// Status. Deliberately not red/green pairs from a generic palette — a
    /// desktop that only ever goes yellow needs its alarms to look unlike it.
    pub const DANGER: Color = rgb(0xe0553f);
    /// The ground behind a destructive verb on hover.
    pub const DANGER_FILL: Color = Color { a: 0.14, ..DANGER };
    /// *Connected* — a link that is not merely up but carrying traffic to a
    /// peer. A status colour beside [`DANGER`] and [`OK`], not a second
    /// accent: it is never used for emphasis, selection or focus, only to say
    /// a radio has a peer on the other end. Bluetooth reads
    /// [`NEUTRAL`] off, [`ACCENT`] powered-but-idle, this connected.
    pub const CONNECTED: Color = rgb(0x4f9fe0);
    pub const OK: Color = rgb(0x7fae5e);
}

pub mod radius {
    pub const WINDOW: f32 = 16.0;
    pub const CARD: f32 = 13.0;
    pub const INSET: f32 = 10.0;
    /// Anything fully round: pills, toggles, knobs.
    pub const PILL: f32 = 99.0;
    /// A canvas chip: squarer than a pill, so an object never reads as a verb.
    pub const CHIP: f32 = 6.0;
    /// Bar-chart caps.
    pub const BAR: f32 = 2.5;
}

pub mod space {
    /// Content column padding, per the spec's 26/30.
    pub const PANE_X: f32 = 30.0;
    pub const PANE_Y: f32 = 26.0;
    /// Between blocks in the content column.
    pub const BLOCK: f32 = 18.0;
    pub const CARD: f32 = 16.0;
    pub const ROW_Y: f32 = 11.0;
    /// The accent bar that marks the current item at the left edge of a nav
    /// item or a choice row. Three pixels, per the spec.
    pub const BAR_W: f32 = 3.0;
    pub const SIDEBAR_W: f32 = 214.0;
    /// The square a small indicator mark (signal bars, battery gauge) is
    /// drawn into. Deliberately smaller than an icon: a mark reports a
    /// magnitude, it does not identify anything.
    pub const MARK: f32 = 11.0;
    pub const MARK_BAR: f32 = 2.0;
    pub const MARK_GAP: f32 = 1.0;
    pub const MARK_CELL_W: f32 = 18.0;
    pub const MARK_BORDER: f32 = 1.0;
    /// A sidebar nav item's selected bar, and the placeholder square that
    /// stands in for its icon.
    pub const NAV_BAR_H: f32 = 18.0;
    pub const NAV_GLYPH: f32 = 15.0;
    /// One device-independent pixel: a border, a rule, an edge highlight.
    pub const HAIRLINE: f32 = 1.0;
    /// The drag track of a numeric control in a settings row.
    pub const SLIDER_W: f32 = 180.0;
    /// The typed-entry box beside that track: wide enough for a signed
    /// four-digit reading, narrow enough that it reads as a readout.
    pub const NUMBER_W: f32 = 64.0;
    /// A free-text field in a settings row.
    pub const FIELD_W: f32 = 220.0;
    /// Between the two halves of one control (track and entry).
    pub const CONTROL_GAP: f32 = 10.0;
    /// Between the pills of one segmented choice.
    pub const PILL_GAP: f32 = 6.0;
    /// A hero's level bar: half the content column, so the reading beside it
    /// keeps the other half.
    pub const HERO_METER_W: f32 = 360.0;
    /// A canvas chip's padding, and the gap between chips in a lane.
    pub const CHIP_Y: f32 = 5.0;
    pub const CHIP_X: f32 = 10.0;
    pub const CHIP_GAP: f32 = 6.0;
    /// Between a chip's ordinal and its name.
    pub const CHIP_ORDINAL_GAP: f32 = 7.0;
    /// One row of chips. An empty lane keeps it, so moving the last chip
    /// out does not collapse the canvas under the pointer.
    pub const CHIP_H: f32 = 28.0;
}

pub mod size {
    pub const PANE_TITLE: f32 = 24.0;
    pub const CARD_TITLE: f32 = 13.0;
    pub const BODY: f32 = 13.0;
    pub const BODY_SMALL: f32 = 12.0;
    pub const MONO: f32 = 11.5;
    /// Small uppercase section labels — mono, wide tracking.
    pub const MICRO: f32 = 10.0;
    pub const BIG_NUMBER: f32 = 32.0;
    /// A single live line of input at hero scale — the launcher's query. Big
    /// enough to be the block you look at, small enough to stay one line.
    pub const PROMPT: f32 = 17.0;
    /// The square an application icon is drawn into, on a bar or in a list.
    pub const ICON: f32 = 20.0;
}

/// The taskbar's own metrics.
///
/// A bar is not a pane: it has no 26/30 content column and no 18px block gap,
/// because its whole job is to be one dense row at an edge of the screen. So
/// its geometry is its own module rather than a reuse of `space`, and it is
/// still here rather than in the bar crate so that a second edge surface
/// (a dock, a second monitor's bar) cannot drift from it.
pub mod bar {
    /// Bar height in logical pixels, and therefore its exclusive zone. Tall
    /// enough for a two-line clock and an icon-led task button.
    pub const HEIGHT: f32 = PILL_H + 2.0 * MARGIN_Y;
    /// The thickness of an edge marker on the bar. The bar itself no longer
    /// draws a rule top or bottom — see [`PILL_H`].
    pub const HAIRLINE: f32 = 1.0;
    /// The height of the bar's own sheet, inside the strip it reserves.
    ///
    /// The bar is a wide pill floating in the reserved strip, not a slab that
    /// spans the screen between two rules. Two full-width hard lines are the
    /// loudest thing that could possibly be drawn across the top of a desktop,
    /// and `docs/STYLE.md` never asked for them: every surface it describes is
    /// a rounded, bordered, softly-lit panel. The margins below are what turns
    /// the strip into a frame around that panel.
    pub const PILL_H: f32 = 40.0;
    /// Air between the bar's sheet and the edges of the strip it reserves.
    ///
    /// Applied as layer-shell margin, never as padding inside the surface:
    /// the compositor blurs the whole surface at `bar.rounding`, so any air
    /// drawn inside it shows as a blurred rim around the pill.
    pub const MARGIN_X: f32 = 10.0;
    pub const MARGIN_Y: f32 = 6.0;
    /// The y of the bar sheet's bottom edge, in surface-local coordinates:
    /// where a popup anchored *below a cell* begins. The sheet fills its
    /// surface, so this is the sheet's own height.
    pub const SHEET_BOTTOM: f32 = PILL_H;
    /// Padding at the far left and far right of the row.
    pub const EDGE: f32 = 8.0;
    /// Gap between two cells of the same zone.
    pub const GAP: f32 = 4.0;
    /// Gap between two zones.
    pub const ZONE_GAP: f32 = 8.0;
    /// Padding inside a clickable cell.
    pub const CELL_X: f32 = 10.0;
    /// A pager tile: a small rounded chip, one per workspace.
    pub const PAGER_W: f32 = 24.0;
    pub const PAGER_H: f32 = 24.0;

    /// Radii, on the spec's inset-chip scale (9-11). A bar cell is a chip:
    /// it is small, it sits on glass, and it is never a card. The bar's own
    /// outline is a pill — [`RADIUS_SHEET`], half its height.
    pub const RADIUS_SHEET: f32 = PILL_H / 2.0;
    pub const RADIUS_CELL: f32 = 11.0;
    pub const RADIUS_TILE: f32 = 9.0;
    /// The rounded square a stand-in icon is drawn into, matching the
    /// reference panes' 6px-on-18px placeholder squares.
    pub const RADIUS_ICON: f32 = 6.0;
    /// The square a tray mark (wifi, bluetooth) is drawn into. Smaller than
    /// an application icon: a status mark reports a state, it does not
    /// identify a program.
    pub const MARK: f32 = 14.0;
    /// Thickness of the launcher mark's ring.
    pub const RING: f32 = 2.0;

    /// The launcher mark's outer diameter: the corona, and the iris when
    /// Oracle-Eyes opens it into an eye. A fraction of the icon square so the
    /// one circle on the row sits inside the same box as the task icons.
    pub const EYE_DISC: f32 = super::size::ICON * 0.85;
    /// The pupil while watching, as a fraction of the iris radius. Wide
    /// enough to read as a pupil and not a ring; small enough that a full
    /// wander still leaves a gold rim all the way round.
    pub const EYE_PUPIL: f32 = 0.4;
    /// How far the pupil's centre may wander from the iris's, as a fraction
    /// of the iris radius. `EYE_PUPIL + EYE_WANDER` is 0.76: on the 8.5px
    /// iris of [`EYE_DISC`] the pupil's far edge reaches 6.46px, inside the
    /// plain ring's 6.5px hole (outer radius less [`RING`]), leaving a 2.04px
    /// rim. The cap is `1 - RING / (EYE_DISC / 2)`, about 0.765: past it the
    /// pupil would cut into the ring drawn beneath the eye and show its gold
    /// through the hole.
    pub const EYE_WANDER: f32 = 0.36;
    /// A glance lands at least this fraction of [`EYE_WANDER`] off centre.
    /// Uniform points in the disc cluster near the middle and read as a
    /// twitch; a glance has to visibly look somewhere.
    pub const EYE_GLANCE: f32 = 0.8;
    /// One dart in this many looks straight ahead again instead.
    pub const EYE_RECENTRE: u64 = 6;
    /// The pupil's radius while thinking: a point, in logical pixels.
    pub const EYE_PINPOINT: f32 = 1.5;
    /// One saccade: quick, eased out, so it reads as a glance and not a drift.
    pub const EYE_DART_MS: u64 = 120;
    /// The pupil opening or narrowing between states.
    pub const EYE_RESIZE_MS: u64 = 200;
    /// The pupil holds still between darts for a random span in this range:
    /// long enough to read as a fixation, short enough to read as scanning.
    pub const EYE_HOLD_MIN_MS: u64 = 400;
    pub const EYE_HOLD_MAX_MS: u64 = 1400;
    /// Frame interval while a dart or resize is in flight — and only then.
    pub const EYE_FRAME_MS: u64 = 16;
    /// Task buttons clamp between these. Past the minimum a button is
    /// icon-only rather than overflowing the row.
    pub const TASK_MIN: f32 = 34.0;
    pub const TASK_MAX: f32 = 176.0;
    /// Height of the ground a task button paints, inside the bar.
    pub const TASK_H: f32 = 34.0;
    /// The running/focused marker under a task button.
    pub const MARKER: f32 = 2.0;

    /// The condensation ladder's rungs, in per-chip logical pixels.
    ///
    /// The stage a chip draws at is *derived*: the strip divides the width the
    /// fixed zones leave over among the windows that are on the workspace, and
    /// the resulting per-chip width is looked up here. So the ladder is "what
    /// fits" and never "how many are open" — opening a window on a 3440px
    /// screen does not condense anything, and three on a 900px one does.
    ///
    /// - at or above [`TASK_FULL`] a chip shows its icon and its title;
    /// - at or above [`TASK_NAME`] it shows its icon and its process name;
    /// - at or above [`TASK_MIN`] it is the icon alone;
    /// - below that it is [`TASK_BARE`]: a ground with its accent and nothing
    ///   in it. That is the last resort and it is still a click target.
    pub const TASK_FULL: f32 = 116.0;
    pub const TASK_NAME: f32 = 68.0;
    pub const TASK_BARE: f32 = 14.0;

    /// Width of a fixed tray cell (mark plus its reading) and of the clock.
    ///
    /// Fixed, and not hugged to their content, because the task strip's width
    /// is computed by subtracting the fixed zones from the bar: a tray that
    /// changed width when the signal reading went from `9` to `100` would move
    /// the ladder's rungs under the pointer.
    pub const TRAY_CELL_W: f32 = 54.0;

    /// Width of a tray cell that is a mark and nothing else.
    ///
    /// Some readings are not worth a number. A signal percentage is one: it
    /// changes constantly, nobody acts on it, and a live digit beside a cone
    /// that already shows the same magnitude is noise with a refresh rate. The
    /// cone keeps the reading; the digits go.
    pub const TRAY_MARK_W: f32 = 30.0;
    /// The gap *inside* the tray. Much tighter than [`ZONE_GAP`], because the
    /// tray's cells are one group and not three zones: the readings sit
    /// shoulder to shoulder the way they do on Windows, and the wide gap is
    /// spent on separating the tray from the task strip instead.
    pub const TRAY_GAP: f32 = 2.0;
    pub const CLOCK_W: f32 = 74.0;
    /// The tray drawer's disclosure arrow — the only triangle on the row.
    pub const ARROW_W: f32 = 20.0;
    pub const ARROW: f32 = 7.0;
    /// The air between the last tray cell and the arrow.
    ///
    /// Wider than the [`ZONE_GAP`] on the arrow's other side on purpose: the
    /// clock centres a short string in [`CLOCK_W`], so it already carries
    /// about a zone gap of empty box on its left edge, where a tray cell's ink
    /// runs almost to its box. Equal *boxes* put the chevron visibly against
    /// the battery; this puts it midway between the two readings the eye
    /// actually measures from.
    pub const ARROW_LEAD: f32 = 2.0 * ZONE_GAP;
    /// Stroke of the arrow's two legs.
    pub const ARROW_STROKE: f32 = 1.5;
    /// The `+N` cell drawn when not even [`TASK_BARE`] chips fit. A bar that
    /// silently omits windows is lying; this is the one honest cell.
    pub const OVERFLOW_W: f32 = 30.0;

    /// Nominal advance of one character of the UI face at `size::BODY_SMALL`,
    /// used to turn a chip's pixel width into a character budget.
    ///
    /// It is an approximation on purpose: iced has no eliding text widget and
    /// the row must stay a pure function of the snapshot, so the label is cut
    /// before layout. Erring a little narrow costs a character; erring wide
    /// would cost the clipping this whole ladder exists to prevent.
    pub const CHAR_W: f32 = 7.0;

    // ------------------------------------------------------------- widgets
    //
    // ADR 0065: every bar widget is `[grip][revealed][core]` in one shell.
    // These are the shell's metrics and its parts', decided once so that a
    // user's custom widget and a shipped preset are the same object.

    /// Height of a widget shell: the same ground a task chip paints, so the
    /// row keeps one baseline.
    pub const WIDGET_H: f32 = TASK_H;
    /// The grip's column: wide enough to be a target at bar height, narrow
    /// enough that a compressed widget is a sliver and not a chip.
    pub const GRIP_W: f32 = 14.0;
    /// One grip dot, square — hard-edged on purpose, so the grip reads as a
    /// machined texture rather than a row of bullets.
    pub const GRIP_DOT: f32 = 2.0;
    /// Air between two grip dots, on both axes.
    pub const GRIP_DOT_GAP: f32 = 2.0;
    pub const GRIP_COLS: usize = 2;
    pub const GRIP_ROWS: usize = 4;
    /// Air between two widgets that are both compressed to their grips. A
    /// run of grips at the full [`GAP`] reads as a row of identical empty
    /// chips; closed up, it reads as one rack of handles — each still its
    /// own cell and its own drag.
    pub const GRIP_RUN_GAP: f32 = 1.0;
    /// How much of the cell ground (hairline and lift) a widget compressed
    /// to its grip keeps, as a fraction. The grip's dots are the object; the
    /// capsule around a lone grip is only there to say where it ends.
    pub const GRIP_GROUND: f32 = 0.4;
    /// Padding inside the shell, either side of the core and the revealed
    /// section.
    pub const WIDGET_X: f32 = 8.0;
    /// Between items inside a core (art and title, meter and meter).
    pub const WIDGET_GAP: f32 = 8.0;
    /// Corner radius of the shell: the task chip's own. A widget is a glass
    /// cell of the same family as the window chips beside it, by the owner's
    /// direction (ADR 0065); the grip is what tells the two apart.
    pub const RADIUS_WIDGET: f32 = RADIUS_CELL;

    /// The visualizer: sixteen bands, each a hard 2px column with 2px of air,
    /// mirrored about the row's midline.
    pub const VIZ_BANDS: usize = 16;
    pub const VIZ_BAR_W: f32 = 2.0;
    pub const VIZ_GAP: f32 = 2.0;
    pub const VIZ_W: f32 = VIZ_BANDS as f32 * (VIZ_BAR_W + VIZ_GAP) - VIZ_GAP;
    pub const VIZ_H: f32 = 20.0;
    /// A silent band still draws this tall, so silence is a dotted rule and
    /// not an empty box.
    pub const VIZ_FLOOR: f32 = 2.0;

    /// A mini meter: a micro label and a reading on one line, a hairline
    /// track under them.
    pub const METER_W: f32 = 52.0;
    pub const METER_H: f32 = 2.0;
    /// Between the label line and the track.
    pub const METER_GAP: f32 = 4.0;

    /// One transport button (prev / play-pause / next), and the glyph drawn
    /// in it.
    pub const TRANSPORT_BTN: f32 = 24.0;
    pub const TRANSPORT_GLYPH: f32 = 9.0;
    /// Stroke of the skip glyphs' stop bar and of the pause glyph's legs.
    pub const TRANSPORT_STROKE: f32 = 2.0;
    pub const TRANSPORT_GAP: f32 = 2.0;
    pub const RADIUS_TRANSPORT: f32 = 5.0;

    /// The volume track and its readout.
    pub const VOLUME_W: f32 = 64.0;
    pub const VOLUME_RAIL: f32 = 2.0;
    /// The knob: a hard vertical tick, not a disc — at 34px a round knob is a
    /// bead on a wire and reads as decoration.
    pub const VOLUME_KNOB_W: u16 = 2;
    pub const VOLUME_KNOB_H: f32 = 12.0;
    /// Room for `100` in the mono face, so the track never shifts as the
    /// reading changes width.
    pub const VOLUME_READOUT_W: f32 = 22.0;
    /// Between the track and its reading.
    pub const VOLUME_GAP: f32 = 6.0;

    /// Album art: a square on the icon grid, a touch larger than an icon
    /// because it carries a picture, not a mark.
    pub const ART: f32 = 24.0;
    pub const RADIUS_ART: f32 = 4.0;
    /// The title/artist column of a media core, fixed so that the shell's
    /// width never tracks the song's name.
    pub const MEDIA_TEXT_W: f32 = 132.0;
    /// Between a two-line label's title and its subtitle.
    pub const LABEL_LINE_GAP: f32 = 1.0;
}

/// The bar, as a settings pane draws it: a live preview strip, the lane of
/// widget tiles under it, and the parts of a widget editor (ADR 0065).
///
/// The preview reuses [`bar`]'s metrics wholesale — it is the bar, at 1:1 —
/// so this holds only what exists in a pane and never on the bar itself.
pub mod canvas {
    use super::bar;

    /// The widest a lane tile gets. Narrower when the lane holds more
    /// widgets than fit, so the lane never wraps: one row is one order.
    pub const TILE_W: f32 = 136.0;
    /// The narrowest a lane tile gets: its grip, a pin and four characters.
    pub const TILE_MIN_W: f32 = 64.0;
    /// A tile is a bar cell, so it is a bar cell's height.
    pub const TILE_H: f32 = bar::TASK_H;
    /// Between two tiles: the bar's own cell gap, doubled, because a lane is
    /// a thing you aim a drag at and a bar is not.
    pub const TILE_GAP: f32 = 2.0 * bar::GAP;

    /// The pin: a square head on a short stem. Hard-edged, like the grip.
    pub const PIN_HEAD: f32 = 6.0;
    pub const PIN_STEM_W: f32 = 2.0;
    pub const PIN_STEM_H: f32 = 4.0;
    /// The column a pin sits in, so a tile does not shift when it gains one.
    pub const PIN_W: f32 = 10.0;

    /// The motion demo's track, and the chip that glides along it.
    pub const GLIDE_W: f32 = super::space::HERO_METER_W;
    pub const GLIDE_CHIP_W: f32 = 2.0 * bar::TASK_MIN;
    pub const GLIDE_RAIL: f32 = 1.0;

    /// Most windows the preview's stepper offers: far enough down the ladder
    /// to reach the `+N` cell on a pane-wide bar.
    pub const WINDOWS_MAX: usize = 40;
    /// What the preview opens on: enough windows that the chips have to
    /// negotiate with the widgets.
    pub const WINDOWS_DEFAULT: usize = 3;

    /// An argument chip in an argv editor, and the gap before its remove
    /// mark.
    pub const ARG_GAP: f32 = 6.0;
    /// The caret under a positioned config error, a hard rule under the
    /// offending span.
    pub const CARET_H: f32 = 2.0;
    /// One character cell of the data face at [`super::size::MONO`]:
    /// JetBrains Mono advances 0.6 em. What lines a caret up under a column.
    pub const MONO_CHAR_W: f32 = 0.6 * super::size::MONO;
    /// The drop marker between lane tiles while one is dragged: a hard
    /// rule where the tile will land.
    pub const DROP_MARK_W: f32 = 2.0;
    /// The width a bar preview solves for before its sheet has been
    /// measured: one frame at most, and never drawn animated.
    pub const SHEET_FALLBACK_W: f32 = 720.0;
}

/// Motion defaults. Taste, not mechanism: the live values are
/// `bar.motion.{enabled, curve, duration-ms}` (ADR 0065), and these apply when
/// they are unset or the compositor is not there.
pub mod motion {
    /// How long a movement takes to settle.
    pub const DURATION_MS: u64 = 220;
    /// One frame of the bar's motion clock, while anything moves.
    pub const FRAME_MS: u64 = 16;
    /// Travel under which a grip's press and release is a tap, not a drag.
    pub const TAP_SLOP: f32 = 4.0;
    /// How far ahead a released drag is projected along its velocity, in
    /// seconds, to choose the rest it snaps to: a flick carries past the
    /// midpoint the finger never reached.
    pub const FLING_LOOKAHEAD_S: f32 = 0.12;
    /// Weight of the newest sample in a drag's smoothed velocity.
    pub const VELOCITY_BLEND: f32 = 0.6;
    /// The first fraction of a cell's travel over which its content stays
    /// fully transparent. Content fades *ahead of* its clip: closing, it is
    /// gone before the edge reaches a glyph; opening, it arrives once there
    /// is room to read it. Never a glyph cut in half at full ink.
    pub const FADE_LEAD: f32 = 0.35;
}

/// A context menu: the mark rail that opens on a right-click.
pub mod menu {
    /// Width of the sheet, and the padding inside its border.
    pub const W: f32 = 208.0;
    pub const PAD: f32 = 6.0;
    /// One verb row, and the padding inside it.
    pub const ROW_H: f32 = 30.0;
    pub const ROW_X: f32 = 8.0;
    /// The square a row's leading mark occupies, and the gap after it.
    pub const MARK: f32 = 16.0;
    pub const MARK_GAP: f32 = 10.0;
    /// The placeholder drawn when the host's icon theme has no such mark.
    pub const MARK_INNER: f32 = 9.0;
    pub const RADIUS_MARK: f32 = 3.0;
    /// A checkable entry's tick box: square, so on (filled) and off (a
    /// hairline frame) are one shape and never read as the rounded
    /// icon placeholder.
    pub const RADIUS_TICK: f32 = 0.0;
    /// Corner radius of the sheet, and of a hovered row inside it.
    pub const RADIUS: f32 = 12.0;
    pub const RADIUS_ROW: f32 = 8.0;
    /// A separator's own height, so a menu's pixel height — fixed at popup
    /// creation — can be computed before layout.
    pub const SEP_H: f32 = 9.0;
}

/// A tray drawer: the reading rail the disclosure arrow opens.
pub mod drawer {
    /// Drawer width. Wider than a context menu — it carries label/value rows
    /// rather than single verbs — and still narrower than a pane.
    pub const W: f32 = 248.0;
    /// One reading row.
    pub const ROW_H: f32 = 30.0;
    /// The drawer's own heading strip, above the rows.
    pub const HEAD_H: f32 = 24.0;
    /// Padding inside the drawer's border.
    pub const PAD: f32 = 8.0;
    /// Padding inside a row.
    pub const ROW_X: f32 = 8.0;
    /// The square a row's leading mark occupies, and the gap after it.
    pub const MARK: f32 = 16.0;
    pub const MARK_GAP: f32 = 9.0;
    /// Corner radius of a hovered row, on the inset-chip scale.
    pub const RADIUS_ROW: f32 = 8.0;
    /// Height of a row's magnitude meter, and the radius that makes it a
    /// lozenge rather than a stick.
    pub const METER_H: f32 = 4.0;
    pub const RADIUS_METER: f32 = 2.0;
    /// The heading strip of a radio drawer, which carries the 25px on/off
    /// toggle beside its label and so cannot be the bare [`HEAD_H`].
    pub const HEAD_SWITCH_H: f32 = 36.0;
    /// The drawer's hero row: the one thing it is connected to, two lines
    /// tall so it cannot be mistaken for a member of the list beneath it.
    pub const CURRENT_H: f32 = 48.0;
    /// The larger mark that leads the hero row.
    pub const CURRENT_MARK: f32 = 20.0;
    /// How many list rows a drawer shows before it stops growing. A popup is
    /// sized once at creation, so a room full of access points must not be
    /// allowed to push the sheet off the bottom of a laptop panel.
    pub const MAX_ROWS: usize = 8;
    /// Air between the two lines of the hero row.
    pub const LINE_GAP: f32 = 2.0;
}

/// The secret prompt: one field in a fixed-size toplevel. Fixed because
/// abyss floats a toplevel whose min and max size agree (ADR 0053), and a
/// password box has nothing to show a user who resizes it.
pub mod secret {
    pub const W: f32 = 404.0;
    /// Tall enough for heading, band and a pill row at `PANE_Y` padding.
    pub const H: f32 = 228.0;
    /// Air between the heading's kind label and the target's name.
    pub const TITLE_GAP: f32 = 4.0;
}

/// Clock format defaults. Taste, not mechanism: the live values are
/// `bar.clock.hour-12` and `bar.clock.date-mdy` in `abyss.kdl`, and these are
/// what the bar uses when the keys are unset or the compositor is not there.
pub mod clock {
    /// 12-hour time with a meridiem suffix (`11:15 PM`) rather than 23:15.
    pub const HOUR_12: bool = true;
    /// `m/d/y` (`9/12/26`) rather than ISO.
    pub const DATE_MDY: bool = true;
}

/// UI type. Three weights, because the spec distinguishes 400/500/600 and a
/// variable font handed to iced whole would flatten all three to 400.
pub mod font {
    use super::Font;
    use iced::font::{Family, Weight};

    const fn sans(weight: Weight) -> Font {
        Font {
            family: Family::Name("Instrument Sans"),
            weight,
            ..Font::DEFAULT
        }
    }

    const fn mono(weight: Weight) -> Font {
        Font {
            family: Family::Name("JetBrains Mono"),
            weight,
            ..Font::DEFAULT
        }
    }

    pub const UI: Font = sans(Weight::Normal);
    pub const UI_MEDIUM: Font = sans(Weight::Medium);
    pub const UI_SEMIBOLD: Font = sans(Weight::Semibold);
    /// All data: units, paths, timestamps, device identifiers.
    pub const DATA: Font = mono(Weight::Normal);
    pub const DATA_MEDIUM: Font = mono(Weight::Medium);

    /// The bytes, for `iced::Settings::fonts`. Loading is the application's
    /// job — a library that registered fonts as a side effect would do it
    /// once per process and be impossible to opt out of.
    pub const BYTES: &[&[u8]] = &[
        include_bytes!("../../../assets/fonts/InstrumentSans-Regular.ttf"),
        include_bytes!("../../../assets/fonts/InstrumentSans-Medium.ttf"),
        include_bytes!("../../../assets/fonts/InstrumentSans-SemiBold.ttf"),
        include_bytes!("../../../assets/fonts/JetBrainsMono-Regular.ttf"),
        include_bytes!("../../../assets/fonts/JetBrainsMono-Medium.ttf"),
    ];
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_vendored_face_is_present_and_is_a_truetype_file() {
        assert_eq!(font::BYTES.len(), 5);
        for face in font::BYTES {
            // Either a bare TrueType outline file or an OpenType wrapper; a
            // missing `include_bytes!` target would not have compiled, but a
            // wrong file very much would have.
            assert!(face.starts_with(&[0x00, 0x01, 0x00, 0x00]) || face.starts_with(b"OTTO"));
        }
    }

    #[test]
    fn the_accent_is_the_one_from_the_style_spec() {
        let c = color::ACCENT;
        let byte = |f: f32| (f * 255.0).round() as u8;
        assert_eq!((byte(c.r), byte(c.g), byte(c.b)), (0xf2, 0xc3, 0x3c));
    }
}

/// Where a popup the bar owns comes from.
///
/// One named choice for every popup the bar has — the context menu and the
/// tray drawers alike — rather than one decision per surface, so that a
/// settings path can flip all of them together and so that the two behaviours
/// cannot drift apart. It is a taste value and lives here for the same reason
/// every other taste value does.
pub mod popup {
    /// The point a popup grows from.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub enum Anchor {
        /// Below the cell that was clicked, aligned to its edge. The popup
        /// stays put while the pointer moves inside the cell, which is what
        /// makes it read as belonging to the chip rather than to the click.
        Cell,
        /// At the pointer, wherever in the cell it happened to be.
        Pointer,
    }

    /// The default: below the cell. The live value is `bar.popup-anchor`
    /// (`"cell"` / `"pointer"`); this is what applies when it is unset or the
    /// compositor is not there.
    pub const ANCHOR: Anchor = Anchor::Cell;
}
