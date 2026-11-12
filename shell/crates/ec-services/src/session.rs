// SPDX-License-Identifier: AGPL-3.0-only
//! Session control: lock, log out, suspend, reboot, power off (ADR 0038).
//!
//! These are the only actions in this crate that *do* something. They go
//! straight to systemd-logind over the system bus, which is where the
//! authorisation actually lives: polkit decides whether this human may power
//! the machine off, and it decides that whether the request comes from here,
//! from `loginctl`, or from a GNOME menu. We add no authority of our own and
//! hold no capability — a compromised control center can ask logind to suspend
//! the machine, which is exactly what the human sitting at it could do anyway.
//!
//! What we do add is honesty about the answer. Every action has a matching
//! `can_*` query, so the power menu can grey out what will fail rather than
//! offering a button that silently does nothing.
//!
//! It also listens, for `ec-lock-trigger` (ADR 0095): logind's `Lock` for the
//! display session and `PrepareForSleep` are what start the locker. Deciding
//! what each one means is [`respond`], a pure function; the bin does the I/O.

use std::os::fd::OwnedFd;
use std::time::Duration;

use zbus::blocking::{Connection, MessageIterator, Proxy};
use zbus::zvariant::OwnedObjectPath;
use zbus::MatchRule;

const LOGIND: &str = "org.freedesktop.login1";
const MANAGER_PATH: &str = "/org/freedesktop/login1";
const MANAGER: &str = "org.freedesktop.login1.Manager";
/// logind resolves this to the caller's own session. Better than reading
/// `XDG_SESSION_ID`, which a parent process can set to anything it likes.
const SESSION_PATH: &str = "/org/freedesktop/login1/session/auto";
const SESSION: &str = "org.freedesktop.login1.Session";
/// The caller's own user. A user unit is in no session, so `session/auto`
/// fails there; the user's `Display` property names the graphical session.
const USER_SELF: &str = "/org/freedesktop/login1/user/self";
const USER: &str = "org.freedesktop.login1.User";

/// What logind said, as far as the lock trigger is concerned.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LoginEvent {
    /// `Session.Lock` on the display session (`loginctl lock-session`).
    Lock,
    /// `Manager.PrepareForSleep(true)`: suspend or hibernate is under way.
    Sleep,
    /// `Manager.PrepareForSleep(false)`: the machine is back.
    Resume,
}

/// What the lock trigger does about it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Response {
    /// Start `ec-lock`. A second one finds the first's claim and exits.
    StartLocker,
    /// Start `ec-lock` and keep holding the sleep delay until it has had
    /// [`SLEEP_GRACE`] to lock, or has exited.
    StartLockerBeforeSleep,
    /// Take a fresh sleep delay for the next suspend.
    Rearm,
}

/// How long a suspend waits for the locker. Under logind's default
/// `InhibitDelayMaxSec` (5 s), so logind never has to cut us off, and never
/// unbounded: a locker that hangs must not keep the machine awake.
pub const SLEEP_GRACE: Duration = Duration::from_secs(3);

/// Event to action. Pure, so the trigger's whole policy is testable.
pub fn respond(event: LoginEvent) -> Response {
    match event {
        LoginEvent::Lock => Response::StartLocker,
        LoginEvent::Sleep => Response::StartLockerBeforeSleep,
        LoginEvent::Resume => Response::Rearm,
    }
}

/// Which [`LoginEvent`] a signal is, if any. `ours` is the display session's
/// object path; `sleeping` is `PrepareForSleep`'s argument when it parsed.
///
/// `Unlock` is deliberately not an event: only the locker's own PAM grant
/// opens the session, never a logind call (`loginctl unlock-session`).
pub fn classify(
    interface: &str,
    member: &str,
    path: &str,
    ours: &str,
    sleeping: Option<bool>,
) -> Option<LoginEvent> {
    match (interface, member) {
        (SESSION, "Lock") if path == ours => Some(LoginEvent::Lock),
        (MANAGER, "PrepareForSleep") if path == MANAGER_PATH => match sleeping? {
            true => Some(LoginEvent::Sleep),
            false => Some(LoginEvent::Resume),
        },
        _ => None,
    }
}

/// Whether to let go of the sleep delay held across a `Sleep`.
pub fn release_sleep_delay(waited: Duration, locker_exited: bool) -> bool {
    locker_exited || waited >= SLEEP_GRACE
}

/// What the human can ask for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    /// Lock this session. The lock screen itself is the compositor's job; this
    /// only tells logind, which tells whoever is listening.
    Lock,
    /// End this session. Everything running as the human dies with it.
    LogOut,
    Suspend,
    Hibernate,
    Reboot,
    PowerOff,
}

impl Action {
    fn method(self) -> &'static str {
        match self {
            Self::Lock => "Lock",
            Self::LogOut => "Terminate",
            Self::Suspend => "Suspend",
            Self::Hibernate => "Hibernate",
            Self::Reboot => "Reboot",
            Self::PowerOff => "PowerOff",
        }
    }

    /// The `Can*` query paired with this action, where logind has one. Lock and
    /// log out have none — they act on a session the caller already owns.
    fn query(self) -> Option<&'static str> {
        match self {
            Self::Lock | Self::LogOut => None,
            Self::Suspend => Some("CanSuspend"),
            Self::Hibernate => Some("CanHibernate"),
            Self::Reboot => Some("CanReboot"),
            Self::PowerOff => Some("CanPowerOff"),
        }
    }

    fn on_manager(self) -> bool {
        self.query().is_some()
    }
}

/// logind's answer to a `Can*` query, kept as three states rather than a bool.
///
/// The distinction matters at the widget: `Challenge` means the action will
/// work but the human will be asked to authenticate first, and a menu that
/// renders that the same as `Yes` produces a password prompt out of nowhere.
/// On this machine `CanSuspend` answers `challenge`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Availability {
    Yes,
    /// Permitted, but polkit will authenticate the human first.
    Challenge,
    /// Refused by policy, or the machine cannot do it at all.
    No,
}

impl Availability {
    fn from_reply(reply: &str) -> Self {
        match reply {
            "yes" => Self::Yes,
            "challenge" => Self::Challenge,
            // "no", "na", and anything logind grows later. An unrecognised
            // answer reads as unavailable: offering a button on a reply we do
            // not understand is the wrong way to be wrong.
            _ => Self::No,
        }
    }

    /// Whether to offer the action at all.
    pub fn offered(self) -> bool {
        matches!(self, Self::Yes | Self::Challenge)
    }
}

/// A handle on logind. Cheap to clone; the connection is shared.
#[derive(Debug, Clone)]
pub struct Session {
    connection: Connection,
}

impl Session {
    /// Fails only if the system bus is unreachable — a container without one,
    /// or a machine not running systemd.
    pub fn connect() -> zbus::Result<Self> {
        Ok(Self {
            connection: Connection::system()?,
        })
    }

    /// Whether logind will do this, asked before it is offered.
    ///
    /// Lock and log out are always available: they act on the session this
    /// process is already in, and logind does not gate them.
    pub fn availability(&self, action: Action) -> Availability {
        let Some(query) = action.query() else {
            return Availability::Yes;
        };
        let Ok(proxy) = self.manager() else {
            return Availability::No;
        };
        proxy
            .call::<_, _, String>(query, &())
            .map_or(Availability::No, |reply| Availability::from_reply(&reply))
    }

    /// Do it.
    ///
    /// `interactive` is passed to logind for the manager-wide actions: it lets
    /// polkit put an authentication prompt in front of the human instead of
    /// refusing outright. Pass `true` from a menu the human just clicked, and
    /// `false` from anything automatic, where there is nobody to answer.
    ///
    /// Returns logind's own error unchanged — a refusal here is a policy
    /// decision and the caller should say so, not retry.
    pub fn perform(&self, action: Action, interactive: bool) -> zbus::Result<()> {
        if action.on_manager() {
            self.manager()?.call::<_, _, ()>(action.method(), &(interactive))
        } else {
            self.session()?.call::<_, _, ()>(action.method(), &())
        }
    }

    /// The object path of this user's graphical session: the one logind
    /// locks with `loginctl lock-session` from inside it.
    pub fn display_session(&self) -> zbus::Result<OwnedObjectPath> {
        let user = Proxy::new(&self.connection, LOGIND, USER_SELF, USER)?;
        let (id, path): (String, OwnedObjectPath) = user.get_property("Display")?;
        if id.is_empty() {
            return Err(zbus::Error::Failure("this user has no display session".into()));
        }
        Ok(path)
    }

    /// A `delay` inhibitor on sleep. logind holds a suspend until every such
    /// fd is closed or `InhibitDelayMaxSec` passes; dropping the fd releases.
    pub fn delay_sleep(&self, why: &str) -> zbus::Result<OwnedFd> {
        let fd: zbus::zvariant::OwnedFd = self
            .manager()?
            .call("Inhibit", &("sleep", "Eclipse screen lock", why, "delay"))?;
        Ok(fd.into())
    }

    /// `Session.Lock` on `session` and `Manager.PrepareForSleep`, as two
    /// iterators (one match rule each). Subscribed before returning, so a
    /// signal sent after this call is not missed.
    pub fn lock_signals(&self, session: &OwnedObjectPath) -> zbus::Result<[MessageIterator; 2]> {
        let lock = MatchRule::builder()
            .msg_type(zbus::message::Type::Signal)
            .sender(LOGIND)?
            .path(session.clone().into_inner())?
            .interface(SESSION)?
            .member("Lock")?
            .build();
        let sleep = MatchRule::builder()
            .msg_type(zbus::message::Type::Signal)
            .sender(LOGIND)?
            .path(MANAGER_PATH)?
            .interface(MANAGER)?
            .member("PrepareForSleep")?
            .build();
        Ok([
            MessageIterator::for_match_rule(lock, &self.connection, Some(16))?,
            MessageIterator::for_match_rule(sleep, &self.connection, Some(16))?,
        ])
    }

    fn manager(&self) -> zbus::Result<Proxy<'_>> {
        Proxy::new(&self.connection, LOGIND, MANAGER_PATH, MANAGER)
    }

    fn session(&self) -> zbus::Result<Proxy<'_>> {
        Proxy::new(&self.connection, LOGIND, SESSION_PATH, SESSION)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_answer_we_do_not_understand_is_not_an_offer() {
        assert_eq!(Availability::from_reply("yes"), Availability::Yes);
        assert_eq!(Availability::from_reply("challenge"), Availability::Challenge);
        for refusal in ["no", "na", "", "YES", "maybe"] {
            assert_eq!(Availability::from_reply(refusal), Availability::No);
        }
    }

    /// A challenge is still an offer — the human authenticates and it happens.
    #[test]
    fn a_challenge_is_offered() {
        assert!(Availability::Yes.offered());
        assert!(Availability::Challenge.offered());
        assert!(!Availability::No.offered());
    }

    /// The two session-scoped actions must not be sent to the manager object,
    /// and the four machine-scoped ones must not be sent to the session.
    #[test]
    fn each_action_goes_to_the_object_that_implements_it() {
        assert!(!Action::Lock.on_manager());
        assert!(!Action::LogOut.on_manager());
        for action in [
            Action::Suspend,
            Action::Hibernate,
            Action::Reboot,
            Action::PowerOff,
        ] {
            assert!(action.on_manager(), "{action:?}");
        }
    }

    const OURS: &str = "/org/freedesktop/login1/session/_32";

    #[test]
    fn every_event_has_its_response() {
        assert_eq!(respond(LoginEvent::Lock), Response::StartLocker);
        assert_eq!(respond(LoginEvent::Sleep), Response::StartLockerBeforeSleep);
        assert_eq!(respond(LoginEvent::Resume), Response::Rearm);
    }

    #[test]
    fn lock_counts_only_for_our_session() {
        assert_eq!(
            classify(SESSION, "Lock", OURS, OURS, None),
            Some(LoginEvent::Lock)
        );
        let theirs = "/org/freedesktop/login1/session/c1";
        assert_eq!(classify(SESSION, "Lock", theirs, OURS, None), None);
        // Wrong interface or object for the member.
        assert_eq!(classify(MANAGER, "Lock", OURS, OURS, None), None);
        assert_eq!(classify(SESSION, "Lock", MANAGER_PATH, OURS, None), None);
    }

    /// Nothing logind says unlocks: the PAM grant inside ec-lock is the only way.
    #[test]
    fn unlock_is_not_an_event() {
        assert_eq!(classify(SESSION, "Unlock", OURS, OURS, None), None);
        for member in ["SessionNew", "SessionRemoved", "PrepareForShutdown"] {
            assert_eq!(classify(MANAGER, member, MANAGER_PATH, OURS, Some(true)), None);
        }
    }

    #[test]
    fn prepare_for_sleep_reads_its_argument() {
        let sleep = |arg| classify(MANAGER, "PrepareForSleep", MANAGER_PATH, OURS, arg);
        assert_eq!(sleep(Some(true)), Some(LoginEvent::Sleep));
        assert_eq!(sleep(Some(false)), Some(LoginEvent::Resume));
        // An argument that did not parse is no event, not a guess.
        assert_eq!(sleep(None), None);
        assert_eq!(classify(MANAGER, "PrepareForSleep", OURS, OURS, Some(true)), None);
    }

    /// The suspend is held until the locker exits or the grace runs out,
    /// whichever is first, and never past the grace.
    #[test]
    fn the_sleep_delay_is_bounded() {
        assert!(!release_sleep_delay(Duration::ZERO, false));
        assert!(!release_sleep_delay(
            SLEEP_GRACE - Duration::from_millis(1),
            false
        ));
        assert!(release_sleep_delay(SLEEP_GRACE, false));
        assert!(release_sleep_delay(Duration::from_secs(3600), false));
        assert!(release_sleep_delay(Duration::ZERO, true));
        assert!(
            SLEEP_GRACE < Duration::from_secs(5),
            "under logind's default InhibitDelayMaxSec"
        );
    }
}
