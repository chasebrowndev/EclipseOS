// SPDX-License-Identifier: AGPL-3.0-only
//! Password prompts through `ec-secret-prompt` on Abyss (T-22 §3, T-17 §5; ADR 0053
//! amendment of 2026-11-15, ADR 0115).
//!
//! Two ways in:
//! - **askpass routing.** An accepted askpass request (`apoth ssh`, `sudo -A` with
//!   `security.sudo-askpass`) of kind `secret`, other than a host-key question, goes to the
//!   prompt instead of the in-window sheet. Its answer goes straight back to the waiting
//!   askpass client from the prompt's thread.
//! - **auto-open** (`security.password-prompt auto`). The focused pane's PTY enters the
//!   credential state (ECHO off, ICANON on) while a job the user ran is in the foreground.
//!   The answer is typed into the pane followed by `\r`. It opens once per read: the state
//!   has to go false before it can open again. It never opens for an unfocused window or
//!   pane, a background job or a session-daemon pane, and never while an askpass request
//!   for the pane is pending. Askpass wins: a request arriving for a pane with an
//!   auto-opened prompt closes that prompt first.
//!
//! Keys typed between the decision and the prompt taking the keyboard are held, not sent
//! to the PTY. They are typed ahead of the answer (the line discipline applies erase and
//! kill to them as if typed), or sent alone on a cancel. An Enter among them answers the
//! read from the terminal, as typing would have, and the prompt is closed. The hold ends
//! when the keyboard leaves the window, or after [`HOLD_FOR`] without that (the prompt did
//! not take focus), when the held keys go to the PTY.
//!
//! Nothing here logs a key, a prompt, an argv or a secret.

use std::path::PathBuf;
use std::time::{Duration, Instant};

use apoth_proto::askpass::Kind;
use apoth_proto::secret_prompt::Via;

use super::App;
use crate::askpass::{Answer, Ask, Prepared, Secret};
use crate::pane::{MainMsg, PaneKey};
use crate::secret_prompt::{self as sp, Running};

/// Held keys go to the PTY if the prompt has not taken the keyboard by then.
const HOLD_FOR: Duration = Duration::from_secs(3);
/// The askpass sheet's host label for a pane armed by `security.sudo-askpass`.
const PANE_LABEL: &str = "this terminal";

/// Keys held while an auto-opened prompt maps.
struct Hold {
    keys: Secret,
    since: Instant,
    /// The keyboard left the window (the prompt took it): nothing more is held.
    over: bool,
}

struct Open {
    run: Running,
    pane: Option<PaneKey>,
    /// The askpass request it answers (`None`: auto-opened for the pane).
    ask: Option<u64>,
    hold: Option<Hold>,
}

/// What the window keeps for password prompts.
#[derive(Default)]
pub(super) struct Passwords {
    open: Vec<Open>,
    /// Panes whose PTY is in the credential state now, and whether this read already
    /// opened (or declined to open) a prompt.
    cred: std::collections::BTreeMap<PaneKey, bool>,
}

/// Everything the auto-open decision looks at (pure, so it is tested alone).
#[derive(Clone, Copy, Debug, Default)]
pub(super) struct Gate {
    /// `security.password-prompt auto`.
    pub(super) auto: bool,
    /// On Abyss with `ec-secret-prompt` installed.
    pub(super) available: bool,
    /// A local PTY (not a session-daemon pane).
    pub(super) local: bool,
    /// The pane is the window's focused pane.
    pub(super) pane_focused: bool,
    /// The window has the keyboard.
    pub(super) window_focused: bool,
    /// An askpass request for this pane is pending (askpass wins).
    pub(super) askpass_pending: bool,
    /// A prompt is already open for this pane.
    pub(super) open: bool,
    /// An OSC 133 command is running, or the foreground group is not the pane's shell.
    pub(super) user_job: bool,
}

impl Gate {
    pub(super) fn opens(self) -> bool {
        self.auto
            && self.available
            && self.local
            && self.pane_focused
            && self.window_focused
            && !self.askpass_pending
            && !self.open
            && self.user_job
    }
}

/// One edge of the credential state for a pane. True on a rising edge that has not fired:
/// the caller decides once per read.
pub(super) fn rising(
    cred: &mut std::collections::BTreeMap<PaneKey, bool>,
    id: PaneKey,
    on: bool,
) -> bool {
    if on {
        let fired = cred.entry(id).or_insert(false);
        !std::mem::replace(fired, true)
    } else {
        cred.remove(&id);
        false
    }
}

/// `apoth` by absolute path (sudo runs `SUDO_ASKPASS` with `execv`).
fn apoth_absolute() -> Option<PathBuf> {
    let p = crate::attach::apoth_exe()?;
    if p.is_absolute() {
        return Some(p);
    }
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .map(|d| d.join(&p))
        .find(|c| c.is_file())
}

impl App {
    /// `security.sudo-askpass`: a prepared channel for pane `id` and the helper path, or
    /// `None` (off, no socket directory, or too many armed).
    pub(super) fn arm_pane(&self, id: PaneKey) -> Option<(Prepared, PathBuf)> {
        if !self.settings.sudo_askpass {
            return None;
        }
        let dir = self.ctl_path()?.parent()?;
        let exe = apoth_absolute()?;
        match Prepared::new(dir, PANE_LABEL) {
            Ok(p) => Some((p.in_pane(Some(id)), exe)),
            Err(e) => {
                tracing::warn!(pane = id, error = %e, "askpass not armed for the pane");
                None
            }
        }
    }

    /// The prompt's binary when it may be used: on Abyss and installed.
    fn prompt_program(&self) -> Option<PathBuf> {
        let on_abyss = self.abyss_theme.is_some();
        #[cfg(debug_assertions)]
        let on_abyss = on_abyss || std::env::var_os("APOTH_SECRET_PROMPT").is_some();
        if !on_abyss {
            return None;
        }
        sp::program()
    }

    /// What main knows for the request: titles, the pane and its foreground group.
    fn prompt_request(
        &self,
        via: Via,
        pane: Option<PaneKey>,
        pid: i32,
        root: i32,
        prompt: String,
        host: Option<String>,
    ) -> sp::Request {
        let p = pane.and_then(|id| self.panes.get(&id));
        let tab = pane
            .and_then(|id| self.session.tab_of(id))
            .map(|t| self.tab_title(t))
            .unwrap_or_default();
        sp::Request {
            via,
            window: self.window_title_now(),
            tab,
            pane: p.map(|p| p.title.clone()).unwrap_or_default(),
            pane_id: pane,
            fg_pgrp: p
                .and_then(|p| p.pty.as_ref())
                .and_then(|t| t.foreground_pgid().ok()),
            pid,
            root,
            prompt,
            host,
        }
    }

    /// An askpass request is pending for `pane`: on the sheet, queued, or with a prompt.
    fn askpass_pending(&self, pane: PaneKey) -> bool {
        self.remote_ui.has_ask_for(pane)
            || self
                .passwords
                .open
                .iter()
                .any(|o| o.ask.is_some() && o.pane == Some(pane))
    }

    /// Route an accepted askpass request to `ec-secret-prompt`, or hand it back for the
    /// sheet (off Abyss, not installed, a kind the sheet answers, or the spawn failed).
    pub(super) fn route_ask(&mut self, a: Ask) -> Result<(), Ask> {
        if a.kind != Kind::Secret || super::remote::is_hostkey_prompt(&a.prompt) {
            return Err(a);
        }
        let Some(program) = self.prompt_program() else {
            return Err(a);
        };
        // Askpass wins over an auto-opened prompt for the same pane.
        if let Some(pane) = a.pane {
            self.close_pane_prompt(pane);
        }
        let host = (a.host != PANE_LABEL).then(|| a.host.clone());
        let r = self.prompt_request(Via::Askpass, a.pane, a.pid, a.armed, a.prompt.clone(), host);
        let answer = a.answer.clone();
        let tx = self.to_main.clone();
        match sp::spawn(&program, r, move |id, ans| {
            let _ = answer.send(ans);
            let _ = tx.send(MainMsg::Prompt(id, None));
        }) {
            Ok(run) => {
                tracing::info!(
                    request = a.id,
                    prompt = run.id,
                    "askpass routed to the secret prompt"
                );
                self.passwords.open.push(Open {
                    run,
                    pane: a.pane,
                    ask: Some(a.id),
                    hold: None,
                });
                Ok(())
            }
            Err(e) => {
                tracing::warn!(error = %e, "secret prompt did not start; using the sheet");
                Err(a)
            }
        }
    }

    /// The askpass request `id` went away: its prompt goes too. True when it had one.
    pub(super) fn withdraw_routed(&mut self, id: u64) -> bool {
        match self.passwords.open.iter().position(|o| o.ask == Some(id)) {
            Some(i) => {
                let o = self.passwords.open.swap_remove(i);
                o.run.kill();
                true
            }
            None => false,
        }
    }

    /// Close an auto-opened prompt for `pane`; held keys are wiped with it (the read they
    /// were typed for is over or taken by askpass).
    fn close_pane_prompt(&mut self, pane: PaneKey) {
        while let Some(i) = self
            .passwords
            .open
            .iter()
            .position(|o| o.ask.is_none() && o.pane == Some(pane))
        {
            let o = self.passwords.open.swap_remove(i);
            o.run.kill();
            drop(o.hold);
        }
    }

    /// The io thread saw the credential state change (ADR 0115).
    pub(super) fn on_credential(&mut self, id: PaneKey, on: bool) {
        if !rising(&mut self.passwords.cred, id, on) {
            if !on {
                self.close_pane_prompt(id);
            }
            return;
        }
        let p = self.panes.get(&id);
        let local = p.is_some_and(|p| p.remote.is_none() && p.pty.is_some());
        let user_job = p.is_some_and(|p| {
            let running = p.shared.running.load(std::sync::atomic::Ordering::Acquire);
            let shell = p.pty.as_ref().map(apoth_pty::Pty::pid);
            let fg = p.pty.as_ref().and_then(|t| t.foreground_pgid().ok());
            running || matches!((fg, shell), (Some(f), Some(s)) if f != s)
        });
        let gate = Gate {
            auto: self.settings.password_prompt,
            available: self.prompt_program().is_some(),
            local,
            pane_focused: self.focused() == Some(id),
            window_focused: self.win_focused,
            askpass_pending: self.askpass_pending(id),
            open: self.passwords.open.iter().any(|o| o.pane == Some(id)),
            user_job,
        };
        if !gate.opens() {
            return;
        }
        self.open_pane_prompt(id);
    }

    fn open_pane_prompt(&mut self, id: PaneKey) {
        let Some(program) = self.prompt_program() else {
            return;
        };
        let Some(p) = self.panes.get(&id) else { return };
        let Some(pty) = p.pty.as_ref() else { return };
        let root = pty.pid();
        // The requester is the foreground group's leader (falls back to the pane's child).
        let pid = pty
            .foreground_pgid()
            .ok()
            .filter(|&g| std::path::Path::new(&format!("/proc/{g}")).exists())
            .unwrap_or(root);
        // The prompt the program printed: the cursor's row up to the cursor.
        let prompt = {
            let t = p.shared.lock();
            let c = t.cursor();
            t.row_text(c.y).chars().take(c.x).collect::<String>()
        };
        let r = self.prompt_request(
            Via::Pty,
            Some(id),
            pid,
            root,
            prompt.trim().to_owned(),
            None,
        );
        let tx = self.to_main.clone();
        match sp::spawn(&program, r, move |pid, ans| {
            let _ = tx.send(MainMsg::Prompt(pid, Some(ans)));
        }) {
            Ok(run) => {
                tracing::info!(pane = id, prompt = run.id, "password prompt opened");
                self.passwords.open.push(Open {
                    run,
                    pane: Some(id),
                    ask: None,
                    hold: Some(Hold {
                        keys: Secret::new(),
                        since: Instant::now(),
                        over: false,
                    }),
                });
            }
            Err(e) => tracing::warn!(error = %e, "password prompt did not start"),
        }
    }

    /// A prompt ended. For a pane prompt, type the held keys and the answer while the pane
    /// still waits for it; otherwise the bytes are wiped.
    pub(super) fn on_prompt(&mut self, id: u64, answer: Option<Answer>) {
        let Some(i) = self.passwords.open.iter().position(|o| o.run.id == id) else {
            // Closed already (withdrawn, the read ended, askpass took over): nothing typed.
            return;
        };
        let o = self.passwords.open.swap_remove(i);
        let (Some(pane), Some(answer)) = (o.pane, answer) else {
            return;
        };
        if o.ask.is_some() || !self.passwords.cred.contains_key(&pane) {
            return;
        }
        let held = o.hold.map(|h| h.keys);
        let mut unit = Vec::with_capacity(apoth_proto::askpass::MAX_SECRET * 2 + 1);
        if let Some(h) = &held {
            unit.extend_from_slice(h.bytes());
        }
        match answer {
            Answer::Secret(s) => {
                unit.extend_from_slice(s.bytes());
                unit.push(b'\r');
                tracing::info!(pane, "password prompt answered");
            }
            Answer::Cancel => tracing::info!(pane, "password prompt cancelled"),
        }
        drop(held);
        if !unit.is_empty() {
            // The PTY queue owns this copy from here; it is not wiped (reported).
            self.send_unit(pane, unit);
        }
    }

    /// Keys for the focused pane while its prompt maps: true when held (not to be sent).
    pub(super) fn hold_key(&mut self, pane: PaneKey, bytes: &[u8]) -> bool {
        let Some(i) = self
            .passwords
            .open
            .iter()
            .position(|o| o.pane == Some(pane) && o.hold.as_ref().is_some_and(|h| !h.over))
        else {
            return false;
        };
        let stale = self.passwords.open[i]
            .hold
            .as_ref()
            .is_some_and(|h| h.since.elapsed() >= HOLD_FOR);
        let enter = bytes.iter().any(|&b| b == b'\r' || b == b'\n');
        if stale || enter {
            // The prompt never took the keyboard, or the user answered here.
            let mut held = self.passwords.open[i].hold.take().map(|h| h.keys);
            let mut unit =
                Vec::with_capacity(held.as_ref().map_or(0, |h| h.bytes().len()) + bytes.len());
            if let Some(h) = &held {
                unit.extend_from_slice(h.bytes());
            }
            held.take();
            if enter {
                let o = self.passwords.open.swap_remove(i);
                o.run.kill();
                unit.extend_from_slice(bytes);
            }
            if !unit.is_empty() {
                self.send_unit(pane, unit);
            }
            return enter;
        }
        let Some(h) = self.passwords.open[i].hold.as_mut() else {
            return false;
        };
        // Past the budget the key is dropped rather than sent: it belongs to the read.
        let _ = h.keys.push_bytes(bytes);
        true
    }

    /// The keyboard left (the prompt took it) or came back: holding ends either way.
    pub(super) fn password_focus(&mut self, focused: bool) {
        for o in &mut self.passwords.open {
            if let Some(h) = o.hold.as_mut()
                && (!focused || h.since.elapsed() >= HOLD_FOR)
            {
                h.over = true;
            }
        }
    }

    /// The pane is gone: its prompts and state go too.
    pub(super) fn password_pane_gone(&mut self, pane: PaneKey) {
        self.passwords.cred.remove(&pane);
        self.close_pane_prompt(pane);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn open_gate() -> Gate {
        Gate {
            auto: true,
            available: true,
            local: true,
            pane_focused: true,
            window_focused: true,
            askpass_pending: false,
            open: false,
            user_job: true,
        }
    }

    #[test]
    fn a_getpass_read_in_the_focused_pane_opens() {
        assert!(open_gate().opens());
    }

    #[test]
    fn never_for_an_unfocused_pane_or_window() {
        assert!(
            !Gate {
                pane_focused: false,
                ..open_gate()
            }
            .opens()
        );
        assert!(
            !Gate {
                window_focused: false,
                ..open_gate()
            }
            .opens()
        );
    }

    #[test]
    fn askpass_wins_and_there_is_never_a_second_prompt() {
        assert!(
            !Gate {
                askpass_pending: true,
                ..open_gate()
            }
            .opens()
        );
        assert!(
            !Gate {
                open: true,
                ..open_gate()
            }
            .opens()
        );
    }

    #[test]
    fn not_for_a_background_job_a_remote_pane_off_or_off_eclipse() {
        for g in [
            Gate {
                user_job: false,
                ..open_gate()
            },
            Gate {
                local: false,
                ..open_gate()
            },
            Gate {
                auto: false,
                ..open_gate()
            },
            Gate {
                available: false,
                ..open_gate()
            },
        ] {
            assert!(!g.opens(), "{g:?}");
        }
    }

    #[test]
    fn one_decision_per_read() {
        let mut c = std::collections::BTreeMap::new();
        assert!(rising(&mut c, 1, true));
        // The same read sampled again: no second decision.
        assert!(!rising(&mut c, 1, true));
        // Another pane is its own read.
        assert!(rising(&mut c, 2, true));
        // The read ends; the next one decides again.
        assert!(!rising(&mut c, 1, false));
        assert!(rising(&mut c, 1, true));
    }
}
