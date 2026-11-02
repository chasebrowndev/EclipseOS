// SPDX-License-Identifier: AGPL-3.0-only

//! `OpenPipeWireRemote`: an fd connected to the user's PipeWire socket.
//!
//! NOTE: this is a plain connection to the session PipeWire instance, not a
//! portal-scoped restricted remote (no `pw_context_connect_fd` permission
//! sandbox yet). Apps that receive it can see other nodes; tightening it needs
//! a PipeWire access-control module and is tracked as follow-up.

use std::os::fd::OwnedFd;
use std::os::unix::net::UnixStream;
use std::path::PathBuf;

pub fn socket_path() -> Option<PathBuf> {
    let name = std::env::var_os("PIPEWIRE_REMOTE").unwrap_or_else(|| "pipewire-0".into());
    let p = PathBuf::from(&name);
    if p.is_absolute() {
        return Some(p);
    }
    Some(PathBuf::from(std::env::var_os("XDG_RUNTIME_DIR")?).join(p))
}

pub fn open_remote() -> std::io::Result<OwnedFd> {
    let path = socket_path().ok_or_else(|| std::io::Error::other("XDG_RUNTIME_DIR unset"))?;
    Ok(UnixStream::connect(path)?.into())
}
