// SPDX-License-Identifier: AGPL-3.0-only
//! The secret store's passphrase prompt (S-08 §2, ADR 0077). **TCB.**
//!
//! S-08 §2 has brokerd unlock "at first human login of the session, via
//! trusted UI", and lock again on screen lock. This is that trusted UI:
//! abyss is brokerd's Compositor peer, and the human types the passphrase
//! into a compositor-drawn, masked entry no client can read.
//!
//! ```text
//!   start (agents add-on on)  → Status → initialised and locked? → unlock prompt
//!   secrets_unlock_prompt     → Status → no store?  → set up: passphrase, then again → Init
//!                                      → locked?    → BeginUnlock → prompt → UnlockAnswer
//!                                      → unlocked?  → nothing
//!   screen lock               → Lock; the next session unlock prompts again
//! ```
//!
//! One SEQPACKET connection, held by a thread of its own for the life of
//! the session: brokerd talks blocking request/response, and the event loop
//! never waits on it. brokerd locks when this connection closes, so it is
//! dialled once and kept (a dropped one is redialled on the next ask).
//!
//! Choices, for the owner's review:
//! - Login prompts only when a store exists. A session with no store is not
//!   nagged; setup is asked for, from Settings → Accounts or the console.
//! - A wrong passphrase asks again with a fresh nonce and says how many
//!   tries are left; brokerd counts them and locks out, not this module.
//! - The passphrase is printable ASCII, the trusted font's range. A store
//!   set up from a TTY with anything else can still be unlocked there.
//! - The passphrase is never logged, and is wiped when the prompt's typed
//!   text drops and after it is encoded (`Secret` zeroizes).

use std::os::fd::OwnedFd;
use std::path::PathBuf;
use std::sync::mpsc;

use ec_brokerd::wire::{Request, Response, Secret, MAX_MESSAGE};
use rustix::net::{self, AddressFamily, RecvFlags, SendFlags, SocketAddrUnix, SocketFlags, SocketType};
use smithay::reexports::calloop::channel::{self, Event};
use zeroize::Zeroizing;

use super::{
    modal::{Button, Modal, Role},
    Choice,
};
use crate::state::AbyssState;

const TOKEN: u64 = (1 << 61) | (1 << 57);
/// brokerd's floor for a new passphrase is its own; this is the prompt's.
pub const MIN: usize = 8;
const MAX: usize = 64;

const BAD_CREDENTIAL: u64 = ec_brokerd::broker::Status::BadCredential as u64;
const RATE_LIMITED: u64 = ec_brokerd::broker::Status::RateLimited as u64;

#[derive(Default)]
pub struct Broker {
    tx: Option<mpsc::Sender<Request>>,
    step: Step,
    /// A prompt waiting for the seat: another prompt was up.
    queued: Option<Modal>,
    /// The prompt is up.
    up: bool,
    /// Unlocked when the screen locked: ask again once it unlocks.
    relock: bool,
    /// The first Status after start decides the login prompt.
    login: bool,
    /// The last answer was a wrong passphrase.
    wrong: bool,
}

impl std::fmt::Debug for Broker {
    // `Step::Confirm` holds the first passphrase entry: say only which step.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let step = match self.step {
            Step::Idle => "idle",
            Step::Asking => "asking",
            Step::Unlock { .. } => "unlock",
            Step::Setup => "setup",
            Step::Confirm(_) => "confirm",
        };
        f.debug_struct("Broker")
            .field("connected", &self.tx.is_some())
            .field("step", &step)
            .field("up", &self.up)
            .finish_non_exhaustive()
    }
}

#[derive(Default)]
enum Step {
    #[default]
    Idle,
    /// A request is with brokerd.
    Asking,
    Unlock {
        nonce: u64,
        passphrase: bool,
    },
    Setup,
    Confirm(Zeroizing<String>),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Note {
    None,
    Wrong(u32),
    Short,
    Mismatch,
}

/// The agents add-on is on: dial and check for the login prompt.
pub fn start(state: &mut AbyssState) {
    if state.trusted_ui.broker.tx.is_some() || socket_path().is_none_or(|p| !p.exists()) {
        return;
    }
    state.trusted_ui.broker.login = true;
    ask(state, Request::Status);
}

/// `secrets_unlock_prompt`: set up, unlock, or nothing, whichever applies.
pub fn prompt(state: &mut AbyssState) {
    if !matches!(state.trusted_ui.broker.step, Step::Idle) {
        return;
    }
    state.trusted_ui.broker.login = false;
    ask(state, Request::Status);
}

/// The screen locked: so does the store (S-08 §2).
pub fn session_locked(state: &mut AbyssState) {
    let b = &mut state.trusted_ui.broker;
    if b.tx.is_none() {
        return;
    }
    b.queued = None;
    b.step = Step::Asking;
    b.relock = true;
    ask(state, Request::Lock);
}

/// The screen unlocked: ask for the store again if locking it closed it.
pub fn session_unlocked(state: &mut AbyssState) {
    if std::mem::take(&mut state.trusted_ui.broker.relock) {
        state.trusted_ui.broker.step = Step::Idle;
        prompt(state);
    }
}

/// A prompt closed: put a queued one up.
pub fn schedule(state: &mut AbyssState) {
    if let Some(m) = state.trusted_ui.broker.queued.take() {
        show(state, m);
    }
}

pub fn owns(state: &AbyssState, token: u64) -> bool {
    token == TOKEN && state.trusted_ui.broker.up
}

pub fn answer(state: &mut AbyssState, choice: Choice) {
    let b = &mut state.trusted_ui.broker;
    b.up = false;
    let step = std::mem::take(&mut b.step);
    let typed = choice.typed.as_ref().map(|t| t.as_str()).unwrap_or("");
    if choice.role == Role::Safe {
        if let Step::Unlock { nonce, .. } = step {
            state.trusted_ui.broker.step = Step::Asking;
            ask(
                state,
                Request::UnlockAnswer {
                    nonce,
                    passphrase: None,
                    cancel: true,
                },
            );
        }
        return;
    }
    match step {
        Step::Unlock { nonce, passphrase } => {
            let pass = passphrase.then(|| Secret::new(typed.as_bytes().to_vec()));
            state.trusted_ui.broker.step = Step::Asking;
            ask(
                state,
                Request::UnlockAnswer {
                    nonce,
                    passphrase: pass,
                    cancel: false,
                },
            );
        }
        Step::Setup => {
            if typed.chars().count() < MIN {
                setup(state, Note::Short);
            } else {
                state.trusted_ui.broker.step = Step::Confirm(Zeroizing::new(typed.to_owned()));
                if let Some(m) = confirm_modal() {
                    show(state, m);
                }
            }
        }
        Step::Confirm(first) => {
            if typed != first.as_str() {
                setup(state, Note::Mismatch);
            } else {
                state.trusted_ui.broker.step = Step::Asking;
                ask(
                    state,
                    Request::Init {
                        pass: Some(Secret::new(first.as_bytes().to_vec())),
                    },
                );
            }
        }
        Step::Idle | Step::Asking => {}
    }
}

// ---- brokerd's answers ------------------------------------------------------

fn reply(state: &mut AbyssState, resp: Option<Response>) {
    let login = std::mem::take(&mut state.trusted_ui.broker.login);
    let Some(resp) = resp else {
        tracing::warn!("brokerd unreachable; the secret store prompt is not shown");
        state.trusted_ui.broker.step = Step::Idle;
        return;
    };
    match resp {
        Response::State {
            unlocked,
            initialised,
            ..
        } => {
            if unlocked || (login && !initialised) {
                state.trusted_ui.broker.step = Step::Idle;
            } else if !initialised {
                setup(state, Note::None);
            } else {
                state.trusted_ui.broker.step = Step::Asking;
                ask(state, Request::BeginUnlock);
            }
        }
        Response::Unlock {
            nonce,
            passphrase,
            attempts_left,
            ..
        } => {
            let note = if std::mem::take(&mut state.trusted_ui.broker.wrong) {
                Note::Wrong(attempts_left)
            } else {
                Note::None
            };
            state.trusted_ui.broker.step = Step::Unlock { nonce, passphrase };
            if let Some(m) = unlock_modal(passphrase, note) {
                show(state, m);
            }
        }
        Response::Unlocked(true) | Response::Ok => {
            if !state.trusted_ui.broker.relock {
                tracing::info!("secret store unlocked");
            }
            state.trusted_ui.broker.step = Step::Idle;
        }
        Response::Unlocked(false) => state.trusted_ui.broker.step = Step::Idle,
        Response::Err(BAD_CREDENTIAL) => {
            // The answer used the nonce up: a fresh prompt, counted down.
            state.trusted_ui.broker.wrong = true;
            state.trusted_ui.broker.step = Step::Asking;
            ask(state, Request::BeginUnlock);
        }
        Response::Err(RATE_LIMITED) => {
            tracing::warn!("secret store unlock is locked out for now");
            state.trusted_ui.broker.step = Step::Idle;
            if let Some(m) = lockout_modal() {
                show(state, m);
            }
        }
        other => {
            tracing::warn!(?other, "secret store prompt: unexpected brokerd answer");
            state.trusted_ui.broker.step = Step::Idle;
        }
    }
}

// ---- the prompts ------------------------------------------------------------

fn setup(state: &mut AbyssState, note: Note) {
    state.trusted_ui.broker.step = Step::Setup;
    let warning = match note {
        Note::Short => Some("Too short: use at least 8 characters."),
        Note::Mismatch => Some("The two entries did not match. Start again."),
        _ => None,
    };
    let m = Modal::new(
        TOKEN,
        "Set up your secret store",
        warning,
        "Agents' account logins and keys are kept here, sealed with a passphrase you choose. \
You type it here once per session. It cannot be recovered.",
        "Choose a passphrase:",
        "",
        buttons("Next"),
    );
    if let Ok(m) = m {
        show(state, m.with_secret_entry(MAX));
    }
}

fn confirm_modal() -> Option<Modal> {
    Modal::new(
        TOKEN,
        "Set up your secret store",
        None,
        "Type the same passphrase again.",
        "Passphrase again:",
        "",
        buttons("Set up"),
    )
    .ok()
    .map(|m| m.with_secret_entry(MAX))
}

fn unlock_modal(passphrase: bool, note: Note) -> Option<Modal> {
    let warning = match note {
        Note::Wrong(1) => Some("Wrong passphrase. One try left before a lockout."),
        Note::Wrong(_) => Some("Wrong passphrase. Try again."),
        _ => None,
    };
    let m = Modal::new(
        TOKEN,
        "Unlock your secret store",
        warning,
        "Agents need it to sign in to your accounts. It locks again when the screen locks.",
        if passphrase { "Passphrase:" } else { "Unlock:" },
        "",
        buttons("Unlock"),
    )
    .ok()?;
    Some(if passphrase { m.with_secret_entry(MAX) } else { m })
}

fn lockout_modal() -> Option<Modal> {
    Modal::new(
        TOKEN,
        "Unlock is paused",
        Some("Too many wrong passphrases."),
        "Wait a few minutes, then unlock again from Settings or the console.",
        "Details:",
        "See: journalctl --user -u ec-brokerd",
        vec![Button {
            label: "OK",
            role: Role::Safe,
        }],
    )
    .ok()
}

fn buttons(go: &'static str) -> Vec<Button> {
    vec![
        Button {
            label: "Not now",
            role: Role::Safe,
        },
        Button {
            label: go,
            role: Role::Other,
        },
    ]
}

fn show(state: &mut AbyssState, m: Modal) {
    if super::open(state, m.clone()) {
        state.trusted_ui.broker.up = true;
    } else {
        state.trusted_ui.broker.queued = Some(m);
    }
}

// ---- the connection ---------------------------------------------------------

fn socket_path() -> Option<PathBuf> {
    let run = std::env::var_os("XDG_RUNTIME_DIR")?;
    Some(PathBuf::from(run).join("eclipse/brokerd.sock"))
}

fn ask(state: &mut AbyssState, req: Request) {
    if state.trusted_ui.broker.tx.is_none() && !spawn(state) {
        reply(state, None);
        return;
    }
    let sent = state
        .trusted_ui
        .broker
        .tx
        .as_ref()
        .is_some_and(|tx| tx.send(req).is_ok());
    if !sent {
        state.trusted_ui.broker.tx = None;
        reply(state, None);
    }
}

fn spawn(state: &mut AbyssState) -> bool {
    let (req_tx, req_rx) = mpsc::channel::<Request>();
    let (resp_tx, resp_rx) = channel::channel::<Option<Response>>();
    let source = state.loop_handle.insert_source(resp_rx, |ev, _, state| match ev {
        Event::Msg(r) => reply(state, r),
        Event::Closed => state.trusted_ui.broker.tx = None,
    });
    if let Err(e) = source {
        tracing::warn!(%e, "brokerd prompt: no event source");
        return false;
    }
    let thread = std::thread::Builder::new()
        .name("ec-brokerd-ui".into())
        .spawn(move || {
            let mut conn: Option<OwnedFd> = None;
            for req in req_rx {
                let resp = call(&mut conn, &req);
                drop(req);
                if resp_tx.send(resp).is_err() {
                    return;
                }
            }
        });
    if thread.is_err() {
        return false;
    }
    state.trusted_ui.broker.tx = Some(req_tx);
    true
}

/// One round trip, dialling first if there is no connection. A failure
/// drops the connection so the next ask redials.
fn call(conn: &mut Option<OwnedFd>, req: &Request) -> Option<Response> {
    if conn.is_none() {
        *conn = dial();
    }
    let fd = conn.as_ref()?;
    let r = round_trip(fd, req);
    if r.is_none() {
        *conn = None;
    }
    r
}

fn dial() -> Option<OwnedFd> {
    let fd = net::socket_with(
        AddressFamily::UNIX,
        SocketType::SEQPACKET,
        SocketFlags::CLOEXEC,
        None,
    )
    .ok()?;
    let addr = SocketAddrUnix::new(&socket_path()?).ok()?;
    net::connect(&fd, &addr).ok()?;
    Some(fd)
}

fn round_trip(fd: &OwnedFd, req: &Request) -> Option<Response> {
    // The encoding of Init and UnlockAnswer holds the passphrase.
    let bytes = Zeroizing::new(req.encode());
    net::send(fd, &bytes, SendFlags::NOSIGNAL).ok()?;
    let mut buf = Zeroizing::new(vec![0u8; MAX_MESSAGE]);
    let (n, full) = loop {
        match net::recv(fd, &mut buf[..], RecvFlags::TRUNC) {
            Ok(r) => break r,
            Err(rustix::io::Errno::INTR) => {}
            Err(_) => return None,
        }
    };
    if n == 0 || full > buf.len() {
        return None;
    }
    Response::decode(&buf[..n]).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_prompt_builds_and_masks_its_entry() {
        for note in [Note::None, Note::Wrong(1), Note::Wrong(3)] {
            let m = unlock_modal(true, note).unwrap();
            assert!(m.takes_text());
            assert!(format!("{m:?}").contains("masked: true"));
        }
        assert!(!unlock_modal(false, Note::None).unwrap().takes_text());
        assert!(format!("{:?}", confirm_modal().unwrap()).contains("masked: true"));
    }

    #[test]
    fn the_typed_passphrase_never_prints() {
        use super::super::modal::{apply, Key};
        let mut m = unlock_modal(true, Note::None).unwrap();
        for c in "hunter22".chars() {
            apply(&mut m, 1, Key::Char(c));
        }
        assert_eq!(m.typed().unwrap().as_str(), "hunter22");
        assert!(!format!("{m:?}").contains("hunter"));
    }
}
