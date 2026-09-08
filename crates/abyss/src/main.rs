// SPDX-License-Identifier: AGPL-3.0-only
//! abyss — the EclipseOS compositor.
//!
//! Startup order follows COMP-01 §5. Phase 1 milestone 1 wires only what a
//! nested (winit) session needs: logging, the Wayland display and core
//! globals, the public socket, one seat, and a render loop.

mod backend;
mod config;
mod input;
mod ipc;
mod outputs;
mod protocols;
mod render;
mod session;
mod shell;
mod state;
mod xwayland;

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
    let nested = std::env::var_os("WAYLAND_DISPLAY").is_some() || std::env::var_os("DISPLAY").is_some();
    let _ = nested;
    #[cfg(all(feature = "winit", feature = "drm"))]
    return if nested {
        BackendKind::Winit
    } else {
        BackendKind::Drm
    };
    #[cfg(all(feature = "winit", not(feature = "drm")))]
    return BackendKind::Winit;
    #[cfg(all(feature = "drm", not(feature = "winit")))]
    return BackendKind::Drm;
}

struct Args {
    backend: BackendKind,
    config: Option<std::path::PathBuf>,
    stats: bool,
    session: bool,
    render_device: Option<String>,
}

fn parse_args() -> Result<Args> {
    let mut kind = default_backend();
    let mut config = None;
    let mut stats = false;
    let mut session = false;
    let mut render_device = None;
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
            "--config" => match args.next() {
                Some(path) => config = Some(std::path::PathBuf::from(path)),
                None => anyhow::bail!("--config requires a path"),
            },
            "--render-device" => match args.next() {
                Some(v) => render_device = Some(v),
                None => anyhow::bail!("--render-device requires a path or pci: address"),
            },
            "--stats" => stats = true,
            "--session" => session = true,
            "-h" | "--help" => {
                println!("usage: abyss [--backend drm|winit] [--config <path.kdl>] [--render-device <path|pci:DDDD:BB:DD.F>] [--stats] [--session]");
                std::process::exit(0);
            }
            other => anyhow::bail!("unknown argument '{other}'"),
        }
    }
    Ok(Args {
        backend: kind,
        config,
        stats,
        session,
        render_device,
    })
}

fn init_logging() {
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
    let journald = tracing_journald::layer().ok();
    let fmt = if journald.is_some() {
        None
    } else {
        Some(tracing_subscriber::fmt::layer().compact())
    };
    tracing_subscriber::registry()
        .with(filter)
        .with(journald)
        .with(fmt)
        .init();
}

fn main() -> Result<()> {
    init_logging();
    let args = parse_args()?;
    tracing::info!(version = env!("CARGO_PKG_VERSION"), kind = ?args.backend, "abyss starting");
    // Reap spawned children without a wait loop: nothing on the hot path may block.
    // `SIG_IGN` would do it, but an ignored SIGCHLD survives `execve` and breaks
    // every child that waits on children of its own (Xwayland forking xkbcomp is
    // the case that bit us). `SA_NOCLDWAIT` on top of the *default* disposition
    // reaps ours and is cleared for children at exec.
    // SAFETY: called once, before any thread or child exists.
    unsafe {
        let mut act: libc::sigaction = std::mem::zeroed();
        act.sa_sigaction = libc::SIG_DFL;
        act.sa_flags = libc::SA_NOCLDWAIT;
        libc::sigemptyset(&mut act.sa_mask);
        libc::sigaction(libc::SIGCHLD, &act, std::ptr::null_mut());
    }
    let mut config = config::Config::load(args.config.as_deref());
    // COMP-01 §5 step 3 / COMP-13 §1.2: validation is total, and an invalid
    // config at startup is a refusal to start, not a silent fallback.
    if !config.errors.is_empty() {
        for e in &config.errors {
            eprintln!("abyss: {e}");
        }
        eprintln!("abyss: refusing to start on an invalid config");
        std::process::exit(1);
    }
    // COMP-01 §4 override precedence: CLI flag > ECLIPSE_RENDER_DEVICE > config.
    if let Some(dev) = args
        .render_device
        .clone()
        .or_else(|| std::env::var("ECLIPSE_RENDER_DEVICE").ok())
    {
        config.misc.render_device = if dev == "auto" { None } else { Some(dev) };
    }
    match args.backend {
        #[cfg(feature = "winit")]
        BackendKind::Winit => backend::winit::run(config, args.stats, args.session),
        #[cfg(feature = "drm")]
        BackendKind::Drm => backend::drm::run(config, args.stats, args.session),
    }
}
