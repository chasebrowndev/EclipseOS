// SPDX-License-Identifier: AGPL-3.0-only
//! The polkit password prompt (COMP-10 §3.14, D-07 §6, ADR 0087). **TCB.**
//!
//! ```text
//!   polkitd → ec-polkit-agent → polkit.sock → this prompt
//!   this prompt → agent-helper.socket (root) → PAM, then polkitd
//! ```
//!
//! `ec-polkit-agent` holds the system-bus side and relays who and what: one
//! line in on `polkit.sock`, one line out, then the connection is shut. Hanging
//! up withdraws the request. It never sees the password.
//!
//! The password goes to one place: polkit's helper, over a connection abyss
//! opens itself and whose peer it checks is uid 0. polkitd accepts the helper's
//! word for a cookie only when the helper's own peer has the uid that registered
//! the agent, and abyss and the agent are the same user. What the helper says
//! back is relayed to the agent and grants nothing: polkitd already heard it
//! from the helper.
//!
//! A forged request (any process as the session owner, past the `/proc` check)
//! can put a prompt up and learn pass or fail. It cannot read what is typed, and
//! the prompt says the request is unverified.
//!
//! Choices, for the owner's review:
//! - A wrong password asks again, up to [`TRIES`], each on a fresh connection,
//!   because the helper is one-shot.
//! - Any second question from PAM (an OTP, say) fails the attempt. The password
//!   is sent once, only to a no-echo prompt, and never to an echoing one.
//! - The password is printable ASCII, the trusted font's range, like the secret
//!   store's. Someone whose password is outside that uses a TTY.

use std::{
    collections::VecDeque,
    io::{ErrorKind, Read, Write},
    net::Shutdown,
    os::{
        fd::OwnedFd,
        unix::{
            fs::PermissionsExt,
            net::{UnixListener, UnixStream},
        },
    },
    path::{Path, PathBuf},
    time::Duration,
};

use rustix::net::{self, AddressFamily, SendFlags, SocketAddrUnix, SocketFlags, SocketType};
use smithay::reexports::calloop::{
    generic::Generic,
    timer::{TimeoutAction, Timer},
    Interest, LoopHandle, Mode, PostAction, RegistrationToken,
};
use zeroize::Zeroizing;

use super::{
    modal::{Button, Modal, Role},
    Choice,
};
use crate::state::AbyssState;

/// The only binary allowed through the door.
const AGENT_EXE: &str = "/usr/bin/ec-polkit-agent";
/// polkit's socket-activated helper. Root-owned directory, checked peer.
const HELPER: &str = "/run/polkit/agent-helper.socket";

const TOKEN_BASE: u64 = (1 << 61) | (1 << 56);
/// Attempts per request, like `polkit-gnome`'s and `sudo`'s.
const TRIES: u8 = 3;
/// The field's own limit. The panel is 60 columns.
const MAX_PASSWORD: usize = 60;
/// Requests waiting. polkitd sends one per pending `pkexec`; a flood is refused.
const MAX_QUEUE: usize = 8;
/// PAM can stall for seconds; past this the helper is treated as a failure.
const CHECK_TIMEOUT: Duration = Duration::from_secs(30);

const MAX_LINE: usize = 4096;
const MAX_USER: usize = 32;
const MAX_COOKIE: usize = 512;
const MAX_ACTION: usize = 128;
const MAX_MESSAGE: usize = 1024;

/// What the agent says it is asking. Abyss acts only on `user` and `cookie`, and
/// polkitd checks both: a user it did not offer and a cookie it did not issue
/// get nowhere.
struct Request {
    user: String,
    cookie: Zeroizing<String>,
    action: String,
    message: String,
}

impl Request {
    /// Strict: the four fields and nothing else. Neither `user` nor `cookie`
    /// can hold a newline, because each is a line to the helper.
    fn parse(line: &[u8]) -> Option<Request> {
        let v: serde_json::Value = serde_json::from_slice(line).ok()?;
        let o = v.as_object()?;
        if o.len() != 4 {
            return None;
        }
        let user = o.get("user")?.as_str()?;
        let user_ok = !user.is_empty()
            && user.len() <= MAX_USER
            && !user.starts_with('-')
            && user
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'));
        let cookie = o.get("cookie")?.as_str()?;
        let cookie_ok =
            !cookie.is_empty() && cookie.len() <= MAX_COOKIE && cookie.bytes().all(|b| b.is_ascii_graphic());
        let action = o.get("action")?.as_str()?;
        let action_ok = !action.is_empty()
            && action.len() <= MAX_ACTION
            && action
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-'));
        let message = o.get("message")?.as_str()?;
        if !user_ok || !cookie_ok || !action_ok || message.len() > MAX_MESSAGE {
            return None;
        }
        Some(Request {
            user: user.to_owned(),
            cookie: Zeroizing::new(cookie.to_owned()),
            action: action.to_owned(),
            message: message.to_owned(),
        })
    }
}

/// What the agent hears. It has no way to learn more.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Outcome {
    Ok,
    Failed,
    Cancelled,
}

fn reply(stream: &UnixStream, outcome: Outcome) {
    let line = match outcome {
        Outcome::Ok => "{\"result\":\"ok\"}\n",
        Outcome::Failed => "{\"result\":\"failed\"}\n",
        Outcome::Cancelled => "{\"result\":\"cancelled\"}\n",
    };
    let _ = (&mut &*stream).write_all(line.as_bytes());
    let _ = stream.shutdown(Shutdown::Both);
}

enum Stage {
    Waiting,
    Prompting(u64),
    /// With the helper. Removing `source` drops the connection and the password.
    Checking {
        source: RegistrationToken,
        timer: RegistrationToken,
    },
}

struct Auth {
    id: u64,
    request: Request,
    agent: UnixStream,
    stage: Stage,
    tries: u8,
}

/// The front of `queue` is the request being worked on.
#[derive(Default)]
pub struct Polkit {
    queue: VecDeque<Auth>,
    next: u64,
    next_token: u64,
    path: Option<PathBuf>,
}

impl std::fmt::Debug for Polkit {
    // A request holds a cookie: say only how many there are.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Polkit")
            .field("queued", &self.queue.len())
            .finish_non_exhaustive()
    }
}

// ---- the door ---------------------------------------------------------------

pub fn start(state: &mut AbyssState, handle: &LoopHandle<'static, AbyssState>) {
    let Some(dir) = crate::ipc::socket_dir() else {
        return;
    };
    if !dir.is_dir() {
        tracing::warn!("no control socket directory; no polkit prompt");
        return;
    }
    let path = dir.join("polkit.sock");
    if path.exists() {
        if UnixStream::connect(&path).is_ok() {
            tracing::error!(path = %path.display(), "polkit socket already in use; not binding");
            return;
        }
        let _ = std::fs::remove_file(&path);
    }
    let listener = match UnixListener::bind(&path) {
        Ok(l) => l,
        Err(e) => {
            tracing::warn!(path = %path.display(), %e, "binding the polkit socket");
            return;
        }
    };
    if let Err(e) = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)) {
        tracing::error!(%e, "tightening the polkit socket; removing it");
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
                        tracing::warn!(%e, "polkit socket accept");
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
    tracing::info!(path = %path.display(), "polkit socket listening");
    state.trusted_ui.polkit.path = Some(path);
}

pub fn cleanup(state: &AbyssState) {
    if let Some(path) = &state.trusted_ui.polkit.path {
        let _ = std::fs::remove_file(path);
    }
}

/// The session owner, running the installed agent. `exe` is `None` when the
/// peer is already gone or cannot be read, and that is a refusal.
fn admit(peer_uid: u32, owner: u32, exe: Option<&Path>) -> bool {
    peer_uid == owner && exe == Some(Path::new(AGENT_EXE))
}

fn accept(state: &mut AbyssState, stream: UnixStream) {
    let admitted = crate::ipc::peer_cred(&stream).is_some_and(|p| {
        let exe = std::fs::read_link(format!("/proc/{}/exe", p.pid)).ok();
        p.pid > 0 && admit(p.uid, crate::ipc::owner_uid(), exe.as_deref())
    });
    if !admitted {
        tracing::warn!("polkit socket peer is not the installed agent; refused");
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
    /// Owns request `id`; any further byte or EOF withdraws it.
    Pending(u64),
    Done,
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
        Conn::Pending(id) => {
            if eof || !got.is_empty() {
                abandon(state, *id);
                *conn = Conn::Done;
                return PostAction::Remove;
            }
            PostAction::Continue
        }
        Conn::Reading(buf) => {
            buf.extend_from_slice(&got);
            let refuse = |conn: &mut Conn| {
                reply(stream, Outcome::Failed);
                *conn = Conn::Done;
                PostAction::Remove
            };
            if buf.len() > MAX_LINE {
                return refuse(conn);
            }
            let Some(nl) = buf.iter().position(|b| *b == b'\n') else {
                if eof {
                    *conn = Conn::Done;
                    return PostAction::Remove;
                }
                return PostAction::Continue;
            };
            let request = (nl + 1 == buf.len())
                .then(|| Request::parse(&buf[..nl]))
                .flatten();
            let (Some(request), Ok(agent)) = (request, stream.try_clone()) else {
                return refuse(conn);
            };
            match ask(state, request, agent) {
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

// ---- the queue --------------------------------------------------------------

fn ask(state: &mut AbyssState, request: Request, agent: UnixStream) -> Option<u64> {
    let p = &mut state.trusted_ui.polkit;
    if p.queue.len() >= MAX_QUEUE {
        reply(&agent, Outcome::Failed);
        return None;
    }
    p.next += 1;
    let id = p.next;
    p.queue.push_back(Auth {
        id,
        request,
        agent,
        stage: Stage::Waiting,
        tries: 0,
    });
    schedule(state);
    Some(id)
}

/// The seat may be free: put the front request's prompt up.
pub fn schedule(state: &mut AbyssState) {
    if state.trusted_ui.is_open() {
        return;
    }
    let p = &mut state.trusted_ui.polkit;
    if !matches!(
        p.queue.front(),
        Some(Auth {
            stage: Stage::Waiting,
            ..
        })
    ) {
        return;
    }
    p.next_token += 1;
    let token = TOKEN_BASE | p.next_token;
    let Some(m) = p
        .queue
        .front()
        .and_then(|a| modal(token, &a.request, a.tries > 0))
    else {
        finish(state, Outcome::Failed);
        return;
    };
    if super::open(state, m) {
        if let Some(a) = state.trusted_ui.polkit.queue.front_mut() {
            a.stage = Stage::Prompting(token);
        }
        tracing::info!("polkit password prompt shown");
    }
}

/// Answer the front request and move on.
fn finish(state: &mut AbyssState, outcome: Outcome) {
    if let Some(a) = state.trusted_ui.polkit.queue.pop_front() {
        reply(&a.agent, outcome);
        tracing::info!(?outcome, "polkit authentication finished");
    }
    schedule(state);
}

/// The agent hung up (polkitd cancelled, or it died). A prompt comes down and an
/// exchange with the helper is dropped, password and all. A waiting prompt
/// behind it gets the seat.
fn abandon(state: &mut AbyssState, id: u64) {
    let p = &mut state.trusted_ui.polkit;
    let Some(i) = p.queue.iter().position(|a| a.id == id) else {
        return;
    };
    let Some(a) = p.queue.remove(i) else {
        return;
    };
    match a.stage {
        Stage::Waiting => {}
        Stage::Prompting(token) => {
            super::cancel(state, token);
            super::approval::schedule(state);
            super::consent::schedule(state);
            super::batch::schedule(state);
            super::install::schedule(state);
            super::broker::schedule(state);
        }
        Stage::Checking { source, timer } => {
            state.loop_handle.remove(source);
            state.loop_handle.remove(timer);
        }
    }
    schedule(state);
}

pub fn owns(state: &AbyssState, token: u64) -> bool {
    matches!(
        state.trusted_ui.polkit.queue.front(),
        Some(Auth { stage: Stage::Prompting(t), .. }) if *t == token
    )
}

/// The prompt was answered. Safe, Escape and the timeout are all a cancel.
pub fn answer(state: &mut AbyssState, choice: Choice) {
    if choice.role == Role::Safe {
        finish(state, Outcome::Cancelled);
        return;
    }
    let password = Zeroizing::new(choice.typed.as_ref().map(|t| t.as_str()).unwrap_or("").to_owned());
    drop(choice);
    if password.is_empty() {
        // Authenticate pressed with nothing typed. Asking PAM would spend a try
        // and count towards a lockout, so the prompt just comes back.
        if let Some(a) = state.trusted_ui.polkit.queue.front_mut() {
            a.stage = Stage::Waiting;
        }
        return;
    }
    if !check(state, password) {
        finish(state, Outcome::Failed);
    }
}

// ---- the prompt -------------------------------------------------------------

const HEADING: &str = "Authentication required";
const WRONG: &str = "That did not work. Check the password and try again.";
const BODY: &str = "Something on this computer needs an administrator's approval. What you type \
                    here goes only to the system's password check.";
const WELL: &str = "The request says (not verified):";

/// Authenticate is `Other`, not `Grant`: knowing the password is the grant, and
/// Enter with one typed is how a password is submitted. An empty field keeps the
/// focus rule, so a reflexive Enter is a cancel (§3.14).
const BUTTONS: [Button; 2] = [
    Button {
        label: "Cancel",
        role: Role::Safe,
    },
    Button {
        label: "Authenticate",
        role: Role::Other,
    },
];

fn modal(token: u64, request: &Request, wrong: bool) -> Option<Modal> {
    let claims = format!("{}\n\n{}", request.message, request.action);
    let m = Modal::new(
        token,
        HEADING,
        wrong.then_some(WRONG),
        BODY,
        WELL,
        &claims,
        BUTTONS.to_vec(),
    )
    .ok()?;
    Some(
        m.with_facts(&[("Password for:", request.user.as_str())])
            .with_secret_entry(MAX_PASSWORD)
            .submit_on_enter(),
    )
}

// ---- the helper -------------------------------------------------------------

/// What one line from the helper asks for. Grants nothing by itself: polkitd
/// heard the outcome from the helper first.
#[derive(Debug, PartialEq, Eq)]
enum Step {
    SendPassword,
    Passed,
    Wrong,
    Wait,
    /// Anything the prompt cannot answer. Fails the request, with no retry.
    Refuse,
}

fn step(line: &[u8], sent: bool) -> Step {
    if line == b"SUCCESS" {
        Step::Passed
    } else if line == b"FAILURE" {
        Step::Wrong
    } else if line.starts_with(b"PAM_PROMPT_ECHO_OFF ") && !sent {
        Step::SendPassword
    } else if line.starts_with(b"PAM_ERROR_MSG ") || line.starts_with(b"PAM_TEXT_INFO ") {
        Step::Wait
    } else {
        Step::Refuse
    }
}

/// Non-blocking: a full backlog is a failure, not a stall on the event loop.
/// `None` unless the peer is root.
fn dial(path: &Path) -> Option<UnixStream> {
    let fd: OwnedFd = net::socket_with(
        AddressFamily::UNIX,
        SocketType::STREAM,
        SocketFlags::CLOEXEC | SocketFlags::NONBLOCK,
        None,
    )
    .ok()?;
    net::connect(&fd, &SocketAddrUnix::new(path).ok()?).ok()?;
    let stream = UnixStream::from(fd);
    match crate::ipc::peer_cred(&stream) {
        Some(p) if p.uid == 0 => Some(stream),
        _ => {
            tracing::warn!("polkit helper socket is not root's; nothing sent");
            None
        }
    }
}

/// Start one exchange. `false`, with the password wiped, if it could not.
fn check(state: &mut AbyssState, password: Zeroizing<String>) -> bool {
    let Some(a) = state.trusted_ui.polkit.queue.front() else {
        return false;
    };
    let id = a.id;
    let Some(stream) = dial(Path::new(HELPER)) else {
        return false;
    };
    let mut hello = Zeroizing::new(Vec::with_capacity(
        a.request.user.len() + a.request.cookie.len() + 2,
    ));
    hello.extend_from_slice(a.request.user.as_bytes());
    hello.push(b'\n');
    hello.extend_from_slice(a.request.cookie.as_bytes());
    hello.push(b'\n');
    if !send_all(&stream, &hello) {
        return false;
    }
    let mut exchange = Exchange {
        buf: Vec::new(),
        password: Some(password),
        sent: false,
    };
    let source = state.loop_handle.insert_source(
        Generic::new(stream, Interest::READ, Mode::Level),
        move |_, stream, state| Ok(helper_readable(state, stream, &mut exchange, id)),
    );
    let Ok(source) = source else {
        return false;
    };
    let timer = state
        .loop_handle
        .insert_source(Timer::from_duration(CHECK_TIMEOUT), move |_, _, state| {
            helper_timed_out(state, id);
            TimeoutAction::Drop
        });
    let Ok(timer) = timer else {
        state.loop_handle.remove(source);
        return false;
    };
    if let Some(a) = state.trusted_ui.polkit.queue.front_mut() {
        a.stage = Stage::Checking { source, timer };
    }
    true
}

/// These lines are far smaller than the socket buffer: a short send is a failure.
fn send_all(stream: &UnixStream, bytes: &[u8]) -> bool {
    matches!(net::send(stream, bytes, SendFlags::NOSIGNAL), Ok(n) if n == bytes.len())
}

struct Exchange {
    buf: Vec<u8>,
    password: Option<Zeroizing<String>>,
    sent: bool,
}

fn helper_timed_out(state: &mut AbyssState, id: u64) {
    let front = state.trusted_ui.polkit.queue.front();
    if let Some(Auth {
        id: fid,
        stage: Stage::Checking { source, .. },
        ..
    }) = front
    {
        if *fid == id {
            let source = *source;
            state.loop_handle.remove(source);
            tracing::warn!("polkit helper did not answer in time");
            finish(state, Outcome::Failed);
        }
    }
}

/// The exchange is over: stop its timer, then act on the result.
fn settle(state: &mut AbyssState, id: u64, outcome: Option<Outcome>) {
    let Some(a) = state.trusted_ui.polkit.queue.front_mut().filter(|a| a.id == id) else {
        return;
    };
    if let Stage::Checking { timer, .. } = a.stage {
        state.loop_handle.remove(timer);
    }
    match outcome {
        Some(o) => finish(state, o),
        // A wrong password: ask again while tries remain.
        None => {
            a.tries += 1;
            if a.tries >= TRIES {
                finish(state, Outcome::Failed);
            } else {
                a.stage = Stage::Waiting;
                schedule(state);
            }
        }
    }
}

fn helper_readable(state: &mut AbyssState, stream: &UnixStream, x: &mut Exchange, id: u64) -> PostAction {
    let mut chunk = [0u8; 512];
    let mut eof = false;
    loop {
        match (&mut &*stream).read(&mut chunk) {
            Ok(0) => {
                eof = true;
                break;
            }
            Ok(n) => x.buf.extend_from_slice(&chunk[..n]),
            Err(e) if e.kind() == ErrorKind::WouldBlock => break,
            Err(e) if e.kind() == ErrorKind::Interrupted => continue,
            Err(_) => {
                eof = true;
                break;
            }
        }
    }
    while let Some(nl) = x.buf.iter().position(|b| *b == b'\n') {
        match step(&x.buf[..nl], x.sent) {
            Step::SendPassword => {
                let Some(password) = x.password.take() else {
                    settle(state, id, Some(Outcome::Failed));
                    return PostAction::Remove;
                };
                let mut out = Zeroizing::new(Vec::with_capacity(password.len() + 1));
                out.extend_from_slice(password.as_bytes());
                out.push(b'\n');
                x.sent = true;
                if !send_all(stream, &out) {
                    settle(state, id, Some(Outcome::Failed));
                    return PostAction::Remove;
                }
            }
            Step::Passed => {
                settle(state, id, Some(Outcome::Ok));
                return PostAction::Remove;
            }
            Step::Wrong => {
                settle(state, id, None);
                return PostAction::Remove;
            }
            Step::Wait => {}
            Step::Refuse => {
                tracing::warn!("polkit helper asked for more than a password; failing");
                settle(state, id, Some(Outcome::Failed));
                return PostAction::Remove;
            }
        }
        x.buf.drain(..=nl);
    }
    if eof || x.buf.len() > MAX_LINE {
        settle(state, id, Some(Outcome::Failed));
        return PostAction::Remove;
    }
    PostAction::Continue
}

#[cfg(test)]
mod tests {
    use std::io::{BufRead, BufReader};

    use smithay::input::keyboard::Keysym;

    use super::*;
    use crate::shell::focus::state_tests::harness;
    use crate::trusted_ui::{arm_now, key};

    const REQ: &[u8] =
        b"{\"user\":\"ada\",\"cookie\":\"c-1\",\"action\":\"org.example.do\",\"message\":\"Do it\"}\n";

    fn connect(state: &mut AbyssState, bytes: &[u8]) -> (UnixStream, UnixStream, Conn, PostAction) {
        let (client, server) = UnixStream::pair().unwrap();
        server.set_nonblocking(true).unwrap();
        (&client).write_all(bytes).unwrap();
        let mut conn = Conn::Reading(Vec::new());
        let post = readable(state, &server, &mut conn);
        (client, server, conn, post)
    }

    fn heard(client: &UnixStream) -> Option<String> {
        client.set_read_timeout(Some(Duration::from_millis(200))).ok()?;
        let mut line = String::new();
        BufReader::new(client).read_line(&mut line).ok()?;
        (!line.is_empty()).then_some(line.trim().to_string())
    }

    #[test]
    fn only_the_session_owner_running_the_installed_agent_gets_in() {
        let agent = Path::new(AGENT_EXE);
        assert!(admit(1000, 1000, Some(agent)));
        assert!(!admit(1001, 1000, Some(agent)), "another user");
        assert!(!admit(0, 1000, Some(agent)), "root is not the session owner");
        assert!(!admit(1000, 1000, Some(Path::new("/tmp/ec-polkit-agent"))));
        assert!(!admit(1000, 1000, None), "a peer that cannot be read");
    }

    #[test]
    fn a_field_that_could_forge_a_helper_line_is_refused() {
        for bad in [
            r#"{"user":"ada\nroot","cookie":"c","action":"a.b","message":"m"}"#,
            r#"{"user":"ada","cookie":"c\nd","action":"a.b","message":"m"}"#,
            r#"{"user":"ada","cookie":"c d","action":"a.b","message":"m"}"#,
            r#"{"user":"-ada","cookie":"c","action":"a.b","message":"m"}"#,
            r#"{"user":"","cookie":"c","action":"a.b","message":"m"}"#,
            r#"{"user":"ada","cookie":"","action":"a.b","message":"m"}"#,
            r#"{"user":"ada","cookie":"c","action":"a b","message":"m"}"#,
            r#"{"user":"ada","cookie":"c","action":"a.b"}"#,
            r#"{"user":"ada","cookie":"c","action":"a.b","message":"m","x":1}"#,
            "garbage",
        ] {
            assert!(Request::parse(bad.as_bytes()).is_none(), "{bad}");
        }
        assert!(Request::parse(&REQ[..REQ.len() - 1]).is_some());
    }

    #[test]
    fn the_password_answers_one_no_echo_prompt_and_nothing_else() {
        assert_eq!(step(b"PAM_PROMPT_ECHO_OFF Password: ", false), Step::SendPassword);
        assert_eq!(step(b"PAM_PROMPT_ECHO_OFF Password: ", true), Step::Refuse);
        assert_eq!(step(b"PAM_PROMPT_ECHO_ON login: ", false), Step::Refuse);
        assert_eq!(step(b"PAM_TEXT_INFO hello", false), Step::Wait);
        assert_eq!(step(b"PAM_ERROR_MSG nope", true), Step::Wait);
        assert_eq!(step(b"SUCCESS", true), Step::Passed);
        assert_eq!(step(b"FAILURE", true), Step::Wrong);
        assert_eq!(step(b"SUCCESSFUL", true), Step::Refuse);
        assert_eq!(step(b"", false), Step::Refuse);
    }

    #[test]
    fn a_request_raises_the_prompt_and_cancel_tells_the_agent() {
        let mut h = harness();
        let (client, _server, conn, post) = connect(&mut h.state, REQ);
        assert!(matches!(conn, Conn::Pending(_)) && matches!(post, PostAction::Continue));
        assert!(crate::trusted_ui::holds_seat(&h.state));
        arm_now(&mut h.state);
        key(&mut h.state, Keysym::Escape);
        assert_eq!(heard(&client).as_deref(), Some(r#"{"result":"cancelled"}"#));
        assert!(!h.state.trusted_ui.active());
    }

    #[test]
    fn an_empty_enter_cancels() {
        let mut h = harness();
        let (client, ..) = connect(&mut h.state, REQ);
        arm_now(&mut h.state);
        key(&mut h.state, Keysym::Return);
        assert_eq!(heard(&client).as_deref(), Some(r#"{"result":"cancelled"}"#));
    }

    #[test]
    fn tab_makes_enter_follow_the_focus() {
        let mut h = harness();
        let (client, ..) = connect(&mut h.state, REQ);
        arm_now(&mut h.state);
        key(&mut h.state, Keysym::a);
        key(&mut h.state, Keysym::Tab);
        key(&mut h.state, Keysym::Tab);
        key(&mut h.state, Keysym::Return);
        assert_eq!(heard(&client).as_deref(), Some(r#"{"result":"cancelled"}"#));
    }

    #[test]
    fn authenticate_with_nothing_typed_asks_again() {
        let mut h = harness();
        let (client, ..) = connect(&mut h.state, REQ);
        let first = h.state.trusted_ui.token().unwrap();
        arm_now(&mut h.state);
        key(&mut h.state, Keysym::Tab);
        key(&mut h.state, Keysym::Return);
        assert_eq!(heard(&client), None);
        let again = h.state.trusted_ui.token().unwrap();
        assert_ne!(first, again);
        assert!(owns(&h.state, again));
        assert_eq!(h.state.trusted_ui.polkit.queue[0].tries, 0);
    }

    #[test]
    fn a_withdrawn_prompt_gives_the_seat_to_a_waiting_request() {
        let mut h = harness();
        let other = Modal::new(1, "Other", None, "Body", "Well", "words", BUTTONS.to_vec()).unwrap();
        assert!(super::super::open(&mut h.state, other));
        let (_client, ..) = connect(&mut h.state, REQ);
        assert_eq!(h.state.trusted_ui.token(), Some(1));
        super::super::cancel(&mut h.state, 1);
        let token = h.state.trusted_ui.token().unwrap();
        assert!(owns(&h.state, token));
    }

    #[test]
    fn hanging_up_takes_the_prompt_down_and_the_next_comes_up() {
        let mut h = harness();
        let (first, server, mut conn, _) = connect(&mut h.state, REQ);
        let (second, ..) = connect(&mut h.state, REQ);
        let token = h.state.trusted_ui.token().unwrap();
        drop(first);
        assert!(matches!(
            readable(&mut h.state, &server, &mut conn),
            PostAction::Remove
        ));
        let next = h.state.trusted_ui.token().unwrap();
        assert_ne!(next, token, "a stale timer must not reach the next prompt");
        assert!(owns(&h.state, next));
        arm_now(&mut h.state);
        key(&mut h.state, Keysym::Escape);
        assert_eq!(heard(&second).as_deref(), Some(r#"{"result":"cancelled"}"#));
    }

    #[test]
    fn talking_after_the_request_withdraws_it() {
        let mut h = harness();
        let (client, server, mut conn, _) = connect(&mut h.state, REQ);
        (&client).write_all(b"ok\n").unwrap();
        assert!(matches!(
            readable(&mut h.state, &server, &mut conn),
            PostAction::Remove
        ));
        assert!(!h.state.trusted_ui.active());
    }

    #[test]
    fn a_flood_is_refused_past_the_queue() {
        let mut h = harness();
        let mut held = Vec::new();
        for _ in 0..MAX_QUEUE {
            held.push(connect(&mut h.state, REQ));
        }
        let (client, ..) = connect(&mut h.state, REQ);
        assert_eq!(heard(&client).as_deref(), Some(r#"{"result":"failed"}"#));
    }

    #[test]
    fn bad_input_gets_no_prompt() {
        let mut h = harness();
        let (client, _s, conn, _) = connect(&mut h.state, b"garbage\n");
        assert_eq!(heard(&client).as_deref(), Some(r#"{"result":"failed"}"#));
        assert!(matches!(conn, Conn::Done));
        assert!(!h.state.trusted_ui.active());
    }

    #[test]
    fn the_prompt_masks_and_never_prints_the_password_or_cookie() {
        let mut h = harness();
        let (_client, ..) = connect(&mut h.state, REQ);
        arm_now(&mut h.state);
        for c in "hunter22".chars() {
            let sym = Keysym::from_char(c);
            key(&mut h.state, sym);
        }
        let shown = format!("{:?}", h.state.trusted_ui);
        assert!(!shown.contains("hunter"), "{shown}");
        assert!(!shown.contains("c-1"), "{shown}");
        assert!(shown.contains("masked: true"));
    }

    /// The exchange against a stand-in helper: exact bytes, password once, and
    /// the outcome relayed.
    #[test]
    fn the_exchange_sends_the_password_once_and_relays_the_result() {
        let mut h = harness();
        let (agent, _s, ..) = connect(&mut h.state, REQ);
        let (helper, ours) = UnixStream::pair().unwrap();
        ours.set_nonblocking(true).unwrap();
        (&helper)
            .write_all(b"PAM_TEXT_INFO hi\nPAM_PROMPT_ECHO_OFF Password: \n")
            .unwrap();
        let id = h.state.trusted_ui.polkit.queue.front().unwrap().id;
        let mut x = Exchange {
            buf: Vec::new(),
            password: Some(Zeroizing::new("hunter22".into())),
            sent: false,
        };
        assert!(matches!(
            helper_readable(&mut h.state, &ours, &mut x, id),
            PostAction::Continue
        ));
        assert!(x.password.is_none() && x.sent, "taken and wiped once sent");
        let mut line = String::new();
        BufReader::new(&helper).read_line(&mut line).unwrap();
        assert_eq!(line, "hunter22\n");
        (&helper).write_all(b"SUCCESS\n").unwrap();
        assert!(matches!(
            helper_readable(&mut h.state, &ours, &mut x, id),
            PostAction::Remove
        ));
        assert_eq!(heard(&agent).as_deref(), Some(r#"{"result":"ok"}"#));
    }

    #[test]
    fn wrong_passwords_ask_again_then_fail() {
        let mut h = harness();
        let (agent, _s, ..) = connect(&mut h.state, REQ);
        let id = h.state.trusted_ui.polkit.queue.front().unwrap().id;
        for _ in 0..TRIES - 1 {
            h.state.trusted_ui.open = None;
            settle(&mut h.state, id, None);
            assert!(h.state.trusted_ui.is_open(), "asked again");
        }
        h.state.trusted_ui.open = None;
        settle(&mut h.state, id, None);
        assert!(!h.state.trusted_ui.is_open());
        assert_eq!(heard(&agent).as_deref(), Some(r#"{"result":"failed"}"#));
    }

    #[test]
    fn a_helper_that_hangs_up_without_a_result_is_a_failure() {
        let mut h = harness();
        let (agent, _s, ..) = connect(&mut h.state, REQ);
        let (helper, ours) = UnixStream::pair().unwrap();
        ours.set_nonblocking(true).unwrap();
        drop(helper);
        let id = h.state.trusted_ui.polkit.queue.front().unwrap().id;
        let mut x = Exchange {
            buf: Vec::new(),
            password: Some(Zeroizing::new("x".into())),
            sent: false,
        };
        assert!(matches!(
            helper_readable(&mut h.state, &ours, &mut x, id),
            PostAction::Remove
        ));
        assert_eq!(heard(&agent).as_deref(), Some(r#"{"result":"failed"}"#));
    }

    #[test]
    fn nothing_is_sent_to_a_helper_that_is_not_root() {
        // This test's own socket is owned by this (non-root) user.
        let path = std::env::temp_dir().join(format!("ec-polkit-test-{}.sock", std::process::id()));
        let _ = std::fs::remove_file(&path);
        let _l = UnixListener::bind(&path).unwrap();
        if crate::ipc::owner_uid() != 0 {
            assert!(dial(&path).is_none());
        }
        let _ = std::fs::remove_file(&path);
    }
}
