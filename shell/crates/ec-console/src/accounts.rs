// SPDX-License-Identifier: AGPL-3.0-only
//! The composer's account picker (ADR 0077): which named account a task's
//! inference runs as.
//!
//! The console never sees a secret. It reads the account *names* the human's
//! secret store holds, by running `ec-secret account list --json` on a worker
//! thread, and hands the chosen name to the compositor's commit slot
//! (`set_account`), which shows it on the card. The account never comes from
//! the agent, and nothing here can read, add or change a value: unlocking
//! is a compositor-drawn prompt this window only asks for, through agentd
//! (`unlock_secrets`, [`crate::net::Cmd::UnlockSecrets`]).

use std::collections::HashMap;
use std::io::Read;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use iced::Subscription;

use crate::app::Message;

/// How often the list is read again while the composer is open: often
/// enough that an unlock or a login done elsewhere shows within a breath.
const POLL: Duration = Duration::from_secs(3);
/// How long `ec-secret` may take before it counts as unreachable.
const DEADLINE: Duration = Duration::from_secs(5);

/// What an account holds, as `ec-secret` names it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Kind {
    /// A Claude Code OAuth token.
    ClaudeCode,
    /// An API key.
    ApiKey,
}

impl Kind {
    /// The kind a package's inference backend runs on. `None` for a backend
    /// this console does not know, which hides the picker.
    pub fn for_backend(backend: &str) -> Option<Kind> {
        match backend {
            "claude-code" => Some(Kind::ClaudeCode),
            "api" => Some(Kind::ApiKey),
            _ => None,
        }
    }

    fn parse(s: &str) -> Option<Kind> {
        match s {
            "claude-code" => Some(Kind::ClaudeCode),
            "api-key" => Some(Kind::ApiKey),
            _ => None,
        }
    }

    /// The empty state's headline.
    pub fn none_yet(self) -> &'static str {
        match self {
            Kind::ClaudeCode => "No Claude Code account yet",
            Kind::ApiKey => "No API key account yet",
        }
    }
}

/// One account: a name and its kind. Never a value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Account {
    pub name: String,
    pub kind: Kind,
}

/// What the secret store said last.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum Store {
    /// Not read yet.
    #[default]
    Unknown,
    Ready(Vec<Account>),
    Locked,
    Uninitialised,
    /// `ec-secret` is not installed, did not answer in time, or could not
    /// reach brokerd. The task then runs as the default account.
    Unavailable,
}

impl Store {
    /// Read `ec-secret account list --json`'s stdout. Anything that is not
    /// the documented shape is `Unavailable`, never a panic.
    pub fn parse(stdout: &[u8], ok: bool) -> Store {
        let Ok(v) = serde_json::from_slice::<serde_json::Value>(stdout) else {
            return Store::Unavailable;
        };
        if !ok || v.get("error").is_some() {
            return Store::Unavailable;
        }
        if v.get("initialised").and_then(|b| b.as_bool()) == Some(false) {
            return Store::Uninitialised;
        }
        if v.get("locked").and_then(|b| b.as_bool()) == Some(true) {
            return Store::Locked;
        }
        let Some(list) = v.get("accounts").and_then(|a| a.as_array()) else {
            return Store::Unavailable;
        };
        Store::Ready(
            list.iter()
                .filter_map(|a| {
                    let name = a.get("name")?.as_str()?;
                    let kind = Kind::parse(a.get("kind")?.as_str()?)?;
                    valid_name(name).then(|| Account {
                        name: name.to_owned(),
                        kind,
                    })
                })
                .collect(),
        )
    }

    /// The names of `kind`, in the store's order.
    pub fn names(&self, kind: Kind) -> Vec<&str> {
        match self {
            Store::Ready(list) => list
                .iter()
                .filter(|a| a.kind == kind)
                .map(|a| a.name.as_str())
                .collect(),
            _ => Vec::new(),
        }
    }
}

/// The account-name grammar (ADR 0077): 1–32 of `[A-Za-z0-9_-]`. A name that
/// breaks it is dropped rather than shown or sent.
pub fn valid_name(s: &str) -> bool {
    (1..=32).contains(&s.len())
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}

/// The human's pick for each kind, for this session.
#[derive(Debug, Clone, Default)]
pub struct Picks(HashMap<Kind, String>);

impl Picks {
    pub fn set(&mut self, kind: Kind, name: &str) {
        self.0.insert(kind, name.to_owned());
    }

    /// The account in effect for `kind`: the last pick if it still exists,
    /// else `default` if present, else the first. `None` when the store has
    /// none of this kind.
    pub fn chosen<'a>(&self, store: &'a Store, kind: Kind) -> Option<&'a str> {
        let names = store.names(kind);
        let last = self.0.get(&kind).map(String::as_str);
        names
            .iter()
            .copied()
            .find(|n| Some(*n) == last)
            .or_else(|| names.iter().copied().find(|n| *n == "default"))
            .or_else(|| names.first().copied())
    }
}

/// The account as the slot wants it: the empty string for the default.
pub fn wire(name: Option<&str>) -> String {
    match name {
        None | Some("default") => String::new(),
        Some(n) => n.to_owned(),
    }
}

/// One read of the store, bounded by [`DEADLINE`]. Blocks: call it only from
/// a worker thread.
pub fn read() -> Store {
    let child = Command::new("ec-secret")
        .args(["account", "list", "--json"])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn();
    let Ok(mut child) = child else {
        return Store::Unavailable;
    };
    let started = Instant::now();
    let status = loop {
        match child.try_wait() {
            Ok(Some(s)) => break s,
            Ok(None) if started.elapsed() < DEADLINE => std::thread::sleep(Duration::from_millis(50)),
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return Store::Unavailable;
            }
        }
    };
    // The answer is a few hundred bytes, well inside a pipe's buffer, so it
    // is all there once the process has exited.
    let mut out = Vec::new();
    if let Some(mut s) = child.stdout.take() {
        let _ = s.read_to_end(&mut out);
    }
    Store::parse(&out, status.success())
}

/// While subscribed (the composer is open): read the store now, then every
/// [`POLL`]. Dropping the subscription ends the thread at its next send.
pub fn feed() -> Subscription<Message> {
    Subscription::run(|| {
        iced::stream::channel(4, async move |mut out| {
            std::thread::spawn(move || loop {
                let store = read();
                if out.try_send(Message::Accounts(store)).is_err() && out.is_closed() {
                    return;
                }
                std::thread::sleep(POLL);
            });
        })
    })
}

/// Open Settings at its Accounts page, and reap it when it exits.
pub fn open_settings() -> Result<(), String> {
    let mut child = Command::new("ec-settings")
        .arg("accounts")
        .stdin(Stdio::null())
        .spawn()
        .map_err(|e| e.to_string())?;
    std::thread::spawn(move || {
        let _ = child.wait();
    });
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const READY: &str = r#"{"initialised":true,"locked":false,"accounts":[
        {"name":"work","kind":"claude-code","rotations":0},
        {"name":"default","kind":"claude-code","rotations":2},
        {"name":"ci","kind":"api-key","rotations":1},
        {"name":"bad name","kind":"claude-code","rotations":0},
        {"name":"x","kind":"ssh","rotations":0}]}"#;

    #[test]
    fn parses_each_state() {
        let s = Store::parse(READY.as_bytes(), true);
        assert_eq!(s.names(Kind::ClaudeCode), vec!["work", "default"]);
        assert_eq!(s.names(Kind::ApiKey), vec!["ci"]);
        let locked = r#"{"initialised":true,"locked":true,"accounts":[]}"#;
        assert_eq!(Store::parse(locked.as_bytes(), true), Store::Locked);
        let fresh = r#"{"initialised":false,"locked":true,"accounts":[]}"#;
        assert_eq!(Store::parse(fresh.as_bytes(), true), Store::Uninitialised);
        let down = r#"{"error":"unavailable","message":"no socket"}"#;
        assert_eq!(Store::parse(down.as_bytes(), false), Store::Unavailable);
        assert_eq!(Store::parse(b"", false), Store::Unavailable);
        assert_eq!(Store::parse(b"not json", true), Store::Unavailable);
    }

    #[test]
    fn default_then_first_then_remembered() {
        let s = Store::parse(READY.as_bytes(), true);
        let mut p = Picks::default();
        assert_eq!(p.chosen(&s, Kind::ClaudeCode), Some("default"));
        assert_eq!(p.chosen(&s, Kind::ApiKey), Some("ci"));
        p.set(Kind::ClaudeCode, "work");
        assert_eq!(p.chosen(&s, Kind::ClaudeCode), Some("work"));
        // A pick that has since gone falls back.
        p.set(Kind::ClaudeCode, "gone");
        assert_eq!(p.chosen(&s, Kind::ClaudeCode), Some("default"));
        assert_eq!(p.chosen(&Store::Locked, Kind::ClaudeCode), None);
    }

    #[test]
    fn default_goes_out_empty() {
        assert_eq!(wire(Some("default")), "");
        assert_eq!(wire(None), "");
        assert_eq!(wire(Some("work")), "work");
    }

    #[test]
    fn backends_map_to_kinds() {
        assert_eq!(Kind::for_backend("claude-code"), Some(Kind::ClaudeCode));
        assert_eq!(Kind::for_backend("api"), Some(Kind::ApiKey));
        assert_eq!(Kind::for_backend("local"), None);
    }
}
