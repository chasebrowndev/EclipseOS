// SPDX-License-Identifier: AGPL-3.0-only
//! The BlueZ pairing agent (`org.bluez.Agent1`), and the session-bus door the
//! secret prompt answers it through.
//!
//! BlueZ asks the agent registered by whoever called `Pair`, so the agent
//! lives on the same system connection the actions use. When BlueZ wants a
//! PIN, a passkey or a yes/no, the agent parks the call, tells the UI with
//! [`Event::BtNeedsSecret`], and waits for [`Prompts::answer`]. Parking is an
//! `await`, not a blocked thread: zbus runs each incoming call as its own
//! task, so a human taking thirty seconds to find a PIN holds up nothing else
//! on the connection.
//!
//! The PIN itself is typed into `eclipse-secret-prompt`, a separate process
//! (ADR 0053). It reaches this process over the session bus — `Pairing.Answer`
//! — which is only accepted while a prompt for that exact device is pending.

use std::future::Future;
use std::pin::Pin;
use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex, PoisonError};
use std::task::{Context, Poll, Waker};

use zbus::interface;
use zbus::zvariant::ObjectPath;

use super::{BtPrompt, Event, Secret};

pub(super) const AGENT_PATH: &str = "/org/eclipse/Services/BluetoothAgent";
/// "KeyboardDisplay": we can show a number and take one typed. Asking for
/// less makes BlueZ fall back to just-works pairing more often, which is the
/// weaker bond.
pub(super) const CAPABILITY: &str = "KeyboardDisplay";

pub(super) const PAIRING_NAME: &str = "org.eclipse.Services.Pairing";
pub(super) const PAIRING_PATH: &str = "/org/eclipse/Services/Pairing";
pub(super) const PAIRING_INTERFACE: &str = "org.eclipse.Services.Pairing";

#[derive(zbus::DBusError, Debug)]
#[zbus(prefix = "org.bluez.Error")]
pub(super) enum AgentError {
    #[zbus(error)]
    ZBus(zbus::Error),
    Rejected(String),
    Canceled(String),
}

/// What the human said. A secret is only ever a PIN or a passkey.
enum Reply {
    Accept(Option<Secret>),
    Reject,
}

/// A one-shot hand-off from whoever answers to the parked agent call.
#[derive(Default)]
struct Slot {
    state: Mutex<(Option<Reply>, Option<Waker>)>,
}

impl Slot {
    fn fill(&self, reply: Reply) {
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        state.0 = Some(reply);
        if let Some(waker) = state.1.take() {
            waker.wake();
        }
    }
}

struct Wait(Arc<Slot>);

impl Future for Wait {
    type Output = Reply;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Reply> {
        let mut state = self.0.state.lock().unwrap_or_else(PoisonError::into_inner);
        match state.0.take() {
            Some(reply) => Poll::Ready(reply),
            None => {
                state.1 = Some(cx.waker().clone());
                Poll::Pending
            }
        }
    }
}

struct Pending {
    addr: String,
    slot: Arc<Slot>,
}

/// The one prompt BlueZ has open, if any. BlueZ runs one pairing at a time,
/// so one slot is the whole state; a second request cancels the first.
#[derive(Clone, Default)]
pub(super) struct Prompts {
    pending: Arc<Mutex<Option<Pending>>>,
}

impl Prompts {
    /// Hand the human's answer to the parked call for `addr`. If no prompt
    /// for that device is open here the answer comes back untouched, and the
    /// caller tries the process that does have it.
    pub(super) fn answer(&self, addr: &str, reply: Option<Secret>) -> Result<(), Option<Secret>> {
        let mut pending = self.pending.lock().unwrap_or_else(PoisonError::into_inner);
        match pending.take() {
            Some(open) if open.addr.eq_ignore_ascii_case(addr) => {
                open.slot.fill(match reply {
                    Some(secret) => Reply::Accept(Some(secret)),
                    None => Reply::Reject,
                });
                Ok(())
            }
            other => {
                *pending = other;
                Err(reply)
            }
        }
    }

    fn open(&self, addr: String) -> Wait {
        let slot = Arc::new(Slot::default());
        let previous = self
            .pending
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .replace(Pending {
                addr,
                slot: Arc::clone(&slot),
            });
        if let Some(previous) = previous {
            previous.slot.fill(Reply::Reject);
        }
        Wait(slot)
    }

    fn cancel(&self) {
        if let Some(open) = self.pending.lock().unwrap_or_else(PoisonError::into_inner).take() {
            open.slot.fill(Reply::Reject);
        }
    }
}

pub(super) struct Agent {
    pub(super) prompts: Prompts,
    pub(super) events: Sender<Event>,
}

impl Agent {
    async fn ask(&self, device: &ObjectPath<'_>, kind: BtPrompt) -> Result<Option<Secret>, AgentError> {
        let addr = address(device)?;
        let wait = self.prompts.open(addr.clone());
        let _ = self.events.send(Event::BtNeedsSecret { addr, kind });
        match wait.await {
            Reply::Accept(secret) => Ok(secret),
            Reply::Reject => Err(rejected()),
        }
    }

    async fn confirm(&self, device: &ObjectPath<'_>, kind: BtPrompt) -> Result<(), AgentError> {
        self.ask(device, kind).await.map(drop)
    }
}

#[interface(name = "org.bluez.Agent1")]
impl Agent {
    async fn release(&self) {
        self.prompts.cancel();
    }

    async fn request_pin_code(&self, device: ObjectPath<'_>) -> Result<String, AgentError> {
        let secret = self.ask(&device, BtPrompt::Pin).await?.ok_or_else(rejected)?;
        let pin = secret.expose();
        // Agent1: 1–16 alphanumeric characters.
        if pin.is_empty() || pin.len() > 16 || !pin.chars().all(|c| c.is_ascii_alphanumeric()) {
            return Err(rejected());
        }
        // This copy goes into zbus's message buffer, which is zbus's to free;
        // ours is wiped when `secret` drops at the end of this call.
        Ok(pin.to_owned())
    }

    async fn display_pin_code(&self, device: ObjectPath<'_>, pincode: String) -> Result<(), AgentError> {
        let addr = address(&device)?;
        let _ = self.events.send(Event::BtNeedsSecret {
            addr,
            kind: BtPrompt::DisplayPin(pincode),
        });
        Ok(())
    }

    async fn request_passkey(&self, device: ObjectPath<'_>) -> Result<u32, AgentError> {
        let secret = self.ask(&device, BtPrompt::Passkey).await?.ok_or_else(rejected)?;
        let text = secret.expose();
        if text.is_empty() || text.len() > 6 || !text.bytes().all(|b| b.is_ascii_digit()) {
            return Err(rejected());
        }
        text.parse().map_err(|_| rejected())
    }

    async fn display_passkey(
        &self,
        device: ObjectPath<'_>,
        passkey: u32,
        _entered: u16,
    ) -> Result<(), AgentError> {
        let addr = address(&device)?;
        let _ = self.events.send(Event::BtNeedsSecret {
            addr,
            kind: BtPrompt::DisplayPasskey(passkey),
        });
        Ok(())
    }

    async fn request_confirmation(&self, device: ObjectPath<'_>, passkey: u32) -> Result<(), AgentError> {
        self.confirm(&device, BtPrompt::Confirm(passkey)).await
    }

    async fn request_authorization(&self, device: ObjectPath<'_>) -> Result<(), AgentError> {
        self.confirm(&device, BtPrompt::Authorize).await
    }

    /// A device asking to use a service without having been trusted. Devices
    /// we pair are trusted at once and never get here, so anything that does
    /// is a stranger: refuse without asking, rather than train the human to
    /// click "yes" on a prompt they did not start.
    async fn authorize_service(&self, _device: ObjectPath<'_>, _uuid: String) -> Result<(), AgentError> {
        Err(rejected())
    }

    async fn cancel(&self) {
        self.prompts.cancel();
        let _ = self.events.send(Event::BtPromptCancelled);
    }
}

/// The session-bus door `eclipse-secret-prompt` answers through.
pub(super) struct Pairing {
    pub(super) prompts: Prompts,
}

#[interface(name = "org.eclipse.Services.Pairing")]
impl Pairing {
    /// Answer the open prompt for `addr`. `value` is the PIN or passkey, or
    /// empty to accept a confirmation. Fails when no prompt for that device is
    /// open, so a stray caller cannot pre-load an answer.
    fn answer(&self, addr: String, value: String) -> zbus::fdo::Result<()> {
        let secret = Secret::new(value);
        if self.prompts.answer(&addr, Some(secret)).is_ok() {
            Ok(())
        } else {
            Err(zbus::fdo::Error::Failed("no pairing prompt open".to_owned()))
        }
    }

    fn reject(&self, addr: String) -> zbus::fdo::Result<()> {
        if self.prompts.answer(&addr, None).is_ok() {
            Ok(())
        } else {
            Err(zbus::fdo::Error::Failed("no pairing prompt open".to_owned()))
        }
    }
}

/// `/org/bluez/hci0/dev_AA_BB_CC_DD_EE_FF` → `AA:BB:CC:DD:EE:FF`. Read off the
/// path rather than asked of BlueZ: a bus call from inside a call BlueZ is
/// waiting on is a call that can wait on itself.
fn address(device: &ObjectPath<'_>) -> Result<String, AgentError> {
    parse_address(device.as_str()).ok_or_else(rejected)
}

fn parse_address(path: &str) -> Option<String> {
    let tail = path.rsplit('/').next()?.strip_prefix("dev_")?;
    let parts = tail.split('_').collect::<Vec<_>>();
    let well_formed = parts.len() == 6
        && parts
            .iter()
            .all(|part| part.len() == 2 && part.bytes().all(|b| b.is_ascii_hexdigit()));
    well_formed.then(|| parts.join(":"))
}

fn rejected() -> AgentError {
    AgentError::Rejected("rejected".to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn device_paths_name_their_address() {
        assert_eq!(
            parse_address("/org/bluez/hci0/dev_AA_BB_CC_DD_EE_0F").as_deref(),
            Some("AA:BB:CC:DD:EE:0F")
        );
        assert_eq!(parse_address("/org/bluez/hci0"), None);
        assert_eq!(parse_address("/org/bluez/hci0/dev_AA_BB"), None);
    }

    #[test]
    fn an_answer_only_lands_on_the_device_that_asked() {
        let prompts = Prompts::default();
        let wait = prompts.open("AA:BB:CC:DD:EE:FF".to_owned());
        assert!(prompts.answer("11:22:33:44:55:66", None).is_err());
        assert!(prompts
            .answer("aa:bb:cc:dd:ee:ff", Some(Secret::new("0000".to_owned())))
            .is_ok());
        // Nothing is open any more, so a second answer has nowhere to go.
        assert!(prompts.answer("AA:BB:CC:DD:EE:FF", None).is_err());
        let state = wait.0.state.lock().unwrap();
        assert!(matches!(state.0, Some(Reply::Accept(Some(_)))));
    }

    #[test]
    fn a_second_prompt_rejects_the_first() {
        let prompts = Prompts::default();
        let first = prompts.open("AA:BB:CC:DD:EE:FF".to_owned());
        let _second = prompts.open("11:22:33:44:55:66".to_owned());
        let state = first.0.state.lock().unwrap();
        assert!(matches!(state.0, Some(Reply::Reject)));
    }
}
