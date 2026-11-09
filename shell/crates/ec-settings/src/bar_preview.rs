// SPDX-License-Identifier: AGPL-3.0-only
//! The bar, solved: where every cell of a preview strip goes, and how wide it
//! is, for a given width, window count and widget order (ADR 0065).
//!
//! Pure, so the compression order is a tested fact rather than a picture. It
//! is the taskbar's own order (`hyperion/src/layout.rs`):
//!
//! 1. widgets that are not important compress to their grip, the last in
//!    order first — each opens only while every chip can stay whole;
//! 2. important widgets hold their core, and draw no grip at all;
//! 3. only then do window chips walk down the ladder — full, name, icon,
//!    bare — and past bare the strip ends in a `+N` cell.
//!
//! A revealed section shows on the bar only after someone pulls it out by
//! its grip, and folds before any pinned core does. That pull is runtime
//! state, not config, so the preview never has one to fold.
//!
//! The widths are estimates from the bar's own tokens, not the bar's layout:
//! the taskbar crate is not a dependency of settings, and a preview that
//! pulled it in would drag its services along.

use ec_ui::tokens::{bar, canvas, space};
use ec_ui::widget::{ShellFrame, ShellSpan, TRANSPORT_W, VOLUME_SLIDER_W};

/// Every built-in widget id, in the order the add picker lists them.
pub const BUILTINS: [&str; 8] = [
    "now-playing",
    "system-usage",
    "volume",
    "network",
    "bluetooth",
    "battery",
    "tray",
    "clock",
];

/// The prefix a user-defined widget's id carries in `bar.widgets.order`.
pub const CUSTOM: &str = "custom:";

/// The windows the preview's chips stand for. Cycled when there are more.
pub const WINDOWS: [(&str, &str); 8] = [
    ("foot", "~/syncedprojects/EclipseOS"),
    ("Firefox", "Abyss compositor - docs"),
    ("Files", "Downloads"),
    ("micro", "view.rs"),
    ("Signal", "Signal"),
    ("mpv", "Midnight City.flac"),
    ("GIMP", "eclipse-mark.xcf"),
    ("Settings", "Taskbar"),
];

/// The short readings the preview draws in place of an icon, so the strip
/// needs no icon theme. One place, so the span and the drawing agree.
pub const VOLUME_TAG: &str = "VOL";
pub const NETWORK_TAG: &str = "NET";
pub const BLUETOOTH_TAG: &str = "BT";
pub const BATTERY_READING: &str = "84";
/// A custom widget's reading is its name, cut to this many characters.
pub const CUSTOM_CHARS: usize = 10;

/// A widget id as a person reads it.
pub fn title(id: &str) -> String {
    match id {
        "now-playing" => "Now Playing".into(),
        "system-usage" => "System Usage".into(),
        "volume" => "Volume".into(),
        "network" => "Network".into(),
        "bluetooth" => "Bluetooth".into(),
        "battery" => "Battery".into(),
        "tray" => "Tray".into(),
        "clock" => "Clock".into(),
        other => other.strip_prefix(CUSTOM).unwrap_or(other).to_owned(),
    }
}

/// The settings that change a widget's width.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Knobs {
    pub art: bool,
    pub visualizer: bool,
    pub gpu: bool,
    pub disk: bool,
}

impl Default for Knobs {
    fn default() -> Self {
        Knobs {
            art: true,
            visualizer: true,
            gpu: true,
            disk: true,
        }
    }
}

/// How many meters System Usage reveals: memory, then GPU and disk if on.
pub fn usage_meters(k: &Knobs) -> usize {
    1 + usize::from(k.gpu) + usize::from(k.disk)
}

fn chars(n: usize) -> f32 {
    n as f32 * bar::CHAR_W
}

/// The core and revealed widths of widget `id` in the preview.
pub fn span(id: &str, k: &Knobs) -> ShellSpan {
    let (core, revealed) = match id {
        "now-playing" => {
            let mut core = bar::MEDIA_TEXT_W;
            if k.art {
                core += bar::ART + bar::GAP;
            }
            if k.visualizer {
                core += bar::GAP + bar::VIZ_W;
            }
            (core, TRANSPORT_W)
        }
        "system-usage" => {
            let m = usage_meters(k) as f32;
            (bar::METER_W, m * bar::METER_W + (m - 1.0) * bar::METER_GAP)
        }
        // The bar draws these three as their symbolic icon alone.
        "volume" => (bar::MARK, VOLUME_SLIDER_W),
        "network" | "bluetooth" => (bar::MARK, 0.0),
        "battery" => (
            space::MARK_CELL_W + space::MARK_BORDER + bar::GAP + chars(BATTERY_READING.len()),
            0.0,
        ),
        "tray" => (2.0 * bar::TRAY_MARK_W, 0.0),
        "clock" => (bar::CLOCK_W, 0.0),
        other => {
            let name = other.strip_prefix(CUSTOM).unwrap_or(other);
            (
                bar::MARK + bar::GAP + chars(name.chars().count().min(CUSTOM_CHARS)),
                0.0,
            )
        }
    };
    ShellSpan { core, revealed }
}

/// A window chip's rung on the ladder, read off its width.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Rung {
    /// A thin tab: no room for anything but the fact of a window.
    Bare,
    Icon,
    /// Icon and application name.
    Name,
    /// Icon and window title.
    Full,
}

impl Rung {
    pub fn of(width: f32) -> Rung {
        if width >= bar::TASK_FULL {
            Rung::Full
        } else if width >= bar::TASK_NAME {
            Rung::Name
        } else if width >= bar::TASK_MIN {
            Rung::Icon
        } else {
            Rung::Bare
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Rung::Full => "full",
            Rung::Name => "name",
            Rung::Icon => "icon",
            Rung::Bare => "bare",
        }
    }
}

/// One widget in the order, as the solver sees it.
#[derive(Debug, Clone, Copy)]
pub struct Cell {
    pub span: ShellSpan,
    pub important: bool,
}

impl Cell {
    /// The core with its shell padding: an open widget's body.
    pub fn core_run(&self) -> f32 {
        self.span.core + 2.0 * bar::WIDGET_X
    }

    /// A widget that can neither compress nor reveal has nothing to drag, so
    /// it draws no grip: it is a plain cell.
    pub fn grip(&self) -> bool {
        !self.important || self.span.revealed > 0.0
    }

    /// The whole cell at `extent` of body.
    pub fn width(&self, extent: f32) -> f32 {
        let grip = if self.grip() { bar::GRIP_W } else { 0.0 };
        (grip + extent.round()).round()
    }

    /// `extent` as the frame a [`ec_ui::widget::widget_shell`] draws.
    pub fn frame(&self, extent: f32, presence: f32) -> ShellFrame {
        ShellFrame {
            open: (extent / self.core_run()).clamp(0.0, 1.0),
            reveal: 0.0,
            presence,
        }
    }
}

/// Where everything goes. Every x is from the left of the sheet's content
/// box (inside its edge padding).
#[derive(Debug, Clone, PartialEq)]
pub struct Solved {
    /// Per widget, in order: the body's width (grip excluded) and the x of
    /// the cell's left edge.
    pub extents: Vec<f32>,
    pub xs: Vec<f32>,
    /// Every shown chip's width.
    pub chip_w: f32,
    /// Chips drawn; the rest are counted in the `+N` cell.
    pub shown: usize,
    pub overflow: usize,
    pub overflow_x: f32,
    /// Widgets that gave way to the chips.
    pub compressed: usize,
}

/// Where the first chip starts with no pager: after the launcher and the
/// gap that closes its zone.
pub const CHIPS_X: f32 = bar::TASK_MIN + bar::ZONE_GAP;

/// The interaction mode the preview draws (COMP-17, `mode` in `abyss.kdl`).
/// It decides what sits left of the widgets: `wm` has the launcher and the
/// workspace pager and no window chips; `hybrid` adds the chips; `de` turns
/// the pager into the desktop grid and puts the notification history bell
/// before the clock.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Mode {
    Wm,
    #[default]
    Hybrid,
    De,
}

impl Mode {
    /// The config's spelling; anything unknown is the default, as abyss
    /// reads it.
    pub fn parse(s: &str) -> Mode {
        match s {
            "wm" => Mode::Wm,
            "de" => Mode::De,
            _ => Mode::Hybrid,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Mode::Wm => "wm",
            Mode::Hybrid => "hybrid",
            Mode::De => "de",
        }
    }

    /// Whether the bar draws window chips in this mode.
    pub fn shows_chips(self) -> bool {
        self != Mode::Wm
    }

    /// The pager's tile size and how many columns and rows of them.
    pub fn pager(self) -> PagerShape {
        match self {
            Mode::De => {
                let rows = canvas::PREVIEW_GRID_ROWS;
                let gap = bar::PAGER_GRID_GAP;
                let h = ((bar::TASK_H - gap * (rows as f32 - 1.0)) / rows as f32).floor();
                PagerShape {
                    tile: (bar::PAGER_GRID_W, h),
                    gap,
                    columns: canvas::PREVIEW_GRID_COLUMNS,
                    rows,
                }
            }
            Mode::Wm | Mode::Hybrid => PagerShape {
                tile: (bar::PAGER_W, bar::PAGER_H),
                gap: bar::GAP,
                columns: canvas::PREVIEW_WORKSPACES,
                rows: 1,
            },
        }
    }

    /// Where the first window chip starts: launcher, pager, a zone gap
    /// after each.
    pub fn chips_x(self) -> f32 {
        CHIPS_X + self.pager().width() + bar::ZONE_GAP
    }
}

/// The pager as the preview draws it.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PagerShape {
    pub tile: (f32, f32),
    pub gap: f32,
    pub columns: usize,
    pub rows: usize,
}

impl PagerShape {
    pub fn width(&self) -> f32 {
        let c = self.columns as f32;
        c * self.tile.0 + (c - 1.0).max(0.0) * self.gap
    }
}

/// The widget `de` puts before the clock, which is not in the order and
/// cannot be moved: the preview draws it where the bar does.
pub const HISTORY: &str = "history";

/// The x of chip `i` at width `w`, with no pager.
pub fn chip_x(i: usize, w: f32) -> f32 {
    chip_x_from(CHIPS_X, i, w)
}

fn chip_x_from(start: f32, i: usize, w: f32) -> f32 {
    start + i as f32 * (w + bar::GAP)
}

/// What `n` chips need to all be at least `each` wide.
fn need(n: usize, each: f32) -> f32 {
    if n == 0 {
        0.0
    } else {
        n as f32 * each + (n - 1) as f32 * bar::GAP
    }
}

/// Room left for the chip strip.
/// Room left for the chip strip; negative when the widgets already run
/// into the pager, which must fold them even with no chips to make room for.
fn room(start: f32, inner: f32, extents: &[f32], cells: &[Cell]) -> f32 {
    if cells.is_empty() {
        return inner - start;
    }
    let run: f32 = cells.iter().zip(extents).map(|(c, e)| c.width(*e)).sum();
    let gaps = (cells.len() - 1) as f32 * bar::GAP;
    inner - start - run - gaps - bar::ZONE_GAP
}

/// Lay the bar out in `inner` pixels (the sheet less its edge padding), with
/// no pager.
pub fn solve(inner: f32, windows: usize, cells: &[Cell]) -> Solved {
    solve_from(CHIPS_X, inner, windows, cells)
}

/// [`solve`] for the bar `mode` draws: its pager before the chips, and no
/// chips at all under `wm`.
pub fn solve_in(mode: Mode, inner: f32, windows: usize, cells: &[Cell]) -> Solved {
    let windows = if mode.shows_chips() { windows } else { 0 };
    solve_from(mode.chips_x(), inner, windows, cells)
}

/// The narrowest `inner` that draws `cells` under `mode` without a widget
/// running into the pager, or into the overflow chip when `windows` are
/// open: every widget that can fold folded to its grip, the important ones
/// whole. A pane narrower than this scrolls the picture rather than draw the
/// bar overlapping itself.
pub fn min_inner(mode: Mode, windows: usize, cells: &[Cell]) -> f32 {
    let run: f32 = cells
        .iter()
        .map(|c| c.width(if c.important { c.core_run() } else { 0.0 }))
        .sum();
    let gaps = cells.len().saturating_sub(1) as f32 * bar::GAP;
    let chips = if windows > 0 && mode.shows_chips() {
        bar::OVERFLOW_W
    } else {
        0.0
    };
    mode.chips_x() + chips + run + gaps + bar::ZONE_GAP
}

fn solve_from(start: f32, inner: f32, windows: usize, cells: &[Cell]) -> Solved {
    let mut extents: Vec<f32> = cells
        .iter()
        .map(|c| if c.important { c.core_run() } else { 0.0 })
        .collect();
    // Open in order while every chip stays whole; the first that does not
    // fit stops the rest, so a narrower bar never opens a widget a wider one
    // kept shut.
    let full = need(windows, bar::TASK_FULL);
    for i in 0..cells.len() {
        if cells[i].important {
            continue;
        }
        extents[i] = cells[i].core_run();
        if room(start, inner, &extents, cells) < full {
            extents[i] = 0.0;
            break;
        }
    }
    let compressed = cells
        .iter()
        .zip(&extents)
        .filter(|(c, e)| !c.important && **e == 0.0)
        .count();

    let avail = room(start, inner, &extents, cells).max(0.0);
    let (chip_w, shown) = if windows == 0 {
        (bar::TASK_MAX, 0)
    } else {
        let n = windows as f32;
        let each = ((avail - (n - 1.0) * bar::GAP) / n).floor();
        if each >= bar::TASK_BARE {
            (each.min(bar::TASK_MAX), windows)
        } else {
            let fits = ((avail - bar::OVERFLOW_W).max(0.0) / (bar::TASK_BARE + bar::GAP)).floor() as usize;
            (bar::TASK_BARE, fits.min(windows))
        }
    };

    // Widgets right-aligned, the last against the edge.
    let mut xs = vec![0.0; cells.len()];
    let mut end = inner;
    for i in (0..cells.len()).rev() {
        xs[i] = end - cells[i].width(extents[i]);
        end = xs[i] - bar::GAP;
    }

    Solved {
        extents,
        xs,
        chip_w,
        shown,
        overflow: windows - shown,
        overflow_x: chip_x_from(start, shown, chip_w),
        compressed,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const WIDE: f32 = 900.0;

    fn cells(ids: &[(&str, bool)]) -> Vec<Cell> {
        ids.iter()
            .map(|(id, important)| Cell {
                span: span(id, &Knobs::default()),
                important: *important,
            })
            .collect()
    }

    fn default_bar() -> Vec<Cell> {
        cells(&[
            ("now-playing", false),
            ("volume", false),
            ("network", false),
            ("bluetooth", false),
            ("battery", true),
            ("tray", false),
            ("clock", true),
        ])
    }

    #[test]
    fn with_room_every_widget_is_open() {
        let bar = default_bar();
        let s = solve(WIDE * 2.0, 1, &bar);
        assert_eq!(s.compressed, 0);
        for (c, e) in bar.iter().zip(&s.extents) {
            assert_eq!(*e, c.core_run());
        }
        assert_eq!(Rung::of(s.chip_w), Rung::Full);
    }

    #[test]
    fn important_widgets_never_compress_and_draw_no_grip() {
        let bar = default_bar();
        let s = solve(WIDE, 40, &bar);
        for (c, e) in bar.iter().zip(&s.extents) {
            if c.important {
                assert_eq!(*e, c.core_run());
                assert!(!c.grip());
            }
        }
        assert_eq!(s.compressed, bar.iter().filter(|c| !c.important).count());
    }

    #[test]
    fn widgets_compress_last_in_order_first() {
        let bar = default_bar();
        for n in 0..=40 {
            let s = solve(WIDE, n, &bar);
            let open: Vec<bool> = bar
                .iter()
                .zip(&s.extents)
                .filter(|(c, _)| !c.important)
                .map(|(_, e)| *e > 0.0)
                .collect();
            let first_shut = open.iter().position(|o| !o).unwrap_or(open.len());
            assert!(open[first_shut..].iter().all(|o| !o), "{n}: {open:?}");
        }
    }

    #[test]
    fn chips_leave_the_top_rung_only_once_the_widgets_have_given_way() {
        let bar = default_bar();
        for n in 1..=40 {
            let s = solve(WIDE, n, &bar);
            if Rung::of(s.chip_w) < Rung::Full || s.overflow > 0 {
                assert_eq!(s.compressed, 5, "{n} windows");
            }
        }
    }

    #[test]
    fn the_ladder_only_goes_down() {
        let bar = default_bar();
        let mut last = Rung::Full;
        let mut last_overflow = 0;
        for n in 1..=40 {
            let s = solve(WIDE, n, &bar);
            let r = Rung::of(s.chip_w);
            assert!(r <= last, "{n}");
            assert!(s.overflow >= last_overflow, "{n}");
            assert_eq!(s.shown + s.overflow, n);
            last = r;
            last_overflow = s.overflow;
        }
        assert!(last_overflow > 0, "40 windows reach the +N cell");
    }

    #[test]
    fn the_strip_never_runs_into_the_widgets() {
        let bar = default_bar();
        for n in 1..=40 {
            let s = solve(WIDE, n, &bar);
            let tail = if s.overflow > 0 {
                bar::GAP + bar::OVERFLOW_W
            } else {
                0.0
            };
            let end = chip_x(s.shown, s.chip_w) - bar::GAP + tail;
            assert!(end <= s.xs[0] - bar::ZONE_GAP + 0.5, "{n}: {end} vs {}", s.xs[0]);
        }
    }

    #[test]
    fn widgets_pack_against_the_right_edge_in_order() {
        let bar = default_bar();
        let s = solve(WIDE, 3, &bar);
        let last = bar.len() - 1;
        let right = s.xs[last] + bar[last].width(s.extents[last]);
        assert!((right - WIDE).abs() < 0.5);
        assert!(s.xs.windows(2).all(|w| w[0] < w[1]));
    }

    #[test]
    fn switching_parts_off_narrows_the_widget() {
        let all = span("now-playing", &Knobs::default());
        let bare = span(
            "now-playing",
            &Knobs {
                art: false,
                visualizer: false,
                ..Knobs::default()
            },
        );
        assert!(bare.core < all.core);
        let fewer = span(
            "system-usage",
            &Knobs {
                gpu: false,
                ..Knobs::default()
            },
        );
        assert!(fewer.revealed < span("system-usage", &Knobs::default()).revealed);
    }

    #[test]
    fn titles_read_as_words() {
        assert_eq!(title("now-playing"), "Now Playing");
        assert_eq!(title("custom:weather"), "weather");
    }

    #[test]
    fn wm_draws_no_chips_and_every_mode_starts_them_after_its_pager() {
        let bar = default_bar();
        let s = solve_in(Mode::Wm, WIDE, 5, &bar);
        assert_eq!((s.shown, s.overflow), (0, 0));
        for m in [Mode::Wm, Mode::Hybrid, Mode::De] {
            assert!(m.chips_x() > CHIPS_X + m.pager().width());
            assert_eq!(Mode::parse(m.as_str()), m);
        }
        assert!(solve_in(Mode::Hybrid, WIDE, 3, &bar).shown > 0);
    }

    #[test]
    fn the_desktop_grid_fits_in_one_task_chip() {
        let p = Mode::De.pager();
        let h = p.rows as f32 * p.tile.1 + (p.rows as f32 - 1.0) * p.gap;
        assert!(h <= bar::TASK_H);
    }

    #[test]
    fn with_no_windows_the_widgets_still_fold_off_the_pager() {
        let mut bar = default_bar();
        bar.extend(cells(&[
            ("system-usage", false),
            ("custom:weather", false),
            ("custom:load", false),
        ]));
        for m in [Mode::Wm, Mode::Hybrid, Mode::De] {
            let s = solve_in(m, 640.0, 0, &bar);
            assert!(s.xs[0] >= m.chips_x() - bar::ZONE_GAP, "{m:?}: {}", s.xs[0]);
            assert!(s.compressed > 0);
        }
    }

    #[test]
    fn at_its_narrowest_the_bar_still_clears_the_pager() {
        let bar = default_bar();
        for m in [Mode::Wm, Mode::Hybrid, Mode::De] {
            let inner = min_inner(m, 0, &bar);
            let s = solve_in(m, inner, 0, &bar);
            assert!(s.xs[0] + 0.5 >= m.chips_x(), "{m:?}");
            // With windows open the overflow chip keeps its room too.
            let inner = min_inner(m, 2, &bar);
            let s = solve_in(m, inner, 2, &bar);
            let chips_end = if m.shows_chips() {
                m.chips_x() + bar::OVERFLOW_W
            } else {
                m.chips_x()
            };
            assert!(s.xs[0] + 0.5 >= chips_end, "{m:?}");
        }
    }
}
