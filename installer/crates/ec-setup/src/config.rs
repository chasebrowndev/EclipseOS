// SPDX-License-Identifier: AGPL-3.0-only
//! The one door to the control socket: COMP-13 §1.4 `set_config_value`.
//!
//! `eclipse-setup` is an ordinary client of the write API (D-07 §7). It is
//! scoped to `abyss.kdl`, has no path into `policy.kdl`, and never sees the
//! phrase. That is enforced here rather than promised: every write in the
//! program goes through [`Writer::set`], and [`is_allowed`] is a closed
//! allowlist. Nothing else in the crate opens the socket or names a config
//! file, so "the app can write exactly these keys" is one function to review
//! and one test to run.

use serde_json::{json, Value};

/// Live keyboard layout (step 1).
pub const KB_LAYOUT: &str = "input.kb-layout";
/// Live keyboard variant (step 1).
pub const KB_VARIANT: &str = "input.kb-variant";
/// The wizard's own namespace (`setup.profile`, `setup.complete`,
/// `setup.pending-preset`). Written behind [`Writer::set_setup`], which
/// tolerates a compositor that does not know the key yet.
pub const SETUP_PREFIX: &str = "setup.";

/// The fixed keys, for the test that pins them. `setup.*` is the prefix rule.
pub const FIXED_KEYS: [&str; 2] = [KB_LAYOUT, KB_VARIANT];

/// Whether an "unknown key" answer is tolerated for `key`. `setup.*`, `mode`
/// and `components.*` rows are being added to the schema on another track, so
/// a compositor that does not know them yet is not a failure: the choice still
/// reaches the seed at Apply, and the live preview is a nicety.
pub fn tolerates_unknown(key: &str) -> bool {
    key.starts_with(SETUP_PREFIX) || key == crate::choices::MODE || key.starts_with("components.")
}

/// Whether this program may ask the compositor to write `key`.
///
/// Exactly [`FIXED_KEYS`], the ten [`crate::choices::SEED_KEYS`], or `setup.` followed by one lowercase word (letters,
/// digits and `-`). No other dotted path is writable, and in particular nothing
/// that could name a policy key: those are owned by `policy.kdl`, which this
/// program cannot write (COMP-13 §1.3), and the compositor refuses them anyway.
pub fn is_allowed(key: &str) -> bool {
    if FIXED_KEYS.contains(&key) || crate::choices::SEED_KEYS.contains(&key) {
        return true;
    }
    match key.strip_prefix(SETUP_PREFIX) {
        Some(rest) => {
            !rest.is_empty()
                && !rest.starts_with('-')
                && rest
                    .bytes()
                    .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == b'-')
        }
        None => false,
    }
}

/// Why a write did not happen. Never carries the value.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum WriteError {
    /// Not on the allowlist. The socket was not touched.
    Refused,
    /// No control socket, or it went away. The wizard carries on: the live
    /// preview is a nicety and the install does not depend on it.
    NoSocket,
    /// The compositor answered with an error (an unknown key, a bad value).
    Rejected,
}

/// Where writes go.
pub enum Writer {
    /// The compositor's control socket, connected on first use.
    Socket(Option<eclipse_ipc::Client>),
    /// Records instead of sending. `--fake-helper` and tests: a dry run must
    /// not change the desktop it runs on.
    Record(Vec<(String, Value)>),
}

impl Writer {
    pub fn socket() -> Writer {
        Writer::Socket(None)
    }

    pub fn recording() -> Writer {
        Writer::Record(Vec::new())
    }

    /// Everything a recording writer was asked to write, in order.
    pub fn recorded(&self) -> &[(String, Value)] {
        match self {
            Writer::Record(v) => v,
            Writer::Socket(_) => &[],
        }
    }

    /// Write one scalar. The allowlist is checked first, before any I/O.
    pub fn set(&mut self, key: &str, value: Value) -> Result<(), WriteError> {
        if !is_allowed(key) {
            return Err(WriteError::Refused);
        }
        match self {
            Writer::Record(log) => {
                log.push((key.to_owned(), value));
                Ok(())
            }
            Writer::Socket(slot) => {
                if slot.is_none() {
                    *slot = eclipse_ipc::Client::connect().ok();
                }
                let client = slot.as_mut().ok_or(WriteError::NoSocket)?;
                match client.call("set_config_value", json!({ "path": key, "value": value })) {
                    Ok(_) => Ok(()),
                    Err(eclipse_ipc::Error::Rpc { .. }) => Err(WriteError::Rejected),
                    Err(_) => {
                        // The socket is gone; reconnect on the next write.
                        *slot = None;
                        Err(WriteError::NoSocket)
                    }
                }
            }
        }
    }

    /// A write whose key may not be in the compositor's schema yet
    /// ([`tolerates_unknown`]): "unknown key" is not an error here, the
    /// wizard's progress is best-effort until the rows land.
    pub fn set_setup(&mut self, key: &str, value: Value) -> Result<(), WriteError> {
        debug_assert!(tolerates_unknown(key));
        match self.set(key, value) {
            Err(WriteError::Rejected) => Ok(()),
            other => other,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_writable_set_is_a_fixed_allowlist() {
        assert_eq!(FIXED_KEYS, ["input.kb-layout", "input.kb-variant"]);
        for k in [
            "input.kb-layout",
            "input.kb-variant",
            "setup.profile",
            "setup.complete",
            "setup.pending-preset",
        ]
        .into_iter()
        .chain(crate::choices::SEED_KEYS)
        {
            assert!(is_allowed(k), "{k}");
        }
    }

    #[test]
    fn nothing_else_is_writable() {
        for k in [
            "",
            "setup.",
            "setup",
            "setup.-x",
            "setup.a.b",
            "setup.A",
            "setup.a b",
            "input.kb-options",
            "input.repeat-rate",
            "policy",
            "policy.kdl",
            "policy.rules",
            "misc.terminal-command",
            "idle.lock-command",
            "components.terminal",
            "components.bar.x",
            "general.gaps-in",
            "decoration.blur.size",
            "bar.eye",
            "bind",
            "../setup.profile",
            "xsetup.profile",
        ] {
            assert!(!is_allowed(k), "{k:?} must not be writable");
        }
    }

    #[test]
    fn a_refused_key_never_reaches_the_sink() {
        let mut w = Writer::recording();
        assert_eq!(w.set("policy.kdl", json!("x")), Err(WriteError::Refused));
        assert_eq!(
            w.set("misc.terminal-command", json!("sh")),
            Err(WriteError::Refused)
        );
        assert!(w.recorded().is_empty());
        assert_eq!(w.set(KB_LAYOUT, json!("de")), Ok(()));
        assert_eq!(w.recorded(), &[(KB_LAYOUT.to_owned(), json!("de"))]);
    }

    #[test]
    fn a_socket_writer_refuses_before_connecting() {
        // No socket is listening in tests; a refusal must not even try.
        let mut w = Writer::socket();
        assert_eq!(w.set("policy.kdl", json!(1)), Err(WriteError::Refused));
        assert!(matches!(w, Writer::Socket(None)));
    }
}
