// SPDX-License-Identifier: AGPL-3.0-only
//! `ec-greeter-run`: starts regreet for greetd (ADR 0088). **TCB.** Renders the
//! machine's settings over the shipped files into the greeter's runtime dir and
//! execs regreet, keeping its pid for the kiosk compositor.
//!
//! This stands between greetd and the only way to log in. Any problem, a panic
//! included, gets a journal line and the shipped files.

use std::io::Write;
use std::os::unix::fs::DirBuilderExt;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};

use ec_greeter::{prepare, Error, STATE_DIR};

const REGREET: &str = "/usr/bin/regreet";
const SHIPPED_CONFIG: &str = "/etc/eclipse/greetd/regreet.toml";
const SHIPPED_STYLE: &str = "/etc/eclipse/greetd/regreet.css";

fn main() -> ExitCode {
    let custom = std::panic::catch_unwind(customised).unwrap_or_else(|_| Err(Error::Io("panic".into())));
    let (config, style) = match custom {
        Ok(Some(paths)) => paths,
        Ok(None) => (SHIPPED_CONFIG.into(), SHIPPED_STYLE.into()),
        Err(e) => {
            say(&format!("using the shipped login screen: {e}"));
            (SHIPPED_CONFIG.into(), SHIPPED_STYLE.into())
        }
    };
    let e = Command::new(REGREET)
        .arg("--config")
        .arg(config)
        .arg("--style")
        .arg(style)
        .exec();
    say(&format!("{REGREET}: {e}"));
    ExitCode::FAILURE
}

/// A journal line. Not `eprintln!`, which panics if the pipe has gone, and this
/// must still reach regreet.
fn say(line: &str) {
    let _ = writeln!(std::io::stderr(), "ec-greeter-run: {line}");
}

/// Paths of the rendered config and style, if the machine customised anything.
fn customised() -> Result<Option<(PathBuf, PathBuf)>, Error> {
    let read = |p: &str| std::fs::read_to_string(p).map_err(|e| Error::Io(format!("{p}: {e}")));
    let (config, style) = (read(SHIPPED_CONFIG)?, read(SHIPPED_STYLE)?);
    let Some((config, style)) = prepare(Path::new(STATE_DIR), &config, &style)? else {
        return Ok(None);
    };
    let runtime = std::env::var_os("XDG_RUNTIME_DIR")
        .filter(|d| !d.is_empty())
        .ok_or_else(|| Error::Io("no XDG_RUNTIME_DIR".into()))?;
    let dir = PathBuf::from(runtime).join("ec-greeter");
    let write = |e: std::io::Error| Error::Io(format!("runtime dir: {e}"));
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(&dir)
        .map_err(write)?;
    let (config_path, style_path) = (dir.join("regreet.toml"), dir.join("regreet.css"));
    std::fs::write(&config_path, config).map_err(write)?;
    std::fs::write(&style_path, style).map_err(write)?;
    Ok(Some((config_path, style_path)))
}
