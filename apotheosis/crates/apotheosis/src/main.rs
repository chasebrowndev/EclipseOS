// SPDX-License-Identifier: AGPL-3.0-only
//! The Apotheosis terminal emulator.
//!
//! Governing spec: T-00 (`docs/ECLIPSEOS_SPECS_APOTHEOSIS.md`).
#![forbid(unsafe_code)]

mod app;
mod binds;
mod copymode;
mod ctl;
mod glyphs;
mod hints;
mod hook;
mod layout;
mod mediahost;
mod mousectl;
mod nav;
mod notify;
mod open;
mod palette;
mod pane;
mod paste;
mod pointer;
mod profiles;
mod reload;
mod render;
mod rules;
mod search;
mod security;
mod settings;
mod ui;
mod view;
mod workers;

use std::io::IsTerminal;

use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;

/// EX_UNAVAILABLE: no usable GPU or compositor.
const EX_UNAVAILABLE: i32 = 69;
/// EX_USAGE.
const EX_USAGE: i32 = 64;

const HELP: &str = "\
Usage: apotheosis [OPTIONS] [-e COMMAND [ARGS...]]

Options:
  -e COMMAND [ARGS...]   run COMMAND instead of the shell (consumes the rest)
  -o KEY=VALUE           override a config key for this run (repeatable)
  --cwd DIR              start in DIR
  --title TITLE          title of the first tab
  --app-id ID            Wayland app id (on Abyss it must start with org.eclipseos.Apotheosis)
  --class ID             alias of --app-id
  --new-instance         do not hand the launch to a running instance
  --wait                 when handed to a running instance, wait for the command and exit
                         with its status
  --profile NAME         start with the named profile (T-11 §6)
  --hold, --layout NAME  accepted; not available yet
  -h, --help             show this help
  -V, --version          show the version
";

/// A run's command line.
#[derive(Debug, Default, PartialEq, Eq)]
struct Run {
    command: Option<Vec<String>>,
    launch: Vec<(String, String)>,
    app_id: Option<String>,
    cwd: Option<String>,
    title: Option<String>,
    new_instance: bool,
    wait: bool,
    hold: bool,
    profile: Option<String>,
    layout: Option<String>,
}

#[derive(Debug, PartialEq, Eq)]
enum Cli {
    Run(Run),
    Help,
    Version,
}

fn parse_args<I: IntoIterator<Item = String>>(args: I) -> Result<Cli, String> {
    let mut it = args.into_iter();
    let mut r = Run::default();
    while let Some(a) = it.next() {
        let mut val = |flag: &str| it.next().ok_or(format!("{flag} needs a value"));
        match a.as_str() {
            "-h" | "--help" => return Ok(Cli::Help),
            "-V" | "--version" => return Ok(Cli::Version),
            "-e" => {
                let rest: Vec<String> = it.by_ref().collect();
                if rest.is_empty() {
                    return Err("-e needs a command".into());
                }
                r.command = Some(rest);
            }
            "--app-id" | "--class" => r.app_id = Some(val("--app-id")?),
            "--cwd" => r.cwd = Some(val("--cwd")?),
            "--title" => r.title = Some(val("--title")?),
            "--profile" => r.profile = Some(val("--profile")?),
            "--layout" => r.layout = Some(val("--layout")?),
            "--new-instance" => r.new_instance = true,
            "--wait" => r.wait = true,
            "--hold" => r.hold = true,
            "-o" => {
                let kv = val("-o")?;
                let (k, v) = kv.split_once('=').ok_or("-o needs KEY=VALUE")?;
                r.launch.push((k.to_owned(), v.to_owned()));
            }
            other => return Err(format!("unknown argument {other:?}")),
        }
    }
    Ok(Cli::Run(r))
}

/// `--cwd`, made absolute against ours.
fn absolute(p: &str) -> std::path::PathBuf {
    let path = std::path::Path::new(p);
    if path.is_absolute() {
        return path.to_owned();
    }
    std::env::current_dir().map_or_else(|_| path.to_owned(), |d| d.join(path))
}

/// The `launch` a second launch sends (T-12 §5).
fn launch_request(r: &Run) -> apoth_proto::params::Launch {
    let cwd = r
        .cwd
        .as_deref()
        .map(absolute)
        .or_else(|| std::env::current_dir().ok());
    apoth_proto::params::Launch {
        argv: r.command.clone().unwrap_or_default(),
        cwd: cwd.map(|p| p.to_string_lossy().into_owned()),
        env: ctl::launch::launch_env(),
        activation_token: std::env::var("XDG_ACTIVATION_TOKEN").ok(),
        overrides: r.launch.iter().cloned().collect(),
        title: r.title.clone(),
        app_id: r.app_id.clone(),
        profile: r.profile.clone(),
        layout: r.layout.clone(),
        hold: r.hold,
        wait: r.wait,
    }
}

fn init_logging() {
    let filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info"));
    let reg = tracing_subscriber::registry().with(filter);
    if !std::io::stderr().is_terminal()
        && let Ok(j) = tracing_journald::layer()
    {
        let _ = reg
            .with(j.with_syslog_identifier("apotheosis".into()))
            .try_init();
        return;
    }
    let _ = reg
        .with(tracing_subscriber::fmt::layer().with_writer(std::io::stderr))
        .try_init();
}

fn main() {
    let cli = match parse_args(std::env::args().skip(1)) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("apotheosis: {e}\n\n{HELP}");
            std::process::exit(EX_USAGE);
        }
    };
    match cli {
        Cli::Help => {
            let _ = std::io::Write::write_all(&mut std::io::stdout(), HELP.as_bytes());
        }
        Cli::Version => {
            let _ = std::io::Write::write_all(
                &mut std::io::stdout(),
                concat!("apotheosis ", env!("CARGO_PKG_VERSION"), "\n").as_bytes(),
            );
        }
        Cli::Run(r) => {
            init_logging();
            if r.hold || r.layout.is_some() {
                eprintln!("apotheosis: --hold and --layout are not available yet");
                std::process::exit(1);
            }
            if let Some(p) = &r.profile {
                settings::set_launch_profile(p);
            }
            if !r.new_instance
                && let ctl::launch::Handoff::Done(code) = ctl::launch::hand_off(launch_request(&r))
            {
                std::process::exit(code);
            }
            let startup = settings::Settings::load(r.launch);
            let first = app::FirstPane {
                cwd: r.cwd.as_deref().map(absolute),
                title: r.title,
            };
            match app::run(startup, r.command, r.app_id, first) {
                Ok(code) => std::process::exit(code),
                Err(e) => {
                    eprintln!("apotheosis: {e}");
                    std::process::exit(EX_UNAVAILABLE);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(a: &[&str]) -> Result<Cli, String> {
        parse_args(a.iter().map(|s| (*s).to_owned()))
    }

    #[test]
    fn parses_flags() {
        assert_eq!(p(&["--version"]), Ok(Cli::Version));
        assert_eq!(
            p(&["-o", "a.b=c", "-e", "ls", "-l"]),
            Ok(Cli::Run(Run {
                command: Some(vec!["ls".into(), "-l".into()]),
                launch: vec![("a.b".into(), "c".into())],
                ..Run::default()
            }))
        );
        assert_eq!(
            p(&["--app-id", "org.eclipseos.Apotheosis.Foo", "-e", "ls"]),
            Ok(Cli::Run(Run {
                command: Some(vec!["ls".into()]),
                app_id: Some("org.eclipseos.Apotheosis.Foo".into()),
                ..Run::default()
            }))
        );
        assert_eq!(
            p(&["--class", "x"]),
            Ok(Cli::Run(Run {
                app_id: Some("x".into()),
                ..Run::default()
            }))
        );
        assert_eq!(
            p(&[
                "--cwd",
                "/tmp",
                "--title",
                "t",
                "--new-instance",
                "--wait",
                "--hold"
            ]),
            Ok(Cli::Run(Run {
                cwd: Some("/tmp".into()),
                title: Some("t".into()),
                new_instance: true,
                wait: true,
                hold: true,
                ..Run::default()
            }))
        );
        assert!(p(&["--app-id"]).is_err());
        assert!(p(&["--cwd"]).is_err());
        assert!(p(&["-e"]).is_err());
        assert!(p(&["-o", "x"]).is_err());
        assert!(p(&["--nope"]).is_err());
    }

    #[test]
    fn second_launch_request() {
        let r = Run {
            command: Some(vec!["make".into()]),
            launch: vec![("font.size".into(), "12".into())],
            cwd: Some("/srv".into()),
            title: Some("build".into()),
            wait: true,
            ..Run::default()
        };
        let l = launch_request(&r);
        assert_eq!(l.argv, ["make"]);
        assert_eq!(l.cwd.as_deref(), Some("/srv"));
        assert_eq!(l.title.as_deref(), Some("build"));
        assert_eq!(l.overrides.get("font.size").map(String::as_str), Some("12"));
        assert!(l.wait && !l.hold);
    }
}
