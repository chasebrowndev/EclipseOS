// SPDX-License-Identifier: AGPL-3.0-only
//! The action path: the only way anything in `status` changes the machine.
//!
//! [`Actions`] is a cheap, cloneable handle. Every method returns at once; the
//! work runs on a short-lived thread of its own and its result comes back as
//! an [`Event`] on the [`Events`] receiver. A view calls these from a click or
//! a key and folds the events in on its next tick — never from `view()`.
//!
//! Events are their own channel rather than more [`super::Update`] variants:
//! the readings feed is matched exhaustively by every consumer (the bar, the
//! control center), and a process that never acts should not have to learn
//! about joins and pairings to keep compiling.
//!
//! All actions share one system-bus connection, and that is load-bearing:
//! BlueZ stops a discovery when the connection that started it closes, and
//! asks the pairing agent registered on the connection that called `Pair`.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::Arc;
use std::time::{Duration, Instant};

use zbus::blocking::Connection;
use zbus::fdo::RequestNameFlags;

use super::agent::{self, Agent, Pairing, Prompts};
use super::bluetooth::{self, BtDevice, BLUEZ};
use super::wifi::{self, WifiDetail, WifiNetwork};
use super::{uncached, Secret};

/// How long a discovery runs if nobody stops it. Scanning costs power and
/// radio time; a picker left open over lunch should not scan all afternoon.
const DISCOVERY_LIMIT: Duration = Duration::from_secs(60);
/// How often the device list is re-sent while discovering.
const DISCOVERY_TICK: Duration = Duration::from_secs(2);

/// Whether this process answers BlueZ's pairing questions.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PairingAgent {
    /// Register as the session's default Bluetooth agent. Exactly one process
    /// should — the bar — or the prompts land in whichever registered last.
    Register,
    /// Do not. `bt_answer` then forwards to the process that did, which is
    /// what `eclipse-secret-prompt` wants.
    None,
}

/// Why a join did not happen.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConnectError {
    /// Secured, and we have no secret for it: open the secret prompt.
    NeedsSecret,
    /// The secret we were given was refused. The profile it made is gone.
    WrongSecret,
    Failed(String),
}

/// What BlueZ wants from the human. The numbers in `Confirm`,
/// `DisplayPin` and `DisplayPasskey` are pairing codes: shown on screen, never
/// in `Debug`.
#[derive(Clone, PartialEq, Eq)]
pub enum BtPrompt {
    /// Type a PIN (legacy pairing): answer with the PIN.
    Pin,
    /// Type a six-digit passkey: answer with the digits.
    Passkey,
    /// Does the device show this number? Answer `Some("")` for yes.
    Confirm(u32),
    /// Allow this device to pair? Answer `Some("")` for yes.
    Authorize,
    /// Type this on the device. No answer is expected.
    DisplayPin(String),
    /// Type this on the device. No answer is expected.
    DisplayPasskey(u32),
}

impl std::fmt::Debug for BtPrompt {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Pin => "Pin",
            Self::Passkey => "Passkey",
            Self::Confirm(_) => "Confirm(<redacted>)",
            Self::Authorize => "Authorize",
            Self::DisplayPin(_) => "DisplayPin(<redacted>)",
            Self::DisplayPasskey(_) => "DisplayPasskey(<redacted>)",
        })
    }
}

/// The result of an action, or something BlueZ asked mid-action. Never
/// carries a secret the human typed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Event {
    WifiEnabled(bool),
    /// One row per SSID, active first, then strongest first.
    WifiNetworks(Vec<WifiNetwork>),
    /// SSIDs of saved profiles.
    WifiSaved(Vec<String>),
    WifiConnect {
        ssid: String,
        result: Result<(), ConnectError>,
    },
    /// `None` when no wifi link is up.
    WifiDetail(Option<WifiDetail>),
    /// Connected first, then paired, then by name.
    BtDevices(Vec<BtDevice>),
    BtDiscovering(bool),
    /// Open the secret prompt (or a yes/no) for `addr`, and answer with
    /// [`Actions::bt_answer`].
    BtNeedsSecret {
        addr: String,
        kind: BtPrompt,
    },
    /// BlueZ gave up on the question: close the prompt.
    BtPromptCancelled,
    BtPair {
        addr: String,
        result: Result<(), String>,
    },
    /// An action that has no event of its own failed.
    Failed {
        action: &'static str,
        reason: String,
    },
}

/// The receiving end of every action's result.
pub struct Events {
    events: Receiver<Event>,
}

impl Events {
    /// The next result, if one is waiting. Never blocks.
    pub fn try_recv(&self) -> Option<Event> {
        self.events.try_recv().ok()
    }

    /// Wait up to `timeout` for the next result. For a subscription thread,
    /// never a view.
    pub fn recv_timeout(&self, timeout: Duration) -> Option<Event> {
        self.events.recv_timeout(timeout).ok()
    }
}

struct Inner {
    system: Connection,
    events: Sender<Event>,
    /// Present when this process registered the pairing agent.
    prompts: Option<Prompts>,
    /// Bumped by every `bt_discover` call; a discovery loop that sees a newer
    /// generation than its own stops, so the latest click always wins.
    discovery: AtomicU64,
    /// Holds `org.eclipse.Services.Pairing` for as long as we live.
    _session: Option<Connection>,
}

/// The action handle. Clone it freely; every clone drives the same
/// connection and reports on the same [`Events`].
#[derive(Clone)]
pub struct Actions {
    inner: Arc<Inner>,
}

/// Connect the action path. With [`PairingAgent::Register`] this process
/// becomes BlueZ's default agent and serves the session-bus door the secret
/// prompt answers through; a machine with no BlueZ still gets wifi.
///
/// Fails only if the system bus is unreachable.
pub fn actions(agent: PairingAgent) -> zbus::Result<(Actions, Events)> {
    let (tx, events) = mpsc::channel();
    let (system, prompts, session) = match agent {
        PairingAgent::None => (Connection::system()?, None, None),
        PairingAgent::Register => {
            let prompts = Prompts::default();
            let system = zbus::blocking::connection::Builder::system()?
                .serve_at(
                    agent::AGENT_PATH,
                    Agent {
                        prompts: prompts.clone(),
                        events: tx.clone(),
                    },
                )?
                .build()?;
            register_agent(&system);
            (system, Some(prompts.clone()), serve_pairing(prompts))
        }
    };
    let actions = Actions {
        inner: Arc::new(Inner {
            system,
            events: tx,
            prompts,
            discovery: AtomicU64::new(0),
            _session: session,
        }),
    };
    Ok((actions, Events { events }))
}

/// Ask BlueZ to route pairing questions here. Without BlueZ (or with another
/// default agent already set) pairing simply falls back to BlueZ's own rules.
fn register_agent(system: &Connection) {
    let Some(manager) = uncached(system, BLUEZ, "/org/bluez", "org.bluez.AgentManager1") else {
        return;
    };
    let path = zbus::zvariant::ObjectPath::from_static_str_unchecked(agent::AGENT_PATH);
    if manager
        .call_method("RegisterAgent", &(&path, agent::CAPABILITY))
        .is_ok()
    {
        let _ = manager.call_method("RequestDefaultAgent", &(&path,));
    }
}

/// Serve the door on the session bus. `DoNotQueue`: if another process
/// already holds it, it also holds the agent, and answers must go there.
fn serve_pairing(prompts: Prompts) -> Option<Connection> {
    let session = zbus::blocking::connection::Builder::session()
        .ok()?
        .serve_at(agent::PAIRING_PATH, Pairing { prompts })
        .ok()?
        .build()
        .ok()?;
    session
        .request_name_with_flags(agent::PAIRING_NAME, RequestNameFlags::DoNotQueue.into())
        .ok()?;
    Some(session)
}

impl Actions {
    // --- wifi ---------------------------------------------------------------

    /// Scan, then send [`Event::WifiNetworks`]. Returns the current list first
    /// so the picker is never empty while the radio looks.
    pub fn wifi_scan(&self) {
        self.run("eclipse-wifi-scan", |inner| {
            inner.send(Event::WifiNetworks(wifi::networks(&inner.system)));
            wifi::scan(&inner.system);
            inner.send(Event::WifiNetworks(wifi::networks(&inner.system)));
        });
    }

    /// Send [`Event::WifiNetworks`] and [`Event::WifiEnabled`] from what
    /// NetworkManager already knows, without scanning.
    pub fn wifi_networks(&self) {
        self.run("eclipse-wifi-list", |inner| {
            if let Some(on) = wifi::enabled(&inner.system) {
                inner.send(Event::WifiEnabled(on));
            }
            inner.send(Event::WifiNetworks(wifi::networks(&inner.system)));
        });
    }

    pub fn set_wifi_enabled(&self, on: bool) {
        self.run("eclipse-wifi-radio", move |inner| {
            match wifi::set_enabled(&inner.system, on) {
                Ok(()) => inner.send(Event::WifiEnabled(on)),
                Err(reason) => inner.failed("set_wifi_enabled", reason),
            }
        });
    }

    /// Join `ssid`. A secured network with no saved profile answers
    /// `Err(NeedsSecret)` without touching anything.
    pub fn wifi_connect(&self, ssid: String) {
        self.run("eclipse-wifi-join", move |inner| {
            let result = wifi::connect(&inner.system, &ssid);
            inner.send(Event::WifiConnect { ssid, result });
            inner.send(Event::WifiNetworks(wifi::networks(&inner.system)));
        });
    }

    /// Join `ssid` with a passphrase. The value is moved into a [`Secret`]
    /// before this returns and wiped when the join finishes; it is never sent
    /// back in an event.
    pub fn wifi_connect_with_secret(&self, ssid: String, secret: String) {
        let secret = Secret::new(secret);
        self.run("eclipse-wifi-join", move |inner| {
            let result = wifi::connect_with_secret(&inner.system, &ssid, &secret);
            drop(secret);
            inner.send(Event::WifiConnect { ssid, result });
            inner.send(Event::WifiNetworks(wifi::networks(&inner.system)));
        });
    }

    pub fn wifi_disconnect(&self) {
        self.run("eclipse-wifi-drop", |inner| {
            if let Err(reason) = wifi::disconnect(&inner.system) {
                inner.failed("wifi_disconnect", reason);
            }
            inner.send(Event::WifiNetworks(wifi::networks(&inner.system)));
        });
    }

    /// Delete the saved profile(s) for `ssid`, then resend the saved list.
    pub fn wifi_forget(&self, ssid: String) {
        self.run("eclipse-wifi-forget", move |inner| {
            if let Err(reason) = wifi::forget(&inner.system, &ssid) {
                inner.failed("wifi_forget", reason);
            }
            inner.send(Event::WifiSaved(wifi::saved_ssids(&inner.system)));
            inner.send(Event::WifiNetworks(wifi::networks(&inner.system)));
        });
    }

    /// Send [`Event::WifiSaved`].
    pub fn wifi_saved(&self) {
        self.run("eclipse-wifi-saved", |inner| {
            inner.send(Event::WifiSaved(wifi::saved_ssids(&inner.system)));
        });
    }

    /// Send [`Event::WifiDetail`]. Takes about a second: the rates are two
    /// samples of the interface counters, one second apart.
    pub fn wifi_detail(&self) {
        self.run("eclipse-wifi-detail", |inner| {
            inner.send(Event::WifiDetail(wifi::detail(&inner.system)));
        });
    }

    // --- bluetooth ----------------------------------------------------------

    pub fn bt_set_powered(&self, on: bool) {
        self.run("eclipse-bt-power", move |inner| {
            if let Err(reason) = bluetooth::set_powered(&inner.system, on) {
                inner.failed("bt_set_powered", reason);
            }
            inner.send(Event::BtDevices(bluetooth::devices(&inner.system)));
        });
    }

    /// Start (up to a minute) or stop looking for nearby devices. While it
    /// runs, [`Event::BtDevices`] arrives every two seconds.
    pub fn bt_discover(&self, on: bool) {
        let generation = self.inner.discovery.fetch_add(1, Ordering::SeqCst) + 1;
        self.run("eclipse-bt-scan", move |inner| {
            let current = || inner.discovery.load(Ordering::SeqCst) == generation;
            if !on {
                let _ = bluetooth::set_discovering(&inner.system, false);
                inner.send(Event::BtDiscovering(false));
                return;
            }
            if let Err(reason) = bluetooth::set_discovering(&inner.system, true) {
                inner.failed("bt_discover", reason);
                inner.send(Event::BtDiscovering(false));
                return;
            }
            inner.send(Event::BtDiscovering(true));
            let deadline = Instant::now() + DISCOVERY_LIMIT;
            while current() && Instant::now() < deadline {
                inner.send(Event::BtDevices(bluetooth::devices(&inner.system)));
                std::thread::sleep(DISCOVERY_TICK);
            }
            // A newer call owns the radio now; leave it to that one.
            if current() {
                let _ = bluetooth::set_discovering(&inner.system, false);
                inner.send(Event::BtDiscovering(false));
                inner.send(Event::BtDevices(bluetooth::devices(&inner.system)));
            }
        });
    }

    /// Send [`Event::BtDevices`].
    pub fn bt_devices(&self) {
        self.run("eclipse-bt-list", |inner| {
            inner.send(Event::BtDevices(bluetooth::devices(&inner.system)));
        });
    }

    pub fn bt_connect(&self, addr: String) {
        self.run("eclipse-bt-connect", move |inner| {
            if let Err(reason) = bluetooth::connect(&inner.system, &addr) {
                inner.failed("bt_connect", reason);
            }
            inner.send(Event::BtDevices(bluetooth::devices(&inner.system)));
        });
    }

    pub fn bt_disconnect(&self, addr: String) {
        self.run("eclipse-bt-disconnect", move |inner| {
            if let Err(reason) = bluetooth::disconnect(&inner.system, &addr) {
                inner.failed("bt_disconnect", reason);
            }
            inner.send(Event::BtDevices(bluetooth::devices(&inner.system)));
        });
    }

    /// Pair, trust and connect. Any question BlueZ asks on the way arrives as
    /// [`Event::BtNeedsSecret`]; the outcome as [`Event::BtPair`].
    pub fn bt_pair(&self, addr: String) {
        self.run("eclipse-bt-pair", move |inner| {
            let result = bluetooth::pair(&inner.system, &addr);
            inner.send(Event::BtPair { addr, result });
            inner.send(Event::BtDevices(bluetooth::devices(&inner.system)));
        });
    }

    /// Unpair and remove.
    pub fn bt_forget(&self, addr: String) {
        self.run("eclipse-bt-forget", move |inner| {
            if let Err(reason) = bluetooth::forget(&inner.system, &addr) {
                inner.failed("bt_forget", reason);
            }
            inner.send(Event::BtDevices(bluetooth::devices(&inner.system)));
        });
    }

    /// Answer an open [`Event::BtNeedsSecret`] for `addr`: `None` refuses,
    /// `Some(pin)` answers a PIN or passkey, `Some("")` says yes to a
    /// confirmation. The value is a [`Secret`] from here on. If this process
    /// is not the agent, the answer goes to the one that is.
    pub fn bt_answer(&self, addr: String, answer: Option<String>) {
        let mut answer = answer.map(Secret::new);
        if let Some(prompts) = &self.inner.prompts {
            // The local hand-off never blocks, so it happens right here.
            match prompts.answer(&addr, answer) {
                Ok(()) => return,
                Err(unanswered) => answer = unanswered,
            }
        }
        self.run("eclipse-bt-answer", move |inner| {
            if let Err(reason) = forward_answer(&addr, answer.as_ref()) {
                inner.failed("bt_answer", reason);
            }
        });
    }

    fn run(&self, name: &'static str, work: impl FnOnce(&Inner) + Send + 'static) {
        let inner = Arc::clone(&self.inner);
        let spawned = std::thread::Builder::new()
            .name(name.to_owned())
            .spawn(move || work(&inner));
        if let Err(error) = spawned {
            self.inner.failed(name, error.to_string());
        }
    }
}

impl Inner {
    fn send(&self, event: Event) {
        // A closed receiver means the UI is gone; there is nobody to tell.
        let _ = self.events.send(event);
    }

    fn failed(&self, action: &'static str, reason: String) {
        self.send(Event::Failed { action, reason });
    }
}

/// Hand an answer to the process holding the agent, over the session bus.
fn forward_answer(addr: &str, answer: Option<&Secret>) -> Result<(), String> {
    let session = Connection::session().map_err(|error| error.to_string())?;
    let door = uncached(
        &session,
        agent::PAIRING_NAME,
        agent::PAIRING_PATH,
        agent::PAIRING_INTERFACE,
    )
    .ok_or("no pairing agent is running")?;
    let sent = match answer {
        Some(secret) => door.call_method("Answer", &(addr, secret.expose())),
        None => door.call_method("Reject", &(addr,)),
    };
    sent.map(drop).map_err(|error| match error {
        zbus::Error::MethodError(_, Some(detail), _) => detail,
        other => other.to_string(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pairing_codes_stay_out_of_debug() {
        let shown = format!(
            "{:?}",
            Event::BtNeedsSecret {
                addr: "AA:BB:CC:DD:EE:FF".to_owned(),
                kind: BtPrompt::DisplayPasskey(123_456),
            }
        );
        assert!(!shown.contains("123456"), "{shown}");
        assert!(!format!("{:?}", BtPrompt::Confirm(654_321)).contains("654321"));
        assert!(!format!("{:?}", BtPrompt::DisplayPin("0000".to_owned())).contains("0000"));
    }
}
