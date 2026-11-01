// SPDX-License-Identifier: AGPL-3.0-only
//! Relay between the Firefox glass extension and abyss (ADR 0079).
//!
//! Extension -> host: native messaging on stdin, a 4-byte native-endian length
//! then JSON. A `{"type":"strips", ...}` message is forwarded to abyss as
//! `set_client_strips`. Host -> extension: `{"type":"status","connected":bool}`
//! on connect, on every change, and after each strips call.
//!
//! Abyss ties the strips to this process's control-socket connection, so
//! exiting on stdin EOF is what takes them off the windows.

use std::io::{self, Read, Write};

use serde_json::{json, Value};

/// Firefox accepts at most 1 MiB from a host; the same bound is applied to
/// what it sends, which is one small object per change.
pub const MAX_MESSAGE: usize = 1024 * 1024;

/// Read one framed message. `Ok(None)` is a clean end of input.
pub fn read_message<R: Read>(r: &mut R) -> io::Result<Option<Value>> {
    let mut len = [0u8; 4];
    match r.read(&mut len[..1])? {
        0 => return Ok(None),
        _ => r.read_exact(&mut len[1..])?,
    }
    let len = u32::from_ne_bytes(len) as usize;
    if len > MAX_MESSAGE {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "message too large"));
    }
    let mut body = vec![0u8; len];
    r.read_exact(&mut body)?;
    serde_json::from_slice(&body)
        .map(Some)
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "message is not JSON"))
}

/// Write one framed message and flush it.
pub fn write_message<W: Write>(w: &mut W, msg: &Value) -> io::Result<()> {
    let body = msg.to_string();
    if body.len() > MAX_MESSAGE {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "message too large"));
    }
    w.write_all(&(body.len() as u32).to_ne_bytes())?;
    w.write_all(body.as_bytes())?;
    w.flush()
}

/// The abyss end, behind a trait so the loop is testable without a compositor.
pub trait Strips {
    /// Make sure there is a connection. `Err` is a message safe to show.
    fn connect(&mut self) -> Result<(), String>;
    /// Send `set_client_strips`. On `Err` the connection is considered gone.
    fn set_client_strips(&mut self, params: Value) -> Result<(), String>;
}

/// The real thing: a lazily (re)connected `ec-ipc` client.
#[derive(Default)]
pub struct Abyss {
    client: Option<ec_ipc::Client>,
}

impl Strips for Abyss {
    fn connect(&mut self) -> Result<(), String> {
        if self.client.is_none() {
            self.client = Some(ec_ipc::Client::connect().map_err(|e| e.to_string())?);
        }
        Ok(())
    }

    fn set_client_strips(&mut self, params: Value) -> Result<(), String> {
        self.connect()?;
        let client = self.client.as_mut().expect("connected above");
        match client.call("set_client_strips", params) {
            Ok(_) => Ok(()),
            Err(e) => {
                // A denial is an answer on a healthy connection; anything else
                // may have killed it, so reconnect on the next message.
                if !e.is_denied() {
                    self.client = None;
                }
                Err(e.to_string())
            }
        }
    }
}

pub struct Host<S: Strips> {
    abyss: S,
    /// What the extension was last told, to send a status on change.
    last: Option<(bool, Option<String>)>,
}

impl<S: Strips> Host<S> {
    pub fn new(abyss: S) -> Self {
        Host { abyss, last: None }
    }

    fn status<W: Write>(&mut self, out: &mut W, result: Result<(), String>, always: bool) -> io::Result<()> {
        let now = match result {
            Ok(()) => (true, None),
            Err(e) => (false, Some(e)),
        };
        if !always && self.last.as_ref() == Some(&now) {
            return Ok(());
        }
        let mut msg = json!({"type": "status", "connected": now.0});
        if let Some(e) = &now.1 {
            msg["error"] = json!(e);
        }
        self.last = Some(now);
        write_message(out, &msg)
    }

    /// Handle one message from the extension.
    pub fn handle<W: Write>(&mut self, msg: &Value, out: &mut W) -> io::Result<()> {
        let result = match strips_params(msg) {
            Some(params) => self.abyss.set_client_strips(params),
            None => self.abyss.connect().and(Err("unrecognised message".to_owned())),
        };
        // A bad message is not an abyss outage; only report it as the error.
        self.status(out, result, true)
    }

    /// The whole loop: announce, then serve until stdin closes.
    pub fn run<R: Read, W: Write>(&mut self, input: &mut R, out: &mut W) -> io::Result<()> {
        let first = self.abyss.connect();
        self.status(out, first, false)?;
        while let Some(msg) = read_message(input)? {
            self.handle(&msg, out)?;
        }
        Ok(())
    }
}

/// The `set_client_strips` params of a well-formed strips message.
fn strips_params(msg: &Value) -> Option<Value> {
    if msg.get("type")?.as_str()? != "strips" {
        return None;
    }
    // Forwarded as the extension sent them; abyss validates keys and windows.
    let keys = msg.get("keys")?.as_array()?;
    let windows = msg.get("windows")?.as_array()?;
    Some(json!({"keys": keys, "windows": windows}))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Default)]
    struct Fake {
        up: bool,
        calls: Vec<Value>,
    }

    impl Strips for Fake {
        fn connect(&mut self) -> Result<(), String> {
            if self.up {
                Ok(())
            } else {
                Err("abyss is not running".into())
            }
        }
        fn set_client_strips(&mut self, params: Value) -> Result<(), String> {
            self.connect()?;
            self.calls.push(params);
            Ok(())
        }
    }

    fn frame(v: &Value) -> Vec<u8> {
        let mut b = Vec::new();
        write_message(&mut b, v).unwrap();
        b
    }

    fn replies(mut out: &[u8]) -> Vec<Value> {
        let mut v = Vec::new();
        while let Some(m) = read_message(&mut out).unwrap() {
            v.push(m);
        }
        v
    }

    #[test]
    fn framing_round_trips_and_uses_a_native_endian_length() {
        let msg = json!({"type": "strips", "keys": [{"color": "#112233", "opacity": 0.6}], "windows": []});
        let bytes = frame(&msg);
        let body = msg.to_string();
        assert_eq!(bytes[..4], (body.len() as u32).to_ne_bytes());
        assert_eq!(&bytes[4..], body.as_bytes());
        assert_eq!(read_message(&mut &bytes[..]).unwrap(), Some(msg));
    }

    #[test]
    fn framing_rejects_oversize_truncated_and_non_json() {
        assert_eq!(read_message(&mut &[][..]).unwrap(), None);
        let big = ((MAX_MESSAGE + 1) as u32).to_ne_bytes();
        assert!(read_message(&mut &big[..]).is_err());
        let mut short = 10u32.to_ne_bytes().to_vec();
        short.extend(b"{}");
        assert!(read_message(&mut &short[..]).is_err());
        let mut junk = 3u32.to_ne_bytes().to_vec();
        junk.extend(b"abc");
        assert!(read_message(&mut &junk[..]).is_err());
        assert!(read_message(&mut &[1u8, 0][..]).is_err());
    }

    #[test]
    fn strips_are_forwarded_and_each_call_is_answered() {
        let m1 = json!({"type": "strips", "keys": [{"color": "#112233", "opacity": 0.6}], "windows": [{"width": 1, "height": 2, "content": 1}]});
        let m2 = json!({"type": "strips", "keys": [{"color": "#112233", "opacity": 0.5}], "windows": []});
        let mut input = frame(&m1);
        input.extend(frame(&m2));
        let mut out = Vec::new();
        let mut host = Host::new(Fake {
            up: true,
            ..Fake::default()
        });
        host.run(&mut &input[..], &mut out).unwrap();
        assert_eq!(host.abyss.calls.len(), 2);
        assert_eq!(host.abyss.calls[0]["windows"], m1["windows"]);
        assert_eq!(host.abyss.calls[1]["keys"][0]["opacity"], 0.5);
        let connected = json!({"type": "status", "connected": true});
        assert_eq!(
            replies(&out),
            vec![connected.clone(), connected.clone(), connected]
        );
    }

    #[test]
    fn an_absent_abyss_reports_disconnected_and_recovers_on_the_next_message() {
        let m = json!({"type": "strips", "keys": [{"color": "#112233", "opacity": 0.6}], "windows": []});
        let mut out = Vec::new();
        let mut host = Host::new(Fake::default());
        let first = host_connect(&mut host);
        host.status(&mut out, first, false).unwrap();
        host.handle(&m, &mut out).unwrap();
        host.abyss.up = true;
        host.handle(&m, &mut out).unwrap();
        let got = replies(&out);
        assert_eq!(got[0]["connected"], false);
        assert_eq!(got[0]["error"], "abyss is not running");
        assert_eq!(got[1]["connected"], false);
        assert_eq!(got[2], json!({"type": "status", "connected": true}));
        assert_eq!(host.abyss.calls.len(), 1);
    }

    fn host_connect(h: &mut Host<Fake>) -> Result<(), String> {
        h.abyss.connect()
    }

    #[test]
    fn startup_announces_once_and_a_bad_message_forwards_nothing() {
        let mut out = Vec::new();
        let mut host = Host::new(Fake {
            up: true,
            ..Fake::default()
        });
        let input = frame(&json!({"type": "nope"}));
        host.run(&mut &input[..], &mut out).unwrap();
        assert!(host.abyss.calls.is_empty());
        let got = replies(&out);
        assert_eq!(got[0], json!({"type": "status", "connected": true}));
        assert_eq!(got[1]["connected"], false);
    }

    #[test]
    fn eof_ends_the_loop() {
        let mut host = Host::new(Fake {
            up: true,
            ..Fake::default()
        });
        assert!(host.run(&mut &[][..], &mut Vec::new()).is_ok());
    }
}
