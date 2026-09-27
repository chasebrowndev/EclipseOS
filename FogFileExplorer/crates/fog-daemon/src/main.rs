// SPDX-License-Identifier: AGPL-3.0-only

//! fogd: the Fog filesystem daemon (FOG §Architecture).

use std::sync::Arc;

use anyhow::{bail, Context};
use fog_daemon::{bind, serve, socket_path, Daemon};
use tracing_subscriber::EnvFilter;

fn main() -> anyhow::Result<()> {
    // Invariant: fogd never runs with privileges.
    if rustix::process::geteuid().is_root() {
        bail!("fogd refuses to run as root");
    }
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()))
        .init();
    let path = socket_path()?;
    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?
        .block_on(async {
            let listener = bind(&path).with_context(|| format!("bind {}", path.display()))?;
            tracing::info!(socket = %path.display(), "fogd listening");
            serve(listener, Arc::new(Daemon::local())).await?;
            Ok(())
        })
}
