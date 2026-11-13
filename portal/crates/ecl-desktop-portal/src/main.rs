// SPDX-License-Identifier: AGPL-3.0-only

//! `ecl-desktop-portal`: Eclipse desktop portal daemon. An untrusted session
//! process (ADR 0096): it holds no capture or input authority and treats
//! everything a caller says as a claim.

mod account;
mod apps;
mod bus;
mod cast;
mod convert;
mod email;
mod error;
mod filechooser;
mod ident;
mod impl_cast;
mod inhibit;
mod launch;
mod notification;
mod openuri;
mod pane;
mod permstore;
mod req;
mod screen;
mod session;
mod settings;
mod spawn;
mod state;
mod trash;

use std::process::ExitCode;

use tracing_subscriber::EnvFilter;

#[tokio::main]
async fn main() -> ExitCode {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .with_writer(std::io::stderr)
        .init();
    match bus::run().await {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            tracing::error!("ecl-desktop-portal: {e:#}");
            ExitCode::FAILURE
        }
    }
}
