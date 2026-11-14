// SPDX-License-Identifier: AGPL-3.0-only
//! `apoth play` (T-12 §5, T-16 §2, ADR 0118): play a video or audio file in the pane through
//! the native media protocol. The file is opened here and its descriptor handed to the
//! terminal over the control socket (`media.attach`, `SCM_RIGHTS`); the terminal decodes it
//! in a sandboxed helper that cannot open files itself. The pane receives only
//! `ESC _ M a=o,s=f,t=<token>`: no path ever crosses the tty.
//!
//! Lifetime (T-16 §1.1). play opens a session (`a=b,i=<pid>,p=<pid>`) so the terminal
//! removes the placement if play dies without cleaning up, and stays in the foreground while
//! the file plays: Space pauses and resumes, `q` stops, Ctrl+C (a byte: the tty's signal
//! keys are off) stops and exits 130. It exits 0 at the end of the file, 69 when the terminal cannot play the media, 1 on another error the
//! terminal reports (`error=<code>`).
//!
//! `--ext` (`-x`): the placement goes to a borderless glass window instead (as `apoth icat
//! --ext`); play waits until the window closes (Escape or `q` in it) and Ctrl+C in the pane
//! closes it. Explicitly running play in the focused pane is the user gesture that lets audio
//! start (T-16 §5). Nothing here logs paths or content.

use std::io::{Read, Write};
use std::os::fd::{AsFd, OwnedFd};
use std::time::{Duration, Instant};

use rustix::termios::{LocalModes, OptionalActions, Termios, tcgetattr, tcgetwinsize, tcsetattr};

use crate::ctl::{ERROR, OK, USAGE};
use crate::icat::INTERRUPTED;

/// The placement's id on the media protocol (one per invocation).
const ID: u32 = 1;
/// Longest wait for the terminal's `ready` (or error) after the open.
const OPEN_TIMEOUT: Duration = Duration::from_secs(15);
/// Default window width for `--ext`, in cells.
const EXT_COLS: u32 = 80;

pub(crate) const HELP: &str = "\
Usage: apoth play [OPTIONS] FILE

Play a video or audio file in the pane (WebM/Matroska: AV1, Opus, Vorbis, FLAC; audio
files: FLAC, Ogg, MP3, WAV, AAC).

  --size CxR        fit the video in a C x R cell box (default: the pane width, 16:9)
  -l, --loop        play again from the start at the end
  --paused          open on the first frame, paused
  --mute            start muted
  --volume N        volume 0..100 (default 100)
  -x, --ext         play in a separate glass window; play waits until it closes

Space pauses and resumes, q stops, Ctrl+C stops and exits 130.
With --ext, Escape or q closes the window; Ctrl+C in the pane closes it and exits 130.
";

/// Parsed options.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Opts {
    pub(crate) size: Option<(u32, u32)>,
    pub(crate) looping: bool,
    pub(crate) paused: bool,
    pub(crate) mute: bool,
    pub(crate) volume: Option<u32>,
    pub(crate) ext: bool,
    pub(crate) file: String,
}

pub(crate) fn parse(args: &[String]) -> Result<Opts, String> {
    let mut o = Opts::default();
    let mut files = Vec::new();
    let mut it = args.iter();
    while let Some(a) = it.next() {
        let mut val = |name: &str| it.next().cloned().ok_or(format!("{name} needs a value"));
        match a.as_str() {
            "--size" => {
                let v = val("--size")?;
                let n = |s: &str| s.parse::<u32>().ok().filter(|n| (1..=1000).contains(n));
                o.size = Some(
                    v.split_once('x')
                        .and_then(|(c, r)| Some((n(c)?, n(r)?)))
                        .ok_or("--size wants CxR")?,
                );
            }
            "-l" | "--loop" => o.looping = true,
            "--paused" => o.paused = true,
            "--mute" => o.mute = true,
            "--volume" => {
                o.volume = Some(
                    val("--volume")?
                        .parse()
                        .ok()
                        .filter(|v: &u32| *v <= 100)
                        .ok_or("--volume wants 0..100")?,
                );
            }
            "-x" | "--ext" => o.ext = true,
            "--" => files.extend(it.by_ref().cloned()),
            s if s.starts_with('-') => return Err(format!("unknown option `{s}`")),
            s => files.push(s.to_string()),
        }
    }
    match <[String; 1]>::try_from(files) {
        Ok([f]) => o.file = f,
        Err(v) if v.is_empty() => return Err("no file given".into()),
        Err(_) => return Err("play takes one file".into()),
    }
    Ok(o)
}

/// The open command (`a=o`) for `token` in a `c` x `r` box.
pub(crate) fn open_apc(o: &Opts, token: &str, (c, r): (u32, u32)) -> String {
    let mut k = format!("a=o,i={ID},s=f,t={token},c={c},r={r}");
    if o.looping {
        k.push_str(",loop=1");
    }
    if o.mute {
        k.push_str(",mute=1");
    }
    if let Some(v) = o.volume {
        k.push_str(&format!(",volume={v}"));
    }
    if o.paused {
        k.push_str(",pause");
    }
    format!("\x1b_M{k}\x1b\\")
}

/// A 16:9 box `cols` wide in cells of `cell` pixels, at most `max_rows` tall.
pub(crate) fn default_box(cols: u32, cell: (u32, u32), max_rows: u32) -> (u32, u32) {
    let c = cols.clamp(1, 1000);
    let h_px = u64::from(c) * u64::from(cell.0.max(1)) * 9 / 16;
    let r = h_px.div_ceil(u64::from(cell.1.max(1))) as u32;
    (c, r.clamp(1, max_rows.max(1)))
}

/// What the terminal said about our placement.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Said {
    Ready,
    Ended,
    Error(String),
    AudioUnavailable,
    Other,
}

/// Replies `ESC _ M i=<id>;<msg> ESC \` in `buf` for our id; consumed bytes are drained.
/// Ctrl+C (3), `q` and Space outside a reply are returned as keys.
pub(crate) fn scan(buf: &mut Vec<u8>) -> (Vec<Said>, Vec<u8>) {
    let mut said = Vec::new();
    let mut keys = Vec::new();
    let mut i = 0;
    while i < buf.len() {
        if buf[i] == 0x1b {
            if buf.len() < i + 3 {
                break;
            }
            if buf[i + 1] == b'_' && buf[i + 2] == b'M' {
                let Some(end) = buf[i..].windows(2).position(|w| w == b"\x1b\\") else {
                    if buf.len() - i > 4096 {
                        i = buf.len();
                    }
                    break;
                };
                let body = &buf[i + 3..i + end];
                if let Some((head, msg)) = body.split_once_byte(b';')
                    && head == format!("i={ID}").as_bytes()
                {
                    said.push(match msg {
                        m if m.starts_with(b"ready") => Said::Ready,
                        b"ended" => Said::Ended,
                        b"audio-unavailable" => Said::AudioUnavailable,
                        m if m.starts_with(b"error=") => {
                            Said::Error(String::from_utf8_lossy(&m[6..]).into_owned())
                        }
                        _ => Said::Other,
                    });
                }
                i += end + 2;
                continue;
            }
            i += 1;
            continue;
        }
        keys.push(buf[i]);
        i += 1;
    }
    buf.drain(..i);
    (said, keys)
}

trait SplitOnce {
    fn split_once_byte(&self, b: u8) -> Option<(&[u8], &[u8])>;
}

impl SplitOnce for [u8] {
    fn split_once_byte(&self, b: u8) -> Option<(&[u8], &[u8])> {
        let i = self.iter().position(|&x| x == b)?;
        Some((&self[..i], &self[i + 1..]))
    }
}

/// The tty, raw while play runs; restored on drop.
struct Tty {
    file: std::fs::File,
    saved: Option<Termios>,
}

impl Tty {
    fn open() -> Option<Tty> {
        let file = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open("/dev/tty")
            .ok()?;
        Some(Tty { file, saved: None })
    }

    /// Cell size in pixels, columns and rows.
    fn cells(&self) -> ((u32, u32), u32, u32) {
        match tcgetwinsize(self.file.as_fd()) {
            Ok(w) if w.ws_col > 0 && w.ws_row > 0 && w.ws_xpixel > 0 && w.ws_ypixel > 0 => (
                (
                    u32::from(w.ws_xpixel) / u32::from(w.ws_col),
                    u32::from(w.ws_ypixel) / u32::from(w.ws_row),
                ),
                u32::from(w.ws_col),
                u32::from(w.ws_row),
            ),
            Ok(w) if w.ws_col > 0 && w.ws_row > 0 => {
                ((10, 20), u32::from(w.ws_col), u32::from(w.ws_row))
            }
            _ => ((10, 20), 80, 24),
        }
    }

    fn raw(&mut self) {
        let Ok(t) = tcgetattr(self.file.as_fd()) else {
            return;
        };
        let mut raw = t.clone();
        raw.local_modes
            .remove(LocalModes::ISIG | LocalModes::ICANON | LocalModes::ECHO);
        if tcsetattr(self.file.as_fd(), OptionalActions::Now, &raw).is_ok() {
            self.saved = Some(t);
        }
    }

    fn write(&mut self, b: &[u8]) -> bool {
        self.file
            .write_all(b)
            .and_then(|()| self.file.flush())
            .is_ok()
    }

    /// Bytes from the tty, waiting up to `until`; `None` on hang-up.
    fn read(&mut self, until: Option<Instant>, out: &mut Vec<u8>) -> Option<()> {
        use rustix::event::{PollFd, PollFlags, Timespec, poll};
        let left = until.map(|u| u.saturating_duration_since(Instant::now()));
        let ts = left.map(|l| Timespec {
            tv_sec: l.as_secs().try_into().unwrap_or(i64::MAX),
            tv_nsec: l.subsec_nanos().into(),
        });
        let mut fds = [PollFd::new(&self.file, PollFlags::IN)];
        match poll(&mut fds, ts.as_ref()) {
            Ok(0) | Err(rustix::io::Errno::INTR) => Some(()),
            Ok(_) => {
                let mut b = [0u8; 1024];
                match self.file.read(&mut b) {
                    Ok(0) | Err(_) => None,
                    Ok(n) => {
                        out.extend_from_slice(&b[..n]);
                        Some(())
                    }
                }
            }
            Err(_) => None,
        }
    }

    /// Ctrl+C (true) or `other` readable or hung up (false); other keys are dropped.
    fn key_or(&mut self, other: std::os::fd::BorrowedFd<'_>) -> bool {
        use rustix::event::{PollFd, PollFlags, poll};
        loop {
            let mut fds = [
                PollFd::new(&self.file, PollFlags::IN),
                PollFd::from_borrowed_fd(other, PollFlags::IN),
            ];
            match poll(&mut fds, None) {
                Ok(_) => {
                    if !fds[1].revents().is_empty() {
                        return false;
                    }
                    if !fds[0].revents().is_empty() {
                        let mut b = [0u8; 64];
                        match self.file.read(&mut b) {
                            Ok(0) | Err(_) => return true,
                            Ok(n) if b[..n].contains(&3) => return true,
                            Ok(_) => {}
                        }
                    }
                }
                Err(rustix::io::Errno::INTR) => {}
                Err(_) => return false,
            }
        }
    }
}

impl Drop for Tty {
    fn drop(&mut self) {
        if let Some(t) = self.saved.take() {
            let _ = tcsetattr(self.file.as_fd(), OptionalActions::Now, &t);
        }
    }
}

/// Connect, say hello and attach `fd`: the client and the token.
fn attach(fd: OwnedFd) -> Result<(crate::ctl::Client, String), String> {
    use apoth_proto::Params;
    use apoth_proto::params::{Empty, Hello};
    let stream = crate::ctl::connect(None).map_err(|crate::ctl::NoInstance(m)| m)?;
    let mut c = crate::ctl::Client::new(stream).map_err(|e| e.kind().to_string())?;
    c.call(Params::Hello(Hello {
        proto: apoth_proto::message::PROTO_VERSION,
        client: "apoth play".into(),
        want: Vec::new(),
    }))
    .map_err(|e| e.message)?;
    let v = c
        .call_fd(Params::MediaAttach(Empty {}), fd.as_fd())
        .map_err(|e| e.message)?;
    drop(fd);
    let token = v
        .get("token")
        .and_then(serde_json::Value::as_str)
        .filter(|t| !t.is_empty() && t.bytes().all(|b| b.is_ascii_digit()))
        .ok_or("the instance sent no token")?
        .to_owned();
    Ok((c, token))
}

fn open_file(path: &str) -> Result<OwnedFd, String> {
    let f = std::fs::File::open(path).map_err(|e| format!("cannot open the file: {}", e.kind()))?;
    let meta = f
        .metadata()
        .map_err(|e| format!("cannot open the file: {}", e.kind()))?;
    if !meta.is_file() {
        return Err("not a regular file".into());
    }
    Ok(f.into())
}

pub(crate) fn run(args: &[String]) -> i32 {
    if args.iter().any(|a| a == "-h" || a == "--help") {
        let _ = std::io::stdout().write_all(HELP.as_bytes());
        return OK;
    }
    let o = match parse(args) {
        Ok(o) => o,
        Err(m) => {
            eprintln!("apoth play: {m}\n\n{HELP}");
            return USAGE;
        }
    };
    let fd = match open_file(&o.file) {
        Ok(f) => f,
        Err(m) => {
            eprintln!("apoth play: {m}");
            return ERROR;
        }
    };
    if o.ext {
        return ext(&o, fd);
    }
    let Some(mut tty) = Tty::open() else {
        eprintln!("apoth play: not in a terminal");
        return ERROR;
    };
    let (c, token) = match attach(fd) {
        Ok(v) => v,
        Err(m) => {
            eprintln!("apoth play: {m}");
            return ERROR;
        }
    };
    // The token is single use: the connection has no further part.
    drop(c);
    let (cell, cols, rows) = tty.cells();
    let size = o
        .size
        .unwrap_or_else(|| default_box(cols, cell, rows.saturating_sub(1)));
    let session = std::process::id();
    // In the foreground the tty goes raw first, so the terminal's replies are never echoed.
    let fg = rustix::termios::tcgetpgrp(tty.file.as_fd()).ok() == Some(rustix::process::getpgrp());
    if fg {
        tty.raw();
    }
    let mut out = format!("\x1b_Ma=b,i={session},p={session}\x1b\\").into_bytes();
    out.extend_from_slice(open_apc(&o, &token, size).as_bytes());
    if !tty.write(&out) {
        return ERROR;
    }
    // In the background this stops play (SIGTTOU) after the open, with the placement up;
    // the session's pidfd still removes it when play is killed.
    if !fg {
        tty.raw();
    }
    let code = wait(&mut tty, &o);
    let end = format!("\x1b_Ma=x,i={ID},q=2\x1b\\\x1b_Ma=e,i={session}\x1b\\");
    tty.write(end.as_bytes());
    drop(tty);
    code
}

/// Play until the end, an error, `q` or Ctrl+C.
fn wait(tty: &mut Tty, o: &Opts) -> i32 {
    let mut buf = Vec::new();
    let mut ready = false;
    let mut paused = o.paused;
    let opened = Instant::now();
    loop {
        let until = (!ready).then(|| opened + OPEN_TIMEOUT);
        if tty.read(until, &mut buf).is_none() {
            return ERROR;
        }
        if !ready && Instant::now() >= opened + OPEN_TIMEOUT {
            eprintln!("\rapoth play: the terminal did not answer (is this Apotheosis?)");
            return ERROR;
        }
        let (said, keys) = scan(&mut buf);
        for s in said {
            match s {
                Said::Ready => ready = true,
                Said::Ended => return OK,
                Said::Error(code) => {
                    eprintln!("\rapoth play: {code}");
                    // T-16 §1.1: 69 (EX_UNAVAILABLE) lets Open with fall back.
                    return if code == "unsupported" { 69 } else { ERROR };
                }
                Said::AudioUnavailable => {
                    eprintln!("\rapoth play: no sound server; playing muted");
                }
                Said::Other => {}
            }
        }
        for k in keys {
            match k {
                3 => return INTERRUPTED,
                b'q' => return OK,
                b' ' => {
                    paused = !paused;
                    let c = if paused { "pause" } else { "play" };
                    if !tty.write(format!("\x1b_Ma=c,i={ID},q=1,{c}\x1b\\").as_bytes()) {
                        return ERROR;
                    }
                }
                _ => {}
            }
        }
    }
}

fn ext(o: &Opts, fd: OwnedFd) -> i32 {
    use apoth_proto::Params;
    use apoth_proto::params::{MediaWindowOpen, MediaWindowWait, MediaWindowWrite};
    let mut tty = Tty::open();
    let cell = tty.as_ref().map_or((10, 20), |t| t.cells().0);
    let (mut c, token) = match attach(fd) {
        Ok(v) => v,
        Err(m) => {
            eprintln!("apoth play: {m}");
            return ERROR;
        }
    };
    let (cols, rows) = o
        .size
        .unwrap_or_else(|| default_box(EXT_COLS, cell, apoth_proto::params::MAX_MEDIA_CELLS - 1));
    let cols = cols.min(apoth_proto::params::MAX_MEDIA_CELLS);
    let rows = rows.min(apoth_proto::params::MAX_MEDIA_CELLS - 1);
    // One row more than the box: the cursor's move below it must not scroll the grid.
    let opened = c.call(Params::MediaWindowOpen(MediaWindowOpen {
        cols,
        rows: rows + 1,
        title: Some("apoth play".into()),
    }));
    let window = match opened {
        Ok(v) => v
            .get("window")
            .and_then(serde_json::Value::as_u64)
            .unwrap_or(0),
        Err(e) => {
            eprintln!("apoth play: {}", e.message);
            return ERROR;
        }
    };
    let bytes = open_apc(o, &token, (cols, rows));
    let w = Params::MediaWindowWrite(MediaWindowWrite {
        window,
        data: crate::icat::b64(bytes.as_bytes()),
    });
    if let Err(e) = c.call(w) {
        eprintln!("apoth play: {}", e.message);
        return ERROR;
    }
    let Ok(wait) = c.send(Params::MediaWindowWait(MediaWindowWait { window })) else {
        return ERROR;
    };
    if c.unbounded().is_err() {
        return ERROR;
    }
    let interrupted = match tty.as_mut() {
        Some(t) => {
            t.raw();
            !c.buffered() && t.key_or(c.stream().as_fd())
        }
        None => false,
    };
    if interrupted {
        // Dropping the connection closes the window, and the window its player.
        drop(c);
        drop(tty);
        return INTERRUPTED;
    }
    let closed = c.reply(wait);
    drop(tty);
    match closed {
        Ok(_) => OK,
        Err(e) => {
            eprintln!("apoth play: {}", e.message);
            ERROR
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(v: &[&str]) -> Vec<String> {
        v.iter().map(|x| x.to_string()).collect()
    }

    #[test]
    fn options_parse_and_reject() {
        let o = parse(&s(&["--size", "40x10", "-l", "--volume", "30", "a.webm"])).unwrap();
        assert_eq!(o.size, Some((40, 10)));
        assert!(o.looping && !o.ext);
        assert_eq!((o.volume, o.file.as_str()), (Some(30), "a.webm"));
        assert!(parse(&s(&["-x", "a"])).unwrap().ext);
        assert!(parse(&s(&[])).is_err());
        assert!(parse(&s(&["a", "b"])).is_err());
        assert!(parse(&s(&["--volume", "101", "a"])).is_err());
        assert!(parse(&s(&["--size", "0x3", "a"])).is_err());
        assert!(parse(&s(&["--bogus", "a"])).is_err());
    }

    #[test]
    fn open_names_the_token_never_the_path() {
        let o = parse(&s(&["--mute", "--paused", "-l", "/home/x/secret.webm"])).unwrap();
        let a = open_apc(&o, "123", (40, 12));
        assert_eq!(
            a,
            "\x1b_Ma=o,i=1,s=f,t=123,c=40,r=12,loop=1,mute=1,pause\x1b\\"
        );
        assert!(!a.contains("secret"));
    }

    #[test]
    fn the_default_box_is_16_by_9_within_the_rows() {
        // 80 cols of 10 px = 800 px wide, 450 px tall, 20-px rows: 23 rows.
        assert_eq!(default_box(80, (10, 20), 100), (80, 23));
        assert_eq!(default_box(80, (10, 20), 10), (80, 10));
    }

    #[test]
    fn replies_and_keys_are_told_apart() {
        let mut b = b"\x1b_Mi=1;ready,w=64,h=36,d=1000\x1b\\ \x1b_Mi=2;ended\x1b\\q\x1b_Mi=1;error=decode\x1b\\\x1b_Mi=1;end"
            .to_vec();
        let (said, keys) = scan(&mut b);
        assert_eq!(said, [Said::Ready, Said::Error("decode".into())]);
        assert_eq!(keys, b" q");
        // The partial reply waits for more.
        assert_eq!(b, b"\x1b_Mi=1;end");
        b.extend_from_slice(b"ed\x1b\\\x03");
        let (said, keys) = scan(&mut b);
        assert_eq!(said, [Said::Ended]);
        assert_eq!(keys, [3]);
        assert!(b.is_empty());
    }
}
