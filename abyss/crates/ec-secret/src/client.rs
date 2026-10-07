// SPDX-License-Identifier: AGPL-3.0-only
//! The brokerd socket client: one SEQPACKET connection, one canonical-CBOR
//! packet each way (`ec_brokerd::wire`). The wire types are brokerd's own, so
//! the encoding cannot drift from the decoder it talks to.

use ec_brokerd::wire::{Request, Response, MAX_MESSAGE};
use rustix::net::{self, AddressFamily, RecvFlags, SendFlags, SocketAddrUnix, SocketFlags, SocketType};
use std::os::fd::OwnedFd;
use std::path::PathBuf;
use zeroize::Zeroizing;

#[derive(Debug, PartialEq, Eq)]
pub enum ClientError {
    /// No socket, or nobody listening.
    Unreachable(String),
    /// brokerd accepted the connection and hung up without a reply: it did
    /// not recognise this binary as `ec-secret`.
    Closed,
    Protocol,
}

pub fn socket_path() -> Result<PathBuf, ClientError> {
    let run = std::env::var_os("XDG_RUNTIME_DIR")
        .ok_or_else(|| ClientError::Unreachable("XDG_RUNTIME_DIR is unset".into()))?;
    Ok(PathBuf::from(run).join("eclipse/brokerd.sock"))
}

pub struct Conn(OwnedFd);

impl Conn {
    pub fn connect() -> Result<Conn, ClientError> {
        let path = socket_path()?;
        let unreachable =
            |e: &dyn std::fmt::Display| ClientError::Unreachable(format!("{}: {e}", path.display()));
        let fd = net::socket_with(
            AddressFamily::UNIX,
            SocketType::SEQPACKET,
            SocketFlags::CLOEXEC,
            None,
        )
        .map_err(|e| unreachable(&e))?;
        let addr = SocketAddrUnix::new(&path).map_err(|e| unreachable(&e))?;
        net::connect(&fd, &addr).map_err(|e| unreachable(&e))?;
        Ok(Conn(fd))
    }

    pub fn call(&self, req: &Request) -> Result<Response, ClientError> {
        // The encoding of Add/Rotate/Init holds the plaintext.
        let bytes = Zeroizing::new(req.encode());
        net::send(&self.0, &bytes, SendFlags::NOSIGNAL).map_err(|_| ClientError::Closed)?;
        let mut buf = Zeroizing::new(vec![0u8; MAX_MESSAGE]);
        let (n, full) = loop {
            match net::recv(&self.0, &mut buf[..], RecvFlags::TRUNC) {
                Ok(r) => break r,
                Err(rustix::io::Errno::INTR) => {}
                Err(_) => return Err(ClientError::Closed),
            }
        };
        if n == 0 {
            return Err(ClientError::Closed);
        }
        if full > MAX_MESSAGE {
            return Err(ClientError::Protocol);
        }
        Response::decode(&buf[..n]).map_err(|_| ClientError::Protocol)
    }
}
