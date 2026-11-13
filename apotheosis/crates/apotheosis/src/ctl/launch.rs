// SPDX-License-Identifier: AGPL-3.0-only
//! A second launch (T-12 §5, T-21 §1): hand the request to the running instance as one
//! `launch` and exit, unless it does not answer within 300 ms or refuses (then this process
//! starts its own window). With `--wait` the child's exit code is relayed. The instance
//! tried is `$APOTHEOSIS_SOCKET` (a launch from inside a pane), else the newest live socket
//! in the runtime directory. Nothing of the request is logged.

use std::collections::BTreeMap;
use std::io::{self, Read, Write};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::time::Duration;

use apoth_proto::limits::{MAX_ELEM_BYTES, MAX_ENV, MAX_SHORT_BYTES};
use apoth_proto::params::{Hello, Launch};
use apoth_proto::{ENV_SOCKET, LineDecoder, Params, Reply, Request, ServerMessage};

/// How long the running instance has to answer (T-21 §1).
pub(crate) const ANSWER: Duration = Duration::from_millis(300);

/// What became of the hand-off.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Handoff {
    /// The instance took it; exit with this code.
    Done(i32),
    /// Start our own instance.
    Own,
}

/// The environment a launch carries, bounded to what one request line holds.
pub(crate) fn launch_env() -> BTreeMap<String, String> {
    let mut budget = apoth_proto::MAX_LINE_BYTES / 2;
    let mut m = BTreeMap::new();
    for (k, v) in std::env::vars_os() {
        let (Ok(k), Ok(v)) = (k.into_string(), v.into_string()) else {
            continue;
        };
        let cost = k.len() + v.len() + 8;
        if k.is_empty() || k.len() > MAX_SHORT_BYTES || v.len() > MAX_ELEM_BYTES || cost > budget {
            continue;
        }
        budget -= cost;
        m.insert(k, v);
        if m.len() == MAX_ENV {
            break;
        }
    }
    m
}

/// Live-looking sockets in `dir`, newest first.
fn candidates(dir: &Path) -> Vec<PathBuf> {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut v: Vec<(std::time::SystemTime, PathBuf)> = rd
        .filter_map(Result::ok)
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|x| x == "sock"))
        .filter_map(|p| Some((std::fs::symlink_metadata(&p).ok()?.modified().ok()?, p)))
        .collect();
    v.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.cmp(&b.1)));
    v.into_iter().map(|(_, p)| p).collect()
}

struct Line {
    stream: UnixStream,
    dec: LineDecoder,
    lines: std::collections::VecDeque<Vec<u8>>,
}

impl Line {
    fn call(&mut self, id: u64, params: Params) -> io::Result<Reply> {
        let line = Request { id, params }
            .to_line()
            .map_err(|e| io::Error::other(e.message))?;
        self.stream.write_all(&line)?;
        let mut buf = [0u8; 8192];
        loop {
            while let Some(l) = self.lines.pop_front() {
                if let Ok(ServerMessage::Reply(r)) = ServerMessage::decode(&l) {
                    return Ok(r);
                }
            }
            let n = self.stream.read(&mut buf)?;
            if n == 0 {
                return Err(io::ErrorKind::UnexpectedEof.into());
            }
            for l in self.dec.feed(&buf[..n]) {
                self.lines
                    .push_back(l.map_err(|e| io::Error::other(e.message))?);
            }
        }
    }
}

fn answering(path: &Path) -> Option<Line> {
    let stream = UnixStream::connect(path).ok()?;
    stream.set_read_timeout(Some(ANSWER)).ok()?;
    stream.set_write_timeout(Some(ANSWER)).ok()?;
    let mut l = Line {
        stream,
        dec: LineDecoder::new(),
        lines: std::collections::VecDeque::new(),
    };
    let hello = Params::Hello(Hello {
        proto: apoth_proto::message::PROTO_VERSION,
        client: "apotheosis".into(),
        want: Vec::new(),
    });
    match l.call(1, hello) {
        Ok(Reply::Ok { .. }) => Some(l),
        _ => None,
    }
}

/// Try to hand `launch` to a running instance.
pub(crate) fn hand_off(launch: Launch) -> Handoff {
    let runtime_dir = std::env::var_os("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .map(|p| p.join("apotheosis"));
    let mut paths: Vec<PathBuf> = std::env::var_os(ENV_SOCKET)
        .filter(|p| !p.is_empty())
        .map(PathBuf::from)
        .into_iter()
        .collect();
    if let Some(d) = runtime_dir {
        paths.extend(candidates(&d));
    }
    let Some(mut line) = paths.iter().find_map(|p| answering(p)) else {
        return Handoff::Own;
    };
    let wait = launch.wait;
    if wait && line.stream.set_read_timeout(None).is_err() {
        return Handoff::Own;
    }
    match line.call(2, Params::Launch(Box::new(launch))) {
        Ok(Reply::Ok { ok, .. }) => {
            let code = match ok.get("exit").and_then(serde_json::Value::as_i64) {
                Some(c) if c >= 0 => (c & 0xff) as i32,
                Some(c) => 128 + ((-c) & 0x7f) as i32,
                None => 0,
            };
            Handoff::Done(code)
        }
        Ok(Reply::Err { error, .. }) => {
            tracing::info!(code = %error.code, "the running instance refused the launch");
            Handoff::Own
        }
        // Without `--wait` a launch that times out may still have opened its tab; starting
        // a second window then is the lesser harm than none at all.
        Err(e) => {
            tracing::info!(kind = ?e.kind(), "the running instance did not answer the launch");
            Handoff::Own
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::net::UnixListener;

    fn tmp(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("apoth-launch-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn a_silent_socket_is_not_an_instance() {
        let d = tmp("silent");
        let _l = UnixListener::bind(d.join("1.sock")).unwrap();
        let t = std::time::Instant::now();
        assert!(answering(&d.join("1.sock")).is_none());
        assert!(t.elapsed() < Duration::from_secs(2));
        assert!(answering(&d.join("missing.sock")).is_none());
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn env_fits_one_line() {
        let env = launch_env();
        let bytes: usize = env.iter().map(|(k, v)| k.len() + v.len() + 8).sum();
        assert!(bytes <= apoth_proto::MAX_LINE_BYTES / 2);
    }
}
