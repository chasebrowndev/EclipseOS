// SPDX-License-Identifier: AGPL-3.0-only
//! `ec-polkit-agent`: the session's polkit authentication agent (COMP-10 §3.14,
//! D-07 §6, ADR 0088). **TCB.**
//!
//! It holds the system-bus side and nothing else. polkitd's `BeginAuthentication`
//! is relayed to the compositor over `polkit.sock`; the compositor draws the
//! prompt and hands the password straight to polkit's root helper. This process
//! never sees it, and hears back only ok, failed or cancelled.
//!
//! Each call runs on its own thread, and a cancel shuts that call's connection,
//! which is how the compositor hears about it.

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Write};
use std::net::Shutdown;
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use zbus::blocking::{Connection, MessageIterator, Proxy};
use zbus::message::{Header, Type};
use zbus::zvariant::{OwnedObjectPath, OwnedValue, Value};

const OBJECT_PATH: &str = "/org/eclipse/PolkitAgent";
const POLKIT: &str = "org.freedesktop.PolicyKit1";
const AGENT_IFACE: &str = "org.freedesktop.PolicyKit1.AuthenticationAgent";
const FAILED: &str = "org.freedesktop.PolicyKit1.Error.Failed";
const CANCELLED: &str = "org.freedesktop.PolicyKit1.Error.Cancelled";
/// Past the prompt's own timeout and three helper checks. Only a stuck
/// compositor ever reaches this.
const WAIT: Duration = Duration::from_secs(600);

type Identity = (String, HashMap<String, OwnedValue>);
type Begin = (
    String,
    String,
    String,
    HashMap<String, String>,
    String,
    Vec<Identity>,
);
/// The live calls, by cookie.
type Calls = Arc<Mutex<HashMap<String, UnixStream>>>;

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            // Journald picks stderr up. The unit retries; until then polkit
            // has no agent and asks fail, as they did before this existed.
            eprintln!("ec-polkit-agent: {e}");
            ExitCode::FAILURE
        }
    }
}

fn run() -> zbus::Result<()> {
    let conn = Connection::system()?;
    // Before registering, so the first call cannot be missed.
    let messages = MessageIterator::from(&conn);
    register(&conn)?;
    let calls: Calls = Arc::default();
    for msg in messages {
        let msg = msg?;
        if msg.message_type() != Type::MethodCall {
            continue;
        }
        let header = msg.header();
        let member = header.member().map(|m| m.as_str().to_owned());
        if !from_polkit(&conn, &header) {
            // polkit's bus policy already keeps everyone else off this
            // interface. A second check, so a policy change cannot put a
            // prompt up.
            let _ = conn.reply_error(&header, FAILED, &"not polkitd");
            continue;
        }
        match member.as_deref() {
            Some("BeginAuthentication") => match msg.body().deserialize::<Begin>() {
                Ok(args) => {
                    let cookie = args.4.clone();
                    // Here and not on the thread, so a cancel arriving next
                    // finds the call.
                    let stream = open_call(&calls, &cookie);
                    let (c, list, call) = (conn.clone(), calls.clone(), msg.clone());
                    let spawned = std::thread::Builder::new()
                        .name("polkit-call".into())
                        .spawn(move || begin(&c, &list, &call.header(), args, stream));
                    if spawned.is_err() {
                        if let Ok(mut c) = calls.lock() {
                            c.remove(&cookie);
                        }
                        let _ = conn.reply_error(&header, FAILED, &"no thread");
                    }
                }
                Err(_) => {
                    let _ = conn.reply_error(&header, FAILED, &"bad arguments");
                }
            },
            Some("CancelAuthentication") => {
                if let Ok(cookie) = msg.body().deserialize::<String>() {
                    if let Some(s) = calls.lock().ok().and_then(|mut c| c.remove(&cookie)) {
                        let _ = s.shutdown(Shutdown::Both);
                    }
                }
                let _ = conn.reply(&header, &());
            }
            _ => {
                let _ = conn.reply_error(
                    &header,
                    "org.freedesktop.DBus.Error.UnknownMethod",
                    &format!("{AGENT_IFACE} has no such method"),
                );
            }
        }
    }
    Ok(())
}

/// Whether polkitd itself sent this. Asked each time: polkitd can restart.
fn from_polkit(conn: &Connection, header: &Header<'_>) -> bool {
    let Some(sender) = header.sender() else {
        return false;
    };
    let owner = conn
        .call_method(
            Some("org.freedesktop.DBus"),
            "/org/freedesktop/DBus",
            Some("org.freedesktop.DBus"),
            "GetNameOwner",
            &(POLKIT,),
        )
        .and_then(|r| r.body().deserialize::<String>());
    owner.is_ok_and(|o| o == sender.as_str())
}

/// Register for the session polkitd will resolve this process to: a user unit
/// is in no session, so polkitd falls back to the user's display session, which
/// is exactly logind's `Display` property. Anything else is refused.
fn register(conn: &Connection) -> zbus::Result<()> {
    let user = Proxy::new(
        conn,
        "org.freedesktop.login1",
        "/org/freedesktop/login1/user/self",
        "org.freedesktop.login1.User",
    )?;
    let (session, _): (String, OwnedObjectPath) = user.get_property("Display")?;
    if session.is_empty() {
        return Err(zbus::Error::Failure(
            "this user has no display session yet".into(),
        ));
    }
    let locale = std::env::var("LANG").unwrap_or_else(|_| "C".into());
    let mut details = HashMap::new();
    details.insert("session-id", Value::from(session.as_str()));
    let subject = ("unix-session", details);
    conn.call_method(
        Some(POLKIT),
        "/org/freedesktop/PolicyKit1/Authority",
        Some("org.freedesktop.PolicyKit1.Authority"),
        "RegisterAuthenticationAgent",
        &(subject, locale.as_str(), OBJECT_PATH),
    )?;
    eprintln!("ec-polkit-agent: registered for session {session}");
    Ok(())
}

/// Connect to the compositor and list the call under its cookie.
fn open_call(calls: &Calls, cookie: &str) -> Option<UnixStream> {
    let stream = UnixStream::connect(door_path()?).ok()?;
    let dup = stream.try_clone().ok()?;
    calls.lock().ok()?.insert(cookie.to_owned(), dup);
    Some(stream)
}

/// One authentication, start to finish.
fn begin(conn: &Connection, calls: &Calls, header: &Header<'_>, args: Begin, stream: Option<UnixStream>) {
    let (action, message, _icon, _details, cookie, identities) = args;
    let result = match stream {
        Some(stream) => relay(&stream, &action, &message, &cookie, &identities),
        None => Relay::Failed,
    };
    if let Ok(mut c) = calls.lock() {
        c.remove(&cookie);
    }
    let _ = match result {
        Relay::Ok => conn.reply(header, &()),
        Relay::Cancelled => conn.reply_error(header, CANCELLED, &"cancelled"),
        Relay::Failed => conn.reply_error(header, FAILED, &"authentication failed"),
    };
}

#[derive(Debug, PartialEq, Eq)]
enum Relay {
    Ok,
    Failed,
    Cancelled,
}

fn relay(stream: &UnixStream, action: &str, message: &str, cookie: &str, identities: &[Identity]) -> Relay {
    let uids: Vec<u32> = identities.iter().filter_map(identity_uid).collect();
    // SAFETY: `getuid` is always safe; it cannot fail and touches no memory.
    let me = unsafe { libc::getuid() };
    let Some(user) = choose(&uids, me).and_then(user_name) else {
        return Relay::Failed;
    };
    if stream.set_read_timeout(Some(WAIT)).is_err() {
        return Relay::Failed;
    }
    let mut line = serde_json::json!({
        "user": user,
        "cookie": cookie,
        "action": action,
        "message": message,
    })
    .to_string();
    line.push('\n');
    let mut writer = stream;
    if writer.write_all(line.as_bytes()).is_err() {
        return Relay::Failed;
    }
    let mut answer = String::new();
    // EOF is what a cancel looks like from here, or a compositor gone.
    match BufReader::new(stream).read_line(&mut answer) {
        Ok(n) if n > 0 => parse_answer(&answer),
        _ => Relay::Cancelled,
    }
}

fn parse_answer(line: &str) -> Relay {
    let v: serde_json::Value = serde_json::from_str(line).unwrap_or_default();
    match v.get("result").and_then(|r| r.as_str()) {
        Some("ok") => Relay::Ok,
        Some("cancelled") => Relay::Cancelled,
        _ => Relay::Failed,
    }
}

fn door_path() -> Option<PathBuf> {
    if let Some(dir) = std::env::var_os("ECLIPSE_SOCKET_DIR").filter(|d| !d.is_empty()) {
        return Some(PathBuf::from(dir).join("polkit.sock"));
    }
    let base = std::env::var_os("XDG_RUNTIME_DIR")?;
    Some(PathBuf::from(base).join("eclipse/polkit.sock"))
}

/// The uid of a `unix-user` identity. polkitd has already expanded groups.
fn identity_uid(identity: &Identity) -> Option<u32> {
    let (kind, details) = identity;
    if kind != "unix-user" {
        return None;
    }
    u32::try_from(details.get("uid")?).ok()
}

/// The person at the keyboard if polkitd will take them, otherwise the first
/// administrator it offered.
fn choose(uids: &[u32], me: u32) -> Option<u32> {
    if uids.contains(&me) {
        Some(me)
    } else {
        uids.first().copied()
    }
}

fn user_name(uid: u32) -> Option<String> {
    let mut buf = vec![0u8; 16 * 1024];
    // SAFETY: an all-zero `passwd` is a valid out-parameter; only read on success.
    let mut pw: libc::passwd = unsafe { std::mem::zeroed() };
    let mut out = std::ptr::null_mut();
    // SAFETY: every pointer is valid for the call, `buf` for its stated length.
    let rc = unsafe { libc::getpwuid_r(uid, &mut pw, buf.as_mut_ptr().cast(), buf.len(), &mut out) };
    if rc != 0 || out.is_null() {
        return None;
    }
    // SAFETY: on success `pw_name` is a NUL-terminated string inside `buf`.
    let name = unsafe { std::ffi::CStr::from_ptr(pw.pw_name) };
    name.to_str().ok().map(str::to_owned)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_person_at_the_keyboard_comes_first() {
        assert_eq!(choose(&[0, 1000, 1001], 1000), Some(1000));
        assert_eq!(choose(&[1001, 1002], 1000), Some(1001));
        assert_eq!(choose(&[], 1000), None);
    }

    #[test]
    fn only_a_unix_user_has_a_uid() {
        let mut d = HashMap::new();
        d.insert("uid".to_owned(), OwnedValue::from(1000u32));
        assert_eq!(identity_uid(&("unix-user".to_owned(), d.clone())), Some(1000));
        assert_eq!(identity_uid(&("unix-group".to_owned(), d)), None);
        assert_eq!(identity_uid(&("unix-user".to_owned(), HashMap::new())), None);
    }

    #[test]
    fn anything_unclear_is_a_failure() {
        assert_eq!(parse_answer("{\"result\":\"ok\"}\n"), Relay::Ok);
        assert_eq!(parse_answer("{\"result\":\"cancelled\"}\n"), Relay::Cancelled);
        assert_eq!(parse_answer("{\"result\":\"failed\"}\n"), Relay::Failed);
        assert_eq!(parse_answer("ok\n"), Relay::Failed);
        assert_eq!(parse_answer("{}\n"), Relay::Failed);
    }
}
