// SPDX-License-Identifier: AGPL-3.0-only

//! The `fogd` connection, as an iced subscription (FOG §Architecture).
//!
//! The stream runs on iced's tokio executor, never on the UI thread. It
//! connects to `fogd.sock`, hands the app a [`Link`] for requests and
//! forwards every [`Reply`]. When `fogd` is absent or goes away it reports
//! [`Event::Down`] once and retries with backoff; the app shows that quietly.

use std::io;
use std::path::PathBuf;
use std::time::Duration;

use fog_proto::{Reply, Request};
use iced::futures::channel::mpsc as ui;
use iced::futures::SinkExt;
use iced::Subscription;
use tokio::net::UnixStream;
use tokio::sync::mpsc;

/// First and last retry delay while `fogd` is not reachable.
const BACKOFF_MIN: Duration = Duration::from_millis(200);
const BACKOFF_MAX: Duration = Duration::from_secs(3);
/// Messages buffered towards the UI before the reader waits.
const UI_QUEUE: usize = 64;

/// Sends requests to the connected `fogd`.
pub type Link = mpsc::UnboundedSender<Request>;

#[derive(Debug, Clone)]
pub enum Event {
    Up(Link),
    Reply(Reply),
    Down,
}

/// `$XDG_RUNTIME_DIR/fog/fogd.sock`, as `fog_daemon::socket_path` computes it.
fn socket_path() -> io::Result<PathBuf> {
    let dir = std::env::var_os("XDG_RUNTIME_DIR")
        .filter(|d| !d.is_empty())
        .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "XDG_RUNTIME_DIR is not set"))?;
    Ok(PathBuf::from(dir).join("fog").join("fogd.sock"))
}

pub fn subscription() -> Subscription<Event> {
    Subscription::run(|| iced::stream::channel(UI_QUEUE, run))
}

async fn run(mut out: ui::Sender<Event>) {
    let mut backoff = BACKOFF_MIN;
    let mut was_up = true;
    loop {
        let stream = match socket_path() {
            Ok(p) => UnixStream::connect(p).await,
            Err(e) => Err(e),
        };
        let Ok(stream) = stream else {
            if was_up {
                was_up = false;
                if out.send(Event::Down).await.is_err() {
                    return;
                }
            }
            tokio::time::sleep(backoff).await;
            backoff = (backoff * 2).min(BACKOFF_MAX);
            continue;
        };
        backoff = BACKOFF_MIN;
        was_up = true;
        let (tx, mut rx) = mpsc::unbounded_channel::<Request>();
        if out.send(Event::Up(tx)).await.is_err() {
            return;
        }
        let (mut rd, mut wr) = stream.into_split();
        // `read_frame` is not cancel-safe, so the reader is never raced
        // against anything but the writer finishing.
        let reader = async {
            while let Ok(Some(reply)) = fog_proto::read_frame::<_, Reply>(&mut rd).await {
                if out.send(Event::Reply(reply)).await.is_err() {
                    return false;
                }
            }
            true
        };
        let writer = async {
            while let Some(req) = rx.recv().await {
                if fog_proto::write_frame(&mut wr, &req).await.is_err() {
                    break;
                }
            }
        };
        let ui_alive = tokio::select! {
            alive = reader => alive,
            () = writer => true,
        };
        if !ui_alive {
            return;
        }
    }
}
