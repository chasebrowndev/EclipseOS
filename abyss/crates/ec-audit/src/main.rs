// SPDX-License-Identifier: AGPL-3.0-only
//! `ec-audit` (S-04 §4, §5).
//!
//! ```text
//! ec-audit [--dir D] verify
//! ec-audit [--dir D] trace --req-id N [--principal P]
//! ec-audit [--dir D] query [--principal P] [--kind K] [--outcome O] [--since 1h]
//! ```
//!
//! Records print one JSON projection per line. Exit status is 0 on an
//! answer, 1 when the store does not verify, 2 on a usage error.

use std::path::PathBuf;
use std::process::ExitCode;

use ec_audit::{parse_since, project, query, records, store_dir, trace, Filter};
use ec_policy_eval::audit::Kind;

const USAGE: &str = "usage: ec-audit [--dir D] verify
       ec-audit [--dir D] trace --req-id N [--principal P]
       ec-audit [--dir D] query [--principal P] [--kind K] [--outcome O] [--since 90s|15m|1h|7d]";

fn usage(why: &str) -> ExitCode {
    eprintln!("ec-audit: {why}\n{USAGE}");
    ExitCode::from(2)
}

fn main() -> ExitCode {
    let mut args = std::env::args().skip(1).peekable();
    let mut dir = store_dir();
    if args.peek().map(String::as_str) == Some("--dir") {
        args.next();
        match args.next() {
            Some(d) => dir = PathBuf::from(d),
            None => return usage("--dir needs a path"),
        }
    }
    let Some(cmd) = args.next() else {
        return usage("no command");
    };
    let mut req_id = None;
    let mut f = Filter::default();
    while let Some(flag) = args.next() {
        let Some(v) = args.next() else {
            return usage(&format!("{flag} needs a value"));
        };
        match flag.as_str() {
            "--req-id" => match v.parse() {
                Ok(n) => req_id = Some(n),
                Err(_) => return usage("--req-id is a number"),
            },
            "--principal" => f.principal = Some(v),
            "--kind" => match Kind::parse(&v) {
                Some(k) => f.kind = Some(k),
                None => return usage(&format!("no kind {v:?}")),
            },
            "--outcome" => f.outcome = Some(v),
            "--since" => match parse_since(&v) {
                Some(ns) => f.since_ns = Some(now_ns().saturating_sub(ns)),
                None => return usage("--since is like 90s, 15m, 1h or 7d"),
            },
            _ => return usage(&format!("unknown flag {flag}")),
        }
    }

    let (verified, all) = match records(&dir) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("ec-audit: {}: {e}", dir.display());
            return ExitCode::from(1);
        }
    };
    let out = match cmd.as_str() {
        "verify" => {
            let head: String = verified.head.iter().map(|b| format!("{b:02x}")).collect();
            println!("ok: {} records, head {head}", verified.records);
            if verified.torn_tail {
                println!("note: the open segment ends in a torn write; policyd drops it on start");
            }
            return ExitCode::SUCCESS;
        }
        "trace" => {
            let Some(n) = req_id else {
                return usage("trace needs --req-id");
            };
            trace(&all, n, f.principal.as_deref())
        }
        "query" => query(&all, &f),
        other => return usage(&format!("unknown command {other}")),
    };
    for r in &out {
        println!("{}", project(r));
    }
    ExitCode::SUCCESS
}

fn now_ns() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| u64::try_from(d.as_nanos()).unwrap_or(u64::MAX))
        .unwrap_or(0)
}
