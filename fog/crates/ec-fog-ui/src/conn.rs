// SPDX-License-Identifier: AGPL-3.0-only

//! The `ec-fogd` connection, as an iced subscription (FOG §Architecture).
//!
//! The stream runs on its own std threads, never on the UI thread. It
//! connects to `fogd.sock`, hands the app a [`Link`] for requests and
//! forwards every [`Reply`]. When `ec-fogd` is absent or goes away it reports
//! [`Event::Down`] once and retries with backoff; the app shows that quietly.

use std::io;
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::sync::mpsc;
use std::time::Duration;

use ec_fog_proto::{Reply, Request};
use iced::futures::channel::{mpsc as ui, oneshot};
use iced::futures::executor::block_on;
use iced::futures::SinkExt;
use iced::Subscription;

/// First and last retry delay while `ec-fogd` is not reachable.
const BACKOFF_MIN: Duration = Duration::from_millis(200);
const BACKOFF_MAX: Duration = Duration::from_secs(3);
/// Messages buffered towards the UI before the reader waits.
const UI_QUEUE: usize = 64;

#[derive(Debug)]
enum Cmd {
    Req(Request),
    /// The reader saw the connection end; the writer stops.
    Close,
}

/// Sends requests to the connected `ec-fogd`.
#[derive(Debug, Clone)]
pub struct Link(mpsc::Sender<Cmd>);

impl Link {
    /// `Err` once the connection is gone.
    pub fn send(&self, req: Request) -> Result<(), ()> {
        self.0.send(Cmd::Req(req)).map_err(|_| ())
    }
}

#[derive(Debug, Clone)]
pub enum Event {
    Up(Link),
    Reply(Reply),
    Down,
}

/// `$XDG_RUNTIME_DIR/fog/fogd.sock`, as `ec_fog_daemon::socket_path` computes it.
fn socket_path() -> io::Result<PathBuf> {
    let dir = std::env::var_os("XDG_RUNTIME_DIR")
        .filter(|d| !d.is_empty())
        .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "XDG_RUNTIME_DIR is not set"))?;
    Ok(PathBuf::from(dir).join("fog").join("fogd.sock"))
}

pub fn subscription() -> Subscription<Event> {
    Subscription::run(|| {
        iced::stream::channel(UI_QUEUE, async move |out| {
            // The connection lives on its own thread; this future only keeps
            // the subscription open until that thread ends (the UI went away).
            let (done, ended) = oneshot::channel::<()>();
            std::thread::spawn(move || {
                run(out);
                let _ = done.send(());
            });
            let _ = ended.await;
        })
    })
}

/// Connect, serve, reconnect with backoff. Returns when the UI is gone.
fn run(mut out: ui::Sender<Event>) {
    let mut backoff = BACKOFF_MIN;
    let mut was_up = true;
    loop {
        let stream = match socket_path() {
            Ok(p) => UnixStream::connect(p),
            Err(e) => Err(e),
        };
        let Ok(stream) = stream else {
            if was_up {
                was_up = false;
                if block_on(out.send(Event::Down)).is_err() {
                    return;
                }
            }
            std::thread::sleep(backoff);
            backoff = (backoff * 2).min(BACKOFF_MAX);
            continue;
        };
        backoff = BACKOFF_MIN;
        was_up = true;
        let (tx, rx) = mpsc::channel::<Cmd>();
        if block_on(out.send(Event::Up(Link(tx.clone())))).is_err() {
            return;
        }
        let Ok(mut wr) = stream.try_clone() else {
            continue;
        };
        let mut rd = stream;
        // The writer runs here; the reader gets a thread and, when the
        // connection ends, tells the writer to stop.
        let mut reader_out = out.clone();
        let reader = std::thread::spawn(move || {
            let mut alive = true;
            while let Ok(Some(reply)) = ec_fog_proto::read_frame_blocking::<_, Reply>(&mut rd) {
                if block_on(reader_out.send(Event::Reply(reply))).is_err() {
                    alive = false;
                    break;
                }
            }
            let _ = tx.send(Cmd::Close);
            alive
        });
        while let Ok(Cmd::Req(req)) = rx.recv() {
            if ec_fog_proto::write_frame_blocking(&mut wr, &req).is_err() {
                break;
            }
        }
        // Writer failed first: unblock the reader.
        let _ = wr.shutdown(std::net::Shutdown::Both);
        if !reader.join().unwrap_or(false) {
            return;
        }
    }
}
