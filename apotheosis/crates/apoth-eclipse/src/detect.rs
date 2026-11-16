// SPDX-License-Identifier: AGPL-3.0-only
//! Abyss detection and the theme feed (T-17 §1, §2).
//!
//! Abyss is present when `ec_ipc` connects on the default socket and `get_theme` answers
//! with seeds. Detection runs once at start on a thread of its own; when it succeeds the
//! thread subscribes to `theme` and forwards each change. On socket loss it reports
//! [`Abyss::Gone`] and detects again with a 1 s → 30 s backoff. Absent at start, the
//! thread ends: nothing here ever polls a compositor that was never there. Detection
//! governs integration features only; it never relaxes peer checks on the control socket
//! (T-12 §4.1). Nothing logged here carries content: only that Abyss came or went.

use std::path::PathBuf;
use std::thread::JoinHandle;
use std::time::Duration;

use ec_ipc::{Client, EventKind};
use ec_theme::Seeds;

use crate::theme::seeds_from_payload;

/// What the detection thread reports.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Abyss {
    /// Abyss is present with these theme seeds (first detection, a reconnect, or a
    /// `theme` event).
    Theme(Seeds),
    /// The connection was lost: fall back until a later [`Abyss::Theme`].
    Gone,
}

/// Retry delays after a lost connection.
#[derive(Clone, Copy, Debug)]
pub struct Backoff {
    pub first: Duration,
    pub max: Duration,
}

impl Default for Backoff {
    /// T-17 §1: 1 s doubling to 30 s.
    fn default() -> Self {
        Backoff {
            first: Duration::from_secs(1),
            max: Duration::from_secs(30),
        }
    }
}

/// Start detection on the default socket. `on` runs on the detection thread.
pub fn spawn(on: impl FnMut(Abyss) + Send + 'static) -> std::io::Result<JoinHandle<()>> {
    spawn_at(None, Backoff::default(), on)
}

/// Start detection on `path` (`None`: the default socket).
pub fn spawn_at(
    path: Option<PathBuf>,
    backoff: Backoff,
    mut on: impl FnMut(Abyss) + Send + 'static,
) -> std::io::Result<JoinHandle<()>> {
    std::thread::Builder::new()
        .name("apoth-abyss".into())
        .spawn(move || run(path.as_deref(), backoff, &mut on))
}

fn connect(path: Option<&std::path::Path>) -> Option<(Client, Seeds)> {
    let mut c = match path {
        Some(p) => Client::connect_to(p),
        None => Client::connect(),
    }
    .ok()?;
    let seeds = seeds_from_payload(&c.call("get_theme", serde_json::json!({})).ok()?)?;
    Some((c, seeds))
}

fn run(path: Option<&std::path::Path>, backoff: Backoff, on: &mut dyn FnMut(Abyss)) {
    let Some(mut conn) = connect(path) else {
        tracing::debug!("Abyss not detected; EclipseOS integration stays off");
        return;
    };
    loop {
        let (mut c, seeds) = conn;
        tracing::info!("Abyss detected");
        on(Abyss::Theme(seeds));
        // A refused subscription still leaves the start theme in force; the connection
        // is then watched for loss only.
        let _ = c.subscribe(&[EventKind::Theme]);
        while let Ok(ev) = c.wait_event() {
            if ev.kind == EventKind::Theme
                && let Some(s) = seeds_from_payload(&ev.data)
            {
                on(Abyss::Theme(s));
            }
        }
        tracing::info!("Abyss connection lost");
        on(Abyss::Gone);
        let mut wait = backoff.first;
        conn = loop {
            std::thread::sleep(wait);
            if let Some(c) = connect(path) {
                break c;
            }
            wait = (wait * 2).min(backoff.max);
        };
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{BufRead, BufReader, Write};
    use std::os::unix::net::{UnixListener, UnixStream};
    use std::sync::mpsc;

    fn payload(base: &str) -> serde_json::Value {
        serde_json::json!({"scheme": "dark", "seeds": {
            "base": base, "ink": "#ffffff", "accent": "#f2c33c",
            "accent_text": null, "frost_tint": "#17140fff", "rim": 0.2,
        }})
    }

    /// Answer requests on `s` until `subscribe`, then return.
    fn serve(s: &UnixStream, base: &str) {
        let mut r = BufReader::new(s.try_clone().unwrap());
        let mut w = s.try_clone().unwrap();
        loop {
            let mut line = String::new();
            if r.read_line(&mut line).unwrap() == 0 {
                return;
            }
            let req: serde_json::Value = serde_json::from_str(&line).unwrap();
            let method = req["method"].as_str().unwrap().to_owned();
            let result = if method == "get_theme" {
                payload(base)
            } else {
                serde_json::json!({})
            };
            let reply = serde_json::json!({"jsonrpc": "2.0", "id": req["id"], "result": result});
            writeln!(w, "{reply}").unwrap();
            if method == "subscribe" {
                return;
            }
        }
    }

    fn event(s: &mut UnixStream, base: &str) {
        let ev = serde_json::json!({"jsonrpc": "2.0", "method": "event",
            "params": {"event": "theme", "data": payload(base)}});
        writeln!(s, "{ev}").unwrap();
    }

    fn base_of(a: Abyss) -> Option<u32> {
        match a {
            Abyss::Theme(s) => {
                let b = |v: f32| (v * 255.0).round() as u32;
                Some((b(s.base.r()) << 16) | (b(s.base.g()) << 8) | b(s.base.b()))
            }
            Abyss::Gone => None,
        }
    }

    #[test]
    fn detects_follows_theme_and_reconnects() {
        let dir = std::env::temp_dir().join(format!("apoth-eclipse-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let sock = dir.join("abyss.sock");
        let l = UnixListener::bind(&sock).unwrap();
        let (tx, rx) = mpsc::channel();
        let quick = Backoff {
            first: Duration::from_millis(5),
            max: Duration::from_millis(20),
        };
        let _h = spawn_at(Some(sock.clone()), quick, move |a| {
            let _ = tx.send(a);
        })
        .unwrap();
        let t = Duration::from_secs(10);

        let (mut s, _) = l.accept().unwrap();
        serve(&s, "#0b0906");
        assert_eq!(base_of(rx.recv_timeout(t).unwrap()), Some(0x0b0906));
        event(&mut s, "#101820");
        assert_eq!(base_of(rx.recv_timeout(t).unwrap()), Some(0x101820));
        // A malformed theme event is ignored, the next good one is not.
        writeln!(
            s,
            "{}",
            serde_json::json!({"jsonrpc": "2.0", "method": "event",
            "params": {"event": "theme", "data": {"seeds": {}}}})
        )
        .unwrap();
        event(&mut s, "#202020");
        assert_eq!(base_of(rx.recv_timeout(t).unwrap()), Some(0x202020));

        drop(s);
        assert_eq!(rx.recv_timeout(t).unwrap(), Abyss::Gone);
        // Reconnect after the backoff: the theme comes again from get_theme.
        let (s, _) = l.accept().unwrap();
        serve(&s, "#303030");
        assert_eq!(base_of(rx.recv_timeout(t).unwrap()), Some(0x303030));
        drop(s);
        assert_eq!(rx.recv_timeout(t).unwrap(), Abyss::Gone);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn absent_at_start_reports_nothing_and_ends() {
        let sock =
            std::env::temp_dir().join(format!("apoth-eclipse-none-{}.sock", std::process::id()));
        let (tx, rx) = mpsc::channel();
        let h = spawn_at(Some(sock), Backoff::default(), move |a| {
            let _ = tx.send(a);
        })
        .unwrap();
        h.join().unwrap();
        assert!(rx.try_recv().is_err());
    }

    #[test]
    fn a_socket_that_is_not_abyss_is_absent() {
        let dir = std::env::temp_dir().join(format!("apoth-eclipse-x-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let sock = dir.join("abyss.sock");
        let l = UnixListener::bind(&sock).unwrap();
        let (tx, rx) = mpsc::channel();
        let h = spawn_at(Some(sock), Backoff::default(), move |a| {
            let _ = tx.send(a);
        })
        .unwrap();
        let (s, _) = l.accept().unwrap();
        let mut r = BufReader::new(s.try_clone().unwrap());
        let mut line = String::new();
        r.read_line(&mut line).unwrap();
        let req: serde_json::Value = serde_json::from_str(&line).unwrap();
        let mut w = s;
        writeln!(
            w,
            "{}",
            serde_json::json!({"jsonrpc": "2.0", "id": req["id"], "result": {"x": 1}})
        )
        .unwrap();
        h.join().unwrap();
        assert!(rx.try_recv().is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
