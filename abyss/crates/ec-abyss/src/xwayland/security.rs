// SPDX-License-Identifier: AGPL-3.0-only
//! The X11 trust boundary (COMP-07 §2).
//!
//! X11 is a separate trust domain: inside XWayland every client can read
//! every other client's input and pixels, and nothing the compositor does
//! changes that. So the classification of an X11 window is not a policy
//! *decision* the compositor is free to make — it is a statement of what is
//! actually true about the domain, and it is clamped here before any rule
//! engine gets a say.
//!
//! This module is pure: it owns no state and touches no protocol objects, so
//! the invariants below are unit-testable without an X server.

/// Data sensitivity class (C-00). Ordered: raising is allowed, lowering is not.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
#[allow(dead_code)] // the full ladder is the vocabulary COMP-11 clamps against
pub enum Sensitivity {
    Public,
    Private,
    Secret,
}

/// How far the compositor trusts the application behind a window.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum AppTrust {
    Standard,
    Trusted,
}

/// Whether a second (agent) seat can address the window concurrently
/// (COMP-04 §8).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(dead_code)] // `Multi` exists for Wayland windows; X11 never gets it
pub enum SeatCompat {
    /// Multiple seats may be routed to the window at once.
    Multi,
    /// One seat at a time, taken under an exclusive focus lock.
    Lock,
}

/// The classification attached to every X11 window.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct X11Class {
    pub sensitivity: Sensitivity,
    pub app_trust: AppTrust,
    pub seat_compat: SeatCompat,
    /// Stamped onto every audit record touching this window (COMP-07 §2).
    pub xwayland: bool,
}

impl Default for X11Class {
    fn default() -> Self {
        Self {
            sensitivity: Sensitivity::Private,
            app_trust: AppTrust::Standard,
            seat_compat: SeatCompat::Lock,
            xwayland: true,
        }
    }
}

/// Clamp an app- or rule-requested classification to what XWayland can
/// actually deliver (COMP-07 §2).
///
/// - the default is `private`, and a request may only *raise* it;
/// - `secret` is refused outright and logged — inside a domain where any X11
///   client can screenshot the window, the guarantee would be a lie;
/// - `app_trust` never exceeds `standard`;
/// - `seat_compat` is always `lock` and no rule can override it.
///
/// `class` is the window's X11 class name (`WM_CLASS`), used only for the
/// refusal log line.
pub fn classify(class: &str, requested: Option<Sensitivity>, requested_trust: Option<AppTrust>) -> X11Class {
    let mut out = X11Class::default();

    match requested {
        Some(Sensitivity::Secret) => {
            tracing::warn!(
                x11_class = class,
                "refusing `secret` classification for an X11 window: XWayland cannot \
                 isolate it from other X11 clients; keeping `private` (COMP-07 §2)"
            );
        }
        // Ratchet: a request may tighten, never loosen.
        Some(s) if s > out.sensitivity => out.sensitivity = s,
        Some(s) if s < out.sensitivity => {
            tracing::debug!(
                x11_class = class,
                requested = ?s,
                "ignoring request to lower an X11 window's sensitivity below `private`"
            );
        }
        _ => {}
    }

    if requested_trust == Some(AppTrust::Trusted) {
        tracing::debug!(
            x11_class = class,
            "ignoring `trusted` app_trust for an X11 window; capped at `standard` (COMP-07 §2)"
        );
    }

    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_to_private_untrusted_locked() {
        let c = classify("xterm", None, None);
        assert_eq!(c.sensitivity, Sensitivity::Private);
        assert_eq!(c.app_trust, AppTrust::Standard);
        assert_eq!(c.seat_compat, SeatCompat::Lock);
        assert!(c.xwayland);
    }

    /// COMP-07 §6: `secret` on an X11 window is refused.
    #[test]
    fn secret_is_refused() {
        assert_eq!(
            classify("Steam", Some(Sensitivity::Secret), None).sensitivity,
            Sensitivity::Private
        );
    }

    /// The default never drops to `public` (COMP-07 §2).
    #[test]
    fn public_never_wins() {
        assert_eq!(
            classify("xeyes", Some(Sensitivity::Public), None).sensitivity,
            Sensitivity::Private
        );
    }

    /// COMP-07 §6: `seat_compat=lock` cannot be overridden by rule — there is
    /// no input by which to request otherwise, and every path returns `Lock`.
    #[test]
    fn seat_compat_is_always_lock() {
        for s in [
            None,
            Some(Sensitivity::Public),
            Some(Sensitivity::Private),
            Some(Sensitivity::Secret),
        ] {
            for t in [None, Some(AppTrust::Standard), Some(AppTrust::Trusted)] {
                let c = classify("any", s, t);
                assert_eq!(c.seat_compat, SeatCompat::Lock);
                assert_eq!(c.app_trust, AppTrust::Standard);
                assert!(c.sensitivity < Sensitivity::Secret);
            }
        }
    }
}
