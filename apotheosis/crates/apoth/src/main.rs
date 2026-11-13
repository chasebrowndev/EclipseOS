// SPDX-License-Identifier: AGPL-3.0-only
//! Apotheosis command-line client: control socket, config, icat, play.
//!
//! Governing spec: T-12 (`docs/ECLIPSEOS_SPECS_APOTHEOSIS.md`). Exit codes (T-12 §5):
//! 0 ok, 1 error, 2 usage, 3 denied, 4 no instance.
#![forbid(unsafe_code)]

mod config;

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
        Some(other) => {
            eprintln!(
                "apoth {}: `{other}` is not yet implemented (T-12, M29)",
                env!("CARGO_PKG_VERSION")
            );
            1
        }
        None => {
            eprintln!("usage: apoth config <validate|get|set|unset|list|path|import-foot> ...");
            2
        }
    };
    std::process::exit(code);
}
