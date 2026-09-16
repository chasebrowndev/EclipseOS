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

use zbus::blocking::{Connection, Proxy};

const LOGIND: &str = "org.freedesktop.login1";
const MANAGER_PATH: &str = "/org/freedesktop/login1";
const MANAGER: &str = "org.freedesktop.login1.Manager";
/// logind resolves this to the caller's own session. Better than reading
/// `XDG_SESSION_ID`, which a parent process can set to anything it likes.
const SESSION_PATH: &str = "/org/freedesktop/login1/session/auto";
const SESSION: &str = "org.freedesktop.login1.Session";

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
}
