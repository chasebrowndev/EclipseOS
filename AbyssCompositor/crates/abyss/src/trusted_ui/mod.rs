// SPDX-License-Identifier: AGPL-3.0-only
//! Trusted UI (COMP-10 §3.10, ADR 0061). **TCB: owner review, line by line.**
//!
//! One surface so far: the destructive-system-action confirmation. A root
//! service (the installer's helper, D-07 §6) asks for a human-seat allow/deny
//! before it erases a disk. The compositor draws the prompt itself, takes the
//! keyboard, and answers only from a key the human pressed.
//!
//! What this module refuses to be: a path from any client to an answer. The
//! request arrives on a root-only socket ([`socket`]); the answer comes from
//! [`on_key`] alone, which is reached only from the physical keyboard filter.
//! There is no method, protocol request, config key or injection call that
//! resolves a prompt, and nothing here logs what the human pressed.
//!
//! Fail-closed everywhere: any path that is not an explicit Allow key is Deny.

pub mod draw;
pub mod socket;

use std::{io::Write, net::Shutdown, os::unix::net::UnixStream, path::PathBuf, time::Duration};

use smithay::{
    input::keyboard::Keysym,
    reexports::calloop::timer::{TimeoutAction, Timer},
};

use crate::state::AbyssState;

/// How long a prompt waits for the human before it is a Deny (COMP-10 §3.2).
pub const TIMEOUT: Duration = Duration::from_secs(120);

/// Longest by-id name accepted. Real ones are well under this.
const MAX_DISK: usize = 200;
const MAX_MODEL: usize = 64;

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
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Decision {
    Allow,
    Deny,
}

/// Which button has focus. Deny is where it starts: a reflexive Enter is a
/// Deny, and Allow needs a deliberate Tab first.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Focus {
    Deny,
    Allow,
}

/// What a key means while the prompt owns the seat.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Key {
    Deny,
    Toggle,
    Activate,
    /// Swallowed: the prompt owns the seat outright, so nothing bound
    /// elsewhere may fire underneath it.
    Ignored,
}

pub fn key(sym: Keysym) -> Key {
    match sym {
        Keysym::Escape => Key::Deny,
        Keysym::Tab | Keysym::ISO_Left_Tab | Keysym::Left | Keysym::Right => Key::Toggle,
        Keysym::Return | Keysym::KP_Enter => Key::Activate,
        _ => Key::Ignored,
    }
}

struct Prompt {
    id: u64,
    request: Request,
    focus: Focus,
    /// A duplicate of the requester's socket. Written once, then shut down.
    reply: UnixStream,
    art: draw::ArtCache,
    /// Whether the card has actually been put on a frame. Allow is refused
    /// until it has: a human cannot approve what was never shown (an output
    /// asleep, a failed upload).
    drawn: bool,
}

#[derive(Default)]
pub struct TrustedUi {
    prompt: Option<Prompt>,
    next_id: u64,
    /// The bound socket, so a clean exit can unlink it.
    pub path: Option<PathBuf>,
}

impl TrustedUi {
    /// Whether a prompt holds the seat. Read by every input path.
    pub fn active(&self) -> bool {
        self.prompt.is_some()
    }
}

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

/// Put a prompt up. `false` (after a Deny to the requester) when one is
/// already pending or the session is locked.
pub fn begin(state: &mut AbyssState, request: Request, stream: UnixStream) -> bool {
    if state.trusted_ui.prompt.is_some() || state.lock.locked {
        reply(&stream, Decision::Deny);
        return false;
    }
    state.trusted_ui.next_id += 1;
    let id = state.trusted_ui.next_id;
    // No client keeps keyboard focus while the prompt is up, so no modifier or
    // key event can reach one (COMP-10 §3.10).
    if let Some(keyboard) = state.seat.get_keyboard() {
        keyboard.set_focus(state, None, smithay::utils::SERIAL_COUNTER.next_serial());
    }
    state.trusted_ui.prompt = Some(Prompt {
        id,
        request,
        focus: Focus::Deny,
        reply: stream,
        art: draw::ArtCache::default(),
        drawn: false,
    });
    let timer = state
        .loop_handle
        .insert_source(Timer::from_duration(TIMEOUT), move |_, _, state| {
            finish_if(state, id, Decision::Deny);
            TimeoutAction::Drop
        });
    if timer.is_err() {
        // No timeout means no bound on how long the seat is held.
        finish_if(state, id, Decision::Deny);
        return false;
    }
    tracing::info!("destructive-action prompt shown");
    crate::backend::damage_all(state);
    true
}

/// End prompt `id` with `d`, if it is still the one on screen.
pub(crate) fn finish_if(state: &mut AbyssState, id: u64, d: Decision) {
    if state.trusted_ui.prompt.as_ref().map(|p| p.id) != Some(id) {
        return;
    }
    let Some(p) = state.trusted_ui.prompt.take() else {
        return;
    };
    reply(&p.reply, d);
    tracing::info!(
        allowed = d == Decision::Allow,
        "destructive-action prompt answered"
    );
    // A locked session keeps the lock screen's focus, not a window behind it.
    if !state.lock.locked {
        crate::shell::refocus_topmost(state);
    }
    crate::backend::damage_all(state);
}

/// The requester went away: nothing to answer, so the prompt goes too.
pub(crate) fn abandon(state: &mut AbyssState, id: u64) {
    finish_if(state, id, Decision::Deny);
}

/// The one place an answer is produced. Called only from the physical
/// keyboard filter.
pub fn on_key(state: &mut AbyssState, k: Key) {
    let Some(p) = state.trusted_ui.prompt.as_mut() else {
        return;
    };
    let id = p.id;
    match k {
        Key::Ignored => {}
        Key::Deny => finish_if(state, id, Decision::Deny),
        Key::Toggle => {
            p.focus = match p.focus {
                Focus::Deny => Focus::Allow,
                Focus::Allow => Focus::Deny,
            };
            p.drawn = false;
            crate::backend::damage_all(state);
        }
        Key::Activate => {
            let d = match p.focus {
                Focus::Allow if p.drawn => Decision::Allow,
                _ => Decision::Deny,
            };
            finish_if(state, id, d);
        }
    }
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
    fn only_the_named_keys_mean_anything() {
        assert_eq!(key(Keysym::Escape), Key::Deny);
        assert_eq!(key(Keysym::Tab), Key::Toggle);
        assert_eq!(key(Keysym::Return), Key::Activate);
        for s in [Keysym::y, Keysym::space, Keysym::a, Keysym::F1, Keysym::BackSpace] {
            assert_eq!(key(s), Key::Ignored, "{s:?} must not answer");
        }
    }

    #[test]
    fn the_capture_pass_cannot_see_the_prompt() {
        let src = include_str!("../render/capture.rs");
        assert!(
            !src.contains("trusted_ui"),
            "capture.rs names the prompt; a prompt in a capture would be read back as screen content"
        );
    }

    #[test]
    fn the_prompt_sits_between_the_selector_and_the_indicator_in_every_backend() {
        for (src, path, indicator_first) in [
            (include_str!("../backend/drm.rs"), "drm.rs", true),
            (include_str!("../backend/winit.rs"), "winit.rs", false),
            (include_str!("../backend/headless.rs"), "headless.rs", false),
        ] {
            let ind = src
                .find("capture::indicator")
                .unwrap_or_else(|| panic!("{path}: no indicator"));
            let prompt = src
                .find("trusted_ui::draw::elements")
                .unwrap_or_else(|| panic!("{path} does not draw the prompt"));
            let sel = src
                .find("select::selector_elements")
                .unwrap_or_else(|| panic!("{path}: no selector"));
            // drm appends top-first; winit and headless splice at 0, which
            // reverses source order.
            assert_eq!(ind < prompt, indicator_first, "{path}: prompt vs indicator");
            assert_eq!(prompt < sel, indicator_first, "{path}: prompt vs selector");
        }
    }

    #[test]
    fn nothing_but_the_keyboard_filter_answers() {
        // `on_key` is the only producer of an answer; it must be reachable from
        // the physical keyboard path and from nothing that takes remote input.
        for (name, src) in [
            ("ipc/methods.rs", include_str!("../ipc/methods.rs")),
            ("ipc/gate.rs", include_str!("../ipc/gate.rs")),
            ("ipc/mod.rs", include_str!("../ipc/mod.rs")),
            ("input/inject.rs", include_str!("../input/inject.rs")),
        ] {
            assert!(
                !src.contains("trusted_ui::on_key") && !src.contains("finish_if"),
                "{name} can resolve a destructive-action prompt"
            );
        }
    }
}
