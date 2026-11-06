// SPDX-License-Identifier: AGPL-3.0-only
//! The notification history panel's model (`mode "de"`): the pure half.
//!
//! The toasts process keeps the history (`ec-services::notifications`); the bar
//! reads it over `org.eclipse.NotificationHistory`, shows it newest first and
//! offers dismiss, clear and a live entry's action buttons. This is what the
//! panel holds between a service report and a draw.
//!
//! **Human content is never logged.** Summaries and bodies are text a person
//! wrote or received; nothing here, or in the view, traces them.

use std::collections::HashSet;

use ec_services::notifications::HistoryEntry;
use ec_ui::tokens::history as tok;

/// The panel's state.
#[derive(Debug, Default)]
pub struct Panel {
    entries: Vec<HistoryEntry>,
    /// Ids the human has had in front of them, for the bell's dot.
    seen: HashSet<u32>,
}

impl Panel {
    /// Replace the list with the service's. Newest first, whatever order it
    /// arrived in (a stable sort, so equal seconds keep the service's order).
    pub fn set(&mut self, mut entries: Vec<HistoryEntry>) {
        entries.sort_by_key(|e| std::cmp::Reverse(e.timestamp));
        self.entries = entries;
        let live: HashSet<u32> = self.entries.iter().map(|e| e.id).collect();
        self.seen.retain(|id| live.contains(id));
    }

    pub fn entries(&self) -> &[HistoryEntry] {
        &self.entries
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Drop one entry now, ahead of the service's confirmation.
    pub fn remove(&mut self, id: u32) {
        self.entries.retain(|e| e.id != id);
        self.seen.remove(&id);
    }

    /// Drop everything now, ahead of the service's confirmation.
    pub fn clear(&mut self) {
        self.entries.clear();
        self.seen.clear();
    }

    /// The panel is in front of the human: everything in it is seen.
    pub fn mark_seen(&mut self) {
        self.seen.extend(self.entries.iter().map(|e| e.id));
    }

    /// Whether the bell wears its dot: something listed has not been in front
    /// of the human yet.
    pub fn unseen(&self) -> bool {
        self.entries.iter().any(|e| !self.seen.contains(&e.id))
    }

    /// The entries the panel lists, capped so the popup has a size.
    pub fn shown(&self) -> &[HistoryEntry] {
        &self.entries[..self.entries.len().min(tok::MAX_ROWS)]
    }

    /// How many are past the cap.
    pub fn hidden(&self) -> usize {
        self.entries.len().saturating_sub(tok::MAX_ROWS)
    }
}

/// A body cut to the panel's few lines: each source line wrapped at a word
/// where it can, the last kept line ended with an ellipsis if text was left.
/// Markup is not interpreted; this is plain text.
pub fn clamp_body(body: &str) -> Vec<String> {
    let mut lines: Vec<String> = Vec::new();
    let mut cut = false;
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

    #[test]
    fn empty_history_is_empty_and_the_bell_is_quiet() {
        let p = Panel::default();
        assert!(p.is_empty());
        assert!(!p.unseen());
        assert!(p.shown().is_empty());
        assert_eq!(p.hidden(), 0);
    }

    #[test]
    fn entries_list_newest_first() {
        let mut p = Panel::default();
        p.set(vec![entry(1, 100, ""), entry(3, 300, ""), entry(2, 200, "")]);
        let ids: Vec<u32> = p.entries().iter().map(|e| e.id).collect();
        assert_eq!(ids, [3, 2, 1]);
    }

    #[test]
    fn remove_drops_one_and_clear_drops_all() {
        let mut p = Panel::default();
        p.set(vec![entry(1, 100, ""), entry(2, 200, ""), entry(3, 300, "")]);
        p.remove(2);
        assert_eq!(p.entries().iter().map(|e| e.id).collect::<Vec<_>>(), [3, 1]);
        p.remove(99);
        assert_eq!(p.entries().len(), 2);
        p.clear();
        assert!(p.is_empty());
        assert!(!p.unseen());
    }

    #[test]
    fn the_dot_lights_for_new_history_and_clears_when_seen() {
        let mut p = Panel::default();
        p.set(vec![entry(1, 100, "")]);
        assert!(p.unseen());
        p.mark_seen();
        assert!(!p.unseen());
        // A new arrival lights it again; the old one stays seen.
        p.set(vec![entry(1, 100, ""), entry(2, 200, "")]);
        assert!(p.unseen());
        p.mark_seen();
        assert!(!p.unseen());
        // An entry that goes away takes its seen mark with it, so an id the
        // service reuses is new again.
        p.set(vec![entry(2, 200, "")]);
        p.set(vec![entry(2, 200, ""), entry(1, 300, "")]);
        assert!(p.unseen());
    }

    #[test]
    fn the_list_is_capped_and_says_how_many_are_past_it() {
        let mut p = Panel::default();
        p.set(
            (0..tok::MAX_ROWS as u32 + 4)
                .map(|i| entry(i, u64::from(i), ""))
                .collect(),
        );
        assert_eq!(p.shown().len(), tok::MAX_ROWS);
        assert_eq!(p.hidden(), 4);
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
