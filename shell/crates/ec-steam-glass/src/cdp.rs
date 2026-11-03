// SPDX-License-Identifier: AGPL-3.0-only
//! Just enough Chrome DevTools Protocol for one job: list Steam's page targets
//! over `GET /json` and talk JSON to one of them over a WebSocket.
//!
//! Loopback only, no TLS, no Origin header (Chromium refuses a browser-looking
//! one). Client frames are masked as RFC 6455 requires; the mask is a constant
//! because it protects nothing on loopback, it only has to be present.

use std::io::{self, Read, Write};
use std::net::TcpStream;
use std::time::Duration;

use serde_json::Value;

const MASK: [u8; 4] = [0x37, 0xfa, 0x21, 0x3d];
const OP_TEXT: u8 = 0x1;
const OP_CLOSE: u8 = 0x8;
const OP_PING: u8 = 0x9;
const OP_PONG: u8 = 0xA;
/// A command may run script in a large hidden page; give it time.
const CALL_TIMEOUT: Duration = Duration::from_secs(30);
/// Larger than any reply Steam sends; a frame past it is a broken peer.
const MAX_FRAME: u64 = 16 * 1024 * 1024;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Target {
    pub id: String,
    pub title: String,
    pub url: String,
    /// The request path of `webSocketDebuggerUrl`, e.g. `/devtools/page/ABC`.
    pub ws_path: String,
}

/// `GET /json`: every debuggable target Steam's CEF currently has.
pub fn list_targets(port: u16) -> io::Result<Vec<Target>> {
    let mut s = TcpStream::connect(("127.0.0.1", port))?;
    s.set_read_timeout(Some(Duration::from_secs(3)))?;
    write!(s, "GET /json HTTP/1.1\r\nHost: localhost:{port}\r\n\r\n")?;
    let head = read_head(&mut s)?;
    // The server keeps the connection open after replying, so the body is
    // read by its length, not to end of stream.
    let len = std::str::from_utf8(&head)
        .ok()
        .and_then(|h| {
            h.lines().find_map(|l| {
                let (k, v) = l.split_once(':')?;
                k.eq_ignore_ascii_case("content-length")
                    .then(|| v.trim().parse::<usize>().ok())?
            })
        })
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "no Content-Length"))?;
    if len as u64 > MAX_FRAME {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "oversized /json"));
    }
    let mut body = vec![0u8; len];
    s.read_exact(&mut body)?;
    parse_targets(&body)
}

/// The reply head up to and including the blank line, read a byte at a time
/// so nothing after it is consumed.
fn read_head(s: &mut TcpStream) -> io::Result<Vec<u8>> {
    let mut head = Vec::new();
    let mut b = [0u8; 1];
    while !head.ends_with(b"\r\n\r\n") {
        s.read_exact(&mut b)?;
        head.push(b[0]);
        if head.len() > 8192 {
            return Err(io::Error::new(io::ErrorKind::InvalidData, "oversized reply head"));
        }
    }
    Ok(head)
}

fn parse_targets(body: &[u8]) -> io::Result<Vec<Target>> {
    let bad = |m: &str| io::Error::new(io::ErrorKind::InvalidData, m.to_owned());
    let v: Value = serde_json::from_slice(body).map_err(|e| bad(&e.to_string()))?;
    let arr = v.as_array().ok_or_else(|| bad("/json is not an array"))?;
    Ok(arr
        .iter()
        .filter(|t| t["type"] == "page")
        .filter_map(|t| {
            let ws = t["webSocketDebuggerUrl"].as_str()?;
            // ws://host:port/path -> /path
            let path = ws.strip_prefix("ws://")?.split_once('/')?.1;
            Some(Target {
                id: t["id"].as_str()?.to_owned(),
                title: t["title"].as_str().unwrap_or_default().to_owned(),
                url: t["url"].as_str().unwrap_or_default().to_owned(),
                ws_path: format!("/{path}"),
            })
        })
        .collect())
}

pub struct Ws {
    stream: TcpStream,
    next_id: u64,
}

impl Ws {
    pub fn connect(port: u16, path: &str) -> io::Result<Ws> {
        let mut stream = TcpStream::connect(("127.0.0.1", port))?;
        stream.set_read_timeout(Some(CALL_TIMEOUT))?;
        write!(
            stream,
            "GET {path} HTTP/1.1\r\nHost: localhost:{port}\r\nUpgrade: websocket\r\n\
             Connection: Upgrade\r\nSec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\n\
             Sec-WebSocket-Version: 13\r\n\r\n"
        )?;
        let head = read_head(&mut stream)?;
        if !head.starts_with(b"HTTP/1.1 101") {
            return Err(io::Error::new(
                io::ErrorKind::ConnectionRefused,
                "websocket upgrade refused",
            ));
        }
        Ok(Ws { stream, next_id: 0 })
    }

    /// Whether a message is waiting, giving up after `wait`. A closed
    /// connection is an error. Peeking leaves the stream untouched, so a read
    /// that gives up cannot leave half a frame consumed.
    pub fn readable(&self, wait: Duration) -> io::Result<bool> {
        self.stream.set_read_timeout(Some(wait))?;
        let mut b = [0u8; 1];
        let r = self.stream.peek(&mut b);
        self.stream.set_read_timeout(Some(CALL_TIMEOUT))?;
        match r {
            Ok(0) => Err(io::Error::new(io::ErrorKind::ConnectionAborted, "closed")),
            Ok(_) => Ok(true),
            Err(e) if matches!(e.kind(), io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut) => Ok(false),
            Err(e) => Err(e),
        }
    }

    /// One CDP command; returns the matching reply's `result`. Events that
    /// arrive first are dropped.
    pub fn call(&mut self, method: &str, params: Value) -> io::Result<Value> {
        self.next_id += 1;
        let id = self.next_id;
        let msg = serde_json::json!({ "id": id, "method": method, "params": params });
        self.send_text(&msg.to_string())?;
        loop {
            let reply: Value = serde_json::from_str(&self.recv_text()?)
                .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e.to_string()))?;
            if reply["id"] == id {
                if let Some(e) = reply.get("error") {
                    return Err(io::Error::other(format!("{method}: {e}")));
                }
                return Ok(reply["result"].clone());
            }
        }
    }

    pub fn send_text(&mut self, s: &str) -> io::Result<()> {
        self.stream.write_all(&encode_frame(OP_TEXT, s.as_bytes()))
    }

    /// The next text message. Pings are answered; a close frame is an error.
    pub fn recv_text(&mut self) -> io::Result<String> {
        let mut msg = Vec::new();
        loop {
            let (fin, op, payload) = read_frame(&mut self.stream)?;
            match op {
                OP_PING => self.stream.write_all(&encode_frame(OP_PONG, &payload))?,
                OP_CLOSE => return Err(io::Error::new(io::ErrorKind::ConnectionAborted, "closed")),
                OP_PONG => {}
                // 0x0 continuation and 0x1 text both append to the message.
                _ => {
                    msg.extend_from_slice(&payload);
                    if fin {
                        return String::from_utf8(msg)
                            .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "non-UTF-8 text"));
                    }
                }
            }
        }
    }
}

/// A masked client frame, FIN set, one fragment.
pub fn encode_frame(op: u8, payload: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(payload.len() + 14);
    out.push(0x80 | op);
    match payload.len() {
        n if n < 126 => out.push(0x80 | n as u8),
        n if n <= u16::MAX as usize => {
            out.push(0x80 | 126);
            out.extend_from_slice(&(n as u16).to_be_bytes());
        }
        n => {
            out.push(0x80 | 127);
            out.extend_from_slice(&(n as u64).to_be_bytes());
        }
    }
    out.extend_from_slice(&MASK);
    out.extend(payload.iter().enumerate().map(|(i, b)| b ^ MASK[i % 4]));
    out
}

/// One frame as `(fin, opcode, unmasked payload)`.
fn read_frame(r: &mut impl Read) -> io::Result<(bool, u8, Vec<u8>)> {
    let mut h = [0u8; 2];
    r.read_exact(&mut h)?;
    let (fin, op) = (h[0] & 0x80 != 0, h[0] & 0x0f);
    let masked = h[1] & 0x80 != 0;
    let len = match h[1] & 0x7f {
        126 => {
            let mut b = [0u8; 2];
            r.read_exact(&mut b)?;
            u16::from_be_bytes(b) as u64
        }
        127 => {
            let mut b = [0u8; 8];
            r.read_exact(&mut b)?;
            u64::from_be_bytes(b)
        }
        n => n as u64,
    };
    if len > MAX_FRAME {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "oversized frame"));
    }
    let mut mask = [0u8; 4];
    if masked {
        r.read_exact(&mut mask)?;
    }
    let mut payload = vec![0u8; len as usize];
    r.read_exact(&mut payload)?;
    if masked {
        for (i, b) in payload.iter_mut().enumerate() {
            *b ^= mask[i % 4];
        }
    }
    Ok((fin, op, payload))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_frame_round_trips_at_every_length_class() {
        for n in [0usize, 5, 125, 126, 300, 70_000] {
            let payload: Vec<u8> = (0..n).map(|i| (i % 251) as u8).collect();
            let wire = encode_frame(OP_TEXT, &payload);
            let (fin, op, back) = read_frame(&mut wire.as_slice()).unwrap();
            assert!(fin);
            assert_eq!(op, OP_TEXT);
            assert_eq!(back, payload, "length {n}");
        }
    }

    #[test]
    fn an_oversized_frame_is_refused_without_allocating_it() {
        let mut wire = vec![0x81, 127];
        wire.extend_from_slice(&u64::MAX.to_be_bytes());
        assert!(read_frame(&mut wire.as_slice()).is_err());
    }

    #[test]
    fn targets_keep_pages_and_drop_the_rest() {
        let raw = b"[\
            {\"id\":\"A\",\"type\":\"page\",\"title\":\"Steam\",\"url\":\"https://steamloopback.host/index.html\",\
             \"webSocketDebuggerUrl\":\"ws://localhost:8080/devtools/page/A\"},\
            {\"id\":\"B\",\"type\":\"worker\",\"title\":\"w\",\"url\":\"\",\
             \"webSocketDebuggerUrl\":\"ws://localhost:8080/devtools/page/B\"},\
            {\"id\":\"C\",\"type\":\"page\",\"title\":\"no socket\",\"url\":\"\"}]";
        let t = parse_targets(raw).unwrap();
        assert_eq!(t.len(), 1);
        assert_eq!(t[0].id, "A");
        assert_eq!(t[0].ws_path, "/devtools/page/A");
    }

    #[test]
    fn a_body_that_is_not_a_target_list_is_an_error() {
        assert!(parse_targets(b"{}").is_err());
        assert!(parse_targets(b"not json").is_err());
    }
}
