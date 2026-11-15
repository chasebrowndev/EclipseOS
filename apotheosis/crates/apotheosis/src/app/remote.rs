// SPDX-License-Identifier: AGPL-3.0-only
//! Remote domains in the window (T-22 §3, §5; T-21 §3): the askpass sheet, the
//! reconnecting notice of a mux domain, the "connection lost" notice of an ssh domain's
//! pane (`apoth ssh --hold` sets the user variable `apoth_ssh_lost`; Reconnect types Enter
//! there, which starts a fresh ssh in the same pane), and the `session.detach` /
//! `session.kill` / `pane.reconnect` actions.
//!
//! The askpass sheet shows the armed host as interface text, the remote's prompt as
//! sanitised program text and, for a host-key question, the whole prompt (with the
//! fingerprint) in the monospace body. The secret is typed into a protected field: the
//! window shows one dot per character, the accessibility tree gets nothing, and the bytes
//! live in a [`Secret`] that is wiped when dropped. Answers come only from real seat input
//! on the sheet (T-12 §1: sheet answers are not Actions). Nothing here logs the prompt or
//! the secret.

use std::collections::{BTreeMap, BTreeSet, VecDeque};

use apoth_input::backend::KeyLookup;
use apoth_input::{Key, Mods, Named};
use apoth_proto::askpass::Kind;
use apoth_ui::bar::{BarKind, BarSpec, ButtonSpec, Key as BarKey};
use apoth_ui::text::{TextRun, sanitise_program_text};
use apoth_ui::tokens::size;

use super::App;
use crate::askpass::{Answer, Ask, Event, MAX_ARMED, Secret};
use crate::pane::PaneKey;

/// The keycap of the askpass submit button: Return answers it (the field has the letters).
const RETURN_CAP: char = '\u{23ce}';
/// Dots shown at most (the count of typed characters past this is not drawn).
const MAX_DOTS: usize = 48;

/// The askpass sheet up now.
pub(super) struct AskSheet {
    ask: Ask,
    field: Secret,
    /// A host-key question: answered yes/no by buttons, the prompt in the body.
    hostkey: bool,
}

/// What the window keeps for remote domains.
#[derive(Default)]
pub(super) struct Remote {
    pub(super) sheet: Option<AskSheet>,
    /// Further requests (one per armed channel at most), shown in arrival order.
    queue: VecDeque<Ask>,
    /// `session.kill` asked: the confirm bar is up.
    pub(super) kill_confirm: bool,
    /// The mux domain's link is down and reconnecting.
    pub(super) link_down: bool,
    /// The user dismissed the reconnecting notice for this drop.
    notice_dismissed: bool,
    /// Panes whose `apoth ssh --hold` lost its link: the host it named (program text).
    lost: BTreeMap<PaneKey, String>,
    /// Lost panes whose notice the user dismissed.
    lost_dismissed: BTreeSet<PaneKey>,
}

impl Remote {
    /// A question owns the keyboard.
    pub(super) fn asking(&self) -> bool {
        self.sheet.is_some() || self.kill_confirm
    }

    /// An askpass request for `pane` is on the sheet or queued for it.
    pub(super) fn has_ask_for(&self, pane: PaneKey) -> bool {
        self.sheet
            .as_ref()
            .is_some_and(|s| s.ask.pane == Some(pane))
            || self.queue.iter().any(|a| a.pane == Some(pane))
    }
}

/// OpenSSH's unknown-host question (asked through askpass as an echoed prompt).
pub(super) fn is_hostkey_prompt(p: &str) -> bool {
    p.contains("fingerprint") && p.contains("(yes/no")
}

fn button(label: &str, key: BarKey) -> ButtonSpec {
    ButtonSpec {
        label: label.into(),
        key,
        primary: false,
    }
}

/// The askpass sheet's spec (T-22 §3).
fn ask_spec(id: apoth_ui::WidgetId, s: &AskSheet) -> BarSpec {
    // The host is the arming `apoth ssh`'s destination: interface text, still stripped of
    // controls and bidi overrides (it was typed by the user, not the remote).
    let host = sanitise_program_text(super::clip(&s.ask.host, 255));
    let prompt = super::clip(&s.ask.prompt, 1024);
    if s.hostkey {
        return BarSpec {
            id,
            kind: BarKind::Consent,
            headline: TextRun::interface(format!("Trust the key of {host}?"), size::BODY).strong(),
            detail: Some(TextRun::interface(
                "This host is not known yet. Compare the fingerprint with one you trust.",
                size::BODY_SMALL,
            )),
            buttons: vec![
                button("Trust and connect", BarKey::Char('t')),
                button("Cancel", BarKey::Escape),
            ],
            escape: Some(1),
            body: Some(TextRun::program(prompt, size::BODY_SMALL)),
        };
    }
    match s.ask.kind {
        Kind::Secret => {
            let n = s.field.chars();
            let field = if n == 0 {
                String::new()
            } else {
                "\u{2022}".repeat(n.min(MAX_DOTS))
            };
            BarSpec {
                id,
                kind: BarKind::Consent,
                headline: TextRun::interface(format!("Sign in to {host}"), size::BODY).strong(),
                detail: Some(TextRun::program(prompt, size::BODY_SMALL)),
                buttons: vec![
                    button("Submit", BarKey::Char(RETURN_CAP)),
                    button("Cancel", BarKey::Escape),
                ],
                escape: Some(1),
                body: Some(TextRun::interface(field, size::BODY)),
            }
        }
        Kind::Confirm => BarSpec {
            id,
            kind: BarKind::Consent,
            headline: TextRun::interface(format!("Allow this for {host}?"), size::BODY).strong(),
            detail: Some(TextRun::program(prompt, size::BODY_SMALL)),
            buttons: vec![
                button("Allow", BarKey::Char('a')),
                button("Deny", BarKey::Escape),
            ],
            escape: Some(1),
            body: None,
        },
        Kind::None => BarSpec {
            id,
            kind: BarKind::Notice,
            headline: TextRun::interface(format!("Message from {host}"), size::BODY).strong(),
            detail: Some(TextRun::program(prompt, size::BODY_SMALL)),
            buttons: vec![button("Dismiss", BarKey::Escape)],
            escape: Some(0),
            body: None,
        },
    }
}

/// The `session.kill` confirm bar.
fn kill_spec(id: apoth_ui::WidgetId, session: &str, remote: bool) -> BarSpec {
    let s = sanitise_program_text(super::clip(session, 64));
    BarSpec {
        id,
        kind: BarKind::Consent,
        headline: TextRun::interface(format!("Kill session \u{201c}{s}\u{201d}?"), size::BODY)
            .strong(),
        detail: Some(TextRun::interface(
            if remote {
                "Every pane of the session on the host closes and its programs end."
            } else {
                "Every pane of the session closes and its programs end."
            },
            size::BODY_SMALL,
        )),
        buttons: vec![
            button("Kill", BarKey::Char('k')),
            button("Cancel", BarKey::Char('c')),
        ],
        escape: Some(1),
        body: None,
    }
}

/// The user variable `apoth ssh --hold` sets while its link is down.
pub(super) const LOST_VAR: &str = "apoth_ssh_lost";

/// The "connection lost" notice of an ssh domain pane (T-22 §4).
fn lost_spec(id: apoth_ui::WidgetId, host: &str) -> BarSpec {
    BarSpec {
        id,
        kind: BarKind::Notice,
        headline: TextRun::interface("Connection lost", size::BODY).strong(),
        detail: Some(TextRun::program(
            &format!(
                "{} \u{2014} the remote shell has ended.",
                super::clip(host, 255)
            ),
            size::BODY_SMALL,
        )),
        buttons: vec![
            button("Reconnect", BarKey::Char('r')),
            button("Dismiss", BarKey::Escape),
        ],
        escape: Some(1),
        body: None,
    }
}

/// The reconnecting notice of a mux domain.
fn reconnect_spec(id: apoth_ui::WidgetId, host: &str) -> BarSpec {
    BarSpec {
        id,
        kind: BarKind::Notice,
        headline: TextRun::interface(
            format!("Reconnecting to {}\u{2026}", sanitise_program_text(host)),
            size::BODY,
        )
        .strong(),
        detail: Some(TextRun::interface(
            "The panes keep running there. Typing waits until the link is back.",
            size::BODY_SMALL,
        )),
        buttons: vec![button("Dismiss", BarKey::Escape)],
        escape: Some(0),
        body: None,
    }
}

impl App {
    /// The bar the remote state wants, if any (askpass first: ssh is waiting on it).
    pub(super) fn remote_bar(&self) -> Option<BarSpec> {
        let id = self.windows()[0];
        if let Some(s) = &self.remote_ui.sheet {
            return Some(ask_spec(id, s));
        }
        if self.remote_ui.kill_confirm
            && let Some(l) = &self.remote
        {
            return Some(kill_spec(id, &l.session, l.host.is_some()));
        }
        None
    }

    /// The reconnecting notice, below every question and above the config bars.
    pub(super) fn reconnect_bar(&self) -> Option<BarSpec> {
        if let Some(host) = self.remote.as_ref().and_then(|l| l.host.as_deref())
            && self.remote_ui.link_down
            && !self.remote_ui.notice_dismissed
        {
            return Some(reconnect_spec(self.windows()[0], host));
        }
        let (_, host) = self.lost_pane()?;
        Some(lost_spec(self.windows()[0], host))
    }

    /// The lost pane the notice is about: the focused one, else the first.
    fn lost_pane(&self) -> Option<(PaneKey, &str)> {
        let shown = |id: &PaneKey| {
            self.panes.contains_key(id) && !self.remote_ui.lost_dismissed.contains(id)
        };
        let f = self.focused();
        f.and_then(|f| self.remote_ui.lost.get_key_value(&f))
            .filter(|(id, _)| shown(id))
            .or_else(|| self.remote_ui.lost.iter().find(|(id, _)| shown(id)))
            .map(|(id, h)| (*id, h.as_str()))
    }

    /// `apoth_ssh_lost` changed in pane `id` (set: the link dropped; cleared: reconnecting).
    pub(super) fn ssh_lost(&mut self, id: PaneKey, host: Option<&str>) {
        match host {
            Some(h) => {
                tracing::info!(pane = id, "ssh connection lost");
                self.remote_ui.lost.insert(id, h.to_owned());
                self.remote_ui.lost_dismissed.remove(&id);
            }
            None => {
                self.remote_ui.lost.remove(&id);
                self.remote_ui.lost_dismissed.remove(&id);
            }
        }
        self.refresh_bar();
    }

    /// `pane.reconnect` / the notice's Reconnect: Enter on the lost pane's "press Enter"
    /// line starts a fresh ssh there. False when no pane lost its link.
    pub(super) fn reconnect_pane(&mut self, id: Option<PaneKey>) -> bool {
        let id = match id.filter(|i| self.remote_ui.lost.contains_key(i)) {
            Some(i) => i,
            None => match self.lost_pane() {
                Some((i, _)) => i,
                None => return false,
            },
        };
        tracing::info!(pane = id, "ssh reconnect");
        self.remote_ui.lost.remove(&id);
        self.send_unit(id, b"\r".to_vec());
        self.refresh_bar();
        true
    }

    /// The askpass broker's events.
    pub(super) fn on_askpass(&mut self, e: Event) {
        match e {
            Event::Ask(a) => {
                // On Abyss the secret prompt answers it instead of the sheet (T-22 §3).
                let Err(a) = self.route_ask(a) else {
                    return;
                };
                tracing::info!(request = a.id, "askpass sheet requested");
                if self.remote_ui.sheet.is_none() && self.started {
                    self.show_ask(a);
                } else if self.remote_ui.queue.len() < MAX_ARMED {
                    self.remote_ui.queue.push_back(a);
                } else {
                    let _ = a.answer.send(Answer::Cancel);
                }
            }
            Event::Withdrawn(id) => {
                if self.withdraw_routed(id) {
                    return;
                }
                self.remote_ui.queue.retain(|a| a.id != id);
                if self
                    .remote_ui
                    .sheet
                    .as_ref()
                    .is_some_and(|s| s.ask.id == id)
                {
                    self.remote_ui.sheet = None;
                    self.next_ask();
                }
            }
        }
    }

    /// The window exists: show a request that came before it.
    pub(super) fn remote_started(&mut self) {
        if self.remote_ui.sheet.is_none() && !self.remote_ui.queue.is_empty() {
            self.next_ask();
        }
    }

    fn show_ask(&mut self, ask: Ask) {
        let hostkey = ask.kind == Kind::Secret && is_hostkey_prompt(&ask.prompt);
        self.remote_ui.sheet = Some(AskSheet {
            ask,
            field: Secret::new(),
            hostkey,
        });
        self.refresh_bar();
    }

    fn next_ask(&mut self) {
        match self.remote_ui.queue.pop_front() {
            Some(a) => self.show_ask(a),
            None => self.refresh_bar(),
        }
    }

    /// Send the sheet's answer and drop it (the field is wiped with it).
    fn answer_ask(&mut self, submit: bool) {
        let Some(s) = self.remote_ui.sheet.take() else {
            return;
        };
        let AskSheet {
            ask,
            field,
            hostkey,
        } = s;
        let answer = match (submit, hostkey, ask.kind) {
            (false, ..) => Answer::Cancel,
            (true, true, _) => {
                let mut yes = Secret::new();
                yes.push_str("yes");
                Answer::Secret(yes)
            }
            (true, false, Kind::Secret) => Answer::Secret(field),
            (true, false, Kind::Confirm) => Answer::Secret(Secret::new()),
            (true, false, Kind::None) => Answer::Cancel,
        };
        tracing::info!(
            request = ask.id,
            submitted = submit,
            "askpass sheet answered"
        );
        let _ = ask.answer.send(answer);
        self.next_ask();
    }

    fn guard_over(&self) -> bool {
        self.bar.as_ref().is_some_and(|o| {
            o.shown_at.elapsed().as_millis()
                >= u128::from(apoth_ui::tokens::motion::CONSENT_GUARD_MS)
        })
    }

    /// A key while a remote question is up (the question owns the keyboard). Repeats edit
    /// the field; answers take a fresh press after the input guard.
    pub(super) fn remote_key(&mut self, look: &KeyLookup, repeat: bool) {
        if self.remote_ui.sheet.is_none() {
            if !repeat && self.remote_ui.kill_confirm {
                self.kill_key(look.key);
            }
            return;
        }
        let typing = self
            .remote_ui
            .sheet
            .as_ref()
            .is_some_and(|s| !s.hostkey && s.ask.kind == Kind::Secret);
        let ctrl = self.held.contains(Mods::CTRL);
        match look.key {
            Key::Named(Named::Escape) if !repeat => {
                if self.guard_over() {
                    self.answer_ask(false);
                }
                return;
            }
            Key::Named(Named::Enter | Named::Kp(apoth_input::Kp::Enter)) if typing => {
                if !repeat && self.guard_over() {
                    self.answer_ask(true);
                }
                return;
            }
            _ => {}
        }
        if typing {
            let Some(s) = self.remote_ui.sheet.as_mut() else {
                return;
            };
            let changed = match look.key {
                Key::Named(Named::Backspace) => {
                    s.field.pop();
                    true
                }
                Key::Char('u') if ctrl => {
                    s.field = Secret::new();
                    true
                }
                _ if ctrl || self.held.contains(Mods::ALT) || self.held.contains(Mods::SUPER) => {
                    false
                }
                _ => {
                    let t = look.text.as_str();
                    !t.is_empty() && !t.chars().any(char::is_control) && s.field.push_str(t)
                }
            };
            if changed {
                self.refresh_bar_quiet();
            }
            return;
        }
        // Buttons by their keys (host-key trust, confirm allow).
        if repeat || !self.guard_over() {
            return;
        }
        if let Key::Char(c) = look.key
            && let Some(o) = &self.bar
            && let Some(i) = o.spec.buttons.iter().position(|b| b.key == BarKey::Char(c))
        {
            self.remote_click(i);
        }
    }

    /// A button of a remote question was clicked (guard checked by the caller).
    pub(super) fn remote_click(&mut self, i: usize) {
        if self.remote_ui.sheet.is_some() {
            let last = self
                .bar
                .as_ref()
                .map_or(0, |o| o.spec.buttons.len().saturating_sub(1));
            // The last button is always Cancel / Deny / Dismiss.
            self.answer_ask(i < last);
        } else if self.remote_ui.kill_confirm {
            self.answer_kill(i == 0);
        }
    }

    /// The field changed: redraw the sheet without restarting its guard or announcing it.
    fn refresh_bar_quiet(&mut self) {
        let spec = self.remote_bar();
        if let (Some(o), Some(spec)) = (self.bar.as_mut(), spec) {
            o.spec = spec;
        }
        self.send_overlay();
    }

    /// A button of the reconnecting or connection-lost notice (`None`: Escape). False when
    /// neither is the bar shown.
    pub(super) fn remote_notice_click(&mut self, i: Option<usize>) -> bool {
        let Some(b) = self.reconnect_bar() else {
            return false;
        };
        if self
            .bar
            .as_ref()
            .is_none_or(|o| o.spec.headline.text != b.headline.text)
        {
            return false;
        }
        let link = self.remote.as_ref().is_some_and(|l| l.host.is_some())
            && self.remote_ui.link_down
            && !self.remote_ui.notice_dismissed;
        if link {
            self.remote_ui.notice_dismissed = true;
        } else if i == Some(0) {
            let focus = self.focused();
            self.reconnect_pane(focus);
            return true;
        } else if let Some((id, _)) = self.lost_pane() {
            self.remote_ui.lost_dismissed.insert(id);
        }
        self.refresh_bar();
        true
    }

    fn kill_key(&mut self, key: Key) {
        let Some(o) = &self.bar else { return };
        let age = o.shown_at.elapsed().as_millis() as u64;
        if let Some(i) = super::paste_answer(&o.spec, age, key) {
            self.answer_kill(i == 0);
        }
    }

    fn answer_kill(&mut self, kill: bool) {
        self.remote_ui.kill_confirm = false;
        self.refresh_bar();
        if kill && let Some(l) = self.remote.as_mut() {
            tracing::info!(session = %l.session, "session kill confirmed");
            l.kill_session();
        }
    }

    /// The link of a mux domain dropped (`true`) or is back (`false`).
    pub(super) fn link_state(&mut self, down: bool) {
        self.remote_ui.link_down = down;
        if down {
            self.remote_ui.notice_dismissed = false;
        }
        self.refresh_bar();
    }

    /// `session.detach`: leave the session daemon, the panes keep running (tmux `d`).
    /// Without a daemon there is nothing to detach from (a no-op, logged).
    pub(super) fn session_detach(&mut self) {
        if self.remote.is_none() {
            tracing::info!("session.detach: this window is not a session view");
            return;
        }
        self.begin_close();
    }

    /// `session.kill`: confirm, then close every pane of the session (tmux `kill-session`).
    pub(super) fn session_kill(&mut self) {
        if self.remote.is_none() {
            tracing::info!("session.kill: this window is not a session view");
            return;
        }
        self.remote_ui.kill_confirm = true;
        self.refresh_bar();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sheet(kind: Kind, prompt: &str) -> AskSheet {
        let (tx, _rx) = std::sync::mpsc::channel();
        AskSheet {
            hostkey: kind == Kind::Secret && is_hostkey_prompt(prompt),
            ask: Ask {
                id: 1,
                host: "u@h\u{202e}x".into(),
                kind,
                prompt: prompt.into(),
                answer: tx,
                pane: None,
                pid: 1,
                armed: 1,
            },
            field: Secret::new(),
        }
    }

    #[test]
    fn the_secret_is_never_in_the_spec() {
        let mut s = sheet(Kind::Secret, "u@h's password: \u{1b}[31m");
        assert!(s.field.push_str("hunter2"));
        let b = ask_spec(apoth_ui::WidgetId(1), &s);
        let all = format!(
            "{}{}{}",
            b.headline.text,
            b.detail.as_ref().unwrap().text,
            b.body.as_ref().unwrap().text
        );
        assert!(!all.contains("hunter2"));
        assert_eq!(b.body.unwrap().text, "\u{2022}".repeat(7));
        // The host is stripped of bidi overrides, the prompt of escapes.
        assert!(!all.contains('\u{202e}') && !all.contains('\u{1b}'));
        assert!(b.headline.text.contains("u@h"));
    }

    #[test]
    fn host_key_questions_show_the_prompt_in_the_body() {
        let p = "The authenticity of host 'h' can't be established.\nED25519 key fingerprint \
                 is SHA256:abc.\nAre you sure you want to continue connecting \
                 (yes/no/[fingerprint])? ";
        let s = sheet(Kind::Secret, p);
        assert!(s.hostkey);
        let b = ask_spec(apoth_ui::WidgetId(1), &s);
        assert!(b.body.unwrap().text.contains("SHA256:abc"));
        assert_eq!(b.buttons.len(), 2);
        assert!(!sheet(Kind::Secret, "Password: ").hostkey);
        assert!(!sheet(Kind::Confirm, p).hostkey);
    }

    #[test]
    fn kill_and_reconnect_bars() {
        let k = kill_spec(apoth_ui::WidgetId(1), "work", true);
        assert!(k.headline.text.contains("work"));
        assert_eq!(k.escape, Some(1));
        let r = reconnect_spec(apoth_ui::WidgetId(1), "h");
        assert_eq!(r.kind, BarKind::Notice);
        let l = lost_spec(apoth_ui::WidgetId(1), "me@h\u{1b}[2J");
        assert_eq!(l.kind, BarKind::Notice);
        assert_eq!(l.buttons[0].label, "Reconnect");
        assert!(!l.detail.unwrap().text.contains('\u{1b}'));
    }
}
