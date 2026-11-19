// SPDX-License-Identifier: AGPL-3.0-only
//! The production [`Auth`]: the greetd IPC (`man greetd-ipc`). **Security-critical.**
//!
//! Frames are a native-endian `u32` length then JSON. greetd runs PAM; this only
//! relays. The secret is written straight from its wiped buffer into a frame that
//! is zeroed after the write, and is never held in a `String` or logged.
//!
//! One worker thread per attempt owns the socket. The UI reads [`Auth::state`]
//! and never waits. Any socket or protocol fault ends in `Refused(Unavailable)`,
//! and a bad password restarts the conversation for the same user, so the form
//! can retry. The session starts only on greetd's own `success` to the last answer.

use std::io::{Read, Write};
use std::os::unix::net::UnixStream;
use std::process::{Command, Stdio};
use std::sync::mpsc::{channel, Receiver, Sender, TryRecvError};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::Value;

use crate::auth::{Auth, AuthState, Power, Refusal};
use crate::secret::Secret;

/// What the greeter starts for a signed-in user (`packaging/abyss.desktop`).
pub const SESSION_CMD: &str = "/usr/bin/abyss-session";
/// A reply from greetd that takes longer than this is a dead greetd.
const REPLY: Duration = Duration::from_secs(60);
/// The longest frame accepted from the socket.
const MAX_FRAME: usize = 64 * 1024;
const MIN_UID: u32 = 1000;
const MAX_UID: u32 = 60000;

enum Cmd {
    Secret(Secret),
    Cancel,
}

pub struct GreetdAuth {
    sock: String,
    users: Vec<String>,
    state: Arc<Mutex<AuthState>>,
    tx: Option<Sender<Cmd>>,
}

impl GreetdAuth {
    /// Reads `GREETD_SOCK` and the account list. `None` without the socket.
    pub fn from_env() -> Option<Self> {
        let sock = std::env::var("GREETD_SOCK").ok().filter(|s| !s.is_empty())?;
        let passwd = std::fs::read_to_string("/etc/passwd").unwrap_or_default();
        Some(GreetdAuth {
            sock,
            users: login_users(&passwd),
            state: Arc::new(Mutex::new(AuthState::Idle)),
            tx: None,
        })
    }

    fn set(&self, s: AuthState) {
        set(&self.state, s);
    }

    fn end_worker(&mut self) {
        if let Some(tx) = self.tx.take() {
            let _ = tx.send(Cmd::Cancel);
        }
    }
}

fn set(state: &Mutex<AuthState>, s: AuthState) {
    if let Ok(mut g) = state.lock() {
        *g = s;
    }
}

/// Accounts a person logs in as: uid in the normal range, a real shell.
pub fn login_users(passwd: &str) -> Vec<String> {
    let mut out: Vec<(u32, String)> = passwd
        .lines()
        .filter_map(|l| {
            let f: Vec<&str> = l.split(':').collect();
            let (name, uid, shell) = (f.first()?, f.get(2)?.parse::<u32>().ok()?, f.get(6)?);
            let real = !(shell.ends_with("nologin") || shell.ends_with("/false") || shell.is_empty());
            let plain = !name.is_empty()
                && name
                    .chars()
                    .all(|c| c.is_ascii_graphic() && c != '"' && c != '\\');
            (real && plain && (MIN_UID..=MAX_UID).contains(&uid)).then(|| (uid, (*name).to_owned()))
        })
        .collect();
    out.sort();
    out.into_iter().map(|(_, n)| n).collect()
}

impl Auth for GreetdAuth {
    fn users(&self) -> Vec<String> {
        self.users.clone()
    }

    fn begin(&mut self, user: &str) {
        self.end_worker();
        if !self.users.iter().any(|u| u == user) {
            self.set(AuthState::Refused(Refusal::Unavailable));
            return;
        }
        self.set(AuthState::Starting);
        let (tx, rx) = channel();
        self.tx = Some(tx);
        let (sock, user, state) = (self.sock.clone(), user.to_owned(), self.state.clone());
        std::thread::spawn(move || {
            let end = converse(&sock, &user, &state, &rx);
            set(&state, end);
        });
    }

    fn submit(&mut self, secret: Secret) {
        if !self.state().takes_keys() {
            return;
        }
        self.set(AuthState::Checking);
        if let Some(tx) = &self.tx {
            let _ = tx.send(Cmd::Secret(secret));
        }
    }

    fn cancel(&mut self) {
        self.end_worker();
        self.set(AuthState::Idle);
    }

    fn power(&mut self, action: Power) {
        let verb = match action {
            Power::Suspend => "suspend",
            Power::Restart => "reboot",
            Power::PowerOff => "poweroff",
        };
        let _ = Command::new("systemctl")
            .arg(verb)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn();
    }

    fn state(&self) -> AuthState {
        self.state
            .lock()
            .map(|g| *g)
            .unwrap_or(AuthState::Refused(Refusal::Unavailable))
    }
}

fn write_frame(s: &mut UnixStream, mut body: Vec<u8>) -> std::io::Result<()> {
    let mut frame = Vec::with_capacity(body.len() + 4);
    frame.extend_from_slice(&(body.len() as u32).to_ne_bytes());
    frame.extend_from_slice(&body);
    wipe(&mut body);
    let r = s.write_all(&frame);
    wipe(&mut frame);
    r
}

fn wipe(v: &mut [u8]) {
    for b in v.iter_mut() {
        // SAFETY: a valid, aligned, exclusive pointer; volatile so it is not elided.
        unsafe { std::ptr::write_volatile(b, 0) };
    }
}

fn read_frame(s: &mut UnixStream) -> Option<Value> {
    let mut len = [0u8; 4];
    s.read_exact(&mut len).ok()?;
    let len = u32::from_ne_bytes(len) as usize;
    if len > MAX_FRAME {
        return None;
    }
    let mut body = vec![0u8; len];
    s.read_exact(&mut body).ok()?;
    serde_json::from_slice(&body).ok()
}

fn json(v: Value) -> Vec<u8> {
    v.to_string().into_bytes()
}

/// `{"type":"post_auth_message_response","response":"<secret>"}`, built by hand
/// so the secret goes from its buffer to the frame with no `String` between.
fn answer(secret: &[u8]) -> Option<Vec<u8>> {
    let text = std::str::from_utf8(secret).ok()?;
    let mut out = Vec::with_capacity(secret.len() * 2 + 64);
    out.extend_from_slice(br#"{"type":"post_auth_message_response","response":""#);
    for c in text.chars() {
        match c {
            '"' => out.extend_from_slice(b"\\\""),
            '\\' => out.extend_from_slice(b"\\\\"),
            c if (c as u32) < 0x20 => out.extend_from_slice(format!("\\u{:04x}", c as u32).as_bytes()),
            c => {
                let mut b = [0u8; 4];
                out.extend_from_slice(c.encode_utf8(&mut b).as_bytes());
                wipe(&mut b);
            }
        }
    }
    out.extend_from_slice(b"\"}");
    Some(out)
}

/// One login attempt, retried on a wrong password. Returns the state it ends in.
fn converse(sock: &str, user: &str, state: &Mutex<AuthState>, rx: &Receiver<Cmd>) -> AuthState {
    let unavailable = AuthState::Refused(Refusal::Unavailable);
    let Ok(mut s) = UnixStream::connect(sock) else {
        return unavailable;
    };
    let _ = s.set_read_timeout(Some(REPLY));
    let _ = s.set_write_timeout(Some(REPLY));
    let mut refused = false;
    'session: loop {
        if write_frame(
            &mut s,
            json(serde_json::json!({"type":"create_session","username":user})),
        )
        .is_err()
        {
            return unavailable;
        }
        loop {
            let Some(reply) = read_frame(&mut s) else {
                return unavailable;
            };
            match reply["type"].as_str() {
                Some("auth_message") => match reply["auth_message_type"].as_str() {
                    Some("secret" | "visible") => {
                        if !refused {
                            set(state, AuthState::Prompting);
                        }
                        let secret = match rx.recv() {
                            Ok(Cmd::Secret(sec)) => sec,
                            _ => {
                                let _ =
                                    write_frame(&mut s, json(serde_json::json!({"type":"cancel_session"})));
                                return AuthState::Idle;
                            }
                        };
                        let Some(body) = answer(secret.as_bytes()) else {
                            return unavailable;
                        };
                        drop(secret);
                        if write_frame(&mut s, body).is_err() {
                            return unavailable;
                        }
                    }
                    // Info and error lines need no answer beyond an acknowledgement.
                    _ => {
                        let ack =
                            json(serde_json::json!({"type":"post_auth_message_response","response":null}));
                        if write_frame(&mut s, ack).is_err() {
                            return unavailable;
                        }
                    }
                },
                Some("success") => {
                    // Authenticated: start the session. greetd replies `success` once
                    // it will exec it when this process exits.
                    let start =
                        json(serde_json::json!({"type":"start_session","cmd":[SESSION_CMD],"env":[]}));
                    if write_frame(&mut s, start).is_err() {
                        return unavailable;
                    }
                    return match read_frame(&mut s).as_ref().and_then(|v| v["type"].as_str()) {
                        Some("success") => AuthState::Granted,
                        _ => unavailable,
                    };
                }
                Some("error") if reply["error_type"].as_str() == Some("auth_error") => {
                    // greetd has dropped the session; open a fresh one for a retry,
                    // staying `Refused` until a new prompt is wanted.
                    set(state, AuthState::Refused(Refusal::Wrong));
                    refused = true;
                    // A cancel (another account chosen) ends this attempt.
                    match rx.try_recv() {
                        Ok(Cmd::Cancel) | Err(TryRecvError::Disconnected) => return AuthState::Idle,
                        _ => continue 'session,
                    }
                }
                _ => return unavailable,
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::net::UnixListener;

    #[test]
    fn people_are_listed_and_system_accounts_are_not() {
        let p = "root:x:0:0::/root:/bin/bash\nsam:x:1001:1001::/home/sam:/bin/zsh\n\
                 ada:x:1000:1000::/home/ada:/bin/bash\ngreeter:x:970:970::/:/usr/bin/nologin\n\
                 old:x:1002:1002::/home/old:/bin/false\nbad\"name:x:1003:1003::/:/bin/sh\nnobody:x:65534:65534::/:/bin/bash\n";
        assert_eq!(login_users(p), ["ada", "sam"]);
    }

    #[test]
    fn the_answer_frame_escapes_and_is_valid_json() {
        let body = answer("p\"a\\ss\u{1}é".as_bytes()).unwrap();
        let v: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(v["response"], "p\"a\\ss\u{1}é");
        assert_eq!(v["type"], "post_auth_message_response");
        assert!(answer(&[0xff, 0xfe]).is_none());
    }

    /// A stand-in greetd: one wrong answer, then the right one.
    fn serve(l: UnixListener, right: &'static str) {
        std::thread::spawn(move || {
            let (mut s, _) = l.accept().unwrap();
            let mut tries = 0;
            loop {
                let Some(req) = read_frame(&mut s) else { return };
                let reply = match req["type"].as_str().unwrap() {
                    "create_session" => {
                        serde_json::json!({"type":"auth_message","auth_message_type":"secret","auth_message":"Password:"})
                    }
                    "post_auth_message_response" => {
                        tries += 1;
                        if req["response"] == right {
                            serde_json::json!({"type":"success"})
                        } else {
                            serde_json::json!({"type":"error","error_type":"auth_error","description":"no"})
                        }
                    }
                    "start_session" => {
                        assert_eq!(req["cmd"][0], SESSION_CMD);
                        write_frame(&mut s, json(serde_json::json!({"type":"success"}))).unwrap();
                        assert_eq!(tries, 2);
                        return;
                    }
                    _ => return,
                };
                write_frame(&mut s, json(reply)).unwrap();
            }
        });
    }

    fn wait(a: &GreetdAuth, want: AuthState) {
        for _ in 0..200 {
            if a.state() == want {
                return;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        panic!("state {:?}, wanted {want:?}", a.state());
    }

    fn typed(s: &str) -> Secret {
        let mut x = Secret::new();
        x.push_str(s);
        x
    }

    #[test]
    fn a_wrong_password_retries_and_the_right_one_starts_the_session() {
        let path = std::env::temp_dir().join(format!("ecg-{}.sock", std::process::id()));
        let _ = std::fs::remove_file(&path);
        serve(UnixListener::bind(&path).unwrap(), "good");
        let mut a = GreetdAuth {
            sock: path.to_string_lossy().into(),
            users: vec!["sam".into()],
            state: Arc::new(Mutex::new(AuthState::Idle)),
            tx: None,
        };
        a.begin("sam");
        wait(&a, AuthState::Prompting);
        a.submit(typed("bad"));
        wait(&a, AuthState::Refused(Refusal::Wrong));
        a.submit(typed("good"));
        wait(&a, AuthState::Granted);
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn no_socket_means_unavailable_and_an_unknown_user_is_refused() {
        let mut a = GreetdAuth {
            sock: "/nonexistent/greetd.sock".into(),
            users: vec!["sam".into()],
            state: Arc::new(Mutex::new(AuthState::Idle)),
            tx: None,
        };
        a.begin("sam");
        wait(&a, AuthState::Refused(Refusal::Unavailable));
        a.begin("root");
        assert_eq!(a.state(), AuthState::Refused(Refusal::Unavailable));
    }
}
