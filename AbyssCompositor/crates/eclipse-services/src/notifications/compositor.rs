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
//! logged the full list, and `eclipse-ctl config validate` repeats it.

use std::sync::mpsc::Sender;
use std::thread;
use std::time::Duration;

use eclipse_ipc::EventKind;
use serde_json::Value;

use super::{CloseReason, Event, Notification, Urgency};

/// The one slot config problems occupy. Reused, so a second report replaces
/// the first on screen instead of stacking.
pub const CONFIG_ID: u32 = u32::MAX;

/// Between connection attempts while the compositor is not there — under a
/// non-Abyss host, or while Abyss restarts.
const RETRY: Duration = Duration::from_secs(5);

pub(super) fn spawn(events: Sender<Event>) {
    let _ = thread::Builder::new()
        .name("eclipse-notifications-abyss".into())
        .spawn(move || run(&events));
}

fn run(events: &Sender<Event>) {
    loop {
        if let Ok(mut client) = eclipse_ipc::Client::connect() {
            if client
                .subscribe(&[EventKind::ConfigError, EventKind::Config])
                .is_ok()
                && !pump(&mut client, events)
            {
                // The GUI dropped its end; nobody is left to show anything.
                return;
            }
        }
        thread::sleep(RETRY);
    }
}

/// Forward events until the socket dies (`true`) or the GUI is gone (`false`).
fn pump(client: &mut eclipse_ipc::Client, events: &Sender<Event>) -> bool {
    loop {
        let Ok(ev) = client.wait_event() else {
            return true;
        };
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
                .unwrap_or("The config has errors; run `eclipse-ctl config validate`.")
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
        assert!(n.body.contains("eclipse-ctl config validate"));
    }
}
