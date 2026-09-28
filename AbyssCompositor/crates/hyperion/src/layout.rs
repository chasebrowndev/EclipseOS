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
//! 1. important widgets never give way: they keep their core, always;
//! 2. a widget the human opened (pinned Open or Revealed) keeps its pinned
//!    width for as long as the chips can stay at least bare — a pin is the
//!    human saying this widget matters more than the chips' labels;
//! 3. everything else — every unpinned widget *and* every window chip —
//!    shrinks together under one shared squeeze `k` in `0..=1`: a widget's
//!    body is `k` of its open body, a chip's share is `k` of the way from
//!    bare to full. `k` is the largest that fits, so nothing on the bar is
//!    squeezed harder than its neighbours, and nothing gives way alone;
//! 4. a widget whose share no longer holds its lead content (its icon, its
//!    art) snaps to its grip, and the room it frees goes back into the same
//!    `k` — capped at the snap point, so a snap never makes anything else
//!    grow when a window is added;
//! 5. only past unpinned widgets at their grips and chips at bare do pinned
//!    widgets fold, revealed sections first, then cores;
//! 6. the chips that still do not fit become the `+N` cell.
//!
//! A widget that is not present (Now Playing with no player) takes nothing,
//! not even a gap.
//!
//! Pins are granted as a *prefix* in `order` — the first that does not fit
//! stops the tier — and the squeeze is one number for everything, so a
//! narrower bar or one more window never opens a widget or widens a chip.
//! That is the monotonicity the tests pin down.

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
    /// The width that shows its process name whole ([`whole_width`] of the
    /// name): the cheapest label a chip can wear.
    pub name: f32,
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
    /// The leading run of the core that has to stay whole for a squeezed
    /// widget to still say what it is — Now Playing's art. `0.0` is the
    /// whole core: a lone mark cannot lose any of itself.
    pub lead: f32,
    /// `bar.widgets.important`: never compressed.
    pub important: bool,
    /// Something to show. Absent widgets take no room at all.
    pub present: bool,
    pub pin: Option<Pin>,
    /// The body width under a live drag, taken exactly: the finger decides.
    pub live: Option<f32>,
}

impl WidgetIn {
    /// Whether this widget keeps a grip while it is open: only a revealed
    /// section gives a grip something to pull out. Network, bluetooth, or
    /// volume with no sink are plain glass cells until they are squeezed.
    pub fn reveals(&self) -> bool {
        self.revealed > 0.0
    }

    /// Whether a grip can ever show: it reveals, or it can be squeezed to
    /// one (ADR 0065: "one without a drag bar gets one when compressed").
    /// An important widget with nothing to reveal never has one.
    pub fn grippable(&self) -> bool {
        self.reveals() || !self.important
    }

    /// The narrowest a squeezed body may be before it snaps to its grip:
    /// its lead content with [`bar::SQUEEZE_AIR`] either side. The shell
    /// keeps the lead in view down to exactly this width.
    pub fn floor(&self) -> f32 {
        let lead = if self.lead > 0.0 {
            self.lead.min(self.core)
        } else {
            self.core
        };
        (lead + 2.0 * bar::SQUEEZE_AIR).min(self.core_run())
    }

    /// The body at squeeze `k`: `k` of its open body, or nothing — the grip
    /// alone — once that no longer holds its lead.
    fn squeezed(&self, k: f32) -> f32 {
        let body = k * self.core_run();
        if body >= self.floor() {
            body
        } else {
            0.0
        }
    }

    /// The grip's column at `extent`. A widget that reveals always has
    /// one; one that does not grows it only as its body closes below its
    /// floor, so an open or squeezed network is a plain cell and one
    /// squeezed shut is a grip — continuously, with no step in between.
    pub fn grip_w(&self, extent: f32) -> f32 {
        if self.reveals() {
            bar::GRIP_W
        } else if self.important {
            0.0
        } else {
            bar::GRIP_W * self.closed(extent)
        }
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

    /// How far it has closed onto its grip at `extent`: `1.0` is the grip
    /// alone, `0.0` any width at or past its [`floor`](Self::floor). A
    /// squeezed widget that still shows its lead is open, as far as its
    /// ground and its neighbours' gaps are concerned.
    pub fn closed(&self, extent: f32) -> f32 {
        if !self.grippable() {
            return 0.0;
        }
        1.0 - (extent / self.floor()).clamp(0.0, 1.0)
    }

    /// The whole cell's width with `extent` of body showing.
    pub fn width(&self, extent: f32) -> f32 {
        if !self.present {
            return 0.0;
        }
        (self.grip_w(extent) + extent.round()).round()
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
    /// Sharing the squeeze with the chips: narrower than its core, still
    /// showing its lead.
    Squeezed,
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
    /// The shared squeeze the unpinned widgets and the chips are at: `1.0`
    /// is nothing squeezed.
    pub squeeze: f32,
}

impl Default for Layout {
    fn default() -> Self {
        Layout {
            chips: Vec::new(),
            detail: Detail::Full,
            hidden: 0,
            widgets: Vec::new(),
            squeeze: 1.0,
        }
    }
}

/// The air before a widget, given how compressed it and the widget before
/// it are (`1.0` is the grip alone). Two grips side by side close up to
/// [`bar::GRIP_RUN_GAP`] — a run of compressed widgets is one rack of
/// handles, not a row of empty chips — and the gap follows the
/// less-compressed of the pair, so it opens continuously as either does.
/// The solver and the row both use this, so an anchor never drifts from
/// the glass it hangs from.
pub fn widget_gap(before: f32, closed: f32) -> f32 {
    let racked = before.min(closed).clamp(0.0, 1.0);
    bar::GAP - racked * (bar::GAP - bar::GRIP_RUN_GAP)
}

/// The room `n` chips need to all be at least `each` wide.
fn chips_need(n: usize, each: f32) -> f32 {
    if n == 0 {
        0.0
    } else {
        n as f32 * each + (n - 1) as f32 * bar::GAP
    }
}

/// One chip's share of the strip at squeeze `k`: bare at 0, full at 1.
/// Full and not [`bar::TASK_MAX`]: past Full a chip gains only air, so the
/// chips spend that air before any widget loses a pixel of content.
fn chip_share(k: f32) -> f32 {
    bar::TASK_BARE + k * (bar::TASK_FULL - bar::TASK_BARE)
}

/// The largest `k` in `lo..=hi` for which `fits` holds, given it holds at
/// `lo`. `fits` must be monotone: true below some point, false above.
fn largest(lo: f32, hi: f32, fits: impl Fn(f32) -> bool) -> f32 {
    if fits(hi) {
        return hi;
    }
    let (mut lo, mut hi) = (lo, hi);
    // Far past a pixel on the widest body the bar can hold.
    for _ in 0..32 {
        let mid = 0.5 * (lo + hi);
        if fits(mid) {
            lo = mid;
        } else {
            hi = mid;
        }
    }
    lo
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
            let gaps: f32 = present
                .windows(2)
                .map(|p| widget_gap(ws[p[0]].closed(extent[p[0]]), ws[p[1]].closed(extent[p[1]])))
                .sum();
            right -= cells + gaps + bar::ZONE_GAP;
        }
        (right - input.lead).max(0.0)
    };
    let n = input.chips.len();
    let bare = chips_need(n, bar::TASK_BARE);

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
    // labels, but not their existence. Every unpinned widget is at its grip
    // while this is decided — it is the squeeze's to give, not the pin's.
    let mut folded = false;
    for (i, w) in ws.iter().enumerate() {
        if settled(w)
            && matches!(w.pin, Some(Pin::Open | Pin::Revealed))
            && !grant(&mut extent, i, w.core_run(), bare)
        {
            folded = true;
            break;
        }
    }
    for (i, w) in ws.iter().enumerate() {
        if settled(w) && w.pin == Some(Pin::Revealed) && !grant(&mut extent, i, w.max_extent(), bare) {
            folded = true;
            break;
        }
    }

    // Everything else shares one squeeze with the chips.
    let squeezable: Vec<bool> = ws
        .iter()
        .map(|w| settled(w) && w.pin.is_none() && !w.important)
        .collect();
    let held = extent.clone();
    let place = |k: f32, snapped: &[bool]| -> Vec<f32> {
        let mut e = held.clone();
        for (i, w) in ws.iter().enumerate() {
            if squeezable[i] {
                e[i] = if snapped[i] { 0.0 } else { w.squeezed(k) };
            }
        }
        e
    };
    let fits = |k: f32, snapped: &[bool]| room(&place(k, snapped)) >= chips_need(n, chip_share(k));
    let none = vec![false; ws.len()];
    let floor_fits = fits(0.0, &none);
    // A pin folded only because everything else was already at its floor:
    // the room it gave up is the chips' `+N` margin, never a squeeze
    // released, or one more window would widen everything the pin let go.
    let first = if floor_fits && !folded {
        largest(0.0, 1.0, |k| fits(k, &none))
    } else {
        0.0
    };
    // Whatever snapped to its grip on the way down stays there, and the
    // room it freed goes back to everyone else — but never past the point
    // it snapped at, or one more window could widen its neighbours.
    let snapped: Vec<bool> = (0..ws.len())
        .map(|i| squeezable[i] && ws[i].squeezed(first) <= 0.0)
        .collect();
    let cap = (0..ws.len())
        .filter(|&i| snapped[i])
        .map(|i| ws[i].floor() / ws[i].core_run())
        .fold(1.0f32, f32::min)
        .max(first);
    let k = if floor_fits && !folded {
        largest(first, cap, |k| fits(k, &snapped))
    } else {
        0.0
    };
    extent = place(k, &snapped);

    let avail = room(&extent);
    let mut layout = Layout {
        widgets: Vec::with_capacity(ws.len()),
        squeeze: k,
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
        let before = (0..i).rev().find(|&j| ws[j].present);
        let width = w.width(extent[i]);
        let state = if w.live.is_some() {
            State::Live
        } else if extent[i] <= 0.0 {
            State::Grip
        } else if extent[i] > w.core_run() {
            State::Revealed
        } else if extent[i] < w.core_run() {
            State::Squeezed
        } else {
            State::Core
        };
        out[i] = WidgetOut {
            x: right - width,
            width,
            extent: extent[i],
            state,
        };
        right -= width + before.map_or(0.0, |j| widget_gap(ws[j].closed(extent[j]), w.closed(extent[i])));
    }
    layout.widgets = out;

    if n == 0 {
        return layout;
    }
    // Squeezed, the chips get their share and no more: the squeeze is one
    // number, and a strip that also took the air a snapped widget left
    // behind would be the one thing on the bar squeezed less than the rest.
    // Unsqueezed, they may take the whole strip, up to the ladder's cap.
    let avail = if floor_fits && k < 1.0 {
        avail.min(chips_need(n, chip_share(k).floor()))
    } else {
        avail
    };
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
/// A chip whose share buys it no words at all — neither its title nor its
/// name fits whole — is an icon, and an icon needs only [`bar::TASK_MIN`]: the
/// rest of its share was air around a glyph, so it goes into the pool too.
/// That is what keeps a dense strip from being a row of wide empty tabs.
///
/// The pool buys whole rungs only, never a partial width that changes
/// nothing on screen: first a name for each chip that is an icon, then a
/// whole title for anyone. Non-minimized windows are offered each first,
/// then left to right, so which chip grows never depends on iteration order
/// or timing.
fn expand(chips: &[ChipIn], base_width: f32, avail: f32) -> Vec<f32> {
    let shown = chips.len();
    if shown == 0 {
        return Vec::new();
    }
    let gaps = (shown - 1) as f32 * bar::GAP;
    let mut remaining = (avail - shown as f32 * base_width - gaps).max(0.0);

    let mut order: Vec<usize> = (0..shown).collect();
    order.sort_by_key(|&i| (chips[i].minimized, i));

    // The cheapest words a chip can wear: its title whole, or its name —
    // which shows only on a chip wide enough for the Name rung.
    let cheapest = |c: &ChipIn| c.whole.min(c.name.max(bar::TASK_NAME));
    let mut widths = vec![base_width; shown];
    if base_width > bar::TASK_MIN {
        for (w, c) in widths.iter_mut().zip(chips) {
            if cheapest(c) > base_width {
                remaining += base_width - bar::TASK_MIN;
                *w = bar::TASK_MIN;
            }
        }
    }
    let mut buy = |widths: &mut Vec<f32>, i: usize, to: f32| {
        let take = (to - widths[i]).floor();
        if take > 0.0 && take <= remaining {
            widths[i] += take;
            remaining -= take;
        }
    };
    for &i in &order {
        if widths[i] < cheapest(&chips[i]) {
            buy(&mut widths, i, cheapest(&chips[i]));
        }
    }
    for &i in &order {
        buy(&mut widths, i, chips[i].whole);
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

/// The rung one window's chip draws at `width`: its title whenever that fits
/// whole, else [`rung`] of what the width buys. The view and the motion both
/// ask this, so a cross-fade starts exactly when the face changes.
pub fn chip_detail(label: &str, name: &str, width: f32) -> Detail {
    if width >= whole_width(label) {
        Detail::Full
    } else {
        rung(label, name, width, detail_of(width))
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
                name: whole_width("app"),
                minimized: i % 3 == 0,
            })
            .collect()
    }

    fn w(core: f32, revealed: f32, important: bool) -> WidgetIn {
        WidgetIn {
            core,
            revealed,
            lead: 0.0,
            important,
            present: true,
            pin: None,
            live: None,
        }
    }

    /// The default order's shape: now-playing (its art the lead), volume,
    /// network, bluetooth, battery (important), tray, clock (important).
    fn defaults() -> Vec<WidgetIn> {
        vec![
            WidgetIn {
                lead: bar::ART,
                ..w(180.0, 76.0, false)
            },
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

    /// At a dense strip's share, a chip that can say nothing is an icon at
    /// [`bar::TASK_MIN`]; the air it gives back buys names first, then titles,
    /// and never more room than the strip had.
    #[test]
    fn an_icon_gives_its_air_back_and_it_buys_names() {
        let base = 72.0;
        let mute = ChipIn {
            whole: whole_width("a long document title"),
            name: whole_width("a-very-long-application-name"),
            minimized: false,
        };
        let short = ChipIn {
            whole: whole_width("a long document title"),
            name: whole_width("discord"),
            minimized: false,
        };
        let chips = [mute, mute, short, mute];
        let avail = 4.0 * base + 3.0 * bar::GAP;
        let w = expand(&chips, base, avail);
        let used: f32 = w.iter().sum::<f32>() + 3.0 * bar::GAP;
        assert!(used <= avail, "{w:?}");
        assert_eq!(w[2], short.name, "the pool bought discord its name: {w:?}");
        assert_eq!(
            chip_detail("a long document title", "discord", w[2]),
            Detail::Name
        );
        for i in [0, 1, 3] {
            assert!(w[i] < base, "a mute chip gave its air back: {w:?}");
            assert_eq!(
                chip_detail("a long document title", "a-very-long-application-name", w[i]),
                Detail::Icon
            );
        }
        // A short title is cheaper than a long app id: it is what is bought.
        let titled = ChipIn {
            whole: whole_width("Downloads"),
            name: whole_width("org.gnome.Nautilus"),
            minimized: false,
        };
        let w = expand(&[titled, mute, mute], base, 3.0 * base + 2.0 * bar::GAP);
        assert_eq!(w[0], titled.whole, "{w:?}");
        // At the Bare rung there is no air to give.
        assert_eq!(
            expand(&chips, bar::TASK_BARE, 4.0 * bar::TASK_BARE + 3.0 * bar::GAP),
            vec![bar::TASK_BARE; 4]
        );
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
                name: whole_width("app"),
                minimized: true,
            },
            ChipIn {
                whole: long,
                name: whole_width("app"),
                minimized: false,
            },
        ];
        assert_eq!(expand(&away_up, base, avail), vec![base, base + want]);
        let equals = [
            ChipIn {
                whole: long,
                name: whole_width("app"),
                minimized: false,
            },
            ChipIn {
                whole: long,
                name: whole_width("app"),
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

    /// The owner's order, swept across every width: while anything is
    /// squeezed, every unpinned widget that is not at its grip is at exactly
    /// the shared `k` of its open body, and the chips at `k` of their way
    /// from bare to full — nothing squeezed alone; a pinned widget folds only
    /// once unpinned widgets are grips and chips bare (or overflowing); every
    /// pinned core is kept before any pinned reveal. Important widgets hold.
    #[test]
    fn the_order_is_squeeze_together_then_fold() {
        let mut widgets = defaults();
        widgets[1].pin = Some(Pin::Revealed);
        widgets[5].pin = Some(Pin::Open);
        let mut saw = [false; 3];
        for n in [3usize, 8, 20, 60] {
            let cs = chips(n);
            for width in (300..=3400).step_by(7) {
                let width = width as f32;
                let l = solve_at(width, &cs, &widgets);
                if l.squeeze < 1.0 && l.hidden == 0 {
                    saw[0] = true;
                    let k = l.squeeze;
                    for (w, o) in widgets.iter().zip(&l.widgets) {
                        if w.pin.is_none() && !w.important && o.state != State::Grip {
                            assert!(
                                (o.extent - k * w.core_run()).abs() < 0.01,
                                "{n} chips at {width}: {o:?} is not at k={k}"
                            );
                        }
                    }
                    let strip: f32 = l.chips.iter().map(|c| c.width).sum::<f32>() + (n - 1) as f32 * bar::GAP;
                    assert!(
                        strip <= chips_need(n, chip_share(k).floor()) + 0.01,
                        "{n} chips at {width}: the strip took more than its share of k={k}"
                    );
                }
                // A pin folds only when its next step (the core, then the
                // reveal) would overflow even bare chips: forcing that step
                // with a live width must push chips into the `+N` cell.
                let folded = widgets
                    .iter()
                    .zip(&l.widgets)
                    .position(|(w, o)| w.pin.is_some() && o.extent < w.max_extent());
                if let Some(i) = folded {
                    saw[1] = true;
                    let w = &widgets[i];
                    let next = if l.widgets[i].extent < w.core_run() {
                        w.core_run()
                    } else {
                        w.max_extent()
                    };
                    // Everything else held where it landed.
                    let mut forced = widgets.clone();
                    for (f, o) in forced.iter_mut().zip(&l.widgets) {
                        f.live = Some(o.extent);
                    }
                    forced[i].live = Some(next);
                    let f = solve_at(width, &cs, &forced);
                    assert!(
                        f.hidden > 0 || f.chips.is_empty(),
                        "{n} chips at {width}: a pin folded that bare chips had room for"
                    );
                }
                let core_short = widgets
                    .iter()
                    .zip(&l.widgets)
                    .any(|(w, o)| w.pin.is_some() && o.extent < w.core_run());
                if core_short {
                    saw[2] = true;
                    assert!(
                        l.widgets.iter().all(|o| o.state != State::Revealed),
                        "{n} chips at {width}: a reveal outlived a pinned core"
                    );
                }
                for (w, o) in widgets.iter().zip(&l.widgets) {
                    if w.important {
                        assert_eq!(o.extent, w.core_run(), "{n} chips at {width}");
                    }
                }
            }
        }
        assert!(saw.iter().all(|s| *s), "the sweep reached every tier: {saw:?}");
    }

    /// Two grips side by side close up to a rack; a grip beside an open
    /// widget keeps the full gap. The solver places by the same rule the
    /// row draws by, so the rack's anchors are exact.
    #[test]
    fn a_run_of_grips_is_racked_and_the_solver_agrees() {
        assert_eq!(widget_gap(1.0, 1.0), bar::GRIP_RUN_GAP);
        assert_eq!(widget_gap(0.0, 1.0), bar::GAP);
        assert_eq!(widget_gap(1.0, 0.0), bar::GAP);
        let mut prev = widget_gap(0.0, 0.0);
        for i in 0..=100 {
            let g = widget_gap(i as f32 / 100.0, 1.0);
            assert!(g <= prev && prev - g < 0.1, "continuous and closing at {i}");
            prev = g;
        }

        let widgets = defaults();
        let l = solve_at(1400.0, &chips(20), &widgets);
        let mut racked = 0;
        for i in 1..widgets.len() {
            let (a, b) = (&l.widgets[i - 1], &l.widgets[i]);
            let air = b.x - (a.x + a.width);
            if a.state == State::Grip && b.state == State::Grip {
                racked += 1;
                assert_eq!(air, bar::GRIP_RUN_GAP, "{i}: {l:?}");
            } else {
                assert_eq!(air, bar::GAP, "{i}: {l:?}");
            }
        }
        assert!(racked > 0, "the 20-chip bar racks its grips: {l:?}");
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

    /// The chips' per-window share of the strip.
    fn share(l: &Layout) -> f32 {
        if l.chips.is_empty() {
            return 0.0;
        }
        let n = l.chips.len() as f32;
        l.chips.iter().map(|c| c.width).sum::<f32>() / n
    }

    /// One more window squeezes the unpinned widgets and the chips together
    /// under one `k`: no widget and no chip share ever grows as windows are
    /// added, and once the squeeze starts, widgets and chips both give.
    #[test]
    fn adding_chips_squeezes_widgets_and_chips_together() {
        let widgets = defaults();
        for width in [1280.0f32, 1920.0, 2560.0] {
            let mut prev: Option<Layout> = None;
            let mut both = false;
            for n in 1..=40usize {
                let l = solve_at(width, &chips(n), &widgets);
                if let Some(p) = &prev {
                    assert!(l.squeeze <= p.squeeze, "{n} at {width}: k grew");
                    for (i, (a, b)) in p.widgets.iter().zip(&l.widgets).enumerate() {
                        assert!(b.width <= a.width, "{n} at {width}: widget {i} widened");
                    }
                    if l.hidden == 0 && p.hidden == 0 {
                        assert!(share(&l) <= share(p) + 0.01, "{n} at {width}: chips widened");
                    }
                    if l.squeeze < p.squeeze && p.squeeze < 1.0 && l.hidden == 0 {
                        let squeezed = |o: &WidgetOut| o.state == State::Squeezed;
                        let shrank = p
                            .widgets
                            .iter()
                            .zip(&l.widgets)
                            .any(|(a, b)| squeezed(a) && b.width < a.width);
                        if shrank && share(&l) < share(p) {
                            both = true;
                        }
                    }
                }
                prev = Some(l);
            }
            assert!(both, "at {width} a window shrank a widget and the chips at once");
        }
    }

    /// A widget the human opened keeps its width, window after window, until
    /// the chips are bare; only then does it fold.
    #[test]
    fn a_pinned_widget_holds_until_chips_are_bare() {
        let mut widgets = defaults();
        widgets[0].pin = Some(Pin::Open);
        let open = widgets[0].width(widgets[0].core_run());
        let mut folded = false;
        for n in 1..=80usize {
            let l = solve_at(1920.0, &chips(n), &widgets);
            if l.widgets[0].width < open {
                folded = true;
                assert_eq!(l.squeeze, 0.0, "{n}: folded while anything could squeeze");
                assert!(
                    l.chips.iter().all(|c| c.width <= bar::TASK_BARE) || l.hidden > 0,
                    "{n}: folded before the chips were bare: {l:?}"
                );
            } else {
                assert!(!folded, "{n}: a fold never reopens as windows are added");
                assert_eq!(l.widgets[0].width, open, "{n}");
            }
        }
        assert!(folded, "80 windows fold even a pin");
    }

    /// Network has no grip on a roomy bar, gains one once squeezed shut, and
    /// is back to a plain cell at its core once pinned open.
    #[test]
    fn a_plain_widget_grows_a_grip_only_when_squeezed_shut() {
        let mut widgets = defaults();
        let net = &widgets[2];
        assert!(!net.reveals() && net.grippable());
        let roomy = solve_at(2560.0, &chips(1), &widgets);
        assert_eq!(roomy.widgets[2].state, State::Core);
        assert_eq!(net.grip_w(roomy.widgets[2].extent), 0.0, "open: no drag bar");
        assert_eq!(roomy.widgets[2].width, net.core_run());
        let crowded = solve_at(1280.0, &chips(20), &widgets);
        assert_eq!(crowded.widgets[2].state, State::Grip);
        assert_eq!(
            crowded.widgets[2].width,
            bar::GRIP_W,
            "squeezed shut: the drag bar alone"
        );
        widgets[2].pin = Some(Pin::Open);
        let reopened = solve_at(1280.0, &chips(20), &widgets);
        assert_eq!(reopened.widgets[2].state, State::Core);
        assert_eq!(widgets[2].grip_w(reopened.widgets[2].extent), 0.0);
        // Important widgets with nothing to reveal never grow one at all.
        assert!(!widgets[6].grippable());
    }

    /// A pin takes its room from the chips, which re-ladder.
    #[test]
    fn a_pinned_widget_takes_room_from_the_chips() {
        let mut widgets = defaults();
        let cs = chips(20);
        let before = solve_at(1600.0, &cs, &widgets);
        assert_ne!(before.widgets[0].state, State::Revealed);
        widgets[0].pin = Some(Pin::Revealed);
        let after = solve_at(1600.0, &cs, &widgets);
        assert_eq!(after.widgets[0].state, State::Revealed);
        assert!(share(&after) < share(&before));
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
