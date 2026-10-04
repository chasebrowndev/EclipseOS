// SPDX-License-Identifier: AGPL-3.0-only
//! One fail-soft read of a compositor-config radius over the control socket.
//!
//! Every pane's glass content radius is meant to live-sync to the
//! compositor's `decoration.rounding` (or, for hyperion's bar sheet,
//! `bar.rounding`) so the client-drawn glass can never drift from the blur
//! backdrop the compositor draws behind it. This mirrors
//! `ec-launcher`'s `fetch_terminal_command` (TERM-01): an unset key, a
//! socket nothing is listening on, and a denied query all collapse to the
//! same answer, `None`, so every caller falls back to its own compile-time
//! token ([`crate::tokens::radius::CARD`] or
//! [`crate::tokens::bar::RADIUS_SHEET`]).

use serde_json::json;

/// Read `path` (e.g. `"decoration.rounding"`) as an `f32`. `None` on any
/// failure — no connection, no such key, or a value that is not a number.
pub fn fetch_config_radius(client: &mut ec_ipc::Client, path: &str) -> Option<f32> {
    let reply = client.call("get_config", json!({ "path": path })).ok()?;
    let value = reply.get("keys")?.as_array()?.first()?.get("value")?;
    value
        .as_f64()
        .or_else(|| value.as_i64().map(|i| i as f64))
        .map(|v| v as f32)
}

/// Whether the compositor draws its material behind translucent surfaces —
/// `decoration.blur.mode` is anything but `"off"`. `None` on any failure.
///
/// This is what decides a floating sheet's ground ([`crate::theme::surface`]):
/// with blur on the client lays a light tint over the compositor's blur, rim
/// and shadow; with it off it must paint an opaque backed ground itself, or
/// the tint lands on nothing (BLUR-01). A caller with no answer treats it as
/// off — an opaque sheet with blur behind it is merely plain, a translucent
/// one with nothing behind it is broken.
pub fn fetch_blur(client: &mut ec_ipc::Client) -> Option<bool> {
    let reply = client
        .call("get_config", json!({ "path": "decoration.blur.mode" }))
        .ok()?;
    let value = reply.get("keys")?.as_array()?.first()?.get("value")?;
    // The old `enabled` bool still loads, so accept it too.
    value
        .as_str()
        .map(|mode| mode != "off")
        .or_else(|| value.as_bool())
}

/// The live glass of a pane with no control-socket client of its own:
/// `decoration.rounding` and [`fetch_blur`] over one short-lived connection.
/// Each half is `None` when the socket or the key is not there.
pub fn fetch_glass() -> (Option<f32>, Option<bool>) {
    match ec_ipc::Client::connect() {
        Ok(mut client) => (
            fetch_config_radius(&mut client, "decoration.rounding"),
            fetch_blur(&mut client),
        ),
        Err(_) => (None, None),
    }
}

/// `ui.show-key-hints` over a connection the caller already holds. `None` on
/// any failure — no such key on an older compositor, or not a bool.
pub fn fetch_key_hints(client: &mut ec_ipc::Client) -> Option<bool> {
    let reply = client
        .call("get_config", json!({ "path": "ui.show-key-hints" }))
        .ok()?;
    reply.get("keys")?.as_array()?.first()?.get("value")?.as_bool()
}

/// `ui.show-key-hints` — whether the launcher and the start menu draw their
/// keyboard hint line — over one short-lived connection, like
/// [`fetch_glass`]. `None` when the socket or the key is not there; callers
/// default to showing the hints.
pub fn fetch_show_key_hints() -> Option<bool> {
    let mut client = ec_ipc::Client::connect().ok()?;
    fetch_key_hints(&mut client)
}

/// One `animations` event as a pane plays it: the style the compositor
/// resolved (preset, speed, reduce-motion and overrides already applied) and
/// the [`Motion`](crate::motion::Motion) it runs on.
#[derive(Debug, Clone, PartialEq)]
pub struct EventAnim {
    pub style: String,
    pub motion: crate::motion::Motion,
}

impl EventAnim {
    /// `style` on the pane's own motion defaults, for a compositor too old to
    /// report `animations`.
    pub fn fallback(style: &str) -> EventAnim {
        EventAnim {
            style: style.to_owned(),
            motion: crate::motion::Motion::DEFAULT,
        }
    }
}

/// `animations.events.<event>` out of a full `get_config` reply. `None` when
/// the object, the event, or its style is absent or the wrong shape — an
/// older compositor — so the caller keeps its own default. A `none` style or
/// a zero duration is [`Motion::SNAP`](crate::motion::Motion::SNAP).
pub fn parse_animation_event(reply: &serde_json::Value, event: &str) -> Option<EventAnim> {
    use crate::motion::{Curve, Motion};
    let e = reply.get("animations")?.get("events")?.get(event)?;
    let style = e.get("style")?.as_str()?.to_owned();
    let ms = e.get("duration-ms")?.as_u64()?;
    let motion = if style == "none" || ms == 0 {
        Motion::SNAP
    } else {
        Motion {
            enabled: true,
            curve: e
                .get("curve")
                .and_then(serde_json::Value::as_str)
                .and_then(Curve::parse)
                .unwrap_or_default(),
            duration: std::time::Duration::from_millis(ms),
        }
    };
    Some(EventAnim { style, motion })
}

/// [`parse_animation_event`] over a connection the caller holds. `None` on
/// any failure.
pub fn fetch_animation(client: &mut ec_ipc::Client, event: &str) -> Option<EventAnim> {
    let reply = client.call("get_config", json!({ "schema": false })).ok()?;
    parse_animation_event(&reply, event)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::motion::Curve;

    #[test]
    fn an_event_reads_its_resolved_style_and_motion() {
        let reply = json!({"animations": {"events": {
            "toast": {"style": "slide", "duration-ms": 240, "curve": "bounce"},
            "chip-add": {"style": "none", "duration-ms": 200, "curve": "spring"},
            "chip-remove": {"style": "fade", "duration-ms": 0, "curve": "spring"},
        }}});
        let t = parse_animation_event(&reply, "toast").unwrap();
        assert_eq!(t.style, "slide");
        assert_eq!(t.motion.curve, Curve::Bounce);
        assert_eq!(t.motion.duration.as_millis(), 240);
        assert!(parse_animation_event(&reply, "chip-add").unwrap().motion.snaps());
        assert!(parse_animation_event(&reply, "chip-remove")
            .unwrap()
            .motion
            .snaps());
    }

    #[test]
    fn a_compositor_without_animations_reads_none() {
        assert_eq!(parse_animation_event(&json!({"keys": []}), "toast"), None);
        assert_eq!(
            parse_animation_event(&json!({"animations": {"events": {}}}), "toast"),
            None
        );
        assert_eq!(parse_animation_event(&json!(null), "toast"), None);
    }
}
