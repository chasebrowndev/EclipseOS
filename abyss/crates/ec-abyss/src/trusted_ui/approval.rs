// SPDX-License-Identifier: AGPL-3.0-only
//! Command approval (ADR 0067; COMP-10 §3.11). TCB.
//!
//! `config::withhold` decides which command widgets are withheld and queues
//! them. This module is the only thing that answers: it pulls one queued
//! widget at a time, puts the fixed prompt up, and on the owner's choice
//! records the approval, reverts or removes the block, or declines for the
//! session. Nothing here is reachable from a socket; `review_widget` only
//! re-queues and then [`schedule`]s.
//!
//! Wording is fixed and `'static`. The widget's name and command reach the
//! prompt only as its untrusted well text: the name on one clamped line, so
//! it cannot draw a fake command or push the real one out of view. A command
//! too long to show whole gets no Accept/Allow; the owner cannot approve what
//! they cannot read.
//!
//! Budget (COMP-10 §6): after Not now, or a timeout, no approval prompt
//! comes up for [`QUIET`], however often `review_widget` or new widgets ask.
//! A peer cannot keep the seat taken.
//!
//! The Oracle Eyes model command (ADR 0072) is asked about the same way,
//! after any queued widget: Revert writes the shipped default back, Allow
//! records its hash under `approvals::MODEL_COMMAND_KEY`.

use std::time::{Duration, Instant};

use smithay::reexports::calloop::timer::{TimeoutAction, Timer};

use crate::config::withhold::{self, PendingKind, PendingModelCommand, PendingWidget};
use crate::state::AbyssState;

use super::modal::{Button, Modal, Role};

const HEADING: &str = "Command approval";
const ALTERED: &str = "This widget has been altered! Altered widgets are not guaranteed to be safe!";
const ALTERED_BODY: &str = "Accept the alteration, or revert to the original.";
const NEW_BODY: &str = "This is not a premade widget. It runs this command as you. \
                        EclipseOS is not responsible for what it does.";
const WELL: &str = "Taskbar widget and the command it runs";
const MODEL_BODY: &str = "Oracle Eyes' model command was changed. It runs this command as you, \
                          on the text Oracle Eyes reads. EclipseOS is not responsible for what it does.";
const MODEL_WELL: &str = "Oracle Eyes model command";
const WELL_CUT: &str = "Too long to show whole, so it cannot be allowed here:";

/// Longest widget name shown, in chars.
const NAME_MAX: usize = 40;

/// Quiet time after a declined prompt.
pub const QUIET: Duration = Duration::from_secs(30);

const NOT_NOW: Button = Button {
    label: "Not now",
    role: Role::Safe,
};

/// What a prompt is asking about.
#[derive(Debug)]
enum Subject {
    Widget(PendingWidget),
    ModelCommand(PendingModelCommand),
}

/// The subject a prompt is asking about, keyed by the prompt's token.
#[derive(Debug, Default)]
pub struct Asking {
    current: Option<(u64, Subject)>,
    next_token: u64,
    /// No new prompt before this (after Not now or a timeout).
    quiet_until: Option<Instant>,
}

/// Look at the queue on the next idle turn. Deferred so a config apply (which
/// may be running inside a socket request or inside a prompt's own answer)
/// finishes before a prompt goes up.
pub fn schedule(state: &mut AbyssState) {
    let _ = state.loop_handle.insert_idle(pump);
}

/// Bring the prompt in line with the queue: drop a prompt whose widget is no
/// longer withheld (hook off, edited, removed), then show the next one.
pub fn pump(state: &mut AbyssState) {
    if let Some((token, s)) = state.trusted_ui.asking.current.as_ref() {
        let still = match s {
            Subject::Widget(w) => state
                .widget_approvals
                .withheld
                .iter()
                .any(|(_, p)| p.name == w.name && p.hash == w.hash),
            Subject::ModelCommand(p) => state.widget_approvals.model_command.withheld.as_ref() == Some(p),
        };
        if still && state.trusted_ui.token() == Some(*token) {
            return;
        }
        let token = *token;
        tracing::info!(subject = name(s), "approval prompt withdrawn: no longer withheld");
        if let Some((_, s)) = state.trusted_ui.asking.current.take() {
            closed(state, &s);
        }
        super::cancel(state, token);
    }
    if state.trusted_ui.is_open() {
        // Another prompt owns the seat; its close schedules us again.
        return;
    }
    if state
        .trusted_ui
        .asking
        .quiet_until
        .is_some_and(|t| Instant::now() < t)
    {
        // The timer set with `quiet_until` pumps again when it ends.
        return;
    }
    let subject = if let Some(w) = withhold::next_pending(state) {
        Subject::Widget(w)
    } else if let Some(p) = withhold::next_pending_model_command(state) {
        Subject::ModelCommand(p)
    } else {
        return;
    };
    state.trusted_ui.asking.next_token += 1;
    let token = state.trusted_ui.asking.next_token;
    let modal = match &subject {
        Subject::Widget(w) => modal_for(token, w),
        Subject::ModelCommand(p) => model_modal_for(token, p),
    };
    let modal = match modal {
        Ok(m) => m,
        Err(e) => {
            // The buttons are constants; this is a programming error. The
            // subject stays withheld.
            tracing::error!(?e, "approval prompt malformed");
            closed(state, &subject);
            return;
        }
    };
    if !super::open(state, modal) {
        closed(state, &subject);
        match subject {
            Subject::Widget(w) => state.widget_approvals.queue.push_front(w),
            Subject::ModelCommand(_) => state.widget_approvals.model_command.queued = true,
        }
        return;
    }
    tracing::info!(subject = name(&subject), "approval prompt shown");
    state.trusted_ui.asking.current = Some((token, subject));
}

fn name(s: &Subject) -> &str {
    match s {
        Subject::Widget(w) => &w.name,
        Subject::ModelCommand(_) => MODEL_WELL,
    }
}

/// The prompt for `s` is gone, by any path.
fn closed(state: &mut AbyssState, s: &Subject) {
    match s {
        Subject::Widget(_) => withhold::prompt_closed(state),
        Subject::ModelCommand(_) => withhold::model_command_prompt_closed(state),
    }
}

fn buttons(other: &'static str, grant: Option<&'static str>) -> Vec<Button> {
    let mut b = vec![
        Button {
            label: other,
            role: Role::Other,
        },
        NOT_NOW,
    ];
    b.extend(grant.map(|label| Button {
        label,
        role: Role::Grant,
    }));
    b
}

fn model_modal_for(token: u64, p: &PendingModelCommand) -> Result<Modal, super::modal::Invalid> {
    let m = Modal::new(
        token,
        HEADING,
        None,
        MODEL_BODY,
        MODEL_WELL,
        &p.command_text,
        buttons("Revert", Some("Allow")),
    )?;
    if !m.is_cut() {
        return Ok(m);
    }
    Modal::new(
        token,
        HEADING,
        None,
        MODEL_BODY,
        WELL_CUT,
        &p.command_text,
        buttons("Revert", None),
    )
}

fn modal_for(token: u64, w: &PendingWidget) -> Result<Modal, super::modal::Invalid> {
    let (warning, body, other, grant) = match w.kind {
        PendingKind::Altered => (Some(ALTERED), ALTERED_BODY, "Revert", "Accept"),
        PendingKind::New => (None, NEW_BODY, "Remove", "Allow"),
    };
    let mut name = crate::render::text::sanitize_line(&w.name);
    if name.chars().count() > NAME_MAX {
        name = name.chars().take(NAME_MAX - 1).collect();
        name.push('\u{2026}');
    }
    let untrusted = format!("widget: {name}\n{}", w.command_text);
    let m = Modal::new(
        token,
        HEADING,
        warning,
        body,
        WELL,
        &untrusted,
        buttons(other, Some(grant)),
    )?;
    if !m.is_cut() {
        return Ok(m);
    }
    Modal::new(
        token,
        HEADING,
        warning,
        body,
        WELL_CUT,
        &untrusted,
        buttons(other, None),
    )
}

/// Whether `token` is an approval prompt's; `resolve` routes on this.
pub fn owns(state: &AbyssState, token: u64) -> bool {
    matches!(state.trusted_ui.asking.current, Some((t, _)) if t == token)
}

/// The owner answered (or the timeout did, as `Safe`).
pub fn answer(state: &mut AbyssState, choice: super::Choice) {
    let Some((token, s)) = state.trusted_ui.asking.current.take() else {
        return;
    };
    debug_assert_eq!(token, choice.token);
    // Closed before acting, so the re-apply below queues afresh rather than
    // skipping this subject as "on screen".
    closed(state, &s);
    let w = match s {
        Subject::Widget(w) => w,
        Subject::ModelCommand(p) => return answer_model_command(state, choice.role, p),
    };
    match choice.role {
        Role::Grant => {
            // The owner's answer holds for this session at once; the store
            // is written off the loop (no fsync on the frame path). If it
            // cannot be written, the widget is asked about again next session.
            tracing::info!(name = w.name, "command widget approved by the owner");
            if let Err(e) = crate::config::approvals::persist(&w.name, w.hash) {
                tracing::error!(name = w.name, error = %e, "approval not stored; it lasts this session only");
            }
            state.widget_approvals.granted.insert((w.name, w.hash));
            withhold::reapply(state);
        }
        Role::Other => {
            let done = match w.kind {
                PendingKind::Altered => crate::ipc::config_rpc::revert_widget(state, &w.name),
                PendingKind::New => crate::ipc::config_rpc::remove_widget(state, &w.name),
            };
            match done {
                Ok(()) => tracing::info!(name = w.name, kind = ?w.kind, "command widget reverted or removed"),
                Err(e) => {
                    tracing::warn!(name = w.name, error = %e.message, "revert/remove refused; widget stays withheld");
                    state.widget_approvals.declined.insert((w.name, w.hash));
                }
            }
        }
        Role::Safe => {
            state.widget_approvals.declined.insert((w.name, w.hash));
            quiet(state);
        }
    }
}

fn answer_model_command(state: &mut AbyssState, role: Role, p: PendingModelCommand) {
    use crate::config::approvals;
    match role {
        Role::Grant => {
            tracing::info!("oracle-eyes model command approved by the owner");
            if let Err(e) = approvals::persist(approvals::MODEL_COMMAND_KEY, p.hash) {
                tracing::error!(error = %e, "approval not stored; it lasts this session only");
            }
            state.widget_approvals.model_command.granted.insert(p.hash);
            withhold::reapply(state);
        }
        Role::Other => match crate::ipc::config_rpc::revert_model_command(state) {
            Ok(()) => tracing::info!("oracle-eyes model command reverted to the default"),
            Err(e) => {
                tracing::warn!(error = %e.message, "revert refused; the model command stays withheld");
                state.widget_approvals.model_command.declined.insert(p.hash);
            }
        },
        Role::Safe => {
            state.widget_approvals.model_command.declined.insert(p.hash);
            quiet(state);
        }
    }
}

/// No approval prompt for [`QUIET`]; pump again when it ends.
fn quiet(state: &mut AbyssState) {
    state.trusted_ui.asking.quiet_until = Some(Instant::now() + QUIET);
    let _ = state
        .loop_handle
        .insert_source(Timer::from_duration(QUIET), |_, _, state| {
            pump(state);
            TimeoutAction::Drop
        });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::widget_hash::WidgetHash;

    fn pending(kind: PendingKind) -> PendingWidget {
        PendingWidget {
            name: "load".into(),
            kind,
            command_text: "uptime".into(),
            hash: WidgetHash([0; 32]),
        }
    }

    #[test]
    fn altered_asks_accept_or_revert_with_the_warning() {
        let m = modal_for(7, &pending(PendingKind::Altered)).unwrap();
        let labels: Vec<_> = m.buttons().iter().map(|b| (b.label, b.role)).collect();
        assert_eq!(
            labels,
            [
                ("Revert", Role::Other),
                ("Not now", Role::Safe),
                ("Accept", Role::Grant)
            ]
        );
        assert_eq!(m.token, 7);
        assert_eq!(m.buttons()[m.safe()].label, "Not now");
    }

    #[test]
    fn new_asks_allow_or_remove_with_the_disclaimer() {
        let m = modal_for(1, &pending(PendingKind::New)).unwrap();
        let labels: Vec<_> = m.buttons().iter().map(|b| (b.label, b.role)).collect();
        assert_eq!(
            labels,
            [
                ("Remove", Role::Other),
                ("Not now", Role::Safe),
                ("Allow", Role::Grant)
            ]
        );
    }

    #[test]
    fn the_name_and_command_stay_in_the_untrusted_well() {
        let mut w = pending(PendingKind::New);
        w.name = "Command approval".into();
        let m = modal_for(1, &w).unwrap();
        let u = m.untrusted().join("\n");
        assert!(u.starts_with("widget: Command approval"), "{u}");
        assert!(u.contains("uptime"));
    }

    const PREMADE: &str = "bar {\n    widget \"load\" { exec \"uptime\"; }\n}\n";

    fn harness(tag: &str, text: &str) -> (crate::shell::focus::state_tests::Harness, std::path::PathBuf) {
        let dir = std::env::temp_dir().join(format!("abyss-prompt-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("widgets")).unwrap();
        std::fs::write(dir.join("widgets/premade.kdl"), PREMADE).unwrap();
        crate::config::catalog::set_test_dir(Some(dir.join("widgets")));
        crate::config::approvals::set_test_path(Some(dir.join("approvals.kdl")));
        let file = dir.join("abyss.kdl");
        std::fs::write(&file, text).unwrap();
        let mut h = crate::shell::focus::state_tests::harness();
        h.state.addons.hooks.insert(crate::addons::Hook::TaskbarWidgets);
        h.state.config = crate::config::Config::load_with(Some(&file), true);
        withhold::reapply(&mut h.state);
        (h, file)
    }

    fn done(file: &std::path::Path) {
        crate::config::catalog::set_test_dir(None);
        crate::config::approvals::set_test_path(None);
        let _ = std::fs::remove_dir_all(file.parent().unwrap());
    }

    fn live(h: &crate::shell::focus::state_tests::Harness) -> Vec<String> {
        h.state
            .config
            .bar
            .custom_widgets
            .iter()
            .map(|w| w.name.clone())
            .collect()
    }

    fn keys(h: &mut crate::shell::focus::state_tests::Harness, syms: &[smithay::input::keyboard::Keysym]) {
        super::super::arm_now(&mut h.state);
        for s in syms {
            super::super::key(&mut h.state, *s);
        }
    }

    use smithay::input::keyboard::Keysym as K;

    const NEW: &str =
        "bar {\n    widgets { order \"custom:x\" \"clock\"; }\n    widget \"x\" { exec \"true\"; }\n}\n";

    #[test]
    fn not_now_then_review_then_allow_runs_it() {
        let (mut h, file) = harness("allow", NEW);
        // The catalog's own `load` is live; `x` is withheld.
        assert_eq!(live(&h), ["load"]);
        pump(&mut h.state);
        assert!(super::super::holds_seat(&h.state));

        // Enter on the default focus is Not now; it declines for the session.
        keys(&mut h, &[K::Return]);
        assert!(!h.state.trusted_ui.is_open());
        assert_eq!(h.state.widget_approvals.declined.len(), 1);
        pump(&mut h.state);
        assert!(!h.state.trusted_ui.is_open(), "a declined widget came back");

        // Not now buys a quiet spell: Review re-queues, but nothing comes up.
        assert_eq!(withhold::review(&mut h.state, "x"), Ok(true));
        pump(&mut h.state);
        assert!(!h.state.trusted_ui.is_open(), "a prompt inside the quiet spell");

        // After it, Enter on Allow does nothing and Space activates it.
        h.state.trusted_ui.asking.quiet_until = None;
        pump(&mut h.state);
        keys(&mut h, &[K::Right, K::Return]);
        assert!(h.state.trusted_ui.is_open(), "Enter granted");
        keys(&mut h, &[K::space]);
        assert!(!h.state.trusted_ui.is_open());
        assert_eq!(live(&h), ["load", "x"]);
        assert!(h.state.widget_approvals.withheld.is_empty());
        // Stored off the loop, so the next session starts with it approved.
        crate::config::approvals::wait_written();
        let x = h
            .state
            .config
            .bar
            .custom_widgets
            .iter()
            .find(|w| w.name == "x")
            .unwrap();
        assert_eq!(
            crate::config::approvals::read().get("x"),
            crate::config::widget_hash::hash(x)
        );
        done(&file);
    }

    #[test]
    fn a_fresh_prompt_ignores_all_but_escape() {
        let (mut h, file) = harness("arm", NEW);
        pump(&mut h.state);
        // `<Tab><space>` typed the instant it appears grants nothing.
        super::super::key(&mut h.state, K::Tab);
        super::super::key(&mut h.state, K::space);
        assert!(h.state.trusted_ui.is_open());
        super::super::key(&mut h.state, K::Escape);
        assert!(!h.state.trusted_ui.is_open(), "Escape counts at once");
        done(&file);
    }

    #[test]
    fn a_name_cannot_forge_a_command_line() {
        let mut w = pending(PendingKind::New);
        w.name = format!("x\n/usr/bin/uptime\n\n\n{}", "n".repeat(200));
        let m = modal_for(1, &w).unwrap();
        let u = m.untrusted();
        assert!(u[0].starts_with("widget: x /usr/bin/uptime"), "{u:?}");
        assert!(u[0].chars().count() <= "widget: ".len() + NAME_MAX);
        assert_eq!(u[1], "uptime");
        assert!(m.buttons().iter().any(|b| b.role == Role::Grant));
    }

    #[test]
    fn a_command_too_long_to_show_cannot_be_allowed() {
        let mut w = pending(PendingKind::New);
        w.command_text = format!("sh -c 'uptime{}; evil'", " ".repeat(460));
        let m = modal_for(1, &w).unwrap();
        assert!(m.is_cut());
        assert!(m.buttons().iter().all(|b| b.role != Role::Grant));
        assert_eq!(m.buttons()[m.safe()].label, "Not now");
    }

    #[test]
    fn remove_drops_a_new_widget() {
        let (mut h, file) = harness("remove", NEW);
        pump(&mut h.state);
        keys(&mut h, &[K::Left, K::space]);
        assert!(!h.state.trusted_ui.is_open());
        assert!(!std::fs::read_to_string(&file).unwrap().contains("widget \"x\""));
        assert!(h.state.widget_approvals.withheld.is_empty());
        pump(&mut h.state);
        assert!(!h.state.trusted_ui.is_open());
        done(&file);
    }

    #[test]
    fn revert_restores_the_premade() {
        let (mut h, file) = harness(
            "revert",
            "bar {\n    widgets { order \"custom:load\"; }\n    widget \"load\" { exec \"sh\" \"-c\" \"evil\"; }\n}\n",
        );
        assert!(live(&h).is_empty());
        pump(&mut h.state);
        keys(&mut h, &[K::Left, K::space]);
        assert_eq!(live(&h), ["load"]);
        assert!(!std::fs::read_to_string(&file).unwrap().contains("evil"));
        done(&file);
    }

    #[test]
    fn a_prompt_is_withdrawn_when_its_widget_changes_or_the_hook_goes() {
        let (mut h, file) = harness("withdraw", NEW);
        pump(&mut h.state);
        let first = h.state.trusted_ui.token();
        // Edited while on screen: the old question is withdrawn, the new one asked.
        std::fs::write(&file, NEW.replace("\"true\"", "\"false\"")).unwrap();
        withhold::reapply(&mut h.state);
        pump(&mut h.state);
        assert!(h.state.trusted_ui.is_open());
        assert_ne!(h.state.trusted_ui.token(), first);
        assert!(
            h.state.widget_approvals.declined.is_empty(),
            "withdrawal is not an answer"
        );

        // Hook off: nothing withheld, the prompt goes.
        h.state.addons.hooks = crate::addons::HookSet::default();
        withhold::reapply(&mut h.state);
        pump(&mut h.state);
        assert!(!h.state.trusted_ui.is_open());
        assert!(h.state.widget_approvals.on_screen.is_none());
        done(&file);
    }

    const OE: &str = "oracle-eyes {\n    model-command \"sh\" \"-c\" \"other\"\n}\n";

    fn oe_harness(tag: &str) -> (crate::shell::focus::state_tests::Harness, std::path::PathBuf) {
        let (mut h, file) = harness(tag, OE);
        h.state.addons.hooks.insert(crate::addons::Hook::Annotations);
        withhold::reapply(&mut h.state);
        assert_eq!(h.state.config.oracle_eyes.model_command, None, "withheld");
        (h, file)
    }

    #[test]
    fn the_model_command_asks_revert_or_allow() {
        let p = PendingModelCommand {
            command_text: "sh -c other".into(),
            hash: WidgetHash([0; 32]),
        };
        let m = model_modal_for(3, &p).unwrap();
        let labels: Vec<_> = m.buttons().iter().map(|b| (b.label, b.role)).collect();
        assert_eq!(
            labels,
            [
                ("Revert", Role::Other),
                ("Not now", Role::Safe),
                ("Allow", Role::Grant)
            ]
        );
        assert_eq!(m.untrusted().join("\n"), "sh -c other");
    }

    #[test]
    fn allowing_the_model_command_makes_it_live_and_stores_it() {
        let (mut h, file) = oe_harness("oe-allow");
        pump(&mut h.state);
        assert!(h.state.trusted_ui.is_open());
        keys(&mut h, &[K::Right, K::space]);
        assert!(!h.state.trusted_ui.is_open());
        assert_eq!(
            h.state.config.oracle_eyes.model_command.as_deref(),
            Some(&["sh".to_owned(), "-c".to_owned(), "other".to_owned()][..])
        );
        crate::config::approvals::wait_written();
        assert!(crate::config::approvals::read()
            .get(crate::config::approvals::MODEL_COMMAND_KEY)
            .is_some());
        done(&file);
    }

    #[test]
    fn reverting_the_model_command_writes_the_default() {
        let (mut h, file) = oe_harness("oe-revert");
        pump(&mut h.state);
        keys(&mut h, &[K::Left, K::space]);
        assert!(!h.state.trusted_ui.is_open());
        assert!(!std::fs::read_to_string(&file).unwrap().contains("other"));
        assert_eq!(
            h.state.config.oracle_eyes.model_command.as_deref(),
            Some(&["claude".to_owned()][..])
        );
        done(&file);
    }

    #[test]
    fn not_now_keeps_the_model_command_withheld() {
        let (mut h, file) = oe_harness("oe-notnow");
        pump(&mut h.state);
        keys(&mut h, &[K::Return]);
        assert!(!h.state.trusted_ui.is_open());
        assert_eq!(h.state.config.oracle_eyes.model_command, None);
        assert_eq!(h.state.widget_approvals.model_command.declined.len(), 1);
        h.state.trusted_ui.asking.quiet_until = None;
        pump(&mut h.state);
        assert!(!h.state.trusted_ui.is_open(), "a declined command came back");
        done(&file);
    }
}
