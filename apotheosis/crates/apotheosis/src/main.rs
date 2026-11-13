// SPDX-License-Identifier: AGPL-3.0-only
//! The Apotheosis terminal emulator.
//!
//! Governing spec: T-00 (`docs/ECLIPSEOS_SPECS_APOTHEOSIS.md`).
#![forbid(unsafe_code)]

mod app;
mod glyphs;
mod hook;
mod pane;
mod reload;
mod render;
mod settings;
mod ui;
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
  --app-id ID            Wayland app id (on Abyss it must start with org.eclipseos.Apotheosis)
  --class ID             alias of --app-id
  -h, --help             show this help
  -V, --version          show the version
";

#[derive(Debug, PartialEq, Eq)]
enum Cli {
    Run {
        command: Option<Vec<String>>,
        launch: Vec<(String, String)>,
        app_id: Option<String>,
    },
    Help,
    Version,
}

fn parse_args<I: IntoIterator<Item = String>>(args: I) -> Result<Cli, String> {
    let mut it = args.into_iter();
    let mut launch = Vec::new();
    let mut command = None;
    let mut app_id = None;
    while let Some(a) = it.next() {
        match a.as_str() {
            "-h" | "--help" => return Ok(Cli::Help),
            "-V" | "--version" => return Ok(Cli::Version),
            "-e" => {
                let rest: Vec<String> = it.by_ref().collect();
                if rest.is_empty() {
                    return Err("-e needs a command".into());
                }
                command = Some(rest);
            }
            "--app-id" | "--class" => {
                app_id = Some(it.next().ok_or("--app-id needs a value")?);
            }
            "-o" => {
                let kv = it.next().ok_or("-o needs KEY=VALUE")?;
                let (k, v) = kv.split_once('=').ok_or("-o needs KEY=VALUE")?;
                launch.push((k.to_owned(), v.to_owned()));
            }
            other => return Err(format!("unknown argument {other:?}")),
        }
    }
    Ok(Cli::Run {
        command,
        launch,
        app_id,
    })
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
        Cli::Run {
            command,
            launch,
            app_id,
        } => {
            init_logging();
            let startup = settings::Settings::load(launch);
            match app::run(startup, command, app_id) {
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
            Ok(Cli::Run {
                command: Some(vec!["ls".into(), "-l".into()]),
                launch: vec![("a.b".into(), "c".into())],
                app_id: None,
            })
        );
        assert_eq!(
            p(&["--app-id", "org.eclipseos.Apotheosis.Foo", "-e", "ls"]),
            Ok(Cli::Run {
                command: Some(vec!["ls".into()]),
                launch: vec![],
                app_id: Some("org.eclipseos.Apotheosis.Foo".into()),
            })
        );
        assert_eq!(
            p(&["--class", "x"]),
            Ok(Cli::Run {
                command: None,
                launch: vec![],
                app_id: Some("x".into()),
            })
        );
        assert!(p(&["--app-id"]).is_err());
        assert!(p(&["-e"]).is_err());
        assert!(p(&["-o", "x"]).is_err());
        assert!(p(&["--nope"]).is_err());
    }
}
