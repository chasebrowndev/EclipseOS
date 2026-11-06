// SPDX-License-Identifier: AGPL-3.0-only
//! The notification widget (`mode "de"`, COMP-17 §2): the bar's bell and the
//! reveal drawer that rolls out of it.
//!
//! The bell carries the mode's mark (a bell, a moon, a bell struck through)
//! and, when something is unread, the count in the bar's mono: `(3)`. It opens
//! a reveal drawer, the wifi and battery drawers' machinery and vocabulary,
//! with the unread notifications at the top, newest first. Opening marks
//! everything read; the rows stay on screen for as long as the drawer is out,
//! then live on in the history.
//!
//! ## Shape
//!
//! Head with the new count, the mode in one segment block (the battery's
//! pattern), the hero (the newest unread, with its body and its live action
//! pills under it), then one-line rows in the network drawer's `ROW_H` (a
//! click reveals the body, the pointer swaps the reading for a dismiss
//! cross), a fold row for the history, and "Clear all" as the way out. Rows
//! are all one line; the only things that make the sheet taller are a body
//! that is revealed and the history that is unfolded, and it retargets its
//! height in place. The sheet never outgrows the surface's reserve
//! ([`drawer::RESERVE_H`]): past it, everything between the mode switch and
//! the way out scrolls. Changing the mode leaves the drawer open; the rows
//! follow the new mode live. Empty, the list is one `drawer_note`.
//!
//! A critical entry is told by a danger-ink mark, not a rail. Unread rows are
//! in the full ink and read ones one step down; a revealed or keyboard-selected
//! row takes the focused-chip glass.
//!
//! ## The accent ledger
//!
//! **None.** The count is mono tertiary: it says "unread", not "live". The
//! selected mode is the focused-chip glass, a brighter fill and a crisp rim,
//! never the accent. The one yellow on the bar is the focused chip's.
//!
//! Bodies are human content and are never logged.

use iced::widget::{container, text, Row};
use iced::{Alignment, Element, Length, Theme};

use ec_services::notifications::{HistoryEntry, Mode, Urgency};
use ec_ui::tokens::{bar, color, drawer, font, history as tok, menu, size};
use ec_ui::widget::{self as parts, EntryRow, ShellFrame};

use super::{Parts, Spans};
use crate::app::{App, Drawer, Message};
use crate::history::{ago, badge, clamp_body, has_verbs, now, title, Panel, Stop};
use crate::view::{elide, Sheet};

/// The mark for each mode: the freedesktop bell, a moon for Quiet (stored, no
/// popup) and the struck-through bell for Do Not Disturb (ignored).
const fn icon(mode: Mode) -> &'static str {
    match mode {
        Mode::Normal => "preferences-system-notifications",
        Mode::Quiet => "weather-clear-night",
        Mode::Dnd => "notifications-disabled",
    }
}

/// The bell's width: the mark, and the count after it when there is one.
fn core_width(count: u32) -> f32 {
    let chars = badge(count).chars().count();
    if chars == 0 {
        bar::MARK
    } else {
        bar::MARK + bar::GAP + chars as f32 * bar::CHAR_W
    }
}

pub fn spans(app: &App) -> Spans {
    Spans {
        core: core_width(app.history.unread()),
        revealed: 0.0,
        lead: 0.0,
        present: true,
    }
}

pub fn view(app: &App, frame: ShellFrame) -> Parts<'_> {
    let ink = frame.core_alpha();
    let mode = app.history.mode();
    let tint = if mode == Mode::Normal {
        color::text_secondary()
    } else {
        color::text_tertiary()
    };
    let mark = parts::mark(
        crate::icons::symbolic(icon(mode)),
        bar::MARK,
        tint.scale_alpha(ink),
    );
    let count = app.history.unread();
    let mut face = Row::new().spacing(bar::GAP).align_y(Alignment::Center).push(mark);
    if count > 0 {
        face = face.push(
            text(badge(count))
                .size(size::MONO)
                .font(font::DATA)
                .wrapping(text::Wrapping::None)
                .color(color::text_secondary().scale_alpha(ink)),
        );
    }
    Parts {
        core: super::press(face, core_width(count), Message::Open(Drawer::History)),
        revealed: None,
    }
}

/// The scrolling region's widget id, so the keyboard can scroll it.
pub(crate) const SCROLL: &str = "history-run";

/// Where a stop sits in the scrolling run, in pixels from its top: the row's
/// own height and the height with its revealed body.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Mark {
    pub stop: Stop,
    pub top: f32,
    pub row: f32,
    pub full: f32,
}

/// The drawer built, and where its stops are.
pub(crate) struct Plan {
    pub sheet: Sheet,
    pub marks: Vec<Mark>,
    /// The region's visible height, and whether the run overflows it.
    pub cap: f32,
    pub run_h: f32,
    pub scrolls: bool,
}

/// The drawer. See the module doc for its anatomy and ledger.
pub(crate) fn sheet(app: &App) -> Sheet {
    plan(app).sheet
}

/// Whether the way out clears the history (else it opens the settings).
pub(crate) fn foot_clears(panel: &Panel) -> bool {
    panel.fresh() > 0 || panel.read_total() > 0
}

pub(crate) fn plan(app: &App) -> Plan {
    let panel = &app.history;
    let fresh = panel.fresh();
    let count = if fresh > 0 {
        format!("{fresh} new")
    } else {
        String::new()
    };
    let mut s = Sheet::new(
        parts::drawer_sheet_head_count("notifications", &count),
        drawer::HEAD_H,
    );

    s.block(mode_switch(panel.mode()), drawer::ROW_H);
    s.rule();

    let at = now();
    let total = panel.read_total();
    let lit = |stop| panel.stop() == Some(stop);
    let mut marks = Vec::new();
    // Everything between the mode switch and the way out is one run. It is
    // laid in place while the whole sheet fits the surface's reserve and is a
    // scrolling region once it would not, so the sheet never outgrows it.
    let mut r = Sheet::run();
    match panel.hero() {
        Some(hero) => {
            r.current(hero_row(hero, at));
            details(&mut r, hero);
            for e in panel.rows() {
                entry(&mut r, &mut marks, panel, e, at, false);
            }
        }
        None => r.row(parts::drawer_note("all caught up")),
    }
    if panel.quick_hidden() > 0 && !panel.expanded() {
        // Opens the history, where the rest are.
        marks.push(Mark::at(Stop::Hidden, r.used(), drawer::ROW_H));
        r.row(parts::drawer_more(
            &format!("+{} more in history", panel.quick_hidden()),
            lit(Stop::Hidden),
            Message::HistoryFold,
        ));
    }
    r.rule();
    marks.push(Mark::at(Stop::Fold, r.used(), drawer::ROW_H));
    r.row(parts::drawer_fold(
        "History",
        &total.to_string(),
        panel.expanded(),
        lit(Stop::Fold),
        Message::HistoryFold,
    ));
    if panel.expanded() {
        if total == 0 {
            r.row(parts::drawer_note("nothing in history"));
        }
        for e in panel.drawer_rows() {
            entry(&mut r, &mut marks, panel, e, at, true);
        }
        if panel.can_grow() {
            marks.push(Mark::at(Stop::More, r.used(), drawer::ROW_H));
            r.row(parts::drawer_more(
                &format!("+{} more", panel.left()),
                lit(Stop::More),
                Message::HistoryMore,
            ));
        } else if panel.left() > 0 {
            r.row(parts::drawer_note(&format!("+{} older", panel.left())));
        }
    }
    // What stays put under the run: the rule and the way out.
    let foot = menu::SEP_H + drawer::ROW_H;
    let cap = (drawer::RESERVE_H - s.used() - foot).max(drawer::ROW_H);
    let (run_h, scrolls) = (r.used(), Sheet::overflows(&r, cap));
    s.scrolled(r, cap, Some((SCROLL, Message::HistoryScrolled)));

    s.rule();
    if foot_clears(panel) {
        s.row(parts::drawer_link_lit(
            "Clear all",
            Message::HistoryClear,
            lit(Stop::Foot),
        ));
    } else {
        s.row(parts::drawer_link_lit(
            "Notification settings",
            Message::OpenSettings("notifications"),
            lit(Stop::Foot),
        ));
    }
    Plan {
        sheet: s,
        marks,
        cap,
        run_h,
        scrolls,
    }
}

impl Mark {
    fn at(stop: Stop, top: f32, row: f32) -> Self {
        Self {
            stop,
            top,
            row,
            full: row,
        }
    }
}

/// Where the scrolling region must go for the keyboard's stop to be in view:
/// `None` if there is nothing to scroll, no stop in the run, or it is
/// already shown. Scrolls as little as it can, to the top for the first stop
/// so the hero shows above it.
pub(crate) fn follow(app: &App) -> Option<f32> {
    let p = plan(app);
    if !p.scrolls {
        return None;
    }
    let panel = &app.history;
    let stop = panel.stop()?;
    let m = p.marks.iter().find(|m| m.stop == stop)?;
    let cur = panel.scroll();
    let max = (p.run_h - p.cap).max(0.0);
    let first = panel.stops().first() == Some(&stop);
    let want = if first {
        0.0
    } else if m.top < cur {
        m.top
    } else if m.top + m.full > cur + p.cap {
        // The revealed body too if it fits, else the row at the top.
        (m.top + m.full - p.cap).min(m.top)
    } else {
        return None;
    };
    let want = want.clamp(0.0, max);
    ((want - cur).abs() > 0.5).then_some(want)
}

/// Whether the keyboard's stop is on screen right now, scroll included. A
/// control under the region (the way out) always is.
pub(crate) fn lit_in_view(app: &App) -> bool {
    let p = plan(app);
    let panel = &app.history;
    let Some(stop) = panel.stop() else {
        return false;
    };
    if !p.scrolls {
        return true;
    }
    match p.marks.iter().find(|m| m.stop == stop) {
        Some(m) => m.top >= panel.scroll() - 0.5 && m.top + m.row <= panel.scroll() + p.cap + 0.5,
        None => stop == Stop::Foot,
    }
}

/// Normal, Quiet, Do not disturb: one segment, one in force, in the battery
/// drawer's block. Short labels: three cells share the sheet's width.
fn mode_switch(mode: Mode) -> Element<'static, Message, Theme> {
    let option = |label, m: Mode| (label, mode == m, Some(Message::HistoryMode(m)));
    container(parts::segment_control(vec![
        option("Normal", Mode::Normal),
        option("Quiet", Mode::Quiet),
        option("DND", Mode::Dnd),
    ]))
    .width(Length::Fill)
    .height(Length::Fixed(drawer::ROW_H))
    .align_y(Alignment::Center)
    .padding([0.0, drawer::ROW_X])
    .into()
}

/// The leading mark: the bell on the ink ramp (a step down once read), or the
/// warning glyph in danger ink for a critical entry.
fn mark_of(e: &HistoryEntry, side: f32, read: bool) -> Element<'static, Message, Theme> {
    if e.urgency == Urgency::Critical {
        return parts::mark(crate::icons::symbolic("dialog-warning"), side, color::danger());
    }
    parts::mark(
        crate::icons::symbolic("preferences-system-notifications"),
        side,
        if read {
            color::text_tertiary()
        } else {
            color::text_secondary()
        },
    )
}

/// The newest unread, on the hero's glass: the summary over its app and age,
/// and the verb that removes it.
fn hero_row(e: &HistoryEntry, at: u64) -> Element<'static, Message, Theme> {
    let state = format!(
        "{} \u{b7} {}",
        elide(&e.app_name, tok::APP_CHARS),
        ago(at, e.timestamp)
    );
    let tint = if e.urgency == Urgency::Critical {
        color::danger()
    } else {
        color::text_tertiary()
    };
    parts::drawer_current(
        mark_of(e, drawer::CURRENT_MARK, false),
        &elide(title(e), tok::HERO_CHARS),
        &state,
        tint,
        Some(("Dismiss", Message::HistoryRemove(e.id))),
    )
}

/// An entry's revealed block: its body, and its live actions as pills.
fn details(s: &mut Sheet, e: &HistoryEntry) {
    let lines = clamp_body(&e.body);
    if !lines.is_empty() {
        let height = parts::drawer_detail_height(lines.len());
        s.block(parts::drawer_detail(lines), height);
    }
    if has_verbs(e) {
        let verbs = e
            .actions
            .iter()
            .map(|(key, label)| {
                (
                    elide(label, tok::ACTION_CHARS),
                    Message::HistoryAction(e.id, key.clone()),
                )
            })
            .collect();
        s.block(parts::drawer_pills(verbs), drawer::ROW_H);
    }
}

/// One row, and its body under it if it is revealed.
fn entry(s: &mut Sheet, marks: &mut Vec<Mark>, panel: &Panel, e: &HistoryEntry, at: u64, read: bool) {
    let id = e.id;
    let selected = panel.selected() == Some(id);
    let revealed = panel.revealed(id);
    let top = s.used();
    s.row(parts::drawer_entry(EntryRow {
        mark: mark_of(e, drawer::MARK, read),
        summary: elide(title(e), tok::SUMMARY_CHARS),
        meta: format!("{} {}", elide(&e.app_name, tok::APP_CHARS), ago(at, e.timestamp)),
        read,
        lit: selected || revealed,
        hot: selected || panel.hot() == Some(id),
        on_press: Message::HistoryToggle(id),
        on_dismiss: Message::HistoryRemove(id),
        on_enter: Message::HistoryHot(Some(id)),
        on_exit: Message::HistoryHot(None),
    }));
    if revealed {
        details(s, e);
    }
    marks.push(Mark {
        stop: Stop::Row(id),
        top,
        row: drawer::ROW_H,
        full: s.used() - top,
    });
}
