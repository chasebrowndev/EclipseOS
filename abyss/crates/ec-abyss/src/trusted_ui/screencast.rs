// SPDX-License-Identifier: AGPL-3.0-only
//! The screen-share confirm and the capture grants behind it (ADR 0097,
//! D-08 §6). **TCB.**
//!
//! ```text
//!   app → ecl-desktop-portal → chooser pane (untrusted) → portal.sock
//!   portal.sock → this prompt → Allow → a grant for one output
//!   grant + /usr/bin/ecl-desktop-portal → screencopy / image-copy admitted
//! ```
//!
//! The chooser is the portal's and proves nothing. This prompt is the consent:
//! drawn by abyss, answered only from the human seat, and it names the screen
//! as abyss resolves it, never as the request describes it.
//!
//! Choices, for the owner's review:
//! - **The grant lives as long as the asking connection.** After `Granted` the
//!   portal keeps the socket open for the whole stream. Hanging up, any further
//!   byte, or the portal dying revokes the grant at once, and the next frame
//!   is refused. Nothing persists: every share asks again (`persistent` is
//!   ignored).
//! - **abyss holds the grant, not policyd.** ADR 0097 has policyd mint it, but
//!   policyd ships in the optional `eclipseos-agents` package. A desktop
//!   without it must still be able to share a screen, so the grant is held
//!   here, bound to `{stream, output, app-id claim}`, for the connection's
//!   life. ADR 0097 is amended in the same change.
//! - **Screens only.** `Source::Window` is refused: abyss has no toplevel
//!   capture source yet (D-08 §6).
//! - **The door is one binary.** The peer must be the session owner running
//!   exactly [`PORTAL_EXE`]. That authenticates which daemon asks, never which
//!   app is behind it, so the app id is shown as a claim.
//!
//! Wire (`ecl-portal-proto`, decoded here by hand so the TCB carries no
//! codec): one request per connection, a `u32` big-endian length, then a
//! postcard `CaptureConfirm`; the answer is framed the same way.

use std::{
    collections::VecDeque,
    io::{ErrorKind, Read, Write},
    net::Shutdown,
    os::unix::{
        fs::PermissionsExt,
        net::{UnixListener, UnixStream},
    },
    path::{Path, PathBuf},
};

use smithay::reexports::calloop::{generic::Generic, Interest, LoopHandle, Mode, PostAction};

use super::{
    modal::{Button, Modal, Role},
    Choice,
};
use crate::state::AbyssState;

/// The only binary allowed through the door, and the only one a grant admits.
pub const PORTAL_EXE: &str = "/usr/bin/ecl-desktop-portal";

const TOKEN_BASE: u64 = (1 << 61) | (1 << 55);
/// A request is a few dozen bytes; anything past this is refused.
const MAX_FRAME: usize = 1024;
const MAX_APP_ID: usize = 255;
const MAX_OUTPUT: usize = 64;
/// Requests waiting for the seat. A flood is refused, not queued.
const MAX_QUEUE: usize = 4;
/// Streams at once. Past this a new share is refused.
const MAX_GRANTS: usize = 4;

// ---- the wire -----------------------------------------------------------------

/// What the portal asks for. Only `output` is acted on, and only after abyss
/// has found an output of that name itself.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Request {
    app_id: String,
    stream: u64,
    output: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Answer {
    Granted(u64),
    Denied,
    Timeout,
}

/// A strict reader over one postcard payload: every read is bounded, and the
/// payload must be consumed exactly.
struct Reader<'a> {
    b: &'a [u8],
}

impl Reader<'_> {
    fn byte(&mut self) -> Option<u8> {
        let (&x, rest) = self.b.split_first()?;
        self.b = rest;
        Some(x)
    }

    /// LEB128, at most ten bytes, no bits past 64.
    fn varint(&mut self) -> Option<u64> {
        let mut v: u64 = 0;
        for i in 0..10 {
            let x = self.byte()?;
            let part = u64::from(x & 0x7f);
            if i == 9 && part > 1 {
                return None;
            }
            v |= part << (7 * i);
            if x & 0x80 == 0 {
                return Some(v);
            }
        }
        None
    }

    fn u32(&mut self) -> Option<u32> {
        u32::try_from(self.varint()?).ok()
    }

    fn bool(&mut self) -> Option<bool> {
        match self.byte()? {
            0 => Some(false),
            1 => Some(true),
            _ => None,
        }
    }

    fn string(&mut self, max: usize) -> Option<String> {
        let n = usize::try_from(self.varint()?).ok()?;
        if n > max || n > self.b.len() {
            return None;
        }
        let (s, rest) = self.b.split_at(n);
        self.b = rest;
        String::from_utf8(s.to_vec()).ok()
    }
}

impl Request {
    /// `CaptureConfirm { app_id_claim, stream, target, audio, persistent }`.
    /// A window target, a bad field or a trailing byte is `None`, a refusal.
    fn parse(payload: &[u8]) -> Option<Request> {
        let mut r = Reader { b: payload };
        let app_id = r.string(MAX_APP_ID)?;
        let stream = r.varint()?;
        // `Source`: 0 = Output { name, width, height }, 1 = Window (refused).
        if r.u32()? != 0 {
            return None;
        }
        let output = r.string(MAX_OUTPUT)?;
        let _width = r.u32()?;
        let _height = r.u32()?;
        let _audio = r.bool()?;
        let _persistent = r.bool()?;
        if !r.b.is_empty() {
            return None;
        }
        let output_ok = !output.is_empty()
            && output
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.'));
        if !output_ok {
            return None;
        }
        let app_id = crate::render::text::sanitize_line(&app_id).trim().to_string();
        Some(Request {
            app_id,
            stream,
            output,
        })
    }
}

/// `CaptureAnswer`: 0 = Granted { grant }, 1 = Denied, 2 = Timeout.
fn encode(answer: Answer) -> Vec<u8> {
    let mut payload = Vec::with_capacity(11);
    match answer {
        Answer::Granted(grant) => {
            payload.push(0);
            let mut v = grant;
            loop {
                let x = (v & 0x7f) as u8;
                v >>= 7;
                if v == 0 {
                    payload.push(x);
                    break;
                }
                payload.push(x | 0x80);
            }
        }
        Answer::Denied => payload.push(1),
        Answer::Timeout => payload.push(2),
    }
    let mut frame = (payload.len() as u32).to_be_bytes().to_vec();
    frame.extend_from_slice(&payload);
    frame
}

/// Tell the portal. A refusal also hangs up; a grant keeps the line open,
/// because the line is the grant's lifetime.
fn reply(stream: &UnixStream, answer: Answer) {
    let _ = (&mut &*stream).write_all(&encode(answer));
    if !matches!(answer, Answer::Granted(_)) {
        let _ = stream.shutdown(Shutdown::Both);
    }
}

// ---- state --------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Stage {
    Waiting,
    Prompting(u64),
}

#[derive(Debug)]
struct Ask {
    id: u64,
    request: Request,
    conn: UnixStream,
    stage: Stage,
}

/// One live share: the portal may capture `output_id`, and nothing else, while
/// `conn` stays open.
#[derive(Debug)]
struct Grant {
    id: u64,
    output_id: u64,
    _conn: UnixStream,
}

/// The front of `queue` is the request being worked on.
#[derive(Debug, Default)]
pub struct Screencast {
    queue: VecDeque<Ask>,
    grants: Vec<Grant>,
    next: u64,
    next_token: u64,
    path: Option<PathBuf>,
}

impl Screencast {
    /// How many shares are live (for the indicator and tests).
    pub fn live(&self) -> usize {
        self.grants.len()
    }
}

/// May `exe` capture `output_id` under a live grant? The lock and the
/// allowlist are checked before this by the caller (`screencopy::admit`).
pub fn granted(state: &AbyssState, exe: &Path, output_id: u64) -> bool {
    exe == Path::new(PORTAL_EXE)
        && state
            .trusted_ui
            .screencast
            .grants
            .iter()
            .any(|g| g.output_id == output_id)
}

/// Is `exe` the portal? Used only to show it the capture globals: seeing a
/// global admits nothing without a grant.
pub fn is_portal(exe: Option<&Path>) -> bool {
    exe == Some(Path::new(PORTAL_EXE))
}

// ---- the door -----------------------------------------------------------------

pub fn start(state: &mut AbyssState, handle: &LoopHandle<'static, AbyssState>) {
    let Some(dir) = crate::ipc::socket_dir() else {
        return;
    };
    if !dir.is_dir() {
        tracing::warn!("no control socket directory; no screen-share confirm");
        return;
    }
    let path = dir.join("portal.sock");
    if path.exists() {
        if UnixStream::connect(&path).is_ok() {
            tracing::error!(path = %path.display(), "portal socket already in use; not binding");
            return;
        }
        let _ = std::fs::remove_file(&path);
    }
    let listener = match UnixListener::bind(&path) {
        Ok(l) => l,
        Err(e) => {
            tracing::warn!(path = %path.display(), %e, "binding the portal socket");
            return;
        }
    };
    if let Err(e) = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)) {
        tracing::error!(%e, "tightening the portal socket; removing it");
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
                        tracing::warn!(%e, "portal socket accept");
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
    tracing::info!(path = %path.display(), "portal socket listening");
    state.trusted_ui.screencast.path = Some(path);
}

pub fn cleanup(state: &AbyssState) {
    if let Some(path) = &state.trusted_ui.screencast.path {
        let _ = std::fs::remove_file(path);
    }
}

/// The session owner, running the installed portal. `exe` is `None` when the
/// peer is gone or unreadable, and that is a refusal.
fn admit(peer_uid: u32, owner: u32, exe: Option<&Path>) -> bool {
    peer_uid == owner && is_portal(exe)
}

fn accept(state: &mut AbyssState, stream: UnixStream) {
    let admitted = crate::ipc::peer_cred(&stream).is_some_and(|p| {
        let exe = std::fs::read_link(format!("/proc/{}/exe", p.pid)).ok();
        p.pid > 0 && admit(p.uid, crate::ipc::owner_uid(), exe.as_deref())
    });
    if !admitted {
        tracing::warn!("portal socket peer is not the installed portal; refused");
        return;
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

/// Where a connection is in its one exchange.
enum Conn {
    Reading(Vec<u8>),
    /// Owns request (and then grant) `id`. Any byte or EOF withdraws it.
    Pending(u64),
    Done,
}

/// `Some(payload)` once `buf` holds exactly one whole frame; `Err` for a frame
/// that is oversized or followed by more bytes.
fn frame(buf: &[u8]) -> Result<Option<&[u8]>, ()> {
    let Some(len) = buf.get(..4) else {
        return Ok(None);
    };
    let len = u32::from_be_bytes([len[0], len[1], len[2], len[3]]) as usize;
    if len == 0 || len > MAX_FRAME {
        return Err(());
    }
    match buf.len().cmp(&(4 + len)) {
        std::cmp::Ordering::Less => Ok(None),
        std::cmp::Ordering::Equal => Ok(Some(&buf[4..])),
        std::cmp::Ordering::Greater => Err(()),
    }
}

fn readable(state: &mut AbyssState, stream: &UnixStream, conn: &mut Conn) -> PostAction {
    let mut chunk = [0u8; 256];
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
                if got.len() > MAX_FRAME + 4 {
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
        Conn::Pending(id) => {
            if eof || !got.is_empty() {
                withdraw(state, *id);
                *conn = Conn::Done;
                return PostAction::Remove;
            }
            PostAction::Continue
        }
        Conn::Reading(buf) => {
            buf.extend_from_slice(&got);
            let refuse = |conn: &mut Conn| {
                reply(stream, Answer::Denied);
                *conn = Conn::Done;
                PostAction::Remove
            };
            let payload = match frame(buf) {
                Err(()) => return refuse(conn),
                Ok(None) if eof => {
                    *conn = Conn::Done;
                    return PostAction::Remove;
                }
                Ok(None) => return PostAction::Continue,
                Ok(Some(p)) => p,
            };
            let (Some(request), Ok(reply_to)) = (Request::parse(payload), stream.try_clone()) else {
                tracing::warn!("portal socket request refused (malformed or not a screen)");
                return refuse(conn);
            };
            match ask(state, request, reply_to) {
                Some(id) => {
                    *conn = Conn::Pending(id);
                    PostAction::Continue
                }
                None => {
                    *conn = Conn::Done;
                    PostAction::Remove
                }
            }
        }
        Conn::Done => PostAction::Remove,
    }
}

// ---- the queue ----------------------------------------------------------------

fn ask(state: &mut AbyssState, request: Request, conn: UnixStream) -> Option<u64> {
    let s = &mut state.trusted_ui.screencast;
    if s.queue.len() >= MAX_QUEUE || s.grants.len() >= MAX_GRANTS || state.lock.locked {
        reply(&conn, Answer::Denied);
        return None;
    }
    s.next += 1;
    let id = s.next;
    s.queue.push_back(Ask {
        id,
        request,
        conn,
        stage: Stage::Waiting,
    });
    schedule(state);
    Some(id)
}

/// The output abyss itself knows by `name`: its id and a label for the prompt.
fn resolve(state: &AbyssState, name: &str) -> Option<(u64, String)> {
    let entry = state.outputs.iter().find(|e| e.connector == name)?;
    let label = match entry.output.current_mode() {
        Some(m) => format!("{name} ({}x{})", m.size.w, m.size.h),
        None => name.to_owned(),
    };
    Some((entry.id, label))
}

/// The seat may be free: put the front request's prompt up.
pub fn schedule(state: &mut AbyssState) {
    if state.trusted_ui.is_open() {
        return;
    }
    let s = &state.trusted_ui.screencast;
    let Some(front) = s.queue.front() else {
        return;
    };
    if front.stage != Stage::Waiting {
        return;
    }
    let output = front.request.output.clone();
    let app_id = front.request.app_id.clone();
    let Some((_, label)) = resolve(state, &output) else {
        finish(state, Answer::Denied);
        return;
    };
    let s = &mut state.trusted_ui.screencast;
    s.next_token += 1;
    let token = TOKEN_BASE | s.next_token;
    let Some(m) = modal(token, &label, &app_id) else {
        finish(state, Answer::Denied);
        return;
    };
    if super::open(state, m) {
        if let Some(a) = state.trusted_ui.screencast.queue.front_mut() {
            a.stage = Stage::Prompting(token);
        }
        tracing::info!("screen-share confirm shown");
    }
}

/// Answer the front request and move on.
fn finish(state: &mut AbyssState, answer: Answer) {
    let Some(a) = state.trusted_ui.screencast.queue.pop_front() else {
        return;
    };
    match answer {
        Answer::Granted(_) => {
            // Re-resolve now: the output must still be there, by the same name
            // the human was shown.
            let Some((output_id, _)) = resolve(state, &a.request.output) else {
                reply(&a.conn, Answer::Denied);
                schedule(state);
                return;
            };
            reply(&a.conn, Answer::Granted(a.id));
            state.trusted_ui.screencast.grants.push(Grant {
                id: a.id,
                output_id,
                _conn: a.conn,
            });
            tracing::info!(stream = a.request.stream, "screen share granted");
        }
        other => {
            reply(&a.conn, other);
            tracing::info!(?other, "screen share not granted");
        }
    }
    schedule(state);
}

/// The portal hung up or broke protocol: a waiting or shown request goes, and a
/// grant ends.
fn withdraw(state: &mut AbyssState, id: u64) {
    let s = &mut state.trusted_ui.screencast;
    if let Some(i) = s.grants.iter().position(|g| g.id == id) {
        s.grants.remove(i);
        tracing::info!("screen share ended");
        return;
    }
    let Some(i) = s.queue.iter().position(|a| a.id == id) else {
        return;
    };
    let Some(a) = s.queue.remove(i) else {
        return;
    };
    if let Stage::Prompting(token) = a.stage {
        super::cancel(state, token);
        super::approval::schedule(state);
        super::consent::schedule(state);
        super::batch::schedule(state);
        super::install::schedule(state);
        super::broker::schedule(state);
    }
    schedule(state);
}

/// The session locked: every share ends. Captures are refused while locked
/// anyway; this makes sure none resumes after unlock without asking again.
pub fn revoke_all(state: &mut AbyssState) {
    let s = &mut state.trusted_ui.screencast;
    if !s.grants.is_empty() {
        tracing::info!(count = s.grants.len(), "screen shares ended by lock");
    }
    for g in s.grants.drain(..) {
        let _ = g._conn.shutdown(Shutdown::Both);
    }
}

pub fn owns(state: &AbyssState, token: u64) -> bool {
    matches!(
        state.trusted_ui.screencast.queue.front(),
        Some(Ask { stage: Stage::Prompting(t), .. }) if *t == token
    )
}

/// Only Share grants. Don't share, Escape and the timeout are all refusals.
pub fn answer(state: &mut AbyssState, choice: Choice) {
    let answer = match (choice.role, choice.timed_out) {
        (Role::Grant, _) => Answer::Granted(0),
        (_, true) => Answer::Timeout,
        _ => Answer::Denied,
    };
    finish(state, answer);
}

// ---- the prompt ---------------------------------------------------------------

const HEADING: &str = "Share your screen?";
const BODY: &str = "An app is asking to see everything on this screen until it stops sharing. \
                    Password managers and private windows stay covered.";
const WELL: &str = "The app says it is (not verified):";

/// Don't share holds the focus. Share grants, so Enter never activates it.
const BUTTONS: [Button; 2] = [
    Button {
        label: "Don't share",
        role: Role::Safe,
    },
    Button {
        label: "Share",
        role: Role::Grant,
    },
];

fn modal(token: u64, screen: &str, app_id: &str) -> Option<Modal> {
    let claim = if app_id.is_empty() {
        "(no name given)"
    } else {
        app_id
    };
    let m = Modal::new(token, HEADING, None, BODY, WELL, claim, BUTTONS.to_vec()).ok()?;
    Some(m.with_facts(&[("Screen:", screen)]))
}

/// Test hook: a live grant for `output_id`, held by the returned end of a
/// socket pair, as the portal would hold it.
#[cfg(test)]
pub(crate) fn grant_for_test(state: &mut AbyssState, output_id: u64) -> (u64, UnixStream) {
    let (ours, theirs) = UnixStream::pair().expect("socketpair");
    let s = &mut state.trusted_ui.screencast;
    s.next += 1;
    let id = s.next;
    s.grants.push(Grant {
        id,
        output_id,
        _conn: ours,
    });
    (id, theirs)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocols::standard::screencopy::{admit as gate, Decision};

    #[test]
    fn a_grant_admits_only_the_portal_and_only_its_screen() {
        let mut h = crate::shell::focus::state_tests::harness();
        let (a, b) = (h.a, h.b);
        let portal = Path::new(PORTAL_EXE);
        let other = Path::new("/usr/bin/obs");
        // No grant, no allowlist: nothing captures, portal included.
        assert_ne!(gate(&h.state, Some(portal), a), Decision::Allow);
        let (id, _held) = grant_for_test(&mut h.state, a);
        assert_eq!(gate(&h.state, Some(portal), a), Decision::Allow);
        // Another screen, or another program, gets nothing from it.
        assert_ne!(gate(&h.state, Some(portal), b), Decision::Allow);
        assert_ne!(gate(&h.state, Some(other), a), Decision::Allow);
        assert_ne!(gate(&h.state, None, a), Decision::Allow);
        // The lock beats a grant.
        h.state.lock.locked = true;
        assert_eq!(gate(&h.state, Some(portal), a), Decision::DenyLocked);
        h.state.lock.locked = false;
        // Hanging up ends it.
        withdraw(&mut h.state, id);
        assert_ne!(gate(&h.state, Some(portal), a), Decision::Allow);
    }

    #[test]
    fn locking_ends_every_share() {
        let mut h = crate::shell::focus::state_tests::harness();
        let a = h.a;
        let (_, held) = grant_for_test(&mut h.state, a);
        let _ = grant_for_test(&mut h.state, h.b);
        h.state.engage_lock();
        assert_eq!(h.state.trusted_ui.screencast.live(), 0);
        h.state.lock.locked = false;
        assert_ne!(gate(&h.state, Some(Path::new(PORTAL_EXE)), a), Decision::Allow);
        // The portal hears the hang-up.
        let mut buf = [0u8; 1];
        assert_eq!((&held).read(&mut buf).unwrap(), 0);
    }

    /// A `CaptureConfirm` as postcard writes it.
    fn confirm(app: &str, stream: u64, target: &[u8], tail: &[u8]) -> Vec<u8> {
        let mut v = vec![app.len() as u8];
        v.extend_from_slice(app.as_bytes());
        v.push(stream as u8);
        v.extend_from_slice(target);
        v.extend_from_slice(tail);
        v
    }

    fn output(name: &str) -> Vec<u8> {
        // variant 0, name, width 1920 (0x80 0x0f), height 1200 (0xb0 0x09)
        let mut v = vec![0, name.len() as u8];
        v.extend_from_slice(name.as_bytes());
        v.extend_from_slice(&[0x80, 0x0f, 0xb0, 0x09]);
        v
    }

    #[test]
    fn a_screen_request_parses() {
        let r = Request::parse(&confirm("com.discordapp.Discord", 7, &output("DP-1"), &[1, 0])).unwrap();
        assert_eq!(r.app_id, "com.discordapp.Discord");
        assert_eq!(r.stream, 7);
        assert_eq!(r.output, "DP-1");
    }

    #[test]
    fn windows_and_malformed_requests_are_refused() {
        let window = [1u8, 5, 1, b't', 1, b'a'];
        assert!(Request::parse(&confirm("a", 1, &window, &[0, 0])).is_none());
        // Trailing byte.
        assert!(Request::parse(&confirm("a", 1, &output("DP-1"), &[0, 0, 0])).is_none());
        // Short.
        assert!(Request::parse(&confirm("a", 1, &output("DP-1"), &[0])).is_none());
        // A bool that is not 0 or 1.
        assert!(Request::parse(&confirm("a", 1, &output("DP-1"), &[2, 0])).is_none());
        // Path or control characters in the output name.
        assert!(Request::parse(&confirm("a", 1, &output("../x"), &[0, 0])).is_none());
        assert!(Request::parse(&confirm("a", 1, &output(""), &[0, 0])).is_none());
        // A length past the payload.
        assert!(Request::parse(&[200, b'a']).is_none());
        assert!(Request::parse(&[]).is_none());
    }

    #[test]
    fn an_overlong_varint_is_refused() {
        let mut r = Reader {
            b: &[0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0x7f],
        };
        assert_eq!(r.varint(), None);
        let mut r = Reader {
            b: &[0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0x01],
        };
        assert_eq!(r.varint(), Some(u64::MAX));
    }

    #[test]
    fn the_app_id_is_reduced_to_printable_text() {
        let r = Request::parse(&confirm("a\u{1b}[31mb", 1, &output("DP-1"), &[0, 0])).unwrap();
        assert!(r.app_id.bytes().all(|b| (0x20..0x7f).contains(&b)));
    }

    #[test]
    fn frames_are_exact() {
        assert_eq!(frame(&[0, 0]), Ok(None));
        assert_eq!(frame(&[0, 0, 0, 2, 9]), Ok(None));
        assert_eq!(frame(&[0, 0, 0, 2, 9, 8]), Ok(Some(&[9u8, 8][..])));
        assert_eq!(frame(&[0, 0, 0, 2, 9, 8, 7]), Err(()));
        assert_eq!(frame(&[0, 0, 0, 0]), Err(()));
        assert_eq!(frame(&[0, 0, 0x10, 0]), Err(()));
    }

    #[test]
    fn answers_encode_as_postcard() {
        assert_eq!(encode(Answer::Denied), vec![0, 0, 0, 1, 1]);
        assert_eq!(encode(Answer::Timeout), vec![0, 0, 0, 1, 2]);
        assert_eq!(encode(Answer::Granted(5)), vec![0, 0, 0, 2, 0, 5]);
        assert_eq!(encode(Answer::Granted(300)), vec![0, 0, 0, 3, 0, 0xac, 0x02]);
    }

    #[test]
    fn only_the_installed_portal_as_the_owner_is_admitted() {
        let portal = Path::new(PORTAL_EXE);
        assert!(admit(1000, 1000, Some(portal)));
        assert!(!admit(1001, 1000, Some(portal)));
        assert!(!admit(1000, 1000, Some(Path::new("/tmp/ecl-desktop-portal"))));
        assert!(!admit(
            1000,
            1000,
            Some(Path::new("/usr/lib/xdg-desktop-portal-wlr"))
        ));
        assert!(!admit(1000, 1000, None));
    }

    #[test]
    fn the_prompt_is_valid_with_refusal_first() {
        let m = modal(TOKEN_BASE | 1, "DP-1 (1920x1200)", "app").unwrap();
        assert_eq!(m.safe(), 0);
        assert_eq!(m.buttons()[1].role, Role::Grant);
    }

    #[test]
    fn nothing_but_the_seat_answers() {
        for (name, src) in [
            ("ipc/methods.rs", include_str!("../ipc/methods.rs")),
            ("ipc/mod.rs", include_str!("../ipc/mod.rs")),
            ("input/inject.rs", include_str!("../input/inject.rs")),
        ] {
            assert!(
                !src.contains("screencast::answer") && !src.contains("screencast::granted"),
                "{name} can answer or forge a screen share"
            );
        }
    }
}
