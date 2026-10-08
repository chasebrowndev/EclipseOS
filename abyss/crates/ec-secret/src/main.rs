// SPDX-License-Identifier: AGPL-3.0-only
//! `ec-secret`: the owner's CLI for brokerd (S-08 §2).
//!
//! brokerd classifies a peer by its executable (`ec-secret` under `/usr/bin`
//! is the Owner peer), so this binary is the human's only way to add, rotate
//! or revoke a secret from a terminal. It never prints a value (brokerd has
//! no op that returns one to the Owner), never takes one from argv or the
//! environment, and zeroes the buffers it typed them into.
//!
//! `account ...` manages the owner's Claude accounts (ADR 0077): see
//! `account.rs` and `login.rs`.
//!
//! Exit codes: 0 ok, 1 refused by brokerd, 2 usage, 3 brokerd unreachable.

#![deny(unsafe_code)]

mod account;
mod accounts;
mod args;
mod client;
mod login;
mod scan;
mod sig;
mod tty;

use args::Cmd;
use client::{ClientError, Conn};
use ec_brokerd::bind::Binding;
use ec_brokerd::broker::Status;
use ec_brokerd::record::{Kind, Mode, NewSecret};
use ec_brokerd::wire::{Request, Response, Secret};
use std::process::ExitCode;

const OK: u8 = 0;
const REFUSED: u8 = 1;
const USAGE: u8 = 2;
const UNREACHABLE: u8 = 3;

/// brokerd enforces no minimum; the owner's passphrase protects every secret
/// at rest, so the CLI requires one.
const MIN_PASSPHRASE_CHARS: usize = 12;

fn main() -> ExitCode {
    let argv: Vec<String> = std::env::args().skip(1).collect();
    let cmd = match args::parse(&argv) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("ec-secret: {e}\ntry: ec-secret --help");
            return ExitCode::from(USAGE);
        }
    };
    ExitCode::from(run(cmd))
}

fn run(cmd: Cmd) -> u8 {
    if cmd == Cmd::Help {
        print!("{}", args::HELP);
        return OK;
    }
    let style = match &cmd {
        Cmd::Account(a) => account::style(a),
        _ => None,
    };
    let conn = match Conn::connect() {
        Ok(c) => c,
        Err(e) => return account::unreachable(style, e),
    };
    // Typed secrets live in this process: no core dump, not ptrace-able by
    // a same-uid peer. Refuse rather than carry on unprotected.
    //
    // Order matters. brokerd decides who a connection is once, at accept,
    // from `/proc/<pid>/exe`, and clearing dumpable makes this process's
    // `/proc/<pid>` root-owned, so a process hardened first is a peer
    // brokerd cannot name and is dropped unheard. So: connect, complete one
    // Status round trip (a reply means brokerd has classified this
    // connection), then harden, then use the same connection.
    let hardens = match &cmd {
        Cmd::Init | Cmd::Unlock | Cmd::Add { .. } | Cmd::Rotate { .. } => true,
        Cmd::Account(a) => account::holds_secret(a),
        _ => false,
    };
    if hardens {
        if let Err(e) = state(&conn) {
            return match style {
                Some(_) => account_fail(style, e),
                None => fail_plain(e),
            };
        }
        if let Err(e) = ec_brokerd::hygiene::harden() {
            let m = format!("cannot harden this process: {e}");
            match style {
                Some(s) => account::emit_error(s, "refused", &m),
                None => eprintln!("ec-secret: {m}"),
            }
            return REFUSED;
        }
    }
    if let Cmd::Account(a) = cmd {
        return account::run(&conn, a);
    }
    match exec(&conn, cmd) {
        Ok(code) => code,
        Err(f) => fail_plain(f),
    }
}

fn fail_plain(f: Fail) -> u8 {
    match f {
        Fail::Transport(e) => transport(e),
        Fail::Input(e) => {
            eprintln!("ec-secret: {}", e.message());
            USAGE
        }
        Fail::Refused(code) => refused(code),
    }
}

/// A pre-hardening failure of a `--json` account command, as its error event.
fn account_fail(style: Option<account::Style>, f: Fail) -> u8 {
    let Some(s) = style else { return fail_plain(f) };
    match f {
        Fail::Transport(e) => account::unreachable(Some(s), e),
        Fail::Input(e) => {
            account::emit_error(s, "refused", e.message());
            USAGE
        }
        Fail::Refused(c) => {
            account::emit_error(s, "refused", &format!("brokerd refused: {}", status_name(c).0));
            REFUSED
        }
    }
}

/// `s` as a JSON string literal.
pub fn json_str(s: &str) -> String {
    let mut o = String::with_capacity(s.len() + 2);
    o.push('"');
    for c in s.chars() {
        match c {
            '"' => o.push_str("\\\""),
            '\\' => o.push_str("\\\\"),
            '\n' => o.push_str("\\n"),
            '\r' => o.push_str("\\r"),
            '\t' => o.push_str("\\t"),
            c if (c as u32) < 0x20 => o.push_str(&format!("\\u{:04x}", c as u32)),
            c => o.push(c),
        }
    }
    o.push('"');
    o
}

enum Fail {
    Transport(ClientError),
    Input(tty::InputError),
    Refused(u64),
}

impl From<ClientError> for Fail {
    fn from(e: ClientError) -> Fail {
        Fail::Transport(e)
    }
}

impl From<tty::InputError> for Fail {
    fn from(e: tty::InputError) -> Fail {
        Fail::Input(e)
    }
}

fn transport(e: ClientError) -> u8 {
    match e {
        ClientError::Unreachable(why) => {
            eprintln!("ec-secret: cannot reach brokerd ({why})");
            eprintln!("is ec-brokerd running? systemctl --user status ec-brokerd");
        }
        ClientError::Closed => {
            eprintln!("ec-secret: brokerd closed the connection without replying");
            eprintln!("it only serves an `ec-secret` installed under /usr/bin (or a dev-peers build);");
            eprintln!("is ec-brokerd running? systemctl --user status ec-brokerd");
        }
        ClientError::Protocol => {
            eprintln!("ec-secret: brokerd sent a reply this client cannot read (version skew?)")
        }
    }
    UNREACHABLE
}

fn status_name(code: u64) -> (String, &'static str) {
    let (name, hint) = match Status::from_code(code) {
        Some(Status::NoCapability) => ("no_capability", "unknown secret name, or this binary is not the owner peer"),
        Some(Status::OutOfScope) => ("out_of_scope", ""),
        Some(Status::RateLimited) => ("rate_limited", "too many wrong passphrases; wait about 60 seconds"),
        Some(Status::StaleGeneration) => ("stale_generation", ""),
        Some(Status::InvalidArgument) => (
            "invalid_argument",
            "wrong state (already initialised / unlocked, no unlock pending), name already exists, or value too large",
        ),
        Some(Status::BrokerLocked) => ("broker_locked", "run: ec-secret unlock"),
        Some(Status::SecretRotated) => ("secret_rotated", ""),
        Some(Status::Internal) => ("internal", "audit stream down, store unreadable or no lockable memory; see journalctl --user -t ec-brokerd"),
        Some(Status::BadCredential) => ("bad_credential", "wrong passphrase"),
        None => return (format!("status_{code}"), ""),
    };
    (name.to_owned(), hint)
}

fn refused(code: u64) -> u8 {
    let (name, hint) = status_name(code);
    if hint.is_empty() {
        eprintln!("ec-secret: brokerd refused: {name}");
    } else {
        eprintln!("ec-secret: brokerd refused: {name} ({hint})");
    }
    REFUSED
}

fn unexpected() -> Fail {
    Fail::Transport(ClientError::Protocol)
}

fn expect_ok(r: Response) -> Result<(), Fail> {
    match r {
        Response::Ok => Ok(()),
        Response::Err(c) => Err(Fail::Refused(c)),
        _ => Err(unexpected()),
    }
}

struct State {
    unlocked: bool,
    initialised: bool,
    passphrase: bool,
}

fn state(conn: &Conn) -> Result<State, Fail> {
    match conn.call(&Request::Status)? {
        Response::State {
            unlocked,
            initialised,
            passphrase,
        } => Ok(State {
            unlocked,
            initialised,
            passphrase,
        }),
        Response::Err(c) => Err(Fail::Refused(c)),
        _ => Err(unexpected()),
    }
}

/// The request `ec-secret add` sends. A bearer secret in `proxy_header` mode
/// is what `Substitute` releases to the egress proxy (broker.rs `substitute`
/// needs `Mode::ProxyHeader`; the binding scope is checked per connection).
pub fn add_request(name: &str, bindings: Vec<Binding>, value: &[u8]) -> Request {
    Request::Add {
        spec: NewSecret {
            name: name.to_owned(),
            kind: Kind::Bearer,
            bound_to: bindings,
            modes: vec![Mode::ProxyHeader],
            requires_prompt: false,
            rotation_hint_days: None,
        },
        value: Secret::new(value.to_vec()),
    }
}

fn passphrase_ok(p: &[u8]) -> bool {
    std::str::from_utf8(p).map_or(p.len() >= MIN_PASSPHRASE_CHARS, |s| {
        s.chars().count() >= MIN_PASSPHRASE_CHARS
    })
}

fn exec(conn: &Conn, cmd: Cmd) -> Result<u8, Fail> {
    match cmd {
        Cmd::Help => unreachable!("handled before connecting"),
        Cmd::Account(_) => unreachable!("handled before exec"),
        Cmd::Status => {
            let s = state(conn)?;
            println!("initialised: {}", if s.initialised { "yes" } else { "no" });
            if !s.initialised {
                println!("next: ec-secret init");
            } else {
                println!("state: {}", if s.unlocked { "unlocked" } else { "locked" });
                println!(
                    "unlock method: {}",
                    if s.passphrase {
                        "passphrase"
                    } else {
                        "confirmation"
                    }
                );
            }
            Ok(OK)
        }
        Cmd::Init => {
            let s = state(conn)?;
            if s.initialised {
                eprintln!("ec-secret: the store is already initialised (ec-secret unlock)");
                return Ok(REFUSED);
            }
            let pass = if s.passphrase {
                let a = tty::read_line("New passphrase: ", false)?;
                if !passphrase_ok(a.as_bytes()) {
                    eprintln!(
                        "ec-secret: passphrase too short: use at least {MIN_PASSPHRASE_CHARS} characters"
                    );
                    return Ok(USAGE);
                }
                let b = tty::read_line("Repeat passphrase: ", false)?;
                if a.as_bytes() != b.as_bytes() {
                    eprintln!("ec-secret: the passphrases differ; nothing was set");
                    return Ok(USAGE);
                }
                Some(Secret::new(a.as_bytes().to_vec()))
            } else {
                None
            };
            expect_ok(conn.call(&Request::Init { pass })?)?;
            println!("initialised; the store is unlocked");
            Ok(OK)
        }
        Cmd::Unlock => {
            let s = state(conn)?;
            if !s.initialised {
                eprintln!("ec-secret: nothing to unlock: run ec-secret init");
                return Ok(REFUSED);
            }
            if s.unlocked {
                println!("already unlocked");
                return Ok(OK);
            }
            // Ask for the passphrase before opening the 60 s unlock window,
            // so a slow typist does not let the nonce expire.
            let pass = if s.passphrase {
                Some(Secret::new(
                    tty::read_line("Passphrase: ", false)?.as_bytes().to_vec(),
                ))
            } else {
                None
            };
            let (nonce, left) = match conn.call(&Request::BeginUnlock)? {
                Response::Unlock {
                    nonce, attempts_left, ..
                } => (nonce, attempts_left),
                Response::Err(c) => return Err(Fail::Refused(c)),
                _ => return Err(unexpected()),
            };
            match conn.call(&Request::UnlockAnswer {
                nonce,
                passphrase: pass,
                cancel: false,
            })? {
                Response::Unlocked(true) => {
                    println!("unlocked");
                    Ok(OK)
                }
                Response::Unlocked(false) => {
                    eprintln!("ec-secret: unlock cancelled");
                    Ok(REFUSED)
                }
                Response::Err(c) => {
                    if Status::from_code(c) == Some(Status::BadCredential) {
                        let left = left.saturating_sub(1);
                        if left == 0 {
                            eprintln!("ec-secret: wrong passphrase; locked out for about 60 seconds");
                        } else {
                            eprintln!("ec-secret: wrong passphrase ({left} attempts left before a 60 second lockout)");
                        }
                        return Ok(REFUSED);
                    }
                    Err(Fail::Refused(c))
                }
                _ => Err(unexpected()),
            }
        }
        Cmd::Lock => {
            expect_ok(conn.call(&Request::Lock)?)?;
            println!("locked");
            Ok(OK)
        }
        Cmd::Add {
            name,
            bindings,
            stdin,
        } => {
            require_unlocked(conn)?;
            let v = tty::read_line(&format!("Value for {name}: "), stdin)?;
            expect_ok(conn.call(&add_request(&name, bindings, v.as_bytes()))?)?;
            println!("added {name}");
            Ok(OK)
        }
        Cmd::Rotate { name, stdin } => {
            require_unlocked(conn)?;
            let v = tty::read_line(&format!("New value for {name}: "), stdin)?;
            expect_ok(conn.call(&Request::Rotate {
                name: name.clone(),
                value: Secret::new(v.as_bytes().to_vec()),
            })?)?;
            println!("rotated {name}");
            Ok(OK)
        }
        Cmd::Revoke { name } => {
            expect_ok(conn.call(&Request::Revoke { name: name.clone() })?)?;
            println!("revoked {name}");
            Ok(OK)
        }
        Cmd::List => match conn.call(&Request::List)? {
            Response::List(items) => {
                if items.is_empty() {
                    println!("no secrets");
                }
                for i in items {
                    println!(
                        "{}  kind={} modes={} rotations={}{}",
                        i.name,
                        i.kind,
                        i.modes.join(","),
                        i.rotation_counter,
                        if i.requires_prompt { " prompt" } else { "" }
                    );
                    for b in i.bound_to {
                        println!("    bound: {b}");
                    }
                }
                Ok(OK)
            }
            Response::Err(c) => Err(Fail::Refused(c)),
            _ => Err(unexpected()),
        },
    }
}

/// Fail before prompting for a value that brokerd could not take anyway.
fn require_unlocked(conn: &Conn) -> Result<(), Fail> {
    let s = state(conn)?;
    if !s.initialised {
        eprintln!("ec-secret: the store is not initialised: run ec-secret init");
        return Err(Fail::Refused(Status::BrokerLocked.code()));
    }
    if !s.unlocked {
        return Err(Fail::Refused(Status::BrokerLocked.code()));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use ec_brokerd::wire::Request as R;

    const V: &str = "sk-ant-TOPSECRETVALUE-12345";

    fn round(r: &R) -> R {
        R::decode(&r.encode()).expect("brokerd's decoder accepts it")
    }

    #[test]
    fn every_owner_op_round_trips_through_brokerds_decoder() {
        let add = add_request(
            "anthropic-api-key",
            vec![Binding::parse("host:api.anthropic.com").unwrap()],
            V.as_bytes(),
        );
        assert_eq!(round(&add), add);
        let rot = R::Rotate {
            name: "k".into(),
            value: Secret::new(V.as_bytes().to_vec()),
        };
        assert_eq!(round(&rot), rot);
        let init = R::Init {
            pass: Some(Secret::new(b"correct horse battery".to_vec())),
        };
        assert_eq!(round(&init), init);
        let ua = R::UnlockAnswer {
            nonce: 7,
            passphrase: Some(Secret::new(b"pw".to_vec())),
            cancel: false,
        };
        assert_eq!(round(&ua), ua);
        for r in [
            R::Status,
            R::BeginUnlock,
            R::Lock,
            R::List,
            R::Revoke { name: "k".into() },
        ] {
            assert_eq!(round(&r), r);
        }
    }

    #[test]
    fn add_is_a_bearer_proxy_header_secret() {
        let R::Add { spec, .. } = add_request("k", vec![Binding::parse("host:a.com").unwrap()], b"v") else {
            panic!()
        };
        assert_eq!(spec.kind, Kind::Bearer);
        assert_eq!(spec.modes, vec![Mode::ProxyHeader]);
        assert!(spec.validate().is_ok(), "brokerd's own record validation passes");
    }

    #[test]
    fn no_debug_output_carries_a_value() {
        let reqs = [
            add_request("k", vec![Binding::parse("host:a.com").unwrap()], V.as_bytes()),
            R::Rotate {
                name: "k".into(),
                value: Secret::new(V.as_bytes().to_vec()),
            },
            R::Init {
                pass: Some(Secret::new(V.as_bytes().to_vec())),
            },
            R::UnlockAnswer {
                nonce: 1,
                passphrase: Some(Secret::new(V.as_bytes().to_vec())),
                cancel: false,
            },
        ];
        for r in &reqs {
            let d = format!("{r:?}");
            assert!(!d.contains("TOPSECRET"), "{d}");
            assert!(!d.contains("115, 107"), "raw bytes in {d}");
        }
    }

    #[test]
    fn passphrase_minimum() {
        assert!(!passphrase_ok(b"short"));
        assert!(!passphrase_ok(b"elevenchars"));
        assert!(passphrase_ok(b"twelve chars"));
    }

    #[test]
    fn status_names() {
        assert_eq!(status_name(20).0, "broker_locked");
        assert_eq!(status_name(65).0, "bad_credential");
        assert_eq!(status_name(999).0, "status_999");
    }
}
