// SPDX-License-Identifier: AGPL-3.0-only
//! Apotheosis command-line client: control socket, config, icat, play.
//!
//! Governing spec: T-12 (`docs/ECLIPSEOS_SPECS_APOTHEOSIS.md`). Exit codes (T-12 §5):
//! 0 ok, 1 error, 2 usage, 3 denied, 4 no instance.
#![forbid(unsafe_code)]

mod config;
mod ctl;

use apoth_config::fs::RealFs;
use std::path::PathBuf;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let code = match args.first().map(String::as_str) {
        Some("config") => {
            let Some(home) = std::env::var_os("HOME").map(PathBuf::from) else {
                eprintln!("apoth: HOME is not set");
                std::process::exit(1);
            };
            let xdg = std::env::var_os("XDG_CONFIG_HOME")
                .map(PathBuf::from)
                .filter(|p| p.is_absolute());
            let mut out = std::io::stdout().lock();
            let mut err = std::io::stderr().lock();
            let mut ctx = config::Ctx {
                fs: &RealFs,
                home,
                xdg,
                out: &mut out,
                err: &mut err,
            };
            config::run(&args[1..], &mut ctx)
        }
        Some("shell-init") => shell_init(&args[1..]),
        Some("-h" | "--help" | "help") => {
            let _ = std::io::Write::write_all(&mut std::io::stdout(), ctl::HELP.as_bytes());
            0
        }
        Some(_) => socket(&args),
        None => {
            let _ = std::io::Write::write_all(&mut std::io::stderr(), ctl::HELP.as_bytes());
            2
        }
    };
    std::process::exit(code);
}

/// A socket command (global options first).
fn socket(args: &[String]) -> i32 {
    let (opts, rest) = match ctl::parse_global(args) {
        Ok(v) => v,
        Err(m) => {
            eprintln!("apoth: {m}");
            return ctl::USAGE;
        }
    };
    let Some((cmd, rest)) = rest.split_first() else {
        eprintln!("apoth: missing command\n\n{}", ctl::HELP);
        return ctl::USAGE;
    };
    if ctl::NOT_YET.contains(&cmd.as_str()) {
        eprintln!(
            "apoth {}: `{cmd}` is not yet implemented",
            env!("CARGO_PKG_VERSION")
        );
        return ctl::ERROR;
    }
    if !ctl::COMMANDS.contains(&cmd.as_str()) {
        eprintln!("apoth: unknown command `{cmd}`\n\n{}", ctl::HELP);
        return ctl::USAGE;
    }
    let mut out = std::io::stdout().lock();
    let mut err = std::io::stderr().lock();
    ctl::run(cmd, rest, opts, &mut out, &mut err)
}

/// `apoth shell-init <shell>`: the snippet panes get, for manual sourcing (T-14 §2).
fn shell_init(args: &[String]) -> i32 {
    let shells = apoth_pty::shell::SHELLS.join("|");
    let [sh] = args else {
        eprintln!("usage: apoth shell-init <{shells}>");
        return ctl::USAGE;
    };
    let Some(s) = apoth_pty::shell::snippet(sh) else {
        eprintln!("apoth shell-init: no snippet for {sh:?} (have {shells})");
        return ctl::USAGE;
    };
    let _ = std::io::Write::write_all(&mut std::io::stdout(), s.as_bytes());
    ctl::OK
}
