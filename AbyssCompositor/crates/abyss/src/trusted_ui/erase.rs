// SPDX-License-Identifier: AGPL-3.0-only
//! The destructive-system-action confirmation (COMP-10 §3.10, ADR 0061), an
//! owner of the modal primitive. **TCB.**
//!
//! A root service (the installer's helper, D-07 §6) asks over [`super::socket`]
//! for a human-seat allow/deny before it erases a disk. The answer comes only
//! from [`super::key`], reached from the physical keyboard filter: pointer
//! clicks on this prompt are dropped (`super::button`), and injected input
//! never reaches a prompt at all. Everything but an explicit Erase is Deny.

use std::{io::Write, net::Shutdown, os::unix::net::UnixStream};

use super::{Button, Choice, Modal, Role};
use crate::state::AbyssState;

/// Longest by-id name accepted. Real ones are well under this.
pub const MAX_DISK: usize = 200;
pub const MAX_MODEL: usize = 64;

/// Erase prompts count up from the top bit, so they never share a token
/// with the command-approval prompts that count up from 1.
pub(super) const TOKEN_BASE: u64 = 1 << 63;

/// What the requester says it is about to do. Shown as its words, not ours:
/// the compositor cannot verify any of it (ADR 0061).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Request {
    /// `/dev/disk/by-id` name, charset-checked.
    pub disk: String,
    /// Already reduced by `text::sanitize_line`.
    pub model: String,
    pub size_bytes: u64,
}

impl Request {
    /// Strict: the four known fields and nothing else, right types, bounded
    /// lengths. `None` is a Deny at the door.
    pub fn parse(line: &[u8]) -> Option<Request> {
        let v: serde_json::Value = serde_json::from_slice(line).ok()?;
        let o = v.as_object()?;
        if o.len() != 4 || o.get("action")?.as_str()? != "erase-disk" {
            return None;
        }
        let disk = o.get("disk")?.as_str()?;
        if disk.is_empty()
            || disk.len() > MAX_DISK
            || !disk
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b':' | b'+' | b'-'))
        {
            return None;
        }
        let model = o.get("model")?.as_str()?;
        if model.len() > MAX_MODEL {
            return None;
        }
        let model = crate::render::text::sanitize_line(model).trim().to_string();
        if model.is_empty() {
            return None;
        }
        let size_bytes = o.get("size_bytes")?.as_u64().filter(|n| *n > 0)?;
        Some(Request {
            disk: disk.to_string(),
            model,
            size_bytes,
        })
    }

    /// The untrusted block: the requester's claims, one per line.
    fn claims(&self) -> String {
        format!(
            "disk:  {}\nmodel: {}\nsize:  {} GB ({} bytes)",
            self.disk,
            self.model,
            self.size_bytes / 1_000_000_000,
            self.size_bytes
        )
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Decision {
    Allow,
    Deny,
}

/// The erase prompt that is up or pending: its token and the requester's
/// socket, written once and then shut down.
#[derive(Debug)]
pub struct Erasing {
    token: u64,
    reply: UnixStream,
}

#[derive(Debug, Default)]
pub struct State {
    erasing: Option<Erasing>,
    next: u64,
}

const HEADING: &str = "Erase this disk?";
const WARNING: &str = "This cannot be undone. Everything on the disk will be lost.";
const BODY: &str = "A system service is asking to erase a whole disk. This prompt is drawn \
                    by the compositor and appears only when the system asks to erase a disk.";
const WELL: &str = "The service says (not verified):";

/// Deny holds the focus. Erase grants, so Enter never activates it: the
/// human has to Tab to it and press Space.
const BUTTONS: [Button; 2] = [
    Button {
        label: "Keep disk",
        role: Role::Safe,
    },
    Button {
        label: "Erase",
        role: Role::Grant,
    },
];

/// Tell the requester, then hang up. Errors are ignored: a requester that
/// never hears back treats it as Deny (COMP-10 §3.10).
pub(crate) fn reply(stream: &UnixStream, d: Decision) {
    let line = match d {
        Decision::Allow => "{\"decision\":\"allow\"}\n",
        Decision::Deny => "{\"decision\":\"deny\"}\n",
    };
    let _ = (&mut &*stream).write_all(line.as_bytes());
    let _ = stream.shutdown(Shutdown::Both);
}

/// Put an erase prompt up. `None`, after a Deny to the requester, when any
/// prompt is already up or the session is locked.
pub fn ask(state: &mut AbyssState, request: Request, stream: UnixStream) -> Option<u64> {
    if state.trusted_ui.is_open() || state.lock.locked {
        reply(&stream, Decision::Deny);
        return None;
    }
    let st = &mut state.trusted_ui.erase;
    st.next += 1;
    let token = TOKEN_BASE | st.next;
    let modal = Modal::new(
        token,
        HEADING,
        Some(WARNING),
        BODY,
        WELL,
        &request.claims(),
        BUTTONS.to_vec(),
    );
    let Ok(modal) = modal else {
        reply(&stream, Decision::Deny);
        return None;
    };
    if !super::open(state, modal) {
        reply(&stream, Decision::Deny);
        return None;
    }
    state.trusted_ui.erase.erasing = Some(Erasing { token, reply: stream });
    tracing::info!("destructive-action prompt shown");
    Some(token)
}

pub fn owns(state: &AbyssState, token: u64) -> bool {
    state
        .trusted_ui
        .erase
        .erasing
        .as_ref()
        .is_some_and(|e| e.token == token)
}

/// The prompt was answered. Only a `Grant` is an Allow; the Safe button,
/// Escape and the timeout are all Deny.
pub fn answer(state: &mut AbyssState, choice: Choice) {
    let Some(e) = state.trusted_ui.erase.erasing.take() else {
        return;
    };
    if e.token != choice.token {
        state.trusted_ui.erase.erasing = Some(e);
        return;
    }
    let d = if choice.role == Role::Grant {
        Decision::Allow
    } else {
        Decision::Deny
    };
    reply(&e.reply, d);
    tracing::info!(
        allowed = d == Decision::Allow,
        "destructive-action prompt answered"
    );
}

/// The requester hung up or broke protocol: the prompt goes, and it hears
/// Deny if it is still listening.
pub fn abandon(state: &mut AbyssState, token: u64) {
    if !owns(state, token) {
        return;
    }
    if let Some(e) = state.trusted_ui.erase.erasing.take() {
        reply(&e.reply, Decision::Deny);
    }
    super::cancel(state, token);
    super::approval::schedule(state);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn line(s: &str) -> Vec<u8> {
        s.as_bytes().to_vec()
    }

    const OK: &str = r#"{"action":"erase-disk","disk":"nvme-Samsung_SSD_980_S64DNX0R:1","model":"Samsung SSD 980","size_bytes":500107862016}"#;

    #[test]
    fn a_well_formed_request_parses() {
        let r = Request::parse(&line(OK)).unwrap();
        assert_eq!(r.model, "Samsung SSD 980");
        assert_eq!(r.size_bytes, 500107862016);
    }

    #[test]
    fn anything_else_is_refused() {
        for bad in [
            "",
            "not json",
            "[]",
            r#"{"action":"erase-disk"}"#,
            // Unknown extra field.
            r#"{"action":"erase-disk","disk":"a","model":"m","size_bytes":1,"x":1}"#,
            // A different action must never be shown as an erase.
            r#"{"action":"format","disk":"a","model":"m","size_bytes":1}"#,
            // Path characters in the by-id name.
            r#"{"action":"erase-disk","disk":"../sda","model":"m","size_bytes":1}"#,
            r#"{"action":"erase-disk","disk":"a b","model":"m","size_bytes":1}"#,
            r#"{"action":"erase-disk","disk":"","model":"m","size_bytes":1}"#,
            // Wrong types and zero size.
            r#"{"action":"erase-disk","disk":"a","model":5,"size_bytes":1}"#,
            r#"{"action":"erase-disk","disk":"a","model":"m","size_bytes":"1"}"#,
            r#"{"action":"erase-disk","disk":"a","model":"m","size_bytes":0}"#,
            r#"{"action":"erase-disk","disk":"a","model":"m","size_bytes":-1}"#,
            // Blank after sanitising.
            r#"{"action":"erase-disk","disk":"a","model":"\u001b\u0000","size_bytes":1}"#,
        ] {
            assert!(Request::parse(&line(bad)).is_none(), "accepted: {bad:?}");
        }
        let long = format!(
            r#"{{"action":"erase-disk","disk":"{}","model":"m","size_bytes":1}}"#,
            "a".repeat(MAX_DISK + 1)
        );
        assert!(Request::parse(&line(&long)).is_none());
    }

    #[test]
    fn model_text_is_reduced_before_it_is_stored() {
        let r = Request::parse(&line(
            r#"{"action":"erase-disk","disk":"a","model":"Disk\u001b[31m é","size_bytes":1}"#,
        ))
        .unwrap();
        assert!(r.model.bytes().all(|b| (0x20..0x7f).contains(&b)));
    }

    #[test]
    fn the_buttons_make_a_valid_prompt_with_deny_first() {
        let m = Modal::new(
            TOKEN_BASE | 1,
            HEADING,
            Some(WARNING),
            BODY,
            WELL,
            "x",
            BUTTONS.to_vec(),
        )
        .unwrap();
        assert_eq!(m.safe(), 0);
        assert_eq!(m.buttons()[1].role, Role::Grant);
    }

    #[test]
    fn nothing_but_the_seat_answers() {
        // An answer is produced only by `choose`, reached from the keyboard
        // filter, a physical click (dropped for this prompt) or the timeout.
        // Nothing that takes remote input may name the prompt's entry points.
        for (name, src) in [
            ("ipc/methods.rs", include_str!("../ipc/methods.rs")),
            ("ipc/gate.rs", include_str!("../ipc/gate.rs")),
            ("ipc/mod.rs", include_str!("../ipc/mod.rs")),
            ("input/inject.rs", include_str!("../input/inject.rs")),
        ] {
            assert!(
                !src.contains("trusted_ui::key")
                    && !src.contains("trusted_ui::button")
                    && !src.contains("erase::answer"),
                "{name} can answer a trusted prompt"
            );
        }
    }
}
