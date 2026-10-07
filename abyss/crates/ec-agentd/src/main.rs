// SPDX-License-Identifier: AGPL-3.0-only
//! `ec-agentd`: the agent daemon (A-01, A-08).
//!
//! * no arguments: the daemon. Serves `console.sock`, hosts agents, links to
//!   `policyd`.
//! * `[--list] <grant.cose>`: the M11 admission client (`ec_agentd::admit`).

use std::process::ExitCode;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

static TERM: AtomicBool = AtomicBool::new(false);

extern "C" fn on_term(_: libc::c_int) {
    // An atomic store is async-signal-safe.
    TERM.store(true, Ordering::Relaxed);
}

fn install_term_handler() {
    // SAFETY: the handler only stores to an atomic.
    unsafe {
        libc::signal(
            libc::SIGTERM,
            on_term as extern "C" fn(libc::c_int) as libc::sighandler_t,
        );
        libc::signal(
            libc::SIGINT,
            on_term as extern "C" fn(libc::c_int) as libc::sighandler_t,
        );
    }
}

fn daemon() -> ExitCode {
    let cfg = match ec_agentd::Config::from_env() {
        Ok(c) => c,
        Err(e) => {
            eprintln!("ec-agentd: {e}");
            return ExitCode::FAILURE;
        }
    };
    install_term_handler();
    let d = match ec_agentd::start(cfg) {
        Ok(d) => d,
        Err(e) => {
            eprintln!("ec-agentd: {e}");
            return ExitCode::FAILURE;
        }
    };
    eprintln!("ec-agentd: serving {}", d.console_socket.display());
    while !TERM.load(Ordering::Relaxed) {
        std::thread::sleep(Duration::from_millis(200));
    }
    d.shutdown();
    ExitCode::SUCCESS
}

fn main() -> ExitCode {
    // Hidden: the launcher's second stage inside bwrap. It never returns.
    // `--sandbox-init <cfg> -- <entrypoint> [args...]`.
    let argv: Vec<String> = std::env::args().skip(1).collect();
    if argv.first().map(String::as_str) == Some("--sandbox-init") {
        let (Some(cfg), Some("--")) = (argv.get(1), argv.get(2).map(String::as_str)) else {
            eprintln!("ec-agentd: --sandbox-init <cfg> -- <entrypoint...>");
            return ExitCode::from(2);
        };
        ec_agentd::sandbox::init_main(cfg, &argv[3..]);
    }
    let mut list = false;
    let mut grant = None;
    for a in std::env::args().skip(1) {
        match a.as_str() {
            "--list" => list = true,
            "-h" | "--help" => {
                println!("usage: ec-agentd                  run the daemon");
                println!("       ec-agentd [--list] <grant.cose>   admit one agent (M11)");
                return ExitCode::SUCCESS;
            }
            _ if grant.is_none() && !a.starts_with('-') => grant = Some(a),
            other => {
                eprintln!("ec-agentd: unexpected argument {other:?}");
                return ExitCode::from(2);
            }
        }
    }
    let Some(grant) = grant else {
        if list {
            eprintln!("usage: ec-agentd [--list] <grant.cose>");
            return ExitCode::from(2);
        }
        return daemon();
    };
    match ec_agentd::admit::run(list, &grant) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("ec-agentd: {e}");
            ExitCode::FAILURE
        }
    }
}
