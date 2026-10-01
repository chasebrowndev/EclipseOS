// SPDX-License-Identifier: AGPL-3.0-only

//! ec-fogd: the Fog filesystem daemon (FOG §Architecture).

use std::sync::Arc;

use anyhow::{bail, Context};
use ec_fog_daemon::activate::inherited_listener;
use ec_fog_daemon::{bind, serve, socket_path, Daemon};
use tokio::net::UnixListener;
use tracing_subscriber::EnvFilter;

fn main() -> anyhow::Result<()> {
    // Invariant: ec-fogd never runs with privileges.
    if rustix::process::geteuid().is_root() {
        bail!("ec-fogd refuses to run as root");
    }
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()))
        .init();
    // Socket-activated by ec-fogd.socket, or bind ourselves (FOG §Architecture).
    let inherited = inherited_listener().context("LISTEN_FDS socket")?;
    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?
        .block_on(async {
            let listener = match inherited {
                Some(l) => {
                    tracing::info!("ec-fogd listening on inherited socket");
                    UnixListener::from_std(l)?
                }
                None => {
                    let path = socket_path()?;
                    let l = bind(&path).with_context(|| format!("bind {}", path.display()))?;
                    tracing::info!(socket = %path.display(), "ec-fogd listening");
                    l
                }
            };
            let daemon = Arc::new(Daemon::local());
            match ec_fog_config::path() {
                Some(p) => {
                    ec_fog_daemon::config::start(&daemon, p);
                }
                None => tracing::warn!("no XDG_CONFIG_HOME or HOME; using default config"),
            }
            serve(listener, daemon).await?;
            Ok(())
        })
}
