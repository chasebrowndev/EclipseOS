// SPDX-License-Identifier: AGPL-3.0-only
//! Toasts while a screen is being shared (ADR 0106).
//!
//! The compositor says when a human capture starts and stops (the `capture`
//! event, `{"active": bool}`) and the human says what toasts should do then
//! (`ui.capture-toasts`, read with `get_config`). [`Share`] holds both, written by
//! the compositor thread and read by the server and the toast stack:
//!
//! * `show`: nothing changes; the compositor does not cover toasts.
//! * `redact` (the default): nothing changes here. The compositor covers the
//!   toast in the shared image and the human sees it normally.
//! * `hold`: the stack keeps arrivals back until the share ends ([`Share::holding`]).
//!   They are not lost and make no sound meanwhile.
//! * `history`: arrivals go straight to the history, unread, with no popup
//!   ([`Share::history`]); the server admits them as `Quiet`.
//!
//! Fail-open on the *flag* is the wrong way round for privacy, so a dead
//! socket is read as "a share may be running": [`Share::lost`] sets it, and the
//! next `capture` event after reconnecting corrects it. With the default mode
//! (`redact`) that changes nothing; with `hold` or `history` it errs towards
//! showing less.

use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};
use std::sync::Arc;

use serde_json::Value;

/// `ui.capture-toasts`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ToastMode {
    Show,
    #[default]
    Redact,
    Hold,
    History,
}

impl ToastMode {
    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "show" => Self::Show,
            "redact" => Self::Redact,
            "hold" => Self::Hold,
            "history" => Self::History,
            _ => return None,
        })
    }

    fn to_u8(self) -> u8 {
        self as u8
    }

    fn from_u8(n: u8) -> Self {
        match n {
            0 => Self::Show,
            2 => Self::Hold,
            3 => Self::History,
            _ => Self::Redact,
        }
    }
}

#[derive(Debug, Default)]
struct Inner {
    active: AtomicBool,
    mode: AtomicU8,
    /// The focused output has a fullscreen window (the compositor's `output`
    /// event, ADR 0127).
    fullscreen: AtomicBool,
    /// `ui.toasts-over-fullscreen`.
    over_fullscreen: AtomicBool,
    /// `ui.restore-offer-in-quiet` is off (stored inverted so the default,
    /// on, is the zero value).
    no_offer_in_quiet: AtomicBool,
}

/// The shared state. Cloning shares it.
#[derive(Debug, Clone)]
pub struct Share(Arc<Inner>);

impl Default for Share {
    fn default() -> Self {
        let inner = Inner::default();
        inner.mode.store(ToastMode::default().to_u8(), Ordering::Relaxed);
        Share(Arc::new(inner))
    }
}

impl Share {
    pub fn set_active(&self, active: bool) {
        self.0.active.store(active, Ordering::Relaxed);
    }

    pub fn set_mode(&self, mode: ToastMode) {
        self.0.mode.store(mode.to_u8(), Ordering::Relaxed);
    }

    pub fn active(&self) -> bool {
        self.0.active.load(Ordering::Relaxed)
    }

    pub fn mode(&self) -> ToastMode {
        ToastMode::from_u8(self.0.mode.load(Ordering::Relaxed))
    }

    /// The stack should keep arrivals back.
    pub fn holding(&self) -> bool {
        self.active() && self.mode() == ToastMode::Hold
    }

    /// Arrivals go straight to the history.
    pub fn history(&self) -> bool {
        self.active() && self.mode() == ToastMode::History
    }

    pub fn set_fullscreen(&self, on: bool) {
        self.0.fullscreen.store(on, Ordering::Relaxed);
    }

    /// `ui.restore-offer-in-quiet` (default true).
    pub fn set_restore_offer_in_quiet(&self, on: bool) {
        self.0.no_offer_in_quiet.store(!on, Ordering::Relaxed);
    }

    pub fn restore_offer_in_quiet(&self) -> bool {
        !self.0.no_offer_in_quiet.load(Ordering::Relaxed)
    }

    pub fn set_over_fullscreen(&self, on: bool) {
        self.0.over_fullscreen.store(on, Ordering::Relaxed);
    }

    /// Toasts are kept off the screen: something is fullscreen and
    /// `ui.toasts-over-fullscreen` is off (the default). App arrivals are
    /// stored unread; a system question waits for this to clear (ADR 0127).
    pub fn fullscreen_suppressed(&self) -> bool {
        self.0.fullscreen.load(Ordering::Relaxed) && !self.0.over_fullscreen.load(Ordering::Relaxed)
    }

    /// Fold one compositor `output` event in: `{"fullscreen": bool, ..}`
    /// describes the focused output. Anything else changes nothing.
    pub fn on_output(&self, data: &Value) {
        if let Some(on) = data.get("fullscreen").and_then(Value::as_bool) {
            self.set_fullscreen(on);
        }
    }

    /// The control socket went away: whether a share is running is unknown.
    pub fn lost(&self) {
        self.set_active(true);
    }

    /// Fold one compositor event in. `capture` carries `{"active": bool}`;
    /// anything else, or a malformed payload, changes nothing.
    pub fn on_capture(&self, data: &Value) {
        if let Some(active) = data.get("active").and_then(Value::as_bool) {
            self.set_active(active);
        }
    }
}

/// `ui.toasts-over-fullscreen` out of a `get_config` reply. `None` on anything
/// else, which leaves the default (off) in place.
pub fn over_fullscreen_of(reply: &Value) -> Option<bool> {
    reply.get("keys")?.as_array()?.first()?.get("value")?.as_bool()
}

/// The mode out of a `get_config {"path": "ui.capture-toasts"}` reply. `None` on
/// anything else (an older compositor without the key), which leaves the
/// default in place.
pub fn mode_of(reply: &Value) -> Option<ToastMode> {
    reply
        .get("keys")?
        .as_array()?
        .first()?
        .get("value")?
        .as_str()
        .and_then(ToastMode::parse)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn the_default_covers_in_the_compositor_and_changes_nothing_here() {
        let s = Share::default();
        assert_eq!(s.mode(), ToastMode::Redact);
        s.set_active(true);
        assert!(!s.holding() && !s.history());
    }

    #[test]
    fn hold_and_history_apply_only_while_a_share_runs() {
        let s = Share::default();
        s.set_mode(ToastMode::Hold);
        assert!(!s.holding());
        s.on_capture(&json!({"active": true}));
        assert!(s.holding() && !s.history());
        s.on_capture(&json!({"active": false}));
        assert!(!s.holding());
        s.set_mode(ToastMode::History);
        s.set_active(true);
        assert!(s.history() && !s.holding());
        s.set_mode(ToastMode::Show);
        assert!(!s.history() && !s.holding());
    }

    #[test]
    fn a_malformed_event_changes_nothing_and_a_lost_socket_assumes_a_share() {
        let s = Share::default();
        s.on_capture(&json!({"active": "yes"}));
        s.on_capture(&json!({}));
        assert!(!s.active());
        s.lost();
        assert!(s.active());
    }

    #[test]
    fn the_mode_is_read_from_a_get_config_reply() {
        let reply = json!({"keys": [{"path": "ui.capture-toasts", "value": "hold"}]});
        assert_eq!(mode_of(&reply), Some(ToastMode::Hold));
        assert_eq!(mode_of(&json!({"keys": []})), None);
        assert_eq!(mode_of(&json!({"keys": [{"value": "drop"}]})), None);
    }

    #[test]
    fn fullscreen_suppresses_toasts_unless_the_setting_is_on() {
        let s = Share::default();
        assert!(!s.fullscreen_suppressed());
        s.on_output(&json!({"focused": 1, "name": "DP-1", "fullscreen": true}));
        assert!(s.fullscreen_suppressed());
        s.set_over_fullscreen(true);
        assert!(!s.fullscreen_suppressed());
        s.set_over_fullscreen(false);
        s.on_output(&json!({"fullscreen": "yes"}));
        assert!(s.fullscreen_suppressed());
        s.on_output(&json!({"fullscreen": false}));
        assert!(!s.fullscreen_suppressed());
    }

    #[test]
    fn the_over_fullscreen_setting_is_read_from_a_get_config_reply() {
        let reply = json!({"keys": [{"path": "ui.toasts-over-fullscreen", "value": true}]});
        assert_eq!(over_fullscreen_of(&reply), Some(true));
        assert_eq!(over_fullscreen_of(&json!({"keys": [{"value": "x"}]})), None);
        assert_eq!(over_fullscreen_of(&json!({})), None);
    }

    #[test]
    fn every_mode_round_trips_through_the_atomic() {
        for m in [
            ToastMode::Show,
            ToastMode::Redact,
            ToastMode::Hold,
            ToastMode::History,
        ] {
            assert_eq!(ToastMode::from_u8(m.to_u8()), m);
        }
    }
}
