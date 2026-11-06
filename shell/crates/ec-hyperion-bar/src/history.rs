// SPDX-License-Identifier: AGPL-3.0-only
//! The notification widget's model (`mode "de"`): the pure half.
//!
//! The toasts process keeps the store and the mode (`ec-services::notifications`);
//! the bar reads them over `org.eclipse.NotificationHistory`. This is what the
//! widget holds between a service report and a draw:
//!
//! - the **mode** and the **unread count** the bell shows;
//! - the **quick view**: the entries that were unread when the popover opened.
//!   Opening marks everything read, so the list is a snapshot taken at open
//!   (plus whatever arrives while it is open) and survives the count falling
//!   to zero under it;
//! - the **history drawer**: one page of already-read entries, with its offset
//!   and the total, collapsed until asked for.
//!
//! **Human content is never logged.** Summaries and bodies are text a person
//! wrote or received; nothing here, or in the view, traces them.

use ec_services::notifications::{HistoryEntry, Mode};
use ec_ui::tokens::history as tok;

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
    /// One page of read history.
    Read {
        entries: Vec<HistoryEntry>,
        total: u32,
        offset: u32,
    },
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
    read: Vec<HistoryEntry>,
    read_total: u32,
    read_offset: u32,
    expanded: bool,
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
                self.read = entries;
                self.read_total = total;
                self.read_offset = offset;
            }
        }
    }

    /// The popover is opening: the unread entries become the quick view and
    /// the drawer folds. Called once per open, not on a reflow.
    pub fn begin_open(&mut self) {
        self.quick = std::mem::take(&mut self.pending);
        self.quick_total = self.unread.max(self.quick.len() as u32);
        self.expanded = false;
        self.read.clear();
        // The read total stays as last reported: the fold row shows it while
        // folded, and the next report corrects it once everything is marked.
        self.read_offset = 0;
    }

    /// The popover is open and more arrived: put the new ids on top.
    /// Returns whether anything was added.
    pub fn absorb(&mut self) -> bool {
        let fresh: Vec<HistoryEntry> = self
            .pending
            .iter()
            .filter(|p| !self.quick.iter().any(|q| q.id == p.id))
            .cloned()
            .collect();
        if fresh.is_empty() {
            return false;
        }
        self.quick_total += fresh.len() as u32;
        let mut merged = fresh;
        merged.append(&mut self.quick);
        self.quick = merged;
        true
    }

    /// The quick view's rows, capped so the popover has a size.
    pub fn quick(&self) -> &[HistoryEntry] {
        &self.quick[..self.quick.len().min(tok::MAX_ROWS)]
    }

    /// How many opened unread are not listed in the quick view (they are in
    /// the history drawer, read).
    pub fn quick_hidden(&self) -> usize {
        (self.quick_total as usize).saturating_sub(self.quick().len())
    }

    pub fn expanded(&self) -> bool {
        self.expanded
    }

    pub fn set_expanded(&mut self, on: bool) {
        self.expanded = on;
        if !on {
            self.read.clear();
        }
    }

    pub fn read(&self) -> &[HistoryEntry] {
        &self.read
    }

    /// How many of the store's read entries the quick view already lists: the
    /// newest ones, once opening has marked them read. They sit at the head of
    /// the read history, so the drawer starts past them.
    pub fn skip(&self) -> u32 {
        if self.unread == 0 {
            self.quick().len() as u32
        } else {
            0
        }
    }

    /// The drawer's rows: the page of read entries minus any the quick view
    /// shows, so an entry is never listed twice.
    pub fn drawer_rows(&self) -> Vec<&HistoryEntry> {
        self.read
            .iter()
            .filter(|e| !self.quick().iter().any(|q| q.id == e.id))
            .collect()
    }

    /// The history drawer's count: read entries not already in the quick view.
    pub fn read_total(&self) -> u32 {
        self.read_total.saturating_sub(self.skip())
    }

    /// Where the first page starts (the store's offset).
    pub fn first_offset(&self) -> u32 {
        self.skip()
    }

    /// The current page's position within the drawer, 1-based: `(first, last)`.
    pub fn span(&self) -> (u32, u32) {
        let skip = self.skip();
        let from = self.read_offset.saturating_sub(skip);
        let shown = self.drawer_rows().len() as u32;
        (from + 1, from + shown)
    }

    /// The offset of the newer page, if there is one.
    pub fn newer(&self) -> Option<u32> {
        let first = self.first_offset();
        (self.read_offset > first).then(|| self.read_offset.saturating_sub(tok::PAGE as u32).max(first))
    }

    /// The offset of the older page, if there is one.
    pub fn older(&self) -> Option<u32> {
        let next = self.read_offset + tok::PAGE as u32;
        (next < self.read_total).then_some(next)
    }

    /// Drop one entry now, ahead of the service's confirmation.
    pub fn remove(&mut self, id: u32) {
        let before = self.quick.len();
        self.quick.retain(|e| e.id != id);
        if self.quick.len() < before {
            self.quick_total = self.quick_total.saturating_sub(1);
        }
        let before = self.read.len();
        self.read.retain(|e| e.id != id);
        if self.read.len() < before {
            self.read_total = self.read_total.saturating_sub(1);
        }
        self.pending.retain(|e| e.id != id);
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
    }
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

/// The pixel height of one entry's block: its head, its summary, one line per
/// body line, its action row if it has live actions, and the air around them.
pub fn entry_height(e: &HistoryEntry) -> f32 {
    let body = clamp_body(&e.body).len() as f32;
    // Head, summary and each body line are separated by one gap apiece.
    let mut h = 2.0 * tok::PAD_Y + tok::HEAD_H + tok::SUMMARY_H + tok::GAP;
    h += body * (tok::LINE_H + tok::GAP);
    if e.live && !e.actions.is_empty() {
        // The buttons are one gap shorter than their slot: the gap above them.
        h += tok::ACTION_H;
    }
    h
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
    fn opening_folds_the_drawer() {
        let mut p = Panel::default();
        p.set_expanded(true);
        p.begin_open();
        assert!(!p.expanded());
        assert!(p.read().is_empty());
    }

    #[test]
    fn read_pages_walk_both_ways() {
        let mut p = Panel::default();
        let page = |offset| Report::Read {
            entries: vec![entry(1, 1, "")],
            total: (tok::PAGE * 2 + 1) as u32,
            offset,
        };
        p.apply(page(0));
        assert_eq!(p.newer(), None);
        assert_eq!(p.older(), Some(tok::PAGE as u32));
        p.apply(page(tok::PAGE as u32 * 2));
        assert_eq!(p.older(), None);
        assert_eq!(p.newer(), Some(tok::PAGE as u32));
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
    fn an_entry_with_live_actions_is_taller_than_one_without() {
        let plain = entry(1, 1, "body");
        let mut live = plain.clone();
        live.live = true;
        live.actions = vec![("reply".to_owned(), "Reply".to_owned())];
        assert!(entry_height(&live) > entry_height(&plain));
        // Actions on an expired entry are not offered, so they take no room.
        let mut stale = live.clone();
        stale.live = false;
        assert_eq!(entry_height(&stale), entry_height(&plain));
    }
}
