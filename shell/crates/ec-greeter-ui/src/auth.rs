// SPDX-License-Identifier: AGPL-3.0-only
//! The seam between the greeter's look and the login backend (ADR 0126, 0089).
//!
//! This crate draws the screen and decides nothing about who may log in. The
//! backend, `greetd` in production, is written by the main session and plugged
//! in through [`Auth`]. [`FakeAuth`] stands in for previews and screenshots.
//!
//! The contract the backend keeps:
//! - Every method returns at once. Work that waits (a greetd socket, a PAM
//!   stack) runs on the backend's own thread, and its progress shows in
//!   [`Auth::state`], which the view reads each frame.
//! - [`Auth::begin`] moves the state to `Starting` before it returns, so the
//!   next frame already shows the choice.
//! - The secret is moved in and never copied out; it is wiped when dropped.
//! - The backend never blocks on the UI, and the UI never blocks on the backend.

use std::time::{Duration, Instant};

use crate::secret::Secret;

/// A power action the login screen offers before anyone is signed in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Power {
    Suspend,
    Restart,
    PowerOff,
}

impl Power {
    pub const ALL: [Power; 3] = [Power::Suspend, Power::Restart, Power::PowerOff];

    fn index(self) -> usize {
        match self {
            Power::Suspend => 0,
            Power::Restart => 1,
            Power::PowerOff => 2,
        }
    }

    /// The words are `ec_glance::cells::POWER`, the table the layout builder's
    /// preview reads too.
    pub fn label(self) -> &'static str {
        ec_glance::cells::POWER[self.index()].0
    }

    /// The label for a narrow tile, where the full one would wrap.
    pub fn short(self) -> &'static str {
        ec_glance::cells::POWER[self.index()].1
    }
}

/// Why the last secret was refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Refusal {
    /// The secret did not match.
    Wrong,
    /// The backend could not check at all (greetd down, PAM misconfigured).
    Unavailable,
}

/// What the backend is doing, as the view reads it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AuthState {
    /// No account chosen; the account list is up.
    Idle,
    /// An account was chosen and the backend is opening its session.
    Starting,
    /// The backend wants the secret: keys reach the password field.
    Prompting,
    /// The secret is with the backend; nothing else may be typed.
    Checking,
    /// The last secret was refused; keys reach the field again.
    Refused(Refusal),
    /// Accepted. The process that drives this screen hands the session over.
    Granted,
}

impl AuthState {
    /// Keys land in the password field now.
    pub fn takes_keys(self) -> bool {
        matches!(self, AuthState::Prompting | AuthState::Refused(_))
    }

    /// The backend is busy; the view keeps redrawing until it answers.
    pub fn busy(self) -> bool {
        matches!(self, AuthState::Starting | AuthState::Checking)
    }
}

/// The login backend. See the module docs for what each method promises.
pub trait Auth: Send {
    /// The account names to offer, in display order.
    fn users(&self) -> Vec<String>;

    /// Choose `user` and start a session for them. The state is `Starting`
    /// when this returns.
    fn begin(&mut self, user: &str);

    /// Answer the open prompt with the typed secret. Consumed here.
    fn submit(&mut self, secret: Secret);

    /// Abandon the attempt and return to `Idle`.
    fn cancel(&mut self);

    /// Run a power action. Returns at once.
    fn power(&mut self, action: Power);

    /// The backend's state now. Read every frame; must not block.
    fn state(&self) -> AuthState;
}

/// How long [`FakeAuth`] takes to answer, so the busy states show on screen.
pub const FAKE_ANSWER: Duration = Duration::from_millis(700);

/// A backend that grants one secret and refuses the rest. For previews and
/// screenshots only: no account here is real and no session is opened.
pub struct FakeAuth {
    users: Vec<String>,
    state: AuthState,
    /// The state a pending answer settles into, and when it does.
    pending: Option<(AuthState, Instant)>,
    answer: Duration,
}

impl FakeAuth {
    /// The secret that grants, for previews.
    pub const SECRET: &'static [u8] = b"demo";

    pub fn new(users: &[&str]) -> Self {
        Self::with_answer(users, FAKE_ANSWER)
    }

    /// [`FakeAuth::new`] with a chosen answer time (tests use zero).
    pub fn with_answer(users: &[&str], answer: Duration) -> Self {
        FakeAuth {
            users: users.iter().map(|u| (*u).to_owned()).collect(),
            state: AuthState::Idle,
            pending: None,
            answer,
        }
    }

    fn settle(&mut self, now: Instant) {
        if let Some((next, at)) = self.pending {
            if now >= at {
                self.state = next;
                self.pending = None;
            }
        }
    }

    fn answer_at(&self, now: Instant) -> Instant {
        now + self.answer
    }
}

impl Auth for FakeAuth {
    fn users(&self) -> Vec<String> {
        self.users.clone()
    }

    fn begin(&mut self, user: &str) {
        let now = Instant::now();
        self.pending = None;
        if self.users.iter().any(|u| u == user) {
            self.state = AuthState::Starting;
            self.pending = Some((AuthState::Prompting, self.answer_at(now)));
        } else {
            self.state = AuthState::Refused(Refusal::Unavailable);
        }
    }

    fn submit(&mut self, secret: Secret) {
        let now = Instant::now();
        self.settle(now);
        if !self.state.takes_keys() {
            return;
        }
        let verdict = if secret.as_bytes() == Self::SECRET {
            AuthState::Granted
        } else {
            AuthState::Refused(Refusal::Wrong)
        };
        self.state = AuthState::Checking;
        self.pending = Some((verdict, self.answer_at(now)));
    }

    fn cancel(&mut self) {
        self.pending = None;
        self.state = AuthState::Idle;
    }

    fn power(&mut self, _action: Power) {
        // A preview has no power to manage.
    }

    fn state(&self) -> AuthState {
        match self.pending {
            Some((next, at)) if Instant::now() >= at => next,
            _ => self.state,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn secret(text: &str) -> Secret {
        let mut s = Secret::new();
        s.push_str(text);
        s
    }

    #[test]
    fn the_right_secret_grants_and_the_wrong_one_is_refused() {
        let mut a = FakeAuth::with_answer(&["robin"], Duration::ZERO);
        assert_eq!(a.state(), AuthState::Idle);
        a.begin("robin");
        assert_eq!(a.state(), AuthState::Prompting);
        a.submit(secret("nope"));
        assert_eq!(a.state(), AuthState::Refused(Refusal::Wrong));
        a.submit(secret("demo"));
        assert_eq!(a.state(), AuthState::Granted);
    }

    #[test]
    fn an_unknown_user_is_unavailable_not_a_prompt() {
        let mut a = FakeAuth::with_answer(&["robin"], Duration::ZERO);
        a.begin("mallory");
        assert_eq!(a.state(), AuthState::Refused(Refusal::Unavailable));
    }

    #[test]
    fn keys_are_refused_while_the_backend_is_busy() {
        let mut a = FakeAuth::new(&["robin"]);
        a.begin("robin");
        assert_eq!(a.state(), AuthState::Starting);
        assert!(!a.state().takes_keys());
        a.submit(secret("demo"));
        assert_eq!(a.state(), AuthState::Starting, "no prompt yet: nothing to answer");
    }

    #[test]
    fn cancel_returns_to_idle() {
        let mut a = FakeAuth::with_answer(&["robin"], Duration::ZERO);
        a.begin("robin");
        a.cancel();
        assert_eq!(a.state(), AuthState::Idle);
    }

    #[test]
    fn power_is_offered_in_order() {
        assert_eq!(Power::ALL[0], Power::Suspend);
        assert_eq!(Power::PowerOff.label(), "Power off");
    }
}
