// SPDX-License-Identifier: AGPL-3.0-only
//! The one call that hands the secret on (ADR 0053).
//!
//! Wifi goes to NetworkManager through `eclipse_services::status::Actions`,
//! and the answer is the join's own result. Bluetooth goes to whichever
//! process holds the pairing agent (the taskbar's supervisor), over the
//! session-bus door `org.eclipse.Services.Pairing`. Both block, so both run
//! off the UI thread (see `app::update`).
//!
//! The copies zbus makes while marshalling are out of reach; every copy this
//! module owns is wiped before it returns.

use std::time::{Duration, Instant};

use eclipse_services::status::{self, ConnectError, Event, PairingAgent};

use crate::{wipe, Target};

/// Longer than the service's own join timeout, so its verdict — not ours —
/// is the one the window shows.
const JOIN_WAIT: Duration = Duration::from_secs(50);

const PAIRING_NAME: &str = "org.eclipse.Services.Pairing";
const PAIRING_PATH: &str = "/org/eclipse/Services/Pairing";

/// Send `secret` for `target`, taking ownership so it can be wiped. `Err`
/// carries a sentence for the window; it never contains the secret.
///
/// A yes to a confirmation or an authorization is the empty answer, which is
/// what the door reads as "accept". A `Show` window never calls this: BlueZ
/// opened no request for it to answer.
pub fn submit(target: &Target, secret: String) -> Result<(), String> {
    match target {
        Target::Wifi { ssid } => join(ssid, secret),
        Target::Bluetooth { addr, .. } => {
            let sent = answer(addr, &secret);
            wipe(secret);
            sent
        }
    }
}

/// Tell the agent the human walked away, so BlueZ stops waiting on a prompt
/// nobody is looking at.
pub fn reject(target: &Target) {
    if !target.answers() {
        return;
    }
    if let Target::Bluetooth { addr, .. } = target {
        let _ = door().and_then(|d| d.call_method("Reject", &(addr.as_str(),)).map_err(said));
    }
}

fn join(ssid: &str, secret: String) -> Result<(), String> {
    let Ok((actions, events)) = status::actions(PairingAgent::None) else {
        wipe(secret);
        return Err("The network service is not running.".to_owned());
    };
    // `Actions` takes the secret and wipes it once NetworkManager has it.
    actions.wifi_connect_with_secret(ssid.to_owned(), secret);
    let deadline = Instant::now() + JOIN_WAIT;
    while let Some(left) = deadline.checked_duration_since(Instant::now()) {
        match events.recv_timeout(left) {
            Some(Event::WifiConnect { ssid: joined, result }) if joined == ssid => {
                return result.map_err(|e| match e {
                    ConnectError::WrongSecret => "Wrong password.".to_owned(),
                    ConnectError::NeedsSecret => "The network refused the password.".to_owned(),
                    ConnectError::Failed(reason) => reason,
                });
            }
            Some(Event::Failed { reason, .. }) => return Err(reason),
            Some(_) => {}
            None => break,
        }
    }
    Err("The network did not answer.".to_owned())
}

fn answer(addr: &str, value: &str) -> Result<(), String> {
    door()?
        .call_method("Answer", &(addr, value))
        .map(drop)
        .map_err(said)
}

fn door() -> Result<zbus::blocking::Proxy<'static>, String> {
    let session = zbus::blocking::Connection::session().map_err(|_| "No session bus.".to_owned())?;
    zbus::blocking::proxy::Builder::new(&session)
        .destination(PAIRING_NAME)
        .and_then(|b| b.path(PAIRING_PATH))
        .and_then(|b| b.interface(PAIRING_NAME))
        .map(|b| b.cache_properties(zbus::proxy::CacheProperties::No))
        .and_then(|b| b.build())
        .map_err(|_| "No pairing agent is running.".to_owned())
}

/// The agent's own refusal, or a plain one. A bus error names the method and
/// the device, never the argument.
fn said(error: zbus::Error) -> String {
    match error {
        zbus::Error::MethodError(_, Some(detail), _) => detail,
        zbus::Error::MethodError(..) => "The pairing was refused.".to_owned(),
        _ => "No pairing agent is running.".to_owned(),
    }
}
