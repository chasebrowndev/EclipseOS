// SPDX-License-Identifier: AGPL-3.0-only
//! helios — the EclipseOS compositor.
//!
//! Startup order follows COMP-01 §5. Phase 1 milestone 1 wires only what a
//! nested (winit) session needs: logging, the Wayland display and core
//! globals, the public socket, one seat, and a render loop.

mod backend;
mod input;
mod protocols;
mod shell;
mod state;

use anyhow::Result;
use tracing_subscriber::{prelude::*, EnvFilter};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BackendKind {
    Winit,
}

fn parse_args() -> Result<BackendKind> {
    let mut kind = BackendKind::Winit;
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        match a.as_str() {
            "--backend" => match args.next().as_deref() {
                Some("winit") => kind = BackendKind::Winit,
                Some(other) => anyhow::bail!("unsupported backend '{other}' (only 'winit' in M1)"),
                None => anyhow::bail!("--backend requires a value"),
            },
            "-h" | "--help" => {
                println!("usage: helios [--backend winit]");
                std::process::exit(0);
            }
            other => anyhow::bail!("unknown argument '{other}'"),
        }
    }
    Ok(kind)
}

fn init_logging() {
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
    let journald = tracing_journald::layer().ok();
    let fmt = if journald.is_some() {
        None
    } else {
        Some(tracing_subscriber::fmt::layer().compact())
    };
    tracing_subscriber::registry().with(filter).with(journald).with(fmt).init();
}

fn main() -> Result<()> {
    init_logging();
    let kind = parse_args()?;
    tracing::info!(version = env!("CARGO_PKG_VERSION"), ?kind, "helios starting");
    match kind {
        BackendKind::Winit => backend::winit::run(),
    }
}
