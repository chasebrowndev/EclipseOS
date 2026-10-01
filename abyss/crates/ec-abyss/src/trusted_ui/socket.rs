// SPDX-License-Identifier: AGPL-3.0-only
//! The request channel for the destructive-action prompt (COMP-10 §3.10,
//! ADR 0061). **TCB.**
//!
//! `$XDG_RUNTIME_DIR/eclipse/trusted.sock`, 0600, beside the control socket
//! but separate from it and outside `ipc::gate`: that table authorises the
//! session owner, and the party asking here is root. A peer whose
//! `SO_PEERCRED` uid is not 0 is refused at `accept`, before a byte is read.
//!
//! One line in, one line out, then the connection is shut. A requester that
//! gets no line treats it as Deny. The compositor never trusts the request's
//! content beyond drawing it (see [`super::erase::Request`]).

use std::{
    io::{ErrorKind, Read},
    os::unix::{
        fs::PermissionsExt,
        net::{UnixListener, UnixStream},
    },
};

use smithay::reexports::calloop::{generic::Generic, Interest, LoopHandle, Mode, PostAction};

use super::erase::{abandon, ask, reply, Decision, Request};
use crate::state::AbyssState;

/// A request is one short line; anything longer is refused.
const MAX_LINE: usize = 2048;

pub fn start(state: &mut AbyssState, handle: &LoopHandle<'static, AbyssState>) {
    let Some(dir) = crate::ipc::socket_dir() else {
        return;
    };
    // `ipc::start` made the directory 0700; without it there is nowhere safe
    // to bind, and a compositor without this socket simply cannot be asked.
    if !dir.is_dir() {
        tracing::warn!("no control socket directory; no trusted socket");
        return;
    }
    let path = dir.join("trusted.sock");
    if path.exists() {
        if UnixStream::connect(&path).is_ok() {
            tracing::error!(path = %path.display(), "trusted socket already in use; not binding");
            return;
        }
        let _ = std::fs::remove_file(&path);
    }
    let listener = match UnixListener::bind(&path) {
        Ok(l) => l,
        Err(e) => {
            tracing::warn!(path = %path.display(), %e, "binding the trusted socket");
            return;
        }
    };
    if let Err(e) = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)) {
        tracing::error!(%e, "tightening the trusted socket; removing it");
        let _ = std::fs::remove_file(&path);
        return;
    }
    if listener.set_nonblocking(true).is_err() {
        let _ = std::fs::remove_file(&path);
        return;
    }
    let inserted = handle.insert_source(
        Generic::new(listener, Interest::READ, Mode::Level),
        |_, listener, state| {
            loop {
                match listener.accept() {
                    Ok((stream, _)) => accept(state, stream),
                    Err(e) if e.kind() == ErrorKind::WouldBlock => break,
                    Err(e) if e.kind() == ErrorKind::Interrupted => continue,
                    Err(e) => {
                        tracing::warn!(%e, "trusted socket accept");
                        break;
                    }
                }
            }
            Ok(PostAction::Continue)
        },
    );
    if inserted.is_err() {
        let _ = std::fs::remove_file(&path);
        return;
    }
    tracing::info!(path = %path.display(), "trusted socket listening");
    state.trusted_ui.path = Some(path);
}

pub fn cleanup(state: &AbyssState) {
    if let Some(path) = &state.trusted_ui.path {
        let _ = std::fs::remove_file(path);
    }
}

/// Where a connection is in its one exchange.
enum Conn {
    Reading(Vec<u8>),
    /// Owns prompt `id`; any further byte is a violation, EOF is abandonment.
    Pending(u64),
    Done,
}

fn accept(state: &mut AbyssState, stream: UnixStream) {
    // The door: root only, before anything is read. Everyone else, including
    // the session owner, is dropped without an answer.
    match crate::ipc::peer_cred(&stream) {
        Some(p) if p.uid == 0 && p.pid > 0 => {}
        _ => {
            tracing::warn!("trusted socket peer is not root; refused");
            return;
        }
    }
    if stream.set_nonblocking(true).is_err() {
        return;
    }
    let mut conn = Conn::Reading(Vec::new());
    let _ = state.loop_handle.insert_source(
        Generic::new(stream, Interest::READ, Mode::Level),
        move |_, stream, state| Ok(readable(state, stream, &mut conn)),
    );
}

fn readable(state: &mut AbyssState, stream: &UnixStream, conn: &mut Conn) -> PostAction {
    let mut chunk = [0u8; 512];
    let mut eof = false;
    let mut got = Vec::new();
    loop {
        match (&mut &*stream).read(&mut chunk) {
            Ok(0) => {
                eof = true;
                break;
            }
            Ok(n) => {
                got.extend_from_slice(&chunk[..n]);
                if got.len() > MAX_LINE {
                    break;
                }
            }
            Err(e) if e.kind() == ErrorKind::WouldBlock => break,
            Err(e) if e.kind() == ErrorKind::Interrupted => continue,
            Err(_) => {
                eof = true;
                break;
            }
        }
    }
    match conn {
        // The prompt is up: the requester may hang up, and may not say more.
        Conn::Pending(id) => {
            if eof || !got.is_empty() {
                abandon(state, *id);
                if !eof {
                    reply(stream, Decision::Deny);
                }
                *conn = Conn::Done;
                return PostAction::Remove;
            }
            PostAction::Continue
        }
        Conn::Reading(buf) => {
            buf.extend_from_slice(&got);
            if buf.len() > MAX_LINE {
                reply(stream, Decision::Deny);
                *conn = Conn::Done;
                return PostAction::Remove;
            }
            let Some(nl) = buf.iter().position(|b| *b == b'\n') else {
                if eof {
                    *conn = Conn::Done;
                    return PostAction::Remove;
                }
                return PostAction::Continue;
            };
            // Exactly one line, and nothing after it.
            let request = (nl + 1 == buf.len())
                .then(|| Request::parse(&buf[..nl]))
                .flatten();
            let Some(request) = request else {
                reply(stream, Decision::Deny);
                *conn = Conn::Done;
                return PostAction::Remove;
            };
            let Ok(dup) = stream.try_clone() else {
                reply(stream, Decision::Deny);
                *conn = Conn::Done;
                return PostAction::Remove;
            };
            if let Some(token) = ask(state, request, dup) {
                *conn = Conn::Pending(token);
                PostAction::Continue
            } else {
                *conn = Conn::Done;
                PostAction::Remove
            }
        }
        Conn::Done => PostAction::Remove,
    }
}

#[cfg(test)]
mod tests {
    use std::io::{BufRead, BufReader, Write};

    use super::*;
    use crate::shell::focus::state_tests::harness;
    use crate::trusted_ui::{arm_now, key};
    use smithay::input::keyboard::Keysym;

    const REQ: &[u8] =
        b"{\"action\":\"erase-disk\",\"disk\":\"nvme-X_1\",\"model\":\"X\",\"size_bytes\":1000}\n";

    /// A requester and the compositor's end, with the request already sent.
    /// `readable` is called directly: the root-only door is `accept`'s, and a
    /// test cannot be root.
    fn connect(state: &mut AbyssState, bytes: &[u8]) -> (UnixStream, UnixStream, Conn, PostAction) {
        let (client, server) = UnixStream::pair().unwrap();
        server.set_nonblocking(true).unwrap();
        (&client).write_all(bytes).unwrap();
        let mut conn = Conn::Reading(Vec::new());
        let post = readable(state, &server, &mut conn);
        (client, server, conn, post)
    }

    fn answer(client: &UnixStream) -> Option<String> {
        client
            .set_read_timeout(Some(std::time::Duration::from_millis(200)))
            .ok()?;
        let mut line = String::new();
        BufReader::new(client).read_line(&mut line).ok()?;
        (!line.is_empty()).then_some(line.trim().to_string())
    }

    /// A connected request with its prompt past the arming delay.
    fn armed(state: &mut AbyssState) -> UnixStream {
        let (client, ..) = connect(state, REQ);
        arm_now(state);
        client
    }

    #[test]
    fn a_request_raises_the_prompt_and_enter_on_the_default_denies() {
        let mut h = harness();
        let (client, _server, conn, post) = connect(&mut h.state, REQ);
        assert!(matches!(conn, Conn::Pending(_)));
        assert!(matches!(post, PostAction::Continue));
        assert!(h.state.trusted_ui.active());
        assert!(crate::trusted_ui::holds_seat(&h.state));
        arm_now(&mut h.state);
        // A reflexive Enter lands on Keep disk.
        key(&mut h.state, Keysym::Return);
        assert_eq!(answer(&client).as_deref(), Some(r#"{"decision":"deny"}"#));
        assert!(!h.state.trusted_ui.active());
    }

    #[test]
    fn erase_needs_a_tab_then_space_and_enter_never_erases() {
        let mut h = harness();
        let client = armed(&mut h.state);
        key(&mut h.state, Keysym::Tab);
        assert!(h.state.trusted_ui.active(), "Tab alone must not answer");
        key(&mut h.state, Keysym::Return);
        assert!(h.state.trusted_ui.active(), "Enter on Erase must not answer");
        key(&mut h.state, Keysym::space);
        assert_eq!(answer(&client).as_deref(), Some(r#"{"decision":"allow"}"#));
    }

    #[test]
    fn nothing_answers_before_the_prompt_is_armed() {
        let mut h = harness();
        let (client, ..) = connect(&mut h.state, REQ);
        key(&mut h.state, Keysym::Tab);
        key(&mut h.state, Keysym::space);
        assert!(h.state.trusted_ui.active());
        assert!(answer(&client).is_none());
    }

    #[test]
    fn a_click_never_answers_the_erase_prompt() {
        let mut h = harness();
        let client = armed(&mut h.state);
        key(&mut h.state, Keysym::Tab);
        crate::trusted_ui::button(&mut h.state, true);
        crate::trusted_ui::button(&mut h.state, false);
        assert!(h.state.trusted_ui.active());
        assert!(answer(&client).is_none());
    }

    /// A `Window` needs a real client, so the guard is pinned by source: the
    /// function every client focus path funnels through must check first.
    #[test]
    fn window_focus_checks_the_prompt_and_lock_first() {
        let src = include_str!("../shell/focus.rs");
        let body = &src[src.find("pub fn focus_window_raising").unwrap()..];
        let head = &body[..body.find("state.").unwrap() + 200];
        assert!(head.contains("state.trusted_ui.active() || state.lock.locked"));
    }

    #[test]
    fn escape_denies_even_with_allow_focused() {
        let mut h = harness();
        let client = armed(&mut h.state);
        key(&mut h.state, Keysym::Tab);
        key(&mut h.state, Keysym::Escape);
        assert_eq!(answer(&client).as_deref(), Some(r#"{"decision":"deny"}"#));
    }

    #[test]
    fn other_keys_never_answer() {
        let mut h = harness();
        let client = armed(&mut h.state);
        for s in [Keysym::y, Keysym::a, Keysym::F1, Keysym::BackSpace] {
            key(&mut h.state, s);
        }
        assert!(h.state.trusted_ui.active());
        assert!(answer(&client).is_none());
    }

    #[test]
    fn a_second_request_is_denied_and_the_first_survives() {
        let mut h = harness();
        let first = armed(&mut h.state);
        let (second, _s, conn, post) = connect(&mut h.state, REQ);
        assert_eq!(answer(&second).as_deref(), Some(r#"{"decision":"deny"}"#));
        assert!(matches!(conn, Conn::Done) && matches!(post, PostAction::Remove));
        assert!(h.state.trusted_ui.active());
        key(&mut h.state, Keysym::Tab);
        key(&mut h.state, Keysym::space);
        assert_eq!(answer(&first).as_deref(), Some(r#"{"decision":"allow"}"#));
    }

    #[test]
    fn a_locked_session_denies_at_once() {
        let mut h = harness();
        h.state.lock.locked = true;
        let (client, ..) = connect(&mut h.state, REQ);
        assert_eq!(answer(&client).as_deref(), Some(r#"{"decision":"deny"}"#));
        assert!(!h.state.trusted_ui.active());
    }

    #[test]
    fn bad_input_is_denied_without_a_prompt() {
        for bad in [
            &b"{\"action\":\"erase-disk\"}\n"[..],
            b"garbage\n",
            // A second line smuggled behind a valid one.
            b"{\"action\":\"erase-disk\",\"disk\":\"a\",\"model\":\"m\",\"size_bytes\":1}\n{}\n",
        ] {
            let mut h = harness();
            let (client, _s, conn, _) = connect(&mut h.state, bad);
            assert_eq!(answer(&client).as_deref(), Some(r#"{"decision":"deny"}"#));
            assert!(matches!(conn, Conn::Done));
            assert!(!h.state.trusted_ui.active());
        }
    }

    #[test]
    fn hanging_up_takes_the_prompt_down() {
        let mut h = harness();
        let (client, server, mut conn, _) = connect(&mut h.state, REQ);
        assert!(h.state.trusted_ui.active());
        drop(client);
        let post = readable(&mut h.state, &server, &mut conn);
        assert!(matches!(post, PostAction::Remove));
        assert!(!h.state.trusted_ui.active());
    }

    #[test]
    fn talking_after_the_request_is_a_violation() {
        let mut h = harness();
        let (client, server, mut conn, _) = connect(&mut h.state, REQ);
        (&client).write_all(b"allow\n").unwrap();
        let post = readable(&mut h.state, &server, &mut conn);
        assert!(matches!(post, PostAction::Remove));
        assert!(!h.state.trusted_ui.active());
    }

    #[test]
    fn the_timeout_answer_is_a_deny() {
        let mut h = harness();
        let (client, ..) = connect(&mut h.state, REQ);
        let token = h.state.trusted_ui.token().unwrap();
        // What the timer does: the Safe button, whatever has focus.
        crate::trusted_ui::resolve(
            &mut h.state,
            crate::trusted_ui::Choice {
                token: token + 1,
                button: 1,
                role: crate::trusted_ui::Role::Grant,
            },
        );
        assert!(answer(&client).is_none(), "a stale token must not answer");
        crate::trusted_ui::key(&mut h.state, Keysym::Escape);
        assert_eq!(answer(&client).as_deref(), Some(r#"{"decision":"deny"}"#));
    }
}
