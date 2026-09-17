// SPDX-License-Identifier: AGPL-3.0-only
//! The `policyd` entry point.
//!
//! Milestone 10 has no socket yet. `main` exists to open the state directory,
//! replay the journal and prove the store survives a restart; milestone 11
//! adds the protocol that talks to it. The daemon itself lives in the library
//! next to this file.

use policyd::tasks;

use std::fs;
use std::io::Read;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};

/// Where the journal and the issuing key live.
///
/// `$ECLIPSE_STATE_DIR` exists so a test or a nested dev instance can run
/// against its own directory; nothing else in the daemon reads the environment.
fn state_dir() -> PathBuf {
    if let Some(d) = std::env::var_os("ECLIPSE_STATE_DIR") {
        return PathBuf::from(d).join("policyd");
    }
    let home = std::env::var_os("HOME").map(PathBuf::from).unwrap_or_default();
    home.join(".local/state/eclipse/policyd")
}

/// Loads the issuing key, generating one on first run.
///
/// The key never leaves this process and the file is `0600` in a `0700`
/// directory: a grant is only worth as much as the exclusivity of the thing
/// that signs it.
fn load_key(dir: &Path) -> std::io::Result<ed25519_dalek::SigningKey> {
    fs::create_dir_all(dir)?;
    fs::set_permissions(dir, fs::Permissions::from_mode(0o700))?;
    let path = dir.join("issuer.key");
    let mut seed = [0u8; 32];
    match fs::OpenOptions::new().read(true).open(&path) {
        Ok(mut f) => {
            f.read_exact(&mut seed)?;
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            getrandom::fill(&mut seed).map_err(std::io::Error::other)?;
            let mut f = fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .open(&path)?;
            use std::io::Write;
            f.write_all(&seed)?;
            f.sync_all()?;
        }
        Err(e) => return Err(e),
    }
    Ok(ed25519_dalek::SigningKey::from_bytes(&seed))
}

fn main() -> std::process::ExitCode {
    let dir = state_dir();
    let key = match load_key(&dir) {
        Ok(k) => k,
        Err(e) => {
            eprintln!("policyd: cannot load the issuing key: {e}");
            return std::process::ExitCode::FAILURE;
        }
    };
    // A store that will not open is fatal, not a warning: without the journal
    // there is nothing to make a grant accountable to.
    let store = match tasks::TaskStore::open(&dir, key) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("policyd: cannot open the audit store: {e}");
            return std::process::ExitCode::FAILURE;
        }
    };
    let live = store.tasks().iter().filter(|t| t.state.is_live()).count();
    let grants = store.grants().iter().filter(|g| !g.revoked).count();
    eprintln!(
        "policyd: {} tasks replayed ({live} live), {grants} live grants; no socket until milestone 11",
        store.tasks().len()
    );
    std::process::ExitCode::SUCCESS
}
