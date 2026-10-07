// SPDX-License-Identifier: AGPL-3.0-only
//! The two things agentd asks of abyss's human control socket (COMP-13 §2,
//! A-08 §7): the `show_decisions` call and the `decisions_pending` event.
//!
//! `ec_ipc::Client` does the call. Its `EventKind` has no `decisions_pending`
//! yet and drops unknown kinds, so the event stream is read here directly.

use std::io::{Read, Write};
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::Sender;
use std::sync::Arc;
use std::time::Duration;

use serde_json::{json, Value};

use crate::core::Msg;
use crate::rpc::{RpcError, DENIED, UNAVAILABLE};

/// `show_decisions` on abyss, blocking: run it off the core thread.
pub fn show_decisions(path: &std::path::Path) -> Result<Value, RpcError> {
    let mut c = ec_ipc::Client::connect_to(path)
        .map_err(|e| RpcError::new(UNAVAILABLE, "unavailable", &format!("compositor: {e}")))?;
    c.call("show_decisions", json!({})).map_err(|e| match e {
        ec_ipc::Error::Rpc { code, message } => RpcError::new(
            if code == -32000 { DENIED } else { code },
            "compositor_refused",
            &message,
        ),
        other => RpcError::new(UNAVAILABLE, "unavailable", &other.to_string()),
    })
}

/// Parses one human-socket line: `Some(count)` for a `decisions_pending`
/// event.
pub fn decisions_count(line: &str) -> Option<u64> {
    let v: Value = serde_json::from_str(line).ok()?;
    if v.get("method")?.as_str()? != "event" {
        return None;
    }
    let p = v.get("params")?;
    if p.get("event")?.as_str()? != "decisions_pending" {
        return None;
    }
    p.get("data")?.get("count")?.as_u64()
}

/// Subscribes to `decisions_pending` and relays each count; redials every
/// second while abyss is away.
pub fn spawn_relay(path: PathBuf, tx: Sender<Msg>, stop: Arc<AtomicBool>) {
    std::thread::spawn(move || {
        while !stop.load(Ordering::Relaxed) {
            if let Ok(mut s) = UnixStream::connect(&path) {
                let _ = s.set_read_timeout(Some(Duration::from_millis(200)));
                let sub = json!({"jsonrpc": "2.0", "id": 1, "method": "subscribe",
                                 "params": {"events": ["decisions_pending"]}});
                if s.write_all(format!("{sub}\n").as_bytes()).is_ok() {
                    read_events(&mut s, &tx, &stop);
                }
            }
            for _ in 0..10 {
                if stop.load(Ordering::Relaxed) {
                    return;
                }
                std::thread::sleep(Duration::from_millis(100));
            }
        }
    });
}

fn read_events(s: &mut UnixStream, tx: &Sender<Msg>, stop: &AtomicBool) {
    let mut buf: Vec<u8> = Vec::new();
    let mut chunk = [0u8; 4096];
    while !stop.load(Ordering::Relaxed) {
        match s.read(&mut chunk) {
            Ok(0) => return,
            Ok(n) => {
                buf.extend_from_slice(&chunk[..n]);
                while let Some(p) = buf.iter().position(|b| *b == b'\n') {
                    let line: Vec<u8> = buf.drain(..=p).collect();
                    if let Some(c) = decisions_count(&String::from_utf8_lossy(&line)) {
                        if tx.send(Msg::Decisions(c)).is_err() {
                            return;
                        }
                    }
                }
                if buf.len() > 64 * 1024 {
                    return;
                }
            }
            Err(e)
                if matches!(
                    e.kind(),
                    std::io::ErrorKind::WouldBlock
                        | std::io::ErrorKind::TimedOut
                        | std::io::ErrorKind::Interrupted
                ) => {}
            Err(_) => return,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_the_event_shape() {
        let l =
            r#"{"jsonrpc":"2.0","method":"event","params":{"event":"decisions_pending","data":{"count":3}}}"#;
        assert_eq!(decisions_count(l), Some(3));
        assert_eq!(decisions_count(r#"{"jsonrpc":"2.0","id":1,"result":{}}"#), None);
        assert_eq!(
            decisions_count(r#"{"method":"event","params":{"event":"focus","data":{"count":1}}}"#),
            None
        );
    }
}
