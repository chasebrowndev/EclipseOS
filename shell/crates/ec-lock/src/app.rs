// SPDX-License-Identifier: AGPL-3.0-only
//! The locker's model, messages and `update`.
//!
//! **The unlock gate.** `iced_sessionlock` sends `unlock_and_destroy` for any
//! message whose `TryInto<UnLockAction>` succeeds, before `update` ever sees
//! it. Here that is [`Message::Unlock`] alone, and it carries a
//! [`Granted`], which only `pam::authenticate` can make. `update` builds it in
//! one place: from [`Machine::finish`]'s grant for the attempt in flight.
//! (`iced_sessionlock` also unlocks on a window-close action: nothing in this
//! crate may issue `iced::window::close`. Its plain exit action does not
//! unlock, and nothing here issues that either.)
//!
//! **The password.** Typed text lands in [`Secret`] and nowhere else we own.
//! [`Typed`] carries one key's worth through the runtime in a fixed array that
//! is zeroed on drop, and `Message`'s `Debug` never prints it.
//!
//! **For the view** (`view.rs`, owned by eclipse-frontend): read the model
//! through [`App::status`], [`App::typed`], [`App::user`], [`App::clock`]
//! and [`App::can_submit`]; emit only [`Message::Submit`] and
//! [`Message::Clear`]. Keys reach the model through [`subscription`], not
//! through a text widget, so no widget ever holds the password.

use std::ffi::CString;
use std::fmt;
use std::time::{Duration, Instant};

use iced::{Subscription, Task};
use iced_sessionlock::actions::UnLockAction;

use crate::clock::{self, Reading};
use crate::machine::{Machine, Phase, Reason};
use crate::pam::{Granted, Verdict};
use crate::secret::Secret;

/// What the view shows, derived from the machine.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Status {
    /// The lock is requested; no surface yet.
    Locking,
    /// Waiting for a password.
    Ready,
    /// PAM is checking.
    Checking,
    /// The last attempt failed. `retry_in_secs` is the wait, rounded up,
    /// before the next is allowed; `None` once it may go.
    Failed {
        reason: Reason,
        failures: u32,
        retry_in_secs: Option<u64>,
    },
    /// PAM said yes; the lock is going away.
    Unlocking,
}

/// One key's worth of typed text, moved through the iced runtime. A fixed
/// array, so it never touches the heap, zeroed when dropped. Text longer than
/// [`Typed::CAP`] bytes from one key (a long compose sequence) is dropped.
pub struct Typed {
    bytes: [u8; Typed::CAP],
    len: u8,
}

impl Typed {
    pub const CAP: usize = 16;

    pub fn new(text: &str) -> Option<Typed> {
        if text.is_empty() || text.len() > Self::CAP {
            return None;
        }
        let mut bytes = [0u8; Self::CAP];
        bytes[..text.len()].copy_from_slice(text.as_bytes());
        Some(Typed {
            bytes,
            len: text.len() as u8,
        })
    }

    fn as_str(&self) -> &str {
        std::str::from_utf8(&self.bytes[..self.len as usize]).unwrap_or("")
    }
}

impl Clone for Typed {
    fn clone(&self) -> Self {
        Typed {
            bytes: self.bytes,
            len: self.len,
        }
    }
}

impl Drop for Typed {
    fn drop(&mut self) {
        self.wipe();
    }
}

impl Typed {
    fn wipe(&mut self) {
        for b in &mut self.bytes {
            // SAFETY: a valid, aligned, owned byte; volatile so the store
            // survives the value going dead.
            unsafe { std::ptr::write_volatile(b, 0) };
        }
        self.len = 0;
    }
}

#[derive(Clone)]
pub enum Message {
    /// Text from one key press. Keyboard subscription only.
    Typed(Typed),
    /// Backspace: drop the last character.
    Erase,
    /// Escape or Ctrl+U, or the view: empty the field.
    Clear,
    /// Enter, or the view's button: try the password.
    Submit,
    /// A second passed: the clock and the backoff countdown.
    Tick,
    /// A lock surface opened.
    SurfaceUp,
    /// The compositor's theme moved and the palette was re-applied (ADR 0084).
    ThemeChanged,
    /// PAM answered attempt `.0`.
    Checked(u64, Verdict),
    /// Release the lock. Built only by `update`, from the machine's grant.
    Unlock(Granted),
}

impl fmt::Debug for Message {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Message::Typed(_) => f.write_str("Typed(<redacted>)"),
            Message::Erase => f.write_str("Erase"),
            Message::Clear => f.write_str("Clear"),
            Message::Submit => f.write_str("Submit"),
            Message::Tick => f.write_str("Tick"),
            Message::SurfaceUp => f.write_str("SurfaceUp"),
            Message::ThemeChanged => f.write_str("ThemeChanged"),
            Message::Checked(a, v) => f.debug_tuple("Checked").field(a).field(v).finish(),
            Message::Unlock(g) => f.debug_tuple("Unlock").field(g).finish(),
        }
    }
}

impl TryInto<UnLockAction> for Message {
    type Error = Message;

    fn try_into(self) -> Result<UnLockAction, Message> {
        match self {
            Message::Unlock(_) => Ok(UnLockAction),
            other => Err(other),
        }
    }
}

pub struct App {
    machine: Machine,
    secret: Secret,
    /// The login name PAM authenticates; from the uid (`crate::user`).
    user: CString,
    user_display: String,
    clock: Reading,
    /// When `update` last ran: what the view's countdown is measured from.
    now: Instant,
}

impl App {
    pub fn new(user: CString) -> Self {
        let user_display = user.to_string_lossy().into_owned();
        App {
            machine: Machine::new(),
            secret: Secret::new(),
            user,
            user_display,
            clock: clock::now(ec_ui::tokens::clock::HOUR_12),
            now: Instant::now(),
        }
    }

    pub fn status(&self) -> Status {
        match self.machine.phase() {
            Phase::Locking => Status::Locking,
            Phase::Idle => Status::Ready,
            Phase::Authenticating { .. } => Status::Checking,
            Phase::Failed { failures, reason, .. } => Status::Failed {
                reason,
                failures,
                retry_in_secs: self.machine.retry_in(self.now).map(ceil_secs),
            },
            Phase::Unlocked => Status::Unlocking,
        }
    }

    /// How many characters are in the field: draw this many bullets.
    pub fn typed(&self) -> usize {
        self.secret.chars()
    }

    /// The account being unlocked, for display.
    pub fn user(&self) -> &str {
        &self.user_display
    }

    pub fn clock(&self) -> &Reading {
        &self.clock
    }

    /// Whether Submit would reach PAM now: something is typed, nothing is in
    /// flight, and no backoff is running.
    pub fn can_submit(&self) -> bool {
        !self.secret.is_empty() && self.machine.can_submit(self.now)
    }

    fn submit(&mut self) -> Task<Message> {
        // ADR 0088's rule: an empty field asks again without asking PAM, so
        // a reflexive Enter costs no try.
        if self.secret.is_empty() {
            return Task::none();
        }
        let Some(attempt) = self.machine.submit(self.now) else {
            return Task::none();
        };
        let password = self.secret.take();
        let rx = crate::auth::start(self.user.clone(), password);
        Task::perform(crate::auth::verdict(rx), move |v| Message::Checked(attempt, v))
    }

    /// Feed a verdict to the machine; the grant, if any, is what unlocks.
    fn checked(&mut self, attempt: u64, verdict: Verdict) -> Option<Granted> {
        self.machine.finish(attempt, verdict, self.now)
    }
}

fn ceil_secs(d: Duration) -> u64 {
    d.as_secs() + u64::from(d.subsec_nanos() > 0)
}

pub fn update(app: &mut App, message: Message) -> Task<Message> {
    app.now = Instant::now();
    match message {
        Message::Typed(t) => {
            // A key can only reach us through a lock surface, so one is up
            // even if its open event went missing.
            app.machine.surfaces_up();
            if app.machine.accepts_input() {
                app.secret.push_str(t.as_str());
            }
        }
        Message::Erase => {
            if app.machine.accepts_input() {
                app.secret.pop();
            }
        }
        Message::Clear => {
            if app.machine.accepts_input() {
                app.secret.clear();
            }
        }
        Message::Submit => {
            app.machine.surfaces_up();
            return app.submit();
        }
        Message::Tick => app.clock = clock::now(ec_ui::tokens::clock::HOUR_12),
        Message::SurfaceUp => app.machine.surfaces_up(),
        Message::ThemeChanged => {}
        Message::Checked(attempt, verdict) => {
            if let Some(granted) = app.checked(attempt, verdict) {
                return Task::done(Message::Unlock(granted));
            }
        }
        // iced_sessionlock takes this before `update`; nothing to do if it
        // ever arrives here.
        Message::Unlock(_) => {}
    }
    Task::none()
}

pub fn subscription(_app: &App) -> Subscription<Message> {
    Subscription::batch([
        iced::event::listen_with(input),
        ticks(),
        ec_ui::ipc::theme_subscription(|| Message::ThemeChanged),
    ])
}

/// Keys and surface opens, from every lock surface. Ctrl, Alt and Super
/// chords type nothing; Ctrl+U clears, as in a terminal.
fn input(event: iced::Event, _status: iced::event::Status, _window: iced::window::Id) -> Option<Message> {
    use iced::keyboard::{key::Named, Event as Kb, Key};
    match event {
        iced::Event::Window(iced::window::Event::Opened { .. }) => Some(Message::SurfaceUp),
        iced::Event::Keyboard(Kb::KeyPressed {
            key, modifiers, text, ..
        }) => match key.as_ref() {
            Key::Named(Named::Enter) => Some(Message::Submit),
            Key::Named(Named::Backspace) => Some(Message::Erase),
            Key::Named(Named::Escape) => Some(Message::Clear),
            Key::Character(c) if modifiers.control() && c.eq_ignore_ascii_case("u") => Some(Message::Clear),
            _ if modifiers.control() || modifiers.alt() || modifiers.logo() => None,
            _ => text.as_deref().and_then(Typed::new).map(Message::Typed),
        },
        _ => None,
    }
}

/// A once-a-second tick, for the clock and the backoff countdown.
fn ticks() -> Subscription<Message> {
    Subscription::run(|| {
        iced::stream::channel(4, async move |mut sender| {
            std::thread::spawn(move || loop {
                std::thread::sleep(Duration::from_secs(1));
                if let Err(e) = sender.try_send(Message::Tick) {
                    if e.is_disconnected() {
                        return;
                    }
                }
            });
        })
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pam::granted_for_test;

    fn app() -> App {
        App::new(CString::new("tester").unwrap())
    }

    fn typed(s: &str) -> Message {
        Message::Typed(Typed::new(s).unwrap())
    }

    #[test]
    fn only_unlock_converts() {
        let all = [
            typed("a"),
            Message::Erase,
            Message::Clear,
            Message::Submit,
            Message::Tick,
            Message::SurfaceUp,
            Message::ThemeChanged,
            Message::Checked(1, Verdict::Granted(granted_for_test())),
            Message::Checked(1, Verdict::Denied),
        ];
        for m in all {
            let shown = format!("{m:?}");
            let r: Result<UnLockAction, Message> = m.try_into();
            assert!(r.is_err(), "{shown} must not unlock");
        }
        let r: Result<UnLockAction, Message> = Message::Unlock(granted_for_test()).try_into();
        assert!(r.is_ok());
    }

    #[test]
    fn debug_never_shows_typed_text() {
        let m = typed("hunter2");
        assert_eq!(format!("{m:?}"), "Typed(<redacted>)");
    }

    #[test]
    fn typing_fills_the_field_and_backspace_and_clear_empty_it() {
        let mut a = app();
        let _ = update(&mut a, Message::SurfaceUp);
        let _ = update(&mut a, typed("p"));
        let _ = update(&mut a, typed("w"));
        assert_eq!(a.typed(), 2);
        assert!(a.can_submit());
        let _ = update(&mut a, Message::Erase);
        assert_eq!(a.typed(), 1);
        let _ = update(&mut a, Message::Clear);
        assert_eq!(a.typed(), 0);
        assert!(!a.can_submit(), "nothing typed");
    }

    #[test]
    fn empty_submit_costs_no_try() {
        let mut a = app();
        let _ = update(&mut a, Message::SurfaceUp);
        let _ = update(&mut a, Message::Submit);
        assert_eq!(a.status(), Status::Ready);
    }

    #[test]
    fn denied_then_granted_through_update() {
        let mut a = app();
        let _ = update(&mut a, Message::SurfaceUp);
        // Stand in for `submit` without running PAM.
        let attempt = a.machine.submit(a.now).unwrap();
        let _ = update(&mut a, Message::Checked(attempt, Verdict::Denied));
        assert!(matches!(
            a.status(),
            Status::Failed {
                reason: Reason::Wrong,
                failures: 1,
                ..
            }
        ));
        // A grant for an attempt that is not in flight unlocks nothing.
        let _ = update(
            &mut a,
            Message::Checked(attempt, Verdict::Granted(granted_for_test())),
        );
        assert_ne!(a.status(), Status::Unlocking);
        a.now += crate::machine::MAX_BACKOFF;
        let next = a.machine.submit(a.now).unwrap();
        assert!(a.checked(next, Verdict::Granted(granted_for_test())).is_some());
        assert_eq!(a.status(), Status::Unlocking);
    }

    #[test]
    fn typing_is_ignored_while_pam_checks() {
        let mut a = app();
        let _ = update(&mut a, Message::SurfaceUp);
        a.machine.submit(a.now).unwrap();
        let _ = update(&mut a, typed("x"));
        assert_eq!(a.typed(), 0);
        assert_eq!(a.status(), Status::Checking);
    }

    #[test]
    fn typed_refuses_long_text_and_zeroes_on_drop() {
        assert!(Typed::new("").is_none());
        assert!(Typed::new(&"x".repeat(Typed::CAP + 1)).is_none());
        // `Drop` is exactly `wipe`.
        let mut t = Typed::new("abc").unwrap();
        t.wipe();
        assert!(t.bytes.iter().all(|b| *b == 0));
        assert_eq!(t.len, 0);
    }

    #[test]
    fn ceil_rounds_up() {
        assert_eq!(ceil_secs(Duration::from_millis(1)), 1);
        assert_eq!(ceil_secs(Duration::from_secs(2)), 2);
        assert_eq!(ceil_secs(Duration::from_millis(2001)), 3);
    }
}
