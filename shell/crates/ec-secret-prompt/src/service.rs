// SPDX-License-Identifier: AGPL-3.0-only
//! The one call that hands the secret on (ADR 0053).
//!
//! Wifi goes to NetworkManager through `ec_services::status::Actions`,
//! and the answer is the join's own result. Bluetooth goes to whichever
//! process holds the pairing agent (`ec-pairing`), over the
//! session-bus door `org.eclipse.Services.Pairing`. An API key goes to
//! `ec-secret account add-key <account> --stdin` (ADR 0077), which stores it
//! with brokerd as the owner. All three block, so all run off the UI thread
//! (see `app::update`).
//!
//! The copies zbus makes while marshalling are out of reach; every copy this
//! module owns is wiped before it returns.

use std::time::{Duration, Instant};

use ec_services::status::{self, ConnectError, Event, PairingAgent};

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
        Target::ApiKey { account } => add_key(account, secret),
    }
}

/// The secret CLI, resolved from `PATH` like every other DE spawn.
const EC_SECRET: &str = "ec-secret";

/// Hand the key to `ec-secret` on its stdin — one line, trimmed, then EOF —
/// and read the verdict off its exit status. The key is never an argument
/// (argv is world-readable in `/proc`) and never in what this returns.
fn add_key(account: &str, secret: String) -> Result<(), String> {
    use std::io::Write;
    use std::process::{Command, Stdio};

    let spawned = Command::new(EC_SECRET)
        .args(["account", "add-key", account, "--stdin"])
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn();
    let mut child = match spawned {
        Ok(c) => c,
        Err(e) => {
            wipe(secret);
            return Err(if e.kind() == std::io::ErrorKind::NotFound {
                "ec-secret is not installed.".to_owned()
            } else {
                "Could not start ec-secret.".to_owned()
            });
        }
    };
    // Written from a slice of the one copy, so no second copy is made; the
    // pipe closes when `stdin` drops at the end of the block, which is the
    // EOF the CLI reads to.
    if let Some(mut stdin) = child.stdin.take() {
        let _ = stdin
            .write_all(secret.trim().as_bytes())
            .and_then(|()| stdin.write_all(b"\n"));
    }
    wipe(secret);
    match child.wait_with_output() {
        Ok(out) if out.status.success() => Ok(()),
        Ok(out) => Err(refusal(&String::from_utf8_lossy(&out.stderr))),
        Err(_) => Err("ec-secret did not finish.".to_owned()),
    }
}

/// `ec-secret`'s stderr as one sentence for the window. Its lines are
/// `ec-secret: <why>`; the states a human can fix get plain words, anything
/// else is shown as the CLI said it. The CLI never prints a value.
fn refusal(stderr: &str) -> String {
    let why = stderr
        .lines()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .map(|l| l.trim_start_matches("ec-secret:").trim())
        .unwrap_or_default();
    if why.contains("cannot reach brokerd") || why.contains("closed the connection") {
        "The secret broker isn't running.".to_owned()
    } else if why.contains("not initialised") {
        "The secret store isn't set up yet.".to_owned()
    } else if why.contains("locked") {
        "The secret store is locked. Unlock it first.".to_owned()
    } else if why.is_empty() {
        "The key was not stored.".to_owned()
    } else {
        why.to_owned()
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

#[cfg(test)]
mod tests {
    use super::refusal;

    #[test]
    fn the_clis_refusals_read_as_sentences() {
        assert_eq!(
            refusal("ec-secret: cannot reach brokerd (no socket)\nis ec-brokerd running?\n"),
            "The secret broker isn't running."
        );
        assert_eq!(
            refusal("ec-secret: brokerd refused: broker_locked (run: ec-secret unlock)"),
            "The secret store is locked. Unlock it first."
        );
        assert_eq!(refusal(""), "The key was not stored.");
        assert_eq!(refusal("ec-secret: value too large\n"), "value too large");
    }
}
