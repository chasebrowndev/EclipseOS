// SPDX-License-Identifier: AGPL-3.0-only
//! `ec-secret account ...` (ADR 0077): the owner's Claude accounts, kept as
//! brokerd secrets by `accounts`. `login` is in `login`; this holds `list`,
//! `add-key` and `remove`.
//!
//! Machine output (`--json`) is a contract the Settings pane reads:
//! `list` prints `{"initialised":..,"locked":..,"accounts":[{"name","kind",
//! "rotations"}]}` and exits 0 even when brokerd is locked (with no accounts);
//! `remove` prints `{"removed":"<name>","kind":"<kind>"}`; failures print
//! `{"error":"<code>","message":".."}` (`login` uses `{"event":"error",..}`).
//! Codes: `unavailable` (brokerd unreachable), `locked`, `uninitialised`,
//! `refused`, `bad_account`.

use crate::accounts::{self, Account, Kind, BINDING, KEY_BASE};
use crate::args::AccountCmd;
use crate::client::{ClientError, Conn};
use crate::{tty, Fail, OK, REFUSED, UNREACHABLE, USAGE};
use ec_brokerd::bind::Binding;
use ec_brokerd::broker::Status;
use ec_brokerd::wire::{Request, Response, Secret};

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Style {
    /// `{"event":"error",..}` lines (login).
    Event,
    /// `{"error":..,"message":..}` (list, remove).
    Flat,
}

pub fn style(c: &AccountCmd) -> Option<Style> {
    match c {
        AccountCmd::Login { json: true, .. } => Some(Style::Event),
        AccountCmd::List { json: true } | AccountCmd::Remove { json: true, .. } => Some(Style::Flat),
        _ => None,
    }
}

pub fn emit_error(style: Style, code: &str, message: &str) {
    let (c, m) = (crate::json_str(code), crate::json_str(message));
    match style {
        Style::Event => println!("{{\"event\":\"error\",\"code\":{c},\"message\":{m}}}"),
        Style::Flat => println!("{{\"error\":{c},\"message\":{m}}}"),
    }
}

/// Whether the command holds a secret value, so the process hardens first.
pub fn holds_secret(c: &AccountCmd) -> bool {
    matches!(c, AccountCmd::Login { .. } | AccountCmd::AddKey { .. })
}

/// brokerd could not be reached.
pub fn unreachable(style: Option<Style>, e: ClientError) -> u8 {
    match style {
        Some(s) => {
            let why = match e {
                ClientError::Unreachable(w) => format!("cannot reach brokerd ({w})"),
                ClientError::Closed => "brokerd closed the connection without replying".to_owned(),
                ClientError::Protocol => "brokerd sent a reply this client cannot read".to_owned(),
            };
            emit_error(s, "unavailable", &why);
            UNREACHABLE
        }
        None => crate::transport(e),
    }
}

fn fail(style: Option<Style>, f: Fail) -> u8 {
    let Some(s) = style else {
        return crate::fail_plain(f);
    };
    match f {
        Fail::Transport(e) => unreachable(Some(s), e),
        Fail::Input(e) => {
            emit_error(s, "refused", e.message());
            USAGE
        }
        Fail::Refused(c) if Status::from_code(c) == Some(Status::BrokerLocked) => {
            emit_error(s, "locked", "brokerd is locked: unlock it first");
            REFUSED
        }
        Fail::Refused(c) => {
            emit_error(
                s,
                "refused",
                &format!("brokerd refused: {}", crate::status_name(c).0),
            );
            REFUSED
        }
    }
}

pub fn run(conn: &Conn, cmd: AccountCmd) -> u8 {
    let st = style(&cmd);
    match cmd {
        AccountCmd::List { json } => list(conn, json),
        AccountCmd::Login { account, json } => crate::login::run(conn, &crate::login::Args { account, json }),
        AccountCmd::AddKey { account, stdin } => add_key(conn, &account, stdin),
        AccountCmd::Remove { account, kind, json } => {
            let Some(name) = accounts::secret_name(kind.base(), &account) else {
                return bad_account(st);
            };
            match conn.call(&Request::Revoke { name }) {
                Ok(Response::Ok) => {
                    if json {
                        println!(
                            "{{\"removed\":{},\"kind\":\"{}\"}}",
                            crate::json_str(&account),
                            kind.as_str()
                        );
                    } else {
                        println!("removed the {} for account {account}", kind_noun(kind));
                    }
                    OK
                }
                Ok(Response::Err(c)) => fail(st, Fail::Refused(c)),
                Ok(_) => fail(st, Fail::Transport(ClientError::Protocol)),
                Err(e) => fail(st, Fail::Transport(e)),
            }
        }
    }
}

fn kind_noun(k: Kind) -> &'static str {
    match k {
        Kind::ClaudeCode => "Claude Code token",
        Kind::ApiKey => "API key",
    }
}

fn bad_account(st: Option<Style>) -> u8 {
    let m = "an account name is 1 to 32 of A-Z a-z 0-9 _ -";
    match st {
        Some(s) => emit_error(s, "bad_account", m),
        None => eprintln!("ec-secret: {m}"),
    }
    USAGE
}

fn list(conn: &Conn, json: bool) -> u8 {
    let st = json.then_some(Style::Flat);
    let s = match crate::state(conn) {
        Ok(s) => s,
        Err(f) => return fail(st, f),
    };
    let ready = s.initialised && s.unlocked;
    let accts: Vec<Account> = if ready {
        match conn.call(&Request::List) {
            Ok(Response::List(items)) => accounts::from_secrets(&items),
            // Locked between the two calls: the same answer as locked.
            Ok(Response::Err(c)) if Status::from_code(c) == Some(Status::BrokerLocked) => Vec::new(),
            Ok(Response::Err(c)) => return fail(st, Fail::Refused(c)),
            Ok(_) => return fail(st, Fail::Transport(ClientError::Protocol)),
            Err(e) => return fail(st, Fail::Transport(e)),
        }
    } else {
        Vec::new()
    };
    if json {
        let items: Vec<String> = accts
            .iter()
            .map(|a| {
                format!(
                    "{{\"name\":{},\"kind\":\"{}\",\"rotations\":{}}}",
                    crate::json_str(&a.name),
                    a.kind.as_str(),
                    a.rotations
                )
            })
            .collect();
        println!(
            "{{\"initialised\":{},\"locked\":{},\"accounts\":[{}]}}",
            s.initialised,
            !s.unlocked,
            items.join(",")
        );
        return OK;
    }
    if !s.initialised {
        println!("the store is not initialised: run ec-secret init");
    } else if !s.unlocked {
        println!("the store is locked: run ec-secret unlock");
    } else if accts.is_empty() {
        println!("no accounts");
    } else {
        println!("{:<34} {:<12} ROTATIONS", "ACCOUNT", "KIND");
        for a in &accts {
            println!("{:<34} {:<12} {}", a.name, a.kind.as_str(), a.rotations);
        }
    }
    OK
}

fn add_key(conn: &Conn, account: &str, stdin: bool) -> u8 {
    let Some(name) = accounts::secret_name(KEY_BASE, account) else {
        return bad_account(None);
    };
    let run = || -> Result<u8, Fail> {
        crate::require_unlocked(conn)?;
        let exists = match conn.call(&Request::List)? {
            Response::List(items) => items.iter().any(|i| i.name == name),
            Response::Err(c) => return Err(Fail::Refused(c)),
            _ => return Err(Fail::Transport(ClientError::Protocol)),
        };
        let v = tty::read_line(&format!("API key for account {account}: "), stdin)?;
        let req = if exists {
            Request::Rotate {
                name: name.clone(),
                value: Secret::new(v.as_bytes().to_vec()),
            }
        } else {
            let b = Binding::parse(BINDING).expect("a constant binding");
            crate::add_request(&name, vec![b], v.as_bytes())
        };
        match conn.call(&req)? {
            Response::Ok => {}
            Response::Err(c) => return Err(Fail::Refused(c)),
            _ => return Err(Fail::Transport(ClientError::Protocol)),
        }
        println!("stored the API key for account {account}");
        Ok(OK)
    };
    run().unwrap_or_else(|f| fail(None, f))
}
