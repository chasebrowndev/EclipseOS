// SPDX-License-Identifier: AGPL-3.0-only
//! The `ec-inferenced` entry point (I-02, ADR 0076).
//!
//! Listens on `$XDG_RUNTIME_DIR/eclipse/inferenced.sock` (directory 0700,
//! socket 0600) and serves `ec-agentd` only. A connection from any other
//! process is dropped before a byte of it is read.

use ec_inferenced::api::UreqHttp;
use ec_inferenced::claude_code;
use ec_inferenced::credential::BrokerdClient;
use ec_inferenced::peer;
use ec_inferenced::router::Router;
use ec_inferenced::server::{serve_conn, Server, MAX_IN_FLIGHT};
use ec_inferenced::shim;
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
    let argv: Vec<String> = std::env::args().skip(1).collect();
    match argv.first().map(String::as_str) {
        // Hidden modes of the Claude Code backend; this binary is its own
        // helper inside the sandbox (src/claude_code.rs, src/shim.rs).
        // `--tool-shim <sock>`: the stdio MCP server `claude` runs.
        Some("--tool-shim") => {
            let Some(sock) = argv.get(1) else {
                eprintln!("ec-inferenced: --tool-shim <sock>");
                return ExitCode::from(2);
            };
            return ExitCode::from(u8::try_from(shim::run(sock)).unwrap_or(1));
        }
        // `--sandbox-init <cfg> [--token-file <path>] -- <prog> [args...]`:
        // the second stage inside bwrap. Never returns.
        Some("--sandbox-init") => {
            let usage = || {
                eprintln!("ec-inferenced: --sandbox-init <cfg> [--token-file <path>] -- <prog> [args...]");
                ExitCode::from(2)
            };
            let Some(cfg) = argv.get(1) else { return usage() };
            let (token_file, rest) = match argv.get(2).map(String::as_str) {
                Some("--token-file") => match argv.get(3) {
                    Some(p) => (Some(p.as_str()), 4),
                    None => return usage(),
                },
                _ => (None, 2),
            };
            if argv.get(rest).map(String::as_str) != Some("--") {
                return usage();
            }
            claude_code::sandbox_init(cfg, token_file, &argv[rest + 1..]);
        }
        _ => {}
    }
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
    let router = Router::new(Arc::new(http), Arc::new(creds));
    // Sessions a previous run left behind (it was killed, not stopped).
    router.claude().sweep_stale();
    let server = Server::new(router, MAX_IN_FLIGHT);

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
