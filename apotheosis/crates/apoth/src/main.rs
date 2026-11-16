// SPDX-License-Identifier: AGPL-3.0-only
//! Apotheosis command-line client: control socket, config, icat, play.
//!
//! Governing spec: T-12 (`docs/ECLIPSEOS_SPECS_APOTHEOSIS.md`). Exit codes (T-12 §5):
//! 0 ok, 1 error, 2 usage, 3 denied, 4 no instance.
#![forbid(unsafe_code)]

mod askpass;
mod config;
mod ctl;
mod icat;
mod play;
mod proxy;
mod server;
mod ssh;

use apoth_config::fs::RealFs;
use std::path::PathBuf;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    // ssh runs `$SSH_ASKPASS PROMPT` (T-22 §3): one argument that is no command of ours,
    // only on the ssh child `apoth ssh` armed.
    if args.len() == 1
        && std::env::var_os(apoth_proto::askpass::ENV_SOCK).is_some()
        && !is_command(&args[0])
    {
        std::process::exit(askpass::run(&args));
    }
    let code = match args.first().map(String::as_str) {
        Some("config") => config_cmd(&args[1..]),
        Some("shell-init") => shell_init(&args[1..]),
        Some("icat") => icat::run(&args[1..]),
        Some("play") => play::run(&args[1..]),
        Some("server") => server::server(&args[1..]),
        Some("attach") => server::attach(&args[1..]),
        Some("detach") => server::detach(&args[1..]),
        Some("ssh") => ssh::run(&args[1..]),
        Some("proxy") => proxy::run(&args[1..]),
        Some("askpass") => askpass::run(&args[1..]),
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

/// A first argument this binary treats as a command (or a global option).
fn is_command(a: &str) -> bool {
    a.starts_with('-')
        || matches!(
            a,
            "config"
                | "shell-init"
                | "icat"
                | "play"
                | "server"
                | "attach"
                | "detach"
                | "ssh"
                | "proxy"
                | "askpass"
                | "help"
        )
        || ctl::COMMANDS.contains(&a)
        || ctl::NOT_YET.contains(&a)
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
    // `apoth set` saves to the file like `apoth config set`; `--temp` is the socket form.
    if cmd == "set" && !ctl::is_temp(rest) {
        if opts.target.is_some() || opts.socket.is_some() {
            eprintln!("apoth set: --match and --socket apply only to `set --temp`");
            return ctl::USAGE;
        }
        let mut a = vec!["set".to_owned()];
        if opts.json {
            a.push("--json".into());
        }
        a.extend(rest.iter().cloned());
        return config_cmd(&a);
    }
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

/// `apoth config …` on the real file system.
fn config_cmd(args: &[String]) -> i32 {
    let Some(home) = std::env::var_os("HOME").map(PathBuf::from) else {
        eprintln!("apoth: HOME is not set");
        return 1;
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
    config::run(args, &mut ctx)
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
