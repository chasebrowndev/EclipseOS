// SPDX-License-Identifier: AGPL-3.0-only
//! Withholding unapproved command widgets (ADR 0067 "Withhold"; COMP-10
//! §3.11).
//!
//! Every road to a new command ends in `config::apply_loaded`: an inotify
//! reload and a `set_config_collection` commit alike. There, [`settle`] keeps
//! a command widget in the live config only if its canonical hash equals the
//! catalog block of the same name or the owner's recorded approval. Anything
//! else is withheld: removed from `bar.custom_widgets` (so `get_config` never
//! serves its argv and the taskbar never runs it), remembered in
//! `state.widget_approvals.withheld`, and queued once in
//! `state.widget_approvals.queue` for the compositor-drawn prompt. Its
//! `custom:<name>` id stays in `bar.widgets.*` and draws nothing.
//!
//! This module decides and queues; it draws nothing and answers nothing. The
//! prompt (Trusted UI, TCB) pulls from [`next_pending`], and on the owner's
//! answer calls `approvals::record_approval`, `config_rpc::revert_widget` or
//! `config_rpc::remove_widget`, or inserts into `widget_approvals.declined` for
//! Not now.

use std::collections::{HashSet, VecDeque};

use super::widget_hash::{self, WidgetHash};
use super::{approvals, Config, CustomWidget, CustomWidgetKind};
use crate::addons::Hook;
use crate::state::AbyssState;

/// Longest command text a prompt is handed, in chars (COMP-10 §3.2 clamp).
pub const COMMAND_TEXT_MAX: usize = 512;

/// Which prompt a withheld widget gets.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PendingKind {
    /// A catalog block of this name exists and the hash differs.
    Altered,
    /// No catalog block of this name.
    New,
}

/// One withheld command widget, as the prompt needs it. Handle-free.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PendingWidget {
    pub name: String,
    pub kind: PendingKind,
    /// Display text for the untrusted block: argv and actions, control and
    /// bidi/format characters stripped, clamped to [`COMMAND_TEXT_MAX`]
    /// chars. Lines are separated by `\n`, which this module inserts and the
    /// widget can never supply.
    pub command_text: String,
    pub hash: WidgetHash,
}

impl PendingWidget {
    fn key(&self) -> (String, WidgetHash) {
        (self.name.clone(), self.hash)
    }

    fn is(&self, name: &str, hash: &WidgetHash) -> bool {
        self.name == name && self.hash == *hash
    }
}

/// Per-session approval state. Owned by `AbyssState`, plain data.
#[derive(Debug, Default)]
pub struct WidgetApprovals {
    /// Every command widget withheld by the last apply, with its index in the
    /// merged `widget` list, so `get_config` keeps file order.
    pub withheld: Vec<(usize, PendingWidget)>,
    /// Waiting for a prompt, oldest first. Never holds a (name, hash) twice,
    /// nor the one on screen.
    pub queue: VecDeque<PendingWidget>,
    /// The (name, hash) the prompt is showing, set by [`next_pending`] and
    /// cleared by [`prompt_closed`].
    pub on_screen: Option<(String, WidgetHash)>,
    /// Not now, for this session. The surface inserts; `review_widget`
    /// removes. A declined (name, hash) stays withheld and is not queued.
    pub declined: HashSet<(String, WidgetHash)>,
}

/// A command widget's display text: `argv`, then one labelled line per
/// action. Arguments with spaces or quotes are single-quoted so the owner
/// sees where each one ends.
pub fn command_text(w: &CustomWidget) -> String {
    let argv = match &w.kind {
        CustomWidgetKind::Exec { argv, .. } | CustomWidgetKind::Stream { argv } => argv,
        CustomWidgetKind::Source { .. } => return String::new(),
    };
    let mut lines = vec![join(argv)];
    for (label, a) in [
        ("on-click", &w.on_click),
        ("on-scroll-up", &w.on_scroll_up),
        ("on-scroll-down", &w.on_scroll_down),
    ] {
        if let Some(a) = a {
            lines.push(format!("{label}: {}", join(a)));
        }
    }
    let text = lines.join("\n");
    if text.chars().count() <= COMMAND_TEXT_MAX {
        return text;
    }
    let mut out: String = text.chars().take(COMMAND_TEXT_MAX - 1).collect();
    out.push('\u{2026}');
    out
}

fn join(argv: &[String]) -> String {
    argv.iter()
        .map(|a| {
            let a: String = a.chars().filter(|c| !hidden(*c)).collect();
            if a.is_empty() || a.chars().any(|c| c.is_whitespace() || c == '\'' || c == '"') {
                format!("'{}'", a.replace('\'', "'\\''"))
            } else {
                a
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// Characters that must not reach a trusted prompt from widget text: C0/C1
/// controls (newlines included) and the invisible format characters that can
/// reorder or hide what is drawn (bidi overrides and isolates, zero-width
/// joiners, BOM).
fn hidden(c: char) -> bool {
    c.is_control()
        || matches!(c,
            '\u{200B}'..='\u{200F}'
            | '\u{202A}'..='\u{202E}'
            | '\u{2060}'..='\u{2064}'
            | '\u{2066}'..='\u{2069}'
            | '\u{FEFF}'
            | '\u{061C}'
            | '\u{00AD}')
}

/// Same definition as a catalog block of the same name: by hash for a
/// command widget, by value for a declarative one.
pub fn is_premade(catalog: &[CustomWidget], w: &CustomWidget) -> bool {
    catalog
        .iter()
        .find(|c| c.name == w.name)
        .is_some_and(|c| match widget_hash::hash(w) {
            Some(h) => widget_hash::hash(c) == Some(h),
            None => c == w,
        })
}

/// Decide which widgets in `next` go live, before it becomes the live config.
/// Called from `apply_loaded` and from `addons::start`; nothing else.
pub(crate) fn settle(state: &mut AbyssState, next: &mut Config) {
    let wa = &mut state.widget_approvals;
    if !state.addons.hooks.is_on(Hook::TaskbarWidgets) {
        // ADR 0066: no hook, no catalog, no withholding, no prompt.
        super::drop_unhooked_widgets(next, state.addons.hooks);
        wa.withheld.clear();
        wa.queue.clear();
        return;
    }
    let commands = next
        .bar
        .custom_widgets
        .iter()
        .any(|w| widget_hash::hash(w).is_some());
    let approved = if commands {
        approvals::read()
    } else {
        approvals::Approvals::default()
    };
    let catalog = &next.catalog;
    let mut withheld = Vec::new();
    let mut index = 0usize;
    next.bar.custom_widgets.retain(|w| {
        let i = index;
        index += 1;
        let Some(h) = widget_hash::hash(w) else {
            return true;
        };
        let premade = catalog.iter().find(|c| c.name == w.name);
        if premade.and_then(widget_hash::hash) == Some(h) || approved.get(&w.name) == Some(h) {
            return true;
        }
        withheld.push((
            i,
            PendingWidget {
                name: w.name.clone(),
                kind: if premade.is_some() {
                    PendingKind::Altered
                } else {
                    PendingKind::New
                },
                command_text: command_text(w),
                hash: h,
            },
        ));
        false
    });

    // Drop queued prompts for definitions that are gone or now approved.
    wa.queue.retain(|q| {
        withheld
            .iter()
            .any(|(_, w): &(usize, PendingWidget)| w.is(&q.name, &q.hash))
    });
    for (_, w) in &withheld {
        let key = w.key();
        let shown = wa.on_screen.as_ref() == Some(&key);
        if shown || wa.declined.contains(&key) || wa.queue.iter().any(|q| q.is(&w.name, &w.hash)) {
            continue;
        }
        tracing::info!(
            name = w.name,
            kind = ?w.kind,
            "command widget withheld until the owner approves it"
        );
        wa.queue.push_back(w.clone());
    }
    wa.withheld = withheld;
}

/// The next prompt to show: the oldest queued, non-declined widget, marked on
/// screen. `None` while one is on screen (one widget per prompt) or nothing
/// is waiting. The surface calls [`prompt_closed`] when the owner answers.
pub fn next_pending(state: &mut AbyssState) -> Option<PendingWidget> {
    let wa = &mut state.widget_approvals;
    if wa.on_screen.is_some() {
        return None;
    }
    while let Some(w) = wa.queue.pop_front() {
        if wa.declined.contains(&w.key()) {
            continue;
        }
        wa.on_screen = Some(w.key());
        return Some(w);
    }
    None
}

/// The prompt is gone (any answer). The surface calls this.
pub fn prompt_closed(state: &mut AbyssState) {
    state.widget_approvals.on_screen = None;
}

/// Not now was chosen for this (name, hash) this session.
pub fn is_declined(state: &AbyssState, name: &str, hash: &WidgetHash) -> bool {
    state
        .widget_approvals
        .declined
        .contains(&(name.to_owned(), *hash))
}

/// Why `review_widget` refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReviewError {
    /// No widget of that name in the config.
    Unknown,
    /// The widget is live: approved, premade, or declarative.
    NotPending,
}

/// `review_widget`: re-queue a withheld widget's prompt (ADR 0067). Clears
/// Not now for its current hash. Carries no answer. `Ok(false)` when it was
/// already queued or on screen: a peer cannot stack prompts.
pub fn review(state: &mut AbyssState, name: &str) -> Result<bool, ReviewError> {
    let wa = &mut state.widget_approvals;
    let Some((_, w)) = wa.withheld.iter().find(|(_, w)| w.name == name) else {
        return Err(
            if state.config.bar.custom_widgets.iter().any(|w| w.name == name) {
                ReviewError::NotPending
            } else {
                ReviewError::Unknown
            },
        );
    };
    let key = w.key();
    wa.declined.remove(&key);
    if wa.on_screen.as_ref() == Some(&key) || wa.queue.iter().any(|q| q.is(&w.name, &w.hash)) {
        return Ok(false);
    }
    wa.queue.push_back(w.clone());
    Ok(true)
}

/// Re-read and re-apply the config after an approval was recorded: the store
/// is not a config source, so the watcher does not see it change.
pub fn reapply(state: &mut AbyssState) {
    state.config_written.clear();
    super::watch::reload_now(state);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn w(text: &str) -> CustomWidget {
        let cfg = crate::config::tests::widgets_cfg(text);
        assert!(cfg.errors.is_empty(), "{:?}", cfg.errors);
        cfg.bar.custom_widgets[0].clone()
    }

    #[test]
    fn command_text_strips_and_clamps() {
        let t = command_text(&w(
            "bar { widget \"x\" { exec \"echo\" \"a b\" \"it's\" \"\" \"z\\u{202E}\\n\\u{1b}[31mred\"; on-click \"foot\"; } }",
        ));
        assert_eq!(t, "echo 'a b' 'it'\\''s' '' z[31mred\non-click: foot");
        assert!(!t.chars().any(|c| hidden(c) && c != '\n'));

        let long = format!("bar {{ widget \"x\" {{ exec \"{}\"; }} }}", "a".repeat(2000));
        let t = command_text(&w(&long));
        assert_eq!(t.chars().count(), COMMAND_TEXT_MAX);
        assert!(t.ends_with('\u{2026}'));
    }
}
