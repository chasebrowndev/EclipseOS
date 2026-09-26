// SPDX-License-Identifier: AGPL-3.0-only
//! The bar's one layout solver (ADR 0065).
//!
//! Every chip and every widget right of the task strip gets its target place
//! and width here, from one pure function of the room and what wants it. The
//! view draws what motion makes of these targets, and every popup anchors
//! from them, so the drawing and the anchors can no longer drift apart the
//! way `chip_span` once did.
//!
//! ## Who gives way, in order
//!
//! As the bar runs out of room:
//!
//! 1. unpinned, non-important widgets compress to their grip, the last in
//!    `bar.widgets.order` first — a widget is a convenience, a window is the
//!    thing you are working in;
//! 2. the chips step down the ladder, Full → Name → Icon → Bare;
//! 3. a widget the human pinned open (or revealed) folds: its revealed
//!    section first, then its core;
//! 4. the chips that still do not fit become the `+N` cell.
//!
//! Revealed sections are only ever shown because someone pulled them out, so
//! "revealed folds first" is step 3's first half: a pin is the human saying
//! this widget matters more than the chips' labels, and it keeps that promise
//! until even bare chips would overflow. Important widgets never compress at
//! all, and a widget that is not present (Now Playing with no player) takes
//! nothing, not even a gap.
//!
//! Each tier is granted as a *prefix* in `order` — the first widget that does
//! not fit stops the tier — so that a narrower bar never opens a widget a
//! wider one kept shut. That is the monotonicity the tests pin down.

use eclipse_ui::tokens::{bar, size};

/// How many characters of a title survive before the ellipsis. The clamp is
/// in characters and not pixels because iced has no eliding text and the row
/// must stay a pure function of the snapshot — a measured elide would depend
/// on the layout pass that has not run yet.
pub const TITLE_CHARS: usize = 18;

/// How much detail one task chip is showing.
///
/// The ladder the bar walks down as windows multiply. It is chosen from the
/// *width a chip actually got*, never from a window count: "what fits" is the
/// question, and the rung is the answer to it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Detail {
    /// Icon and title — the document.
    Full,
    /// Icon and application name — `kitty`, not the directory it is in.
    Name,
    /// Icon alone.
    Icon,
    /// Not even the icon: a bare tab of accent or grey. The last resort,
    /// before which everything else has already been spent.
    Bare,
}

/// The rung a width buys on its own.
pub fn detail_of(width: f32) -> Detail {
    if width >= bar::TASK_FULL {
        Detail::Full
    } else if width >= bar::TASK_NAME {
        Detail::Name
    } else if width >= bar::TASK_MIN {
        Detail::Icon
    } else {
        Detail::Bare
    }
}

/// One window chip, as far as the solver cares.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ChipIn {
    /// The width that shows its label whole ([`whole_width`]).
    pub whole: f32,
    /// A put-away window yields spare room to one that is up.
    pub minimized: bool,
}

/// What the human last did to a widget with its grip. Lasts until the
/// widget's content changes category (ADR 0065).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Pin {
    Collapsed,
    Open,
    Revealed,
}

/// One widget, as far as the solver cares.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct WidgetIn {
    /// The core's own width, shell padding excluded.
    pub core: f32,
    /// The revealed section's own width; 0 for none.
    pub revealed: f32,
    /// `bar.widgets.important`: never compressed.
    pub important: bool,
    /// Something to show. Absent widgets take no room at all.
    pub present: bool,
    pub pin: Option<Pin>,
    /// The body width under a live drag, taken exactly: the finger decides.
    pub live: Option<f32>,
}

impl WidgetIn {
    /// Whether this widget draws a grip. A widget that can neither compress
    /// nor reveal has nothing to drag, so it is a plain glass cell; anything
    /// else gets one (ADR 0065: "one without a drag bar gets one when
    /// compressed").
    pub fn grip(&self) -> bool {
        !self.important || self.revealed > 0.0
    }

    /// The core with its shell padding: the body width of an open widget.
    pub fn core_run(&self) -> f32 {
        self.core + 2.0 * bar::WIDGET_X
    }

    /// The revealed section with the gap that joins it to the core.
    pub fn reveal_run(&self) -> f32 {
        if self.revealed > 0.0 {
            self.revealed + bar::WIDGET_GAP
        } else {
            0.0
        }
    }

    /// The widest the body gets.
    pub fn max_extent(&self) -> f32 {
        self.core_run() + self.reveal_run()
    }

    /// The narrowest the body may get: an important widget keeps its core.
    pub fn min_extent(&self) -> f32 {
        if self.important {
            self.core_run()
        } else {
            0.0
        }
    }

    /// The whole cell's width with `extent` of body showing.
    pub fn width(&self, extent: f32) -> f32 {
        if !self.present {
            return 0.0;
        }
        let grip = if self.grip() { bar::GRIP_W } else { 0.0 };
        (grip + extent.round()).round()
    }
}

/// Everything the solver reads.
#[derive(Debug, Clone, Copy)]
pub struct Input<'a> {
    /// The bar's width in logical pixels.
    pub width: f32,
    /// Where the task strip begins: the launcher, the pager and their air.
    pub lead: f32,
    pub chips: &'a [ChipIn],
    /// In `bar.widgets.order`, left to right.
    pub widgets: &'a [WidgetIn],
}

/// Where a widget landed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum State {
    /// Not present: zero width.
    Gone,
    /// Compressed to its grip.
    Grip,
    /// Its core, and no more.
    Core,
    /// Core and revealed section.
    Revealed,
    /// Under a live drag, somewhere in between.
    Live,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ChipOut {
    pub x: f32,
    pub width: f32,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct WidgetOut {
    pub x: f32,
    pub width: f32,
    /// The body's target width, grip excluded: what motion animates.
    pub extent: f32,
    pub state: State,
}

/// The solver's answer.
#[derive(Debug, Clone, PartialEq)]
pub struct Layout {
    /// One per *shown* chip, in strip order; chips past `chips.len()` are in
    /// the `+N` cell.
    pub chips: Vec<ChipOut>,
    /// The rung the strip's shared floor bought; each chip may still step
    /// down per [`rung`].
    pub detail: Detail,
    /// How many windows the `+N` cell stands for.
    pub hidden: usize,
    /// One per input widget, in the same order.
    pub widgets: Vec<WidgetOut>,
}

impl Default for Layout {
    fn default() -> Self {
        Layout {
            chips: Vec::new(),
            detail: Detail::Full,
            hidden: 0,
            widgets: Vec::new(),
        }
    }
}

/// The room `n` chips need to all be at least `each` wide.
fn chips_need(n: usize, each: f32) -> f32 {
    if n == 0 {
        0.0
    } else {
        n as f32 * each + (n - 1) as f32 * bar::GAP
    }
}

/// Solve the bar. Pure: same input, same answer, no clock, no I/O.
pub fn solve(input: Input<'_>) -> Layout {
    let ws = input.widgets;
    let mut extent: Vec<f32> = ws
        .iter()
        .map(|w| match (w.present, w.live) {
            (false, _) => 0.0,
            (true, Some(live)) => live.clamp(w.min_extent(), w.max_extent()),
            (true, None) => w.min_extent(),
        })
        .collect();

    // The strip's room given the widgets' current extents.
    let room = |extent: &[f32]| -> f32 {
        let present: Vec<usize> = (0..ws.len()).filter(|&i| ws[i].present).collect();
        let mut right = input.width - bar::EDGE;
        if !present.is_empty() {
            let cells: f32 = present.iter().map(|&i| ws[i].width(extent[i])).sum();
            right -= cells + (present.len() - 1) as f32 * bar::GAP + bar::ZONE_GAP;
        }
        (right - input.lead).max(0.0)
    };
    let n = input.chips.len();
    let bare = chips_need(n, bar::TASK_BARE);
    let full = chips_need(n, bar::TASK_FULL);

    // Raise `i` to `to` if the chips keep `floor` afterwards.
    let grant = |extent: &mut Vec<f32>, i: usize, to: f32, floor: f32| -> bool {
        if extent[i] >= to {
            return true;
        }
        let was = extent[i];
        extent[i] = to;
        if room(extent) >= floor {
            true
        } else {
            extent[i] = was;
            false
        }
    };

    let settled = |w: &WidgetIn| w.present && w.live.is_none();
    // Pins, cores first and then reveals: the human's word beats the chips'
    // labels, but not their existence.
    for (i, w) in ws.iter().enumerate() {
        if settled(w)
            && matches!(w.pin, Some(Pin::Open | Pin::Revealed))
            && !grant(&mut extent, i, w.core_run(), bare)
        {
            break;
        }
    }
    for (i, w) in ws.iter().enumerate() {
        if settled(w) && w.pin == Some(Pin::Revealed) && !grant(&mut extent, i, w.max_extent(), bare) {
            break;
        }
    }
    // Everything else opens only while the chips can stay whole.
    for (i, w) in ws.iter().enumerate() {
        if settled(w) && w.pin.is_none() && !w.important && !grant(&mut extent, i, w.core_run(), full) {
            break;
        }
    }

    let avail = room(&extent);
    let mut layout = Layout {
        widgets: Vec::with_capacity(ws.len()),
        ..Layout::default()
    };

    // Widgets, right-aligned from the bar's edge.
    let mut right = input.width - bar::EDGE;
    let mut out = vec![
        WidgetOut {
            x: right,
            width: 0.0,
            extent: 0.0,
            state: State::Gone,
        };
        ws.len()
    ];
    for i in (0..ws.len()).rev() {
        let w = &ws[i];
        if !w.present {
            out[i].x = right;
            continue;
        }
        let width = w.width(extent[i]);
        let state = if w.live.is_some() {
            State::Live
        } else if extent[i] <= 0.0 {
            State::Grip
        } else if extent[i] > w.core_run() {
            State::Revealed
        } else {
            State::Core
        };
        out[i] = WidgetOut {
            x: right - width,
            width,
            extent: extent[i],
            state,
        };
        right -= width + bar::GAP;
    }
    layout.widgets = out;

    if n == 0 {
        return layout;
    }
    let (base, detail, shown) = ladder(avail, n);
    // The `+N` cell's room is not slack: it is spoken for.
    let strip = if shown < n {
        avail - bar::GAP - bar::OVERFLOW_W
    } else {
        avail
    };
    let widths = expand(&input.chips[..shown], base, strip);
    let mut x = input.lead;
    for width in widths {
        layout.chips.push(ChipOut { x, width });
        x += width + bar::GAP;
    }
    layout.detail = detail;
    layout.hidden = n - shown;
    layout
}

/// How the strip will draw `count` windows in `avail` pixels.
///
/// Returns the per-chip width, the rung of the ladder that width buys, and
/// how many chips are drawn — fewer than `count` only when even bare tabs
/// will not fit, in which case the remainder becomes a `+N` cell so that the
/// bar overflows *visibly* instead of running off its own end.
///
/// A fixed width, because iced hands a `Fill` child of a `Row` exact
/// `min == max` limits, so `max_width` on it does nothing at all and the
/// chips once ran four times their cap.
fn ladder(avail: f32, count: usize) -> (f32, Detail, usize) {
    debug_assert!(count > 0);
    let n = count as f32;
    let each = ((avail - (n - 1.0) * bar::GAP) / n).floor();
    if each >= bar::TASK_BARE {
        let width = each.min(bar::TASK_MAX);
        return (width, detail_of(width), count);
    }
    // Past the bottom of the ladder: bare tabs plus a counter for the rest.
    let room = (avail - bar::OVERFLOW_W).max(0.0);
    let shown = (room / (bar::TASK_BARE + bar::GAP)).floor().max(0.0) as usize;
    (bar::TASK_BARE, Detail::Bare, shown.min(count))
}

/// The strip's genuine slack, handed out to the chips that can use it.
///
/// `ladder` gives every shown chip the same floor on purpose — that uniform
/// share is what keeps the strip a grid — but dividing `avail` evenly rarely
/// uses every pixel of it, and a title that would have fit whole in that
/// leftover room still dropped to its process name for no reason but that the
/// room went undrawn. Only the surplus is up for grabs, so the ladder's
/// no-overflow guarantee still holds for the total.
///
/// Non-minimized windows are offered it first, then left to right, so which
/// chip grows never depends on iteration order or timing.
fn expand(chips: &[ChipIn], base_width: f32, avail: f32) -> Vec<f32> {
    let shown = chips.len();
    if shown == 0 {
        return Vec::new();
    }
    let gaps = (shown - 1) as f32 * bar::GAP;
    let mut remaining = (avail - shown as f32 * base_width - gaps).max(0.0);

    let mut order: Vec<usize> = (0..shown).collect();
    order.sort_by_key(|&i| (chips[i].minimized, i));

    let mut widths = vec![base_width; shown];
    for i in order {
        if remaining <= 0.0 {
            break;
        }
        let want = (chips[i].whole - base_width).max(0.0).floor();
        let take = want.min(remaining);
        widths[i] += take;
        remaining -= take;
    }
    widths
}

/// The rung one chip can actually draw at, given the rung the strip's width
/// bought and the words this particular window wants to say.
///
/// An ellipsis is worse than no detail, so detail is drawn only when it fits
/// *whole*: a window whose path is too long drops to its process name while
/// the chip beside it keeps its title. The process name is the floor for
/// text; if even that will not fit whole the chip becomes an icon.
pub fn rung(label: &str, name: &str, width: f32, strip: Detail) -> Detail {
    let fits = |s: &str, d: Detail| s.chars().count() <= budget(width, d);
    match strip {
        Detail::Full if fits(label, Detail::Full) => Detail::Full,
        Detail::Full | Detail::Name if fits(name, Detail::Name) => Detail::Name,
        Detail::Full | Detail::Name => Detail::Icon,
        other => other,
    }
}

/// How many characters of label a chip of `width` pixels can hold once the
/// icon and the padding have taken theirs.
pub fn budget(width: f32, detail: Detail) -> usize {
    let icon = if detail == Detail::Full || detail == Detail::Name {
        size::ICON + bar::GAP + bar::GAP
    } else {
        0.0
    };
    let text = width - icon - 2.0 * bar::CELL_X;
    ((text / bar::CHAR_W).floor().max(0.0) as usize).min(TITLE_CHARS)
}

/// The pixel width a chip needs to show `label` whole — [`budget`] run in
/// reverse, with no [`TITLE_CHARS`] ceiling and no [`bar::TASK_MAX`] cap.
pub fn whole_width(label: &str) -> f32 {
    size::ICON + 2.0 * bar::GAP + 2.0 * bar::CELL_X + label.chars().count() as f32 * bar::CHAR_W
}

#[cfg(test)]
mod tests {
    use super::*;

    const LEAD: f32 = 120.0;

    fn chips(n: usize) -> Vec<ChipIn> {
        (0..n)
            .map(|i| ChipIn {
                whole: whole_width("a window title"),
                minimized: i % 3 == 0,
            })
            .collect()
    }

    fn w(core: f32, revealed: f32, important: bool) -> WidgetIn {
        WidgetIn {
            core,
            revealed,
            important,
            present: true,
            pin: None,
            live: None,
        }
    }

    /// The default order's shape: now-playing, volume, network, bluetooth,
    /// battery (important), tray, clock (important).
    fn defaults() -> Vec<WidgetIn> {
        vec![
            w(180.0, 76.0, false),
            w(14.0, 92.0, false),
            w(14.0, 0.0, false),
            w(14.0, 0.0, false),
            w(38.0, 0.0, true),
            w(40.0, 0.0, false),
            w(58.0, 0.0, true),
        ]
    }

    fn solve_at(width: f32, chips: &[ChipIn], widgets: &[WidgetIn]) -> Layout {
        solve(Input {
            width,
            lead: LEAD,
            chips,
            widgets,
        })
    }

    fn ends(l: &Layout, widgets: &[WidgetIn]) -> Vec<(f32, f32)> {
        let mut v: Vec<(f32, f32)> = l.chips.iter().map(|c| (c.x, c.x + c.width)).collect();
        v.extend(
            l.widgets
                .iter()
                .zip(widgets)
                .filter(|(_, w)| w.present)
                .map(|(o, _)| (o.x, o.x + o.width)),
        );
        v
    }

    // ---- ported ladder tests

    /// Room for two is room for two full chips — and no more than the cap,
    /// or two windows become two half-bar slabs.
    #[test]
    fn a_roomy_strip_shows_full_chips_at_the_cap() {
        let (w, detail, shown) = ladder(4000.0, 2);
        assert_eq!(w, bar::TASK_MAX);
        assert_eq!(detail, Detail::Full);
        assert_eq!(shown, 2);
    }

    /// The owner's rule: an ellipsized detail is worse than no detail. A chip
    /// never renders one, at any width, for any label.
    #[test]
    fn a_chip_never_renders_an_ellipsis() {
        let long = "…/syncedprojects/EclipseOS/AbyssCompositor";
        for width in [
            bar::TASK_BARE,
            bar::TASK_MIN,
            bar::TASK_NAME,
            bar::TASK_FULL,
            bar::TASK_MAX,
        ] {
            for strip in [Detail::Full, Detail::Name, Detail::Icon, Detail::Bare] {
                let r = rung(long, "kitty", width, strip);
                let drawn = match r {
                    Detail::Full => Some(long),
                    Detail::Name => Some("kitty"),
                    Detail::Icon | Detail::Bare => None,
                };
                if let Some(drawn) = drawn {
                    assert!(drawn.chars().count() <= budget(width, r), "{drawn:?} at {width}");
                }
            }
        }
    }

    /// Per chip, not per bar: a short title keeps its detail while the long
    /// one beside it drops to its process name.
    #[test]
    fn one_chips_long_title_does_not_demote_its_neighbour() {
        let w = bar::TASK_MAX;
        assert_eq!(rung("notes.md", "micro", w, Detail::Full), Detail::Full);
        assert_eq!(
            rung("…/syncedprojects/EclipseOS", "kitty", w, Detail::Full),
            Detail::Name
        );
    }

    #[test]
    fn a_name_that_will_not_fit_whole_becomes_an_icon() {
        assert_eq!(
            rung(
                "whatever",
                "a-very-long-application-name",
                bar::TASK_NAME,
                Detail::Full
            ),
            Detail::Icon
        );
    }

    #[test]
    fn the_ladder_is_driven_by_room_and_not_by_count() {
        assert_eq!(ladder(1400.0, 8).1, Detail::Full);
        assert_eq!(ladder(700.0, 8).1, Detail::Name);
        assert_eq!(ladder(300.0, 8).1, Detail::Icon);
        assert_eq!(ladder(160.0, 8).1, Detail::Bare);
    }

    #[test]
    fn the_strip_never_exceeds_its_room() {
        for avail in [90.0f32, 160.0, 300.0, 700.0, 1400.0] {
            for count in 1..40usize {
                let (w, _, shown) = ladder(avail, count);
                let drawn = shown as f32 * w + (shown.max(1) - 1) as f32 * bar::GAP;
                let tail = if shown < count {
                    bar::OVERFLOW_W + bar::GAP
                } else {
                    0.0
                };
                assert!(drawn + tail <= avail + 0.5, "{avail} / {count}");
            }
        }
    }

    #[test]
    fn windows_past_the_end_become_a_counter() {
        let (_, detail, shown) = ladder(120.0, 30);
        assert_eq!(detail, Detail::Bare);
        assert!(shown < 30);
    }

    #[test]
    fn expansion_favours_the_window_that_is_up_then_the_left() {
        let long = whole_width("a title much longer than eighteen characters wide");
        let base = bar::TASK_MAX;
        let want = (long - base).floor();
        let avail = 2.0 * base + bar::GAP + want;
        let away_up = [
            ChipIn {
                whole: long,
                minimized: true,
            },
            ChipIn {
                whole: long,
                minimized: false,
            },
        ];
        assert_eq!(expand(&away_up, base, avail), vec![base, base + want]);
        let equals = [
            ChipIn {
                whole: long,
                minimized: false,
            },
            ChipIn {
                whole: long,
                minimized: false,
            },
        ];
        assert_eq!(expand(&equals, base, avail), vec![base + want, base]);
        // Packed: no slack, every chip at its floor.
        assert_eq!(expand(&equals, base, 2.0 * base + bar::GAP), vec![base, base]);
    }

    // ---- solver invariants

    /// Nothing overlaps, nothing crosses the bar's edge, and the strip keeps
    /// its air from the widgets — at every width, for every chip count.
    #[test]
    fn nothing_overlaps_and_everything_fits() {
        let widgets = defaults();
        for n in [0usize, 1, 3, 8, 20, 60] {
            let cs = chips(n);
            for width in (300..=3400).step_by(37) {
                let width = width as f32;
                let l = solve_at(width, &cs, &widgets);
                let mut spans = ends(&l, &widgets);
                spans.sort_by(|a, b| a.0.total_cmp(&b.0));
                for pair in spans.windows(2) {
                    assert!(pair[0].1 <= pair[1].0 + 0.01, "{n} chips at {width}: {pair:?}");
                }
                let important: f32 = widgets
                    .iter()
                    .filter(|w| w.important)
                    .map(|w| w.width(w.core_run()) + bar::GAP)
                    .sum();
                if width
                    > LEAD
                        + important
                        + widgets.len() as f32 * (bar::GRIP_W + bar::GAP)
                        + bar::OVERFLOW_W
                        + bar::EDGE
                        + bar::ZONE_GAP
                {
                    let last = spans.last().map_or(0.0, |s| s.1);
                    assert!(last <= width - bar::EDGE + 0.01, "{n} chips at {width}");
                    if let (Some(chip), Some(first)) =
                        (l.chips.last(), l.widgets.iter().find(|w| w.width > 0.0))
                    {
                        let tail = if l.hidden > 0 {
                            bar::GAP + bar::OVERFLOW_W
                        } else {
                            0.0
                        };
                        assert!(
                            chip.x + chip.width + tail <= first.x - bar::ZONE_GAP + 0.01,
                            "{n} chips at {width}: {l:?}"
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn important_widgets_never_compress() {
        let widgets = defaults();
        for n in [0usize, 8, 60, 200] {
            for width in [200.0f32, 800.0, 1400.0, 3000.0] {
                let l = solve_at(width, &chips(n), &widgets);
                for (o, w) in l.widgets.iter().zip(&widgets) {
                    if w.important {
                        assert_eq!(o.extent, w.core_run(), "{n} at {width}");
                        assert_ne!(o.state, State::Grip);
                    }
                }
            }
        }
    }

    /// As the bar narrows, no widget ever opens further, and the chips never
    /// climb a rung.
    #[test]
    fn narrowing_never_opens_anything() {
        let mut widgets = defaults();
        widgets[1].pin = Some(Pin::Revealed);
        for n in [0usize, 3, 8, 20] {
            let cs = chips(n);
            let mut prev: Option<Layout> = None;
            for width in (300..=3400).rev().step_by(11) {
                let l = solve_at(width as f32, &cs, &widgets);
                if let Some(p) = &prev {
                    for (a, b) in p.widgets.iter().zip(&l.widgets) {
                        assert!(b.extent <= a.extent, "{n} chips at {width}");
                    }
                }
                prev = Some(l);
            }
        }
    }

    #[test]
    fn the_solver_is_deterministic() {
        let widgets = defaults();
        let cs = chips(13);
        assert_eq!(solve_at(1500.0, &cs, &widgets), solve_at(1500.0, &cs, &widgets));
    }

    /// Widgets compress before chips leave Full, last in order first.
    #[test]
    fn widgets_give_way_before_chip_labels_last_first() {
        let widgets = defaults();
        let cs = chips(8);
        // Wide: everything open, chips full.
        let l = solve_at(3000.0, &cs, &widgets);
        assert!(l.widgets.iter().all(|w| w.state == State::Core));
        assert_eq!(l.detail, Detail::Full);
        // Find a width where one widget has compressed: it must be the last
        // non-important one, and the chips must still be Full.
        let mut seen = false;
        for width in (1000..3000).rev() {
            let l = solve_at(width as f32, &cs, &widgets);
            let compressed: Vec<usize> = (0..widgets.len())
                .filter(|&i| l.widgets[i].state == State::Grip)
                .collect();
            if compressed.len() == 1 {
                assert_eq!(
                    compressed,
                    vec![5],
                    "the tray, last non-important in order, goes first"
                );
                assert_eq!(l.detail, Detail::Full);
                seen = true;
                break;
            }
        }
        assert!(seen);
    }

    /// A pin takes its room from the chips, which re-ladder.
    #[test]
    fn a_pinned_widget_takes_room_from_the_chips() {
        let mut widgets = defaults();
        let cs = chips(20);
        let before = solve_at(1600.0, &cs, &widgets);
        assert_eq!(before.widgets[0].state, State::Grip);
        widgets[0].pin = Some(Pin::Revealed);
        let after = solve_at(1600.0, &cs, &widgets);
        assert_eq!(after.widgets[0].state, State::Revealed);
        assert!(after.chips[0].width < before.chips[0].width);
        widgets[0].pin = Some(Pin::Collapsed);
        let wide = solve_at(3400.0, &chips(0), &widgets);
        assert_eq!(wide.widgets[0].state, State::Grip, "collapsed stays collapsed");
    }

    #[test]
    fn an_absent_widget_takes_nothing() {
        let mut widgets = defaults();
        widgets[0].present = false;
        let l = solve_at(1600.0, &chips(4), &widgets);
        assert_eq!(l.widgets[0].width, 0.0);
        assert_eq!(l.widgets[0].state, State::Gone);
        let all = solve_at(1600.0, &chips(4), &defaults());
        assert!(l.chips[0].width >= all.chips[0].width);
    }

    #[test]
    fn a_live_drag_is_taken_exactly() {
        let mut widgets = defaults();
        widgets[0].live = Some(57.0);
        let l = solve_at(1200.0, &chips(20), &widgets);
        assert_eq!(l.widgets[0].extent, 57.0);
        assert_eq!(l.widgets[0].state, State::Live);
        widgets[0].live = Some(1.0e6);
        let l = solve_at(1200.0, &chips(20), &widgets);
        assert_eq!(l.widgets[0].extent, widgets[0].max_extent());
    }
}
