// SPDX-License-Identifier: AGPL-3.0-only
//! `ec-policyd task open|close`: the human opening and closing a task
//! (A-04 §3, origin `human`; §4 cancel).
//!
//! A client on `policyd.sock`, held to the same peer check as abyss: only
//! the session user is served. It skips the key offer and any table the
//! daemon pushes, sends one link message, and for `open` waits for the
//! answer naming its request.
//!
//! ```text
//! ec-policyd task open --principal agent:notes --for 2h \
//!     --cap 'scene.list workspace:human' --cap 'seat.pointer app_id:foot' \
//!     --out notes.cose 'Tidy this week's notes'
//! ec-policyd task close --principal agent:notes
//! ```
//!
//! The grant written by `--out` is what `ec-agentd` presents.

use std::os::unix::fs::OpenOptionsExt;
use std::time::Duration;

use ec_policy_eval::link::{FromPolicyd, ToPolicyd};

const USAGE: &str = "usage: ec-policyd task open --principal <p> --for <N>(m|h) --cap '<capability> <scope>...' \
                     [--cap ...] [--out <grant.cose>] <statement>\n       ec-policyd task close --principal <p>";

/// How long `open` waits for its answer.
const ANSWER: Duration = Duration::from_secs(5);

/// The longest task the CLI opens. A-04 gives every task a hard deadline;
/// this keeps a typo from making it a week.
const MAX_FOR_MS: u64 = 24 * 3_600_000;

fn duration_ms(s: &str) -> Option<u64> {
    let (n, unit) = s.split_at(s.len().checked_sub(1)?);
    let n: u64 = n.parse().ok()?;
    let ms = match unit {
        "m" => n.checked_mul(60_000)?,
        "h" => n.checked_mul(3_600_000)?,
        _ => return None,
    };
    (1..=MAX_FOR_MS).contains(&ms).then_some(ms)
}

struct Open {
    principal: String,
    for_ms: u64,
    caps: Vec<String>,
    out: Option<String>,
    statement: String,
}

fn parse_open(args: &[String]) -> Option<Open> {
    let (mut principal, mut for_ms, mut caps, mut out, mut statement) = (None, None, Vec::new(), None, None);
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--principal" => principal = Some(it.next()?.clone()),
            "--for" => for_ms = Some(duration_ms(it.next()?)?),
            "--cap" => caps.push(it.next()?.clone()),
            "--out" => out = Some(it.next()?.clone()),
            s if !s.starts_with('-') && statement.is_none() => statement = Some(s.to_owned()),
            _ => return None,
        }
    }
    // A line break inside one `--cap` would smuggle a second capability.
    if caps.is_empty() || caps.iter().any(|c| c.contains('\n')) {
        return None;
    }
    Some(Open {
        principal: principal?,
        for_ms: for_ms?,
        caps,
        out,
        statement: statement?,
    })
}

fn connect() -> std::io::Result<std::os::fd::OwnedFd> {
    use rustix::net::{self, SocketAddrUnix};
    let conn = crate::seqpacket()?;
    net::connect(&conn, &SocketAddrUnix::new(crate::socket_path()?)?)?;
    net::sockopt::set_socket_timeout(&conn, net::sockopt::Timeout::Recv, Some(ANSWER))?;
    Ok(conn)
}

fn send(conn: &std::os::fd::OwnedFd, m: &ToPolicyd) -> std::io::Result<()> {
    rustix::net::send(conn, &m.encode(), rustix::net::SendFlags::NOSIGNAL)?;
    Ok(())
}

/// The answer to `req`, skipping the key offer, tables and broadcasts.
fn await_answer(conn: &std::os::fd::OwnedFd, req: u64) -> std::io::Result<FromPolicyd> {
    let mut buf = vec![0u8; ec_policy_eval::link::MAX_MESSAGE];
    loop {
        let (n, _) = rustix::net::recv(conn, &mut buf[..], rustix::net::RecvFlags::empty())?;
        if n == 0 {
            return Err(std::io::Error::other("policyd hung up"));
        }
        match FromPolicyd::decode(&buf[..n]) {
            Ok(m @ (FromPolicyd::TaskOpened { req: r, .. } | FromPolicyd::TaskRefused { req: r, .. }))
                if r == req =>
            {
                return Ok(m)
            }
            _ => continue,
        }
    }
}

fn now_ms() -> u64 {
    crate::now_ms()
}

fn open(o: Open) -> Result<(), String> {
    let conn = connect().map_err(|e| format!("policyd.sock: {e}"))?;
    let mut r = [0u8; 4];
    getrandom::fill(&mut r).map_err(|e| e.to_string())?;
    let req = u64::from(u32::from_le_bytes(r));
    send(
        &conn,
        &ToPolicyd::OpenTask {
            req,
            principal: o.principal.clone(),
            statement: o.statement,
            deadline_ms: now_ms() + o.for_ms,
            scope: o.caps.join("\n"),
        },
    )
    .map_err(|e| format!("sending: {e}"))?;
    match await_answer(&conn, req).map_err(|e| format!("waiting for policyd: {e}"))? {
        FromPolicyd::TaskOpened { grant, .. } => {
            match o.out {
                Some(path) => {
                    use std::io::Write;
                    std::fs::OpenOptions::new()
                        .write(true)
                        .create_new(true)
                        .mode(0o600)
                        .open(&path)
                        .and_then(|mut f| f.write_all(&grant))
                        .map_err(|e| format!("{path}: {e}"))?;
                    println!("task opened for {}; grant in {path}", o.principal);
                }
                None => println!("task opened for {}", o.principal),
            }
            Ok(())
        }
        FromPolicyd::TaskRefused { reason, .. } => Err(format!("refused: {reason}")),
        _ => unreachable!("await_answer returns only answers"),
    }
}

pub fn task(args: &[String]) -> std::process::ExitCode {
    let r = match args.first().map(String::as_str) {
        Some("open") => match parse_open(&args[1..]) {
            Some(o) => open(o),
            None => Err(USAGE.into()),
        },
        Some("close") => match &args[1..] {
            [flag, p] if flag == "--principal" => connect()
                .and_then(|c| send(&c, &ToPolicyd::CloseTask { principal: p.clone() }))
                .map_err(|e| format!("policyd.sock: {e}")),
            _ => Err(USAGE.into()),
        },
        _ => Err(USAGE.into()),
    };
    match r {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("ec-policyd: {e}");
            std::process::ExitCode::FAILURE
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(a: &[&str]) -> Vec<String> {
        a.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn open_parses_and_refuses_what_it_should() {
        let o = parse_open(&args(&[
            "--principal",
            "agent:a",
            "--for",
            "2h",
            "--cap",
            "scene.list workspace:human",
            "tidy",
        ]))
        .unwrap();
        assert_eq!((o.for_ms, o.statement.as_str()), (7_200_000, "tidy"));
        assert!(
            parse_open(&args(&["--principal", "agent:a", "--for", "2h", "tidy"])).is_none(),
            "no cap"
        );
        assert!(parse_open(&args(&[
            "--principal",
            "agent:a",
            "--for",
            "25h",
            "--cap",
            "x y:z",
            "t"
        ]))
        .is_none());
        assert!(parse_open(&args(&[
            "--principal",
            "agent:a",
            "--for",
            "2h",
            "--cap",
            "a b:c\nd e:f",
            "t"
        ]))
        .is_none());
        assert!(
            parse_open(&args(&["--for", "2h", "--cap", "a b:c", "t"])).is_none(),
            "no principal"
        );
    }
}
