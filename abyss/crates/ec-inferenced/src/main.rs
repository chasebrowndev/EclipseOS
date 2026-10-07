// SPDX-License-Identifier: AGPL-3.0-only
//! The `ec-inferenced` entry point (I-02, ADR 0076).
//!
//! Listens on `$XDG_RUNTIME_DIR/eclipse/inferenced.sock` (directory 0700,
//! socket 0600) and serves `ec-agentd` only. A connection from any other
//! process is dropped before a byte of it is read.

use ec_inferenced::api::UreqHttp;
use ec_inferenced::credential::BrokerdClient;
use ec_inferenced::peer;
use ec_inferenced::router::Router;
use ec_inferenced::server::{serve_conn, Server, MAX_IN_FLIGHT};
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::UnixListener;
use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::Arc;

fn socket_path() -> std::io::Result<PathBuf> {
    let run =
        std::env::var_os("XDG_RUNTIME_DIR").ok_or_else(|| std::io::Error::other("XDG_RUNTIME_DIR unset"))?;
    let dir = PathBuf::from(run).join("eclipse");
    fs::create_dir_all(&dir)?;
    fs::set_permissions(&dir, fs::Permissions::from_mode(0o700))?;
    Ok(dir.join("inferenced.sock"))
}

fn main() -> ExitCode {
    // How the peer rule was built (same flag as brokerd and policyd): a
    // symlinked dev session needs `dev-peers` or agentd is refused.
    if std::env::args().nth(1).as_deref() == Some("--build-info") {
        println!(
            "{}",
            if cfg!(feature = "dev-peers") {
                "dev-peers"
            } else {
                "strict-peers"
            }
        );
        return ExitCode::SUCCESS;
    }
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("ec-inferenced: {e}");
            ExitCode::FAILURE
        }
    }
}

fn run() -> Result<(), String> {
    let path = socket_path().map_err(|e| format!("runtime dir: {e}"))?;
    let http = UreqHttp::new().map_err(|e| format!("TLS setup: {e}"))?;
    let creds = BrokerdClient::from_env().ok_or("XDG_RUNTIME_DIR unset")?;
    let server = Server::new(Router::new(Arc::new(http), Arc::new(creds)), MAX_IN_FLIGHT);

    // A stale socket from a crashed run would make bind fail.
    let _ = fs::remove_file(&path);
    let listener = UnixListener::bind(&path).map_err(|e| format!("bind {}: {e}", path.display()))?;
    fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).map_err(|e| format!("chmod: {e}"))?;
    eprintln!("ec-inferenced: listening on {}", path.display());

    let me = rustix::process::getuid();
    for conn in listener.incoming() {
        let Ok(conn) = conn else { continue };
        let ok = match rustix::net::sockopt::socket_peercred(&conn) {
            Ok(c) if c.uid == me => peer::classify(c.pid.as_raw_nonzero().get()),
            _ => false,
        };
        if !ok {
            continue;
        }
        let server = server.clone();
        let _ = std::thread::Builder::new()
            .name("conn".into())
            .spawn(move || serve_conn(server, conn));
    }
    Ok(())
}
