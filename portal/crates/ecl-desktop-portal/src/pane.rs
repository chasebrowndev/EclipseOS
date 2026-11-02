// SPDX-License-Identifier: AGPL-3.0-only

//! Launch `ecl-portal-ui` over a private socketpair and exchange one
//! `PaneRequest` / `PaneReply` (length-prefixed postcard). One pane per
//! request; dropping the future kills the pane ("kill-on-Close").
//!
//! Contract with the pane binary: `ecl-portal-ui --fd N` where N is its end of
//! the socketpair; it reads one request frame, writes one reply frame, exits.
//! A pane carries no authority: its reply is only ever a choice the daemon
//! then validates and acts on.

use std::time::Duration;

use ecl_portal_core::frame::{self, FrameError};
use ecl_portal_proto::{PaneReply, PaneRequest};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::UnixStream;

use crate::spawn::spawn_with_fd;

pub const PANE_BIN: &str = "ecl-portal-ui";

/// A question left unanswered this long is treated as cancelled.
const PANE_TIMEOUT: Duration = Duration::from_secs(600);

#[derive(Debug, thiserror::Error)]
pub enum PaneError {
    #[error("cannot start ecl-portal-ui: {0}")]
    Spawn(std::io::Error),
    #[error("pane io: {0}")]
    Io(#[from] std::io::Error),
    #[error("pane protocol: {0}")]
    Frame(#[from] FrameError),
    #[error("pane timed out")]
    Timeout,
}

pub async fn ask(req: &PaneRequest) -> Result<PaneReply, PaneError> {
    let (child, mine) = spawn_with_fd(PANE_BIN, |fd| vec!["--fd".into(), fd.to_string().into()])
        .map_err(PaneError::Spawn)?;
    // `child` is dropped on every exit path below, which kills the pane.
    let _child = child;
    let mut sock = UnixStream::from_std(mine)?;
    sock.write_all(&frame::encode(req)?).await?;
    let exchange = async {
        let mut hdr = [0u8; 4];
        sock.read_exact(&mut hdr).await?;
        let n = frame::check_len(hdr)?;
        let mut body = vec![0u8; n];
        sock.read_exact(&mut body).await?;
        Ok::<_, PaneError>(frame::decode::<PaneReply>(&body)?)
    };
    tokio::time::timeout(PANE_TIMEOUT, exchange)
        .await
        .map_err(|_| PaneError::Timeout)?
}
