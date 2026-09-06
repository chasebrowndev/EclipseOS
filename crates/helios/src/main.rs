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
    #[cfg(feature = "winit")]
    Winit,
    #[cfg(feature = "drm")]
    Drm,
}

/// Nest under an existing session when there is one; otherwise drive KMS.
fn default_backend() -> BackendKind {
    let nested =
        std::env::var_os("WAYLAND_DISPLAY").is_some() || std::env::var_os("DISPLAY").is_some();
    let _ = nested;
    #[cfg(all(feature = "winit", feature = "drm"))]
    return if nested { BackendKind::Winit } else { BackendKind::Drm };
    #[cfg(all(feature = "winit", not(feature = "drm")))]
    return BackendKind::Winit;
    #[cfg(all(feature = "drm", not(feature = "winit")))]
    return BackendKind::Drm;
}

fn parse_args() -> Result<BackendKind> {
    let mut kind = default_backend();
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        match a.as_str() {
            "--backend" => match args.next().as_deref() {
                #[cfg(feature = "winit")]
                Some("winit") => kind = BackendKind::Winit,
                #[cfg(feature = "drm")]
                Some("drm") => kind = BackendKind::Drm,
                Some(other) => {
                    anyhow::bail!("unsupported backend '{other}' (expected 'drm' or 'winit')")
                }
                None => anyhow::bail!("--backend requires a value"),
            },
            "-h" | "--help" => {
                println!("usage: helios [--backend drm|winit]");
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
        #[cfg(feature = "winit")]
        BackendKind::Winit => backend::winit::run(),
        #[cfg(feature = "drm")]
        BackendKind::Drm => backend::drm::run(),
    }
}
