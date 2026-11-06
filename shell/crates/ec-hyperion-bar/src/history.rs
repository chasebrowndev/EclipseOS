// SPDX-License-Identifier: AGPL-3.0-only
//! The notification widget's model (`mode "de"`): the pure half.
//!
//! The toasts process keeps the store and the mode (`ec-services::notifications`);
//! the bar reads them over `org.eclipse.NotificationHistory`. This is what the
//! widget holds between a service report and a draw:
//!
//! - the **mode** and the **unread count** the bell shows;
//! - the **quick view**: the entries that were unread when the drawer opened.
//!   Opening marks everything read, so the list is a snapshot taken at open
//!   (plus whatever arrives while it is open) and survives the count falling
//!   to zero under it. Its first entry is the hero, the rest are rows;
//! - the **history section**: the already-read entries, as many as have been
//!   asked for ("+N more" asks for another step), folded until asked for;
//! - the **row state**: which row the pointer is on, which one is revealed,
//!   which one the keyboard is on.
//!
//! **Human content is never logged.** Summaries and bodies are text a person
//! wrote or received; nothing here, or in the view, traces them.

use ec_services::notifications::{HistoryEntry, Mode};
use ec_ui::tokens::{drawer, history as tok};

/// What the history thread reports.
#[derive(Debug, Clone, PartialEq)]
pub enum Report {
    /// The service's mode (a `ModeChanged` signal or a poll).
    Mode(Mode),
    /// The unread count and the newest unread entries, capped to what the
    /// quick view can show.
    Unread {
        count: u32,
        entries: Vec<HistoryEntry>,
        /// How many read entries the store holds, for the drawer's label.
        read_total: u32,
    },
    /// Ids that can no longer be invoked (the app withdrew them, a toast
    /// expired), so a quick-view row already on screen drops its buttons.
    Gone(Vec<u32>),
    /// One page of read history.
    Read {
        entries: Vec<HistoryEntry>,
        total: u32,
        offset: u32,
    },
}

/// Something the keyboard can stand on: a row, or one of the sheet's controls.
/// The hero is not one (its pills are the pointer's).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stop {
    Row(u32),
    /// "+N more in history" under the quick view.
    Hidden,
    /// The History fold header.
    Fold,
    /// "+N more" at the foot of the unfolded history.
    More,
    /// The way out ("Clear all", or the settings link when empty).
    Foot,
}

/// The widget's state.
#[derive(Debug, Default)]
pub struct Panel {
    mode: Mode,
    unread: u32,
    /// The newest unread entries as last reported: the quick view's next
    /// contents, kept ready so opening needs no round trip.
    pending: Vec<HistoryEntry>,
    /// The quick view while the popover is open.
    quick: Vec<HistoryEntry>,
    /// How many were unread at open, for the "+N in history" note.
    quick_total: u32,
    /// How much of the service's unread count the head's total has already
    /// taken in, so a count that grows under a flood is counted once.
    credited: u32,
    read: Vec<HistoryEntry>,
    read_total: u32,
    read_offset: u32,
    /// How many read entries the section lists (and asks the service for).
    read_limit: usize,
    expanded: bool,
    /// The row the pointer is on: its reading gives way to the dismiss cross.
    hot: Option<u32>,
    /// The row whose body is revealed (the hero's always is).
    open: Option<u32>,
    /// The stop the keyboard is on.
    sel: Option<Stop>,
    /// Ids removed since the drawer opened, so a stale page cannot promote
    /// one back into the quick view.
    gone: Vec<u32>,
    /// Where the scrolling region is scrolled to, in pixels from its top.
    scroll: f32,
}

impl Panel {
    pub fn mode(&self) -> Mode {
        self.mode
    }

    pub fn set_mode(&mut self, mode: Mode) {
        self.mode = mode;
    }

    /// The bell's badge.
    pub fn unread(&self) -> u32 {
        self.unread
    }

    /// Apply a report. `Unread` also refreshes what the next open will show.
    pub fn apply(&mut self, report: Report) {
        match report {
            Report::Mode(m) => self.mode = m,
            Report::Gone(ids) => {
                for e in self.quick.iter_mut().filter(|e| ids.contains(&e.id)) {
                    e.live = false;
                }
            }
            Report::Unread {
                count,
                entries,
                read_total,
            } => {
                self.unread = count;
                self.pending = entries;
                self.read_total = read_total;
            }
            Report::Read {
                entries,
                total,
                offset,
            } => {
                self.read_total = total;
                self.promote(&entries, offset);
                self.read = entries;
                self.read_total = total;
                self.read_offset = offset;
            }
        }
    }

    /// The drawer is opening: the unread entries become the quick view and
    /// the history folds. Called once per open, not on a reflow.
    pub fn begin_open(&mut self) {
        self.quick = std::mem::take(&mut self.pending);
        self.quick_total = self.unread.max(self.quick.len() as u32);
        self.credited = self.unread;
        self.expanded = false;
        self.read.clear();
        // The read total stays as last reported: the fold row shows it while
        // folded, and the next report corrects it once everything is marked.
        self.read_offset = 0;
        self.read_limit = tok::PAGE;
        self.hot = None;
        self.open = None;
        self.sel = None;
        self.gone.clear();
        self.scroll = 0.0;
    }

    /// Whether the quick view has room and the store has unread-at-open
    /// entries it is not showing: after a removal, the next ones step up.
    pub fn needs_backfill(&self) -> bool {
        self.unread == 0 && self.quick.len() < tok::MAX_ROWS && self.quick_hidden() > 0
    }

    /// Fill the quick view from a page of read history: the newest entries
    /// it does not show are the unread-at-open ones it had no room for.
    fn promote(&mut self, page: &[HistoryEntry], offset: u32) {
        if !self.needs_backfill() || offset as usize > self.quick.len() {
            return;
        }
        let room = (tok::MAX_ROWS - self.quick.len()).min(self.quick_hidden());
        let take: Vec<HistoryEntry> = page
            .iter()
            .filter(|e| !self.gone.contains(&e.id) && !self.quick.iter().any(|q| q.id == e.id))
            .take(room)
            .cloned()
            .collect();
        self.quick.extend(take);
    }

    /// The drawer is open and more arrived: put the new ids on top and add
    /// them to the head's total. The report carries only a page of the newest
    /// unread, so a burst bigger than the page is counted from the service's
    /// unread count (what it grew by since the last look), not from the ids.
    /// Returns whether anything arrived.
    pub fn absorb(&mut self) -> bool {
        let grown = self.unread.saturating_sub(self.credited);
        self.credited = self.unread;
        let fresh: Vec<HistoryEntry> = self
            .pending
            .iter()
            .filter(|p| !self.quick.iter().any(|q| q.id == p.id))
            .cloned()
            .collect();
        let arrived = grown.max(fresh.len() as u32);
        if arrived == 0 {
            return false;
        }
        self.quick_total += arrived;
        let mut merged = fresh;
        merged.append(&mut self.quick);
        self.quick = merged;
        true
    }

    /// The quick view's entries, capped so the drawer has a size.
    pub fn quick(&self) -> &[HistoryEntry] {
        &self.quick[..self.quick.len().min(tok::MAX_ROWS)]
    }

    /// How many were unread when the drawer opened (and still stand): the
    /// head's "N new".
    pub fn fresh(&self) -> u32 {
        self.quick_total
    }

    /// The newest unread: the drawer's hero.
    pub fn hero(&self) -> Option<&HistoryEntry> {
        self.quick().first()
    }

    /// The quick view under the hero.
    pub fn rows(&self) -> &[HistoryEntry] {
        self.quick().get(1..).unwrap_or(&[])
    }

    /// How many opened unread are not listed in the quick view (they are in
    /// the history section, read).
    pub fn quick_hidden(&self) -> usize {
        (self.quick_total as usize).saturating_sub(self.quick().len())
    }

    pub fn expanded(&self) -> bool {
        self.expanded
    }

    pub fn set_expanded(&mut self, on: bool) {
        self.expanded = on;
        self.read_limit = tok::PAGE;
        if !on {
            self.read.clear();
        }
    }

    pub fn read(&self) -> &[HistoryEntry] {
        &self.read
    }

    /// How many of the store's read entries the quick view already lists: the
    /// newest ones, once opening has marked them read. They sit at the head of
    /// the read history, so the section starts past them.
    pub fn skip(&self) -> u32 {
        if self.unread == 0 {
            self.quick().len() as u32
        } else {
            0
        }
    }

    /// The section's rows: the read entries minus any the quick view shows, so
    /// an entry is never listed twice.
    pub fn drawer_rows(&self) -> Vec<&HistoryEntry> {
        self.read
            .iter()
            .filter(|e| !self.quick().iter().any(|q| q.id == e.id))
            .take(self.cap())
            .collect()
    }

    /// The history section's count: read entries not already in the quick view.
    pub fn read_total(&self) -> u32 {
        self.read_total.saturating_sub(self.skip())
    }

    /// Where the section starts in the store.
    pub fn first_offset(&self) -> u32 {
        self.skip()
    }

    /// How many entries the section lists (a fresh panel lists one step).
    fn cap(&self) -> usize {
        self.read_limit.max(tok::PAGE)
    }

    /// How many entries the section asks the service for.
    pub fn limit(&self) -> u32 {
        self.cap() as u32
    }

    /// Read entries the section does not list yet.
    pub fn left(&self) -> usize {
        (self.read_total() as usize).saturating_sub(self.drawer_rows().len())
    }

    /// Whether "+N more" can list another step: there are more, and the
    /// section is under its ceiling.
    pub fn can_grow(&self) -> bool {
        self.left() > 0 && self.cap() < tok::HISTORY_MAX
    }

    /// List another step of read entries. Returns the new limit.
    pub fn grow(&mut self) -> u32 {
        self.read_limit = (self.cap() + tok::PAGE).min(tok::HISTORY_MAX);
        self.limit()
    }

    /// The pointer is on this row, or on none. Moving onto a row takes the
    /// cross from the keyboard: whichever moved last owns it.
    pub fn set_hot(&mut self, id: Option<u32>) {
        if id.is_some() {
            self.sel = None;
        }
        self.hot = id;
    }

    pub fn hot(&self) -> Option<u32> {
        self.hot
    }

    /// Whether `id`'s body is revealed: the hero's always, a row's on a click.
    pub fn revealed(&self, id: u32) -> bool {
        self.open == Some(id) || self.hero().is_some_and(|h| h.id == id)
    }

    /// Reveal a row's body, or hide it if it is the one revealed.
    pub fn toggle(&mut self, id: u32) {
        self.open = if self.open == Some(id) { None } else { Some(id) };
    }

    /// The row the keyboard is on, if it is on a row.
    pub fn selected(&self) -> Option<u32> {
        match self.sel {
            Some(Stop::Row(id)) => Some(id),
            _ => None,
        }
    }

    /// The stop the keyboard is on.
    pub fn stop(&self) -> Option<Stop> {
        self.sel
    }

    /// Every stop the keyboard walks, top to bottom: the quick view under the
    /// hero (which has its own pills), the note that opens the rest, the fold
    /// header, the unfolded history with its "+N more", and the way out. Only
    /// what is on the sheet.
    pub fn stops(&self) -> Vec<Stop> {
        let mut s: Vec<Stop> = self.rows().iter().map(|e| Stop::Row(e.id)).collect();
        if self.quick_hidden() > 0 && !self.expanded {
            s.push(Stop::Hidden);
        }
        s.push(Stop::Fold);
        if self.expanded {
            s.extend(self.drawer_rows().iter().map(|e| Stop::Row(e.id)));
            if self.can_grow() {
                s.push(Stop::More);
            }
        }
        s.push(Stop::Foot);
        s
    }

    /// Move the keyboard `delta` stops, from none onto the first. Stops at
    /// the ends.
    pub fn step(&mut self, delta: i32) {
        // The keyboard has the rows now: a stale pointer must not hold a cross.
        self.hot = None;
        let stops = self.stops();
        let at = self.sel.and_then(|s| stops.iter().position(|i| *i == s));
        let to = match at {
            None => 0,
            Some(i) => (i as i64 + i64::from(delta)).clamp(0, stops.len() as i64 - 1) as usize,
        };
        self.sel = Some(stops[to]);
    }

    /// Drop the keyboard's row and put the keyboard on its neighbour. Returns
    /// the id that went; a control is never removed.
    pub fn remove_selected(&mut self) -> Option<u32> {
        let id = self.selected()?;
        let stops = self.stops();
        let at = stops.iter().position(|i| *i == Stop::Row(id))?;
        // The stop below, else the one above (a control, if no row is left).
        let next = stops
            .get(at + 1)
            .copied()
            .or_else(|| at.checked_sub(1).and_then(|p| stops.get(p).copied()));
        self.remove(id);
        self.sel = next;
        Some(id)
    }

    /// Where the scrolling region is scrolled to.
    pub fn scroll(&self) -> f32 {
        self.scroll
    }

    pub fn set_scroll(&mut self, y: f32) {
        self.scroll = y;
    }

    /// Drop one entry now, ahead of the service's confirmation.
    pub fn remove(&mut self, id: u32) {
        let before = self.quick.len();
        self.quick.retain(|e| e.id != id);
        if self.quick.len() < before {
            self.quick_total = self.quick_total.saturating_sub(1);
            // Already marked read in the store: it leaves the read count too.
            if self.unread == 0 {
                self.read_total = self.read_total.saturating_sub(1);
            }
            self.gone.push(id);
        }
        let before = self.read.len();
        self.read.retain(|e| e.id != id);
        if self.read.len() < before {
            self.read_total = self.read_total.saturating_sub(1);
            self.gone.push(id);
        }
        self.pending.retain(|e| e.id != id);
        for slot in [&mut self.hot, &mut self.open] {
            if *slot == Some(id) {
                *slot = None;
            }
        }
        if self.sel == Some(Stop::Row(id)) {
            self.sel = None;
        }
    }

    /// Drop everything now, ahead of the service's confirmation.
    pub fn clear(&mut self) {
        self.quick.clear();
        self.quick_total = 0;
        self.read.clear();
        self.read_total = 0;
        self.read_offset = 0;
        self.pending.clear();
        self.unread = 0;
        (self.hot, self.open, self.sel) = (None, None, None);
        self.scroll = 0.0;
    }
}

/// What a row calls an entry: its summary, else the app's name, else a word.
pub fn title(e: &HistoryEntry) -> &str {
    [e.summary.trim(), e.app_name.trim()]
        .into_iter()
        .find(|t| !t.is_empty())
        .unwrap_or("notification")
}

/// The badge text: `(3)`, and `(99+)` past two digits. Empty for none.
pub fn badge(count: u32) -> String {
    match count {
        0 => String::new(),
        1..=99 => format!("({count})"),
        _ => "(99+)".to_owned(),
    }
}

/// A body cut to the panel's few lines: each source line wrapped at a word
/// where it can, the last kept line ended with an ellipsis if text was left.
/// Markup is not interpreted; this is plain text.
pub fn clamp_body(body: &str) -> Vec<String> {
    let mut lines: Vec<String> = Vec::new();
    let mut cut = false;
    let body = ec_ui::markup::plain(body);
    'outer: for source in body.lines().map(str::trim).filter(|l| !l.is_empty()) {
        let mut rest = source;
        while !rest.is_empty() {
            if lines.len() == tok::BODY_LINES {
                cut = true;
                break 'outer;
            }
            let (head, tail) = split_at_width(rest, tok::BODY_CHARS);
            lines.push(head.to_owned());
            rest = tail.trim_start();
        }
    }
    if cut {
        if let Some(last) = lines.last_mut() {
            let mut kept: String = last.chars().take(tok::BODY_CHARS.saturating_sub(1)).collect();
            kept = kept.trim_end().to_owned();
            kept.push('\u{2026}');
            *last = kept;
        }
    }
    lines
}

/// `s` split after at most `width` characters, at the last space if there is
/// one in reach.
fn split_at_width(s: &str, width: usize) -> (&str, &str) {
    if s.chars().count() <= width {
        return (s, "");
    }
    let end = s.char_indices().nth(width).map_or(s.len(), |(i, _)| i);
    let head = &s[..end];
    match head.rfind(' ') {
        Some(space) if space > 0 => (&s[..space], &s[space + 1..]),
        _ => (head, &s[end..]),
    }
}

/// How long ago, in a word the panel's mono column fits: `now`, `4m`, `3h`,
/// `2d`. A timestamp in the future (a clock set back) reads `now`.
pub fn ago(now: u64, then: u64) -> String {
    let secs = now.saturating_sub(then);
    match secs {
        0..=59 => "now".to_owned(),
        60..=3599 => format!("{}m", secs / 60),
        3600..=86_399 => format!("{}h", secs / 3600),
        _ => format!("{}d", secs / 86_400),
    }
}

/// Whether an entry offers its action buttons: only while it is still up.
pub fn has_verbs(e: &HistoryEntry) -> bool {
    e.live && !e.actions.is_empty()
}

/// The pixel height of an entry's revealed block: its body lines and its row
/// of pills. Zero for an entry with neither.
pub fn detail_height(e: &HistoryEntry) -> f32 {
    let lines = clamp_body(&e.body).len();
    let body = if lines == 0 {
        0.0
    } else {
        ec_ui::widget::drawer_detail_height(lines)
    };
    body + if has_verbs(e) { drawer::ROW_H } else { 0.0 }
}

/// Seconds since the epoch.
pub fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

#[cfg(test)]
mod tests {
    use super::*;
    use ec_services::notifications::Urgency;

    fn entry(id: u32, ts: u64, body: &str) -> HistoryEntry {
        HistoryEntry {
            id,
            app_name: "mail".to_owned(),
            app_icon: String::new(),
            summary: format!("s{id}"),
            body: body.to_owned(),
            urgency: Urgency::Normal,
            timestamp: ts,
            actions: Vec::new(),
            live: false,
        }
    }

    fn unread(p: &mut Panel, ids: &[u32]) {
        p.apply(Report::Unread {
            count: ids.len() as u32,
            entries: ids.iter().map(|i| entry(*i, u64::from(*i), "")).collect(),
            read_total: 0,
        });
    }

    #[test]
    fn opening_snapshots_the_unread_and_the_count_may_fall_under_it() {
        let mut p = Panel::default();
        unread(&mut p, &[3, 2]);
        assert_eq!(p.unread(), 2);
        p.begin_open();
        // The service marks everything read; the quick view keeps its rows.
        unread(&mut p, &[]);
        assert_eq!(p.unread(), 0);
        assert_eq!(p.quick().len(), 2);
        assert!(!p.absorb(), "nothing new");
    }

    #[test]
    fn arrivals_while_open_go_on_top_once() {
        let mut p = Panel::default();
        unread(&mut p, &[2]);
        p.begin_open();
        unread(&mut p, &[5, 2]);
        assert!(p.absorb());
        assert_eq!(p.quick().iter().map(|e| e.id).collect::<Vec<_>>(), [5, 2]);
        assert!(!p.absorb());
    }

    #[test]
    fn the_quick_view_is_capped_and_counts_what_is_past_it() {
        let mut p = Panel::default();
        let ids: Vec<u32> = (0..tok::MAX_ROWS as u32 + 3).rev().collect();
        unread(&mut p, &ids);
        p.begin_open();
        assert_eq!(p.quick().len(), tok::MAX_ROWS);
        assert_eq!(p.quick_hidden(), 3);
    }

    #[test]
    fn a_withdrawn_notification_loses_its_buttons_in_the_open_quick_view() {
        let mut p = Panel::default();
        let mut live = entry(5, 5, "");
        live.live = true;
        live.actions = vec![("ok".to_owned(), "Yes".to_owned())];
        p.apply(Report::Unread {
            count: 1,
            entries: vec![live],
            read_total: 0,
        });
        p.begin_open();
        assert!(has_verbs(&p.quick()[0]));
        p.apply(Report::Gone(vec![5]));
        assert!(!has_verbs(&p.quick()[0]));
    }

    #[test]
    fn opening_folds_the_drawer() {
        let mut p = Panel::default();
        p.set_expanded(true);
        p.begin_open();
        assert!(!p.expanded());
        assert!(p.read().is_empty());
    }

    #[test]
    fn the_section_grows_a_step_at_a_time_up_to_its_ceiling() {
        let mut p = Panel::default();
        let many = (tok::HISTORY_MAX * 2) as u32;
        p.apply(Report::Read {
            entries: (1..=many).map(|i| entry(i, u64::from(i), "")).collect(),
            total: many,
            offset: 0,
        });
        assert_eq!(p.drawer_rows().len(), tok::PAGE);
        assert!(p.can_grow());
        assert_eq!(p.grow() as usize, tok::PAGE * 2);
        assert_eq!(p.drawer_rows().len(), tok::PAGE * 2);
        while p.can_grow() {
            p.grow();
        }
        assert_eq!(p.limit() as usize, tok::HISTORY_MAX);
        assert_eq!(p.left(), tok::HISTORY_MAX);
    }

    #[test]
    fn a_burst_bigger_than_the_page_is_counted_whole() {
        let mut p = Panel::default();
        p.begin_open();
        // 26 arrive while open; the service reports its count and a page.
        let page = |top: u32| -> Vec<HistoryEntry> {
            (top - tok::MAX_ROWS as u32 + 1..=top)
                .rev()
                .map(|i| entry(i, u64::from(i), ""))
                .collect()
        };
        p.apply(Report::Unread {
            count: 9,
            entries: page(9),
            read_total: 0,
        });
        assert!(p.absorb());
        p.apply(Report::Unread {
            count: 26,
            entries: page(26),
            read_total: 0,
        });
        assert!(p.absorb());
        assert_eq!(p.fresh(), 26);
        assert_eq!(p.quick().len(), tok::MAX_ROWS);
        assert_eq!(p.quick_hidden(), 26 - tok::MAX_ROWS);
        // Marked read, then five more.
        p.apply(Report::Unread {
            count: 0,
            entries: Vec::new(),
            read_total: 26,
        });
        assert!(!p.absorb());
        p.apply(Report::Unread {
            count: 5,
            entries: page(31),
            read_total: 26,
        });
        assert!(p.absorb());
        assert_eq!(p.fresh(), 31);
        // A repeat of the same report adds nothing.
        assert!(!p.absorb());
        assert_eq!(p.fresh(), 31);
    }

    #[test]
    fn the_pointer_and_the_keyboard_do_not_both_hold_the_cross() {
        let mut p = Panel::default();
        unread(&mut p, &[4, 3, 2, 1]);
        p.begin_open();
        p.step(1);
        assert_eq!(p.selected(), Some(3));
        p.set_hot(Some(2));
        assert_eq!((p.selected(), p.hot()), (None, Some(2)));
        p.step(1);
        assert_eq!(p.hot(), None);
        assert!(p.selected().is_some());
    }

    #[test]
    fn an_entry_without_a_summary_is_named_by_its_app() {
        let mut e = entry(1, 1, "");
        e.summary = "  ".to_owned();
        assert_eq!(title(&e), "mail");
        e.app_name.clear();
        assert_eq!(title(&e), "notification");
        e.summary = "hi".to_owned();
        assert_eq!(title(&e), "hi");
    }

    #[test]
    fn the_hero_is_the_newest_and_always_revealed() {
        let mut p = Panel::default();
        unread(&mut p, &[3, 2, 1]);
        p.begin_open();
        assert_eq!(p.hero().map(|e| e.id), Some(3));
        assert_eq!(p.rows().iter().map(|e| e.id).collect::<Vec<_>>(), [2, 1]);
        assert!(p.revealed(3));
        assert!(!p.revealed(2));
        p.toggle(2);
        assert!(p.revealed(2));
        p.toggle(2);
        assert!(!p.revealed(2));
    }

    #[test]
    fn the_keyboard_walks_the_rows_and_delete_lands_on_the_neighbour() {
        let mut p = Panel::default();
        unread(&mut p, &[4, 3, 2, 1]);
        p.begin_open();
        p.step(1);
        assert_eq!(
            p.selected(),
            Some(3),
            "none steps onto the first row, under the hero"
        );
        p.step(1);
        p.step(1);
        assert_eq!(p.selected(), Some(1), "the last row");
        p.step(1);
        assert_eq!(p.stop(), Some(Stop::Fold), "then the controls");
        p.step(-1);
        p.step(-1);
        assert_eq!(p.selected(), Some(2));
        assert_eq!(p.remove_selected(), Some(2));
        assert_eq!(p.selected(), Some(1));
        assert_eq!(p.remove_selected(), Some(1));
        assert_eq!(p.stop(), Some(Stop::Fold), "no row below: the control under them");
    }

    #[test]
    fn opening_clears_the_row_state() {
        let mut p = Panel::default();
        unread(&mut p, &[3, 2]);
        p.begin_open();
        p.step(1);
        p.toggle(2);
        p.set_hot(Some(2));
        p.begin_open();
        assert_eq!((p.selected(), p.hot()), (None, None));
        assert!(!p.revealed(2));
    }

    #[test]
    fn the_drawer_skips_what_the_quick_view_shows() {
        let mut p = Panel::default();
        unread(&mut p, &[3, 2]);
        p.apply(Report::Unread {
            count: 2,
            entries: vec![entry(3, 3, ""), entry(2, 2, "")],
            read_total: 5,
        });
        p.begin_open();
        // Everything is marked read: the store now holds 7, two of them listed.
        p.apply(Report::Unread {
            count: 0,
            entries: Vec::new(),
            read_total: 7,
        });
        assert_eq!(p.skip(), 2);
        assert_eq!(p.read_total(), 5);
        p.apply(Report::Read {
            entries: vec![entry(2, 2, ""), entry(1, 1, "")],
            total: 7,
            offset: 0,
        });
        assert_eq!(p.drawer_rows().iter().map(|e| e.id).collect::<Vec<_>>(), [1]);
    }

    #[test]
    fn the_folded_count_survives_opening() {
        let mut p = Panel::default();
        p.apply(Report::Unread {
            count: 0,
            entries: Vec::new(),
            read_total: 9,
        });
        p.begin_open();
        assert_eq!(p.read_total(), 9);
    }

    #[test]
    fn markup_in_a_body_is_stripped() {
        assert_eq!(clamp_body("a <b>bold</b> &amp; b"), ["a bold & b"]);
    }

    #[test]
    fn remove_drops_from_both_lists_and_clear_from_all() {
        let mut p = Panel::default();
        unread(&mut p, &[3, 2]);
        p.begin_open();
        p.apply(Report::Read {
            entries: vec![entry(1, 1, "")],
            total: 1,
            offset: 0,
        });
        p.remove(2);
        p.remove(1);
        assert_eq!(p.quick().len(), 1);
        assert!(p.read().is_empty());
        assert_eq!(p.read_total(), 0);
        p.clear();
        assert!(p.quick().is_empty());
        assert_eq!(p.unread(), 0);
    }

    #[test]
    fn the_badge_reads_in_a_few_characters() {
        assert_eq!(badge(0), "");
        assert_eq!(badge(7), "(7)");
        assert_eq!(badge(99), "(99)");
        assert_eq!(badge(100), "(99+)");
    }

    #[test]
    fn a_long_body_is_clamped_to_a_few_lines_with_an_ellipsis() {
        let body = "word ".repeat(200);
        let lines = clamp_body(&body);
        assert_eq!(lines.len(), tok::BODY_LINES);
        assert!(lines.last().expect("line").ends_with('\u{2026}'));
        assert!(lines.iter().all(|l| l.chars().count() <= tok::BODY_CHARS));
    }

    #[test]
    fn a_short_body_is_kept_whole() {
        assert_eq!(clamp_body("hello there"), ["hello there"]);
        assert!(clamp_body("").is_empty());
        assert!(clamp_body("  \n \n").is_empty());
        let two = clamp_body("one\ntwo");
        assert_eq!(two, ["one", "two"]);
    }

    #[test]
    fn a_body_of_exactly_the_clamp_is_not_marked_cut() {
        let lines = clamp_body("a\nb\nc");
        assert_eq!(lines, ["a", "b", "c"]);
        assert!(clamp_body("a\nb\nc\nd").last().expect("l").ends_with('\u{2026}'));
    }

    #[test]
    fn a_word_longer_than_a_line_is_split_not_dropped() {
        let long = "x".repeat(tok::BODY_CHARS + 10);
        let lines = clamp_body(&long);
        assert_eq!(lines.len(), 2);
        assert_eq!(lines.concat().chars().count(), long.chars().count());
    }

    #[test]
    fn time_reads_in_the_largest_unit() {
        assert_eq!(ago(1000, 1000), "now");
        assert_eq!(ago(1000, 950), "now");
        assert_eq!(ago(1000, 1100), "now");
        assert_eq!(ago(1000, 1000 - 240), "4m");
        assert_eq!(ago(100_000, 100_000 - 3 * 3600 - 5), "3h");
        assert_eq!(ago(1_000_000, 1_000_000 - 2 * 86_400), "2d");
    }

    #[test]
    fn an_entry_with_live_actions_reveals_more_than_one_without() {
        let plain = entry(1, 1, "body");
        let mut live = plain.clone();
        live.live = true;
        live.actions = vec![("reply".to_owned(), "Reply".to_owned())];
        assert!(detail_height(&live) > detail_height(&plain));
        // Actions on an expired entry are not offered, so they take no room.
        let mut stale = live.clone();
        stale.live = false;
        assert_eq!(detail_height(&stale), detail_height(&plain));
        assert_eq!(detail_height(&entry(1, 1, "")), 0.0);
    }

    #[test]
    fn removing_a_quick_row_backfills_from_the_hidden_unread() {
        let mut p = Panel::default();
        let n = tok::MAX_ROWS as u32 + 2;
        // The service reports the count and one page of the newest.
        p.apply(Report::Unread {
            count: n,
            entries: (3..=n).rev().map(|i| entry(i, u64::from(i), "")).collect(),
            read_total: 0,
        });
        p.begin_open();
        p.apply(Report::Unread {
            count: 0,
            entries: Vec::new(),
            read_total: n,
        });
        assert_eq!((p.quick().len(), p.quick_hidden()), (tok::MAX_ROWS, 2));
        p.remove(n);
        assert!(p.needs_backfill());
        assert_eq!(p.fresh(), n - 1);
        let page: Vec<_> = (1..n).rev().map(|i| entry(i, u64::from(i), "")).collect();
        // A stale page that still lists the removed id never brings it back.
        let mut stale = vec![entry(n, u64::from(n), "")];
        stale.extend(page.clone());
        p.apply(Report::Read {
            entries: stale,
            total: n,
            offset: p.first_offset(),
        });
        assert_eq!(p.quick().len(), tok::MAX_ROWS);
        assert!(p.quick().iter().all(|e| e.id != n));
        assert_eq!(p.quick_hidden(), 1);
        assert!(!p.needs_backfill() || p.quick_hidden() > 0);
    }

    #[test]
    fn the_keyboard_reaches_the_controls_and_never_removes_one() {
        let mut p = Panel::default();
        unread(&mut p, &[3, 2, 1]);
        p.begin_open();
        for _ in 0..10 {
            p.step(1);
        }
        assert_eq!(p.stop(), Some(Stop::Foot));
        assert_eq!(p.remove_selected(), None);
        p.step(-1);
        assert_eq!(p.stop(), Some(Stop::Fold));
        assert_eq!(p.remove_selected(), None);
    }
}
