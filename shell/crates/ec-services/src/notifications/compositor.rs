// SPDX-License-Identifier: AGPL-3.0-only
//! The compositor's `config-error` event, shown as a notification.
//!
//! Abyss reports a config it could not honour on the control socket's
//! `config-error` event — a failed hot reload, and, after a startup that
//! dropped invalid nodes, a replay to every new subscriber (ADR 0064). The
//! event has no reader of its own on screen, so this thread turns it into one
//! notification the stack renders like any other: one message, however many
//! errors, replaced by the next and withdrawn when a reload succeeds.
//!
//! It posts straight into the GUI's channel rather than over the bus, under a
//! fixed id outside the range the D-Bus side hands out in practice (it counts
//! up from 1). Nothing but a text line is carried; the compositor already
//! logged the full list, and `ec-ctl config validate` repeats it.

use std::sync::mpsc::Sender;
use std::thread;
use std::time::Duration;

use ec_ipc::EventKind;
use serde_json::Value;

use super::{share, CloseReason, Event, Notification, Share, Urgency};

/// The one slot config problems occupy. Reused, so a second report replaces
/// the first on screen instead of stacking.
pub const CONFIG_ID: u32 = u32::MAX;

/// Between connection attempts while the compositor is not there — under a
/// non-Abyss host, or while Abyss restarts.
const RETRY: Duration = Duration::from_secs(5);

pub(super) fn spawn(events: Sender<Event>, share: Share) {
    let _ = thread::Builder::new()
        .name("eclipse-notifications-abyss".into())
        .spawn(move || run(&events, &share));
}

fn run(events: &Sender<Event>, share: &Share) {
    loop {
        if let Ok(mut client) = ec_ipc::Client::connect() {
            if client
                .subscribe(&[EventKind::ConfigError, EventKind::Config, EventKind::Capture])
                .is_ok()
            {
                // The compositor replays a running share on subscribe, so the
                // flag is right from here. The mode is read once now and again
                // whenever a reload lands.
                share.set_active(false);
                refresh_mode(&mut client, share);
                if !pump(&mut client, events, share) {
                    // The GUI dropped its end; nobody is left to show anything.
                    return;
                }
            }
        }
        // Without the socket nothing says a share ended: assume one runs.
        share.lost();
        thread::sleep(RETRY);
    }
}

/// `ui.capture-toasts`; an older compositor without the key leaves the default.
fn refresh_mode(client: &mut ec_ipc::Client, share: &Share) {
    if let Some(mode) = client
        .call("get_config", serde_json::json!({"path": "ui.capture-toasts"}))
        .ok()
        .and_then(|r| share::mode_of(&r))
    {
        share.set_mode(mode);
    }
}

/// Forward events until the socket dies (`true`) or the GUI is gone (`false`).
fn pump(client: &mut ec_ipc::Client, events: &Sender<Event>, share: &Share) -> bool {
    loop {
        let Ok(ev) = client.wait_event() else {
            return true;
        };
        match ev.kind {
            EventKind::Capture => share.on_capture(&ev.data),
            EventKind::Config => refresh_mode(client, share),
            _ => {}
        }
        if let Some(out) = to_event(ev.kind, &ev.data) {
            if events.send(out).is_err() {
                return false;
            }
        }
    }
}

/// `config-error` posts (or replaces) the notice; `config` — a load that took —
/// withdraws it. Anything else is not ours.
fn to_event(kind: EventKind, data: &Value) -> Option<Event> {
    match kind {
        EventKind::ConfigError => {
            let body = data
                .get("summary")
                .and_then(Value::as_str)
                .filter(|s| !s.is_empty())
                .unwrap_or("The config has errors; run `ec-ctl config validate`.")
                .to_owned();
            Some(Event::Posted(Box::new(Notification {
                id: CONFIG_ID,
                app_name: "abyss".into(),
                summary: "Config problem".into(),
                body,
                icon: String::new(),
                // Critical so it waits for the human: at startup nobody is
                // looking yet, and a notice that expired unseen is the silent
                // failure ADR 0064 exists to remove.
                urgency: Urgency::Critical,
                actions: Vec::new(),
                expires_in: None,
                transient: false,
                group: "abyss".into(),
                ..Notification::default()
            })))
        }
        EventKind::Config => Some(Event::Closed {
            id: CONFIG_ID,
            reason: CloseReason::ByCall,
        }),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn a_config_error_is_one_persistent_notice_and_a_good_load_withdraws_it() {
        let data = json!({"summary": "abyss.kdl: 1 problem ignored \u{2014} line 14: x", "errors": [{}, {}]});
        let Some(Event::Posted(n)) = to_event(EventKind::ConfigError, &data) else {
            panic!("config-error must post");
        };
        assert_eq!(n.id, CONFIG_ID);
        assert_eq!(n.body, "abyss.kdl: 1 problem ignored \u{2014} line 14: x");
        assert_eq!(n.expires_in, None);

        assert_eq!(
            to_event(EventKind::Config, &json!({})),
            Some(Event::Closed {
                id: CONFIG_ID,
                reason: CloseReason::ByCall
            })
        );
        assert_eq!(to_event(EventKind::Window, &json!({})), None);
    }

    #[test]
    fn an_old_compositor_without_a_summary_still_gets_a_notice() {
        let Some(Event::Posted(n)) = to_event(EventKind::ConfigError, &json!({"errors": []})) else {
            panic!("config-error must post");
        };
        assert!(n.body.contains("ec-ctl config validate"));
    }
}
