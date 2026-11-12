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
    Normal,
    Secret,
}

// Plain data a `window-rule` can name, so it lives in `ec-abyss-config`.
pub use ec_abyss_config::trust::{AppTrust, SeatCompat};

/// The classification attached to every X11 window.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct X11Class {
    pub sensitivity: Sensitivity,
    pub app_trust: AppTrust,
    pub seat_compat: SeatCompat,
    /// Stamped onto every audit record touching this window (COMP-07 §2).
    pub xwayland: bool,
    /// A rule asked for `secret`: the window is withheld from compositor
    /// captures (ADR 0106, the COMP-07 §2 amendment). This is capture
    /// redaction only. [`Self::sensitivity`] stays `normal`, and any other
    /// X11 client can still read the window through the X server.
    pub redact_capture: bool,
}

impl Default for X11Class {
    fn default() -> Self {
        Self {
            sensitivity: Sensitivity::Normal,
            app_trust: AppTrust::Standard,
            seat_compat: SeatCompat::Lock,
            xwayland: true,
            redact_capture: false,
        }
    }
}

/// Clamp an app- or rule-requested classification to what XWayland can
/// actually deliver (COMP-07 §2).
///
/// - the default is `private`, and a request may only *raise* it;
/// - the *class* is never `secret` — inside a domain where any X11 client can
///   screenshot the window, an isolation guarantee would be a lie. A request
///   for it instead sets [`X11Class::redact_capture`], which only keeps the
///   window out of compositor captures (ADR 0106);
/// - `app_trust` never exceeds `standard`;
/// - `seat_compat` is always `lock` and no rule can override it.
///
/// `class` is the window's X11 class name (`WM_CLASS`), used only for the
/// log line.
pub fn classify(class: &str, requested: Option<Sensitivity>, requested_trust: Option<AppTrust>) -> X11Class {
    let mut out = X11Class::default();

    match requested {
        Some(Sensitivity::Secret) => {
            out.redact_capture = true;
            tracing::info!(
                x11_class = class,
                "X11 window marked `secret` for capture redaction only: XWayland cannot \
                 isolate it from other X11 clients; class stays `normal` (COMP-07 §2)"
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
        assert_eq!(c.sensitivity, Sensitivity::Normal);
        assert_eq!(c.app_trust, AppTrust::Standard);
        assert_eq!(c.seat_compat, SeatCompat::Lock);
        assert!(c.xwayland);
    }

    /// COMP-07 §2/§6: `secret` on an X11 window never becomes its class; it
    /// only asks for capture redaction.
    #[test]
    fn secret_redacts_capture_but_is_never_the_class() {
        let c = classify("Steam", Some(Sensitivity::Secret), None);
        assert_eq!(c.sensitivity, Sensitivity::Normal);
        assert!(c.redact_capture);
        for s in [None, Some(Sensitivity::Public), Some(Sensitivity::Normal)] {
            assert!(!classify("Steam", s, None).redact_capture);
        }
    }

    /// The default never drops to `public` (COMP-07 §2).
    #[test]
    fn public_never_wins() {
        assert_eq!(
            classify("xeyes", Some(Sensitivity::Public), None).sensitivity,
            Sensitivity::Normal
        );
    }

    /// COMP-07 §6: `seat_compat=lock` cannot be overridden by rule — there is
    /// no input by which to request otherwise, and every path returns `Lock`.
    #[test]
    fn seat_compat_is_always_lock() {
        for s in [
            None,
            Some(Sensitivity::Public),
            Some(Sensitivity::Normal),
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
