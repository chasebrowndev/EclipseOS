// SPDX-License-Identifier: AGPL-3.0-only
//! `apoth icat` (T-12 §5, T-15 §1, T-16 §1 tier A and §1.1): show images in the pane through
//! the kitty graphics protocol. PNG, APNG, GIF, JPEG and WebP decode here, in the client
//! (`apoth-image::formats`, under the same caps the terminal uses); the terminal receives
//! RGBA (zlib-compressed, `f=32,o=z`) or the PNG bytes as they are (`f=100`), in 4096-byte
//! base64 chunks. Animated files become kitty animations (`a=f` frames, `a=a` playback).
//!
//! Lifetime (T-16 §1.1). icat runs in the foreground of the pane. It opens a session
//! (`ESC _ M a=b,i=<pid>`) so the terminal can remove everything it placed if it dies
//! without cleaning up. A still image prints and icat exits at once (`a=e`: the image stays
//! in the scrollback). An animation keeps icat in the foreground while it plays: Ctrl+C
//! (read as a byte: the tty's signal keys are off while icat waits) deletes its placements,
//! restores the tty and exits 130; a finite `--loop` ends the same way with 0 once played.
//!
//! `--ext` (T-16 §1.1, T-12 §2): the same bytes go to a borderless glass window instead,
//! through the control socket (`media.window-open`, `media.window-write`). icat stays in the
//! foreground until the window closes (Escape or `q` in it, or the compositor's close), then
//! exits 0; Ctrl+C in the pane drops the connection, which closes the window, and exits 130.
//! The window belongs to the connection, so a killed icat takes its window with it.
//!
//! Nothing here logs file content or paths.

use std::io::{Read, Write};
use std::os::fd::AsFd;
use std::time::{Duration, Instant};

use apoth_image::Limits;
use apoth_image::formats::{Kind, decode_any, sniff};
use apoth_image::placeholder::{DIACRITICS, PLACEHOLDER};
use rustix::termios::{LocalModes, OptionalActions, Termios, tcgetattr, tcgetwinsize, tcsetattr};

use crate::ctl::{ERROR, OK, USAGE};

/// Exit status after Ctrl+C.
pub(crate) const INTERRUPTED: i32 = 130;
/// Base64 bytes per APC chunk.
const CHUNK: usize = 4096;

pub(crate) const HELP: &str = "\
Usage: apoth icat [OPTIONS] FILE...

Show images in the pane (kitty graphics). PNG, APNG, GIF, JPEG, WebP; `-` reads stdin.

  --place WxH@LxT        draw in a W x H cell box at column L, row T (cursor unchanged)
  -z, --z-index Z        stacking order (negative: under the text)
  -l, --loop N           animation passes: -1 forever (default), 0 first frame only
  --hold                 wait for a key before exiting
  --unicode-placeholder  draw through Unicode placeholder cells (tmux-safe)
  --image-id N           use image id N (1..16777215)
  --clear                delete every image on the screen and exit
  -x, --ext              show in a separate glass window; icat waits until it closes

Animations play while icat runs; Ctrl+C removes them and exits 130.
With --ext, Escape or q closes the window; Ctrl+C in the pane closes it and exits 130.
";

/// Parsed options.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Opts {
    pub(crate) place: Option<(u32, u32, u32, u32)>,
    pub(crate) z: i32,
    pub(crate) loops: i64,
    pub(crate) hold: bool,
    pub(crate) unicode: bool,
    pub(crate) image_id: Option<u32>,
    pub(crate) clear: bool,
    pub(crate) ext: bool,
    pub(crate) files: Vec<String>,
}

fn place_arg(s: &str) -> Option<(u32, u32, u32, u32)> {
    let (wh, lt) = s.split_once('@')?;
    let (w, h) = wh.split_once('x')?;
    let (l, t) = lt.split_once('x')?;
    let n = |v: &str| v.parse::<u32>().ok().filter(|&n| n <= 10_000);
    let (w, h) = (n(w)?, n(h)?);
    (w > 0 && h > 0).then_some((w, h, n(l)?, n(t)?))
}

pub(crate) fn parse(args: &[String]) -> Result<Opts, String> {
    let mut o = Opts {
        loops: -1,
        ..Opts::default()
    };
    let mut it = args.iter();
    while let Some(a) = it.next() {
        let mut val = |name: &str| it.next().cloned().ok_or(format!("{name} needs a value"));
        match a.as_str() {
            "--place" => {
                o.place = Some(place_arg(&val("--place")?).ok_or("--place wants WxH@LxT")?)
            }
            "-z" | "--z-index" => {
                o.z = val("--z-index")?
                    .parse()
                    .map_err(|_| "--z-index wants a number")?;
            }
            "-l" | "--loop" => {
                o.loops = val("--loop")?
                    .parse()
                    .ok()
                    .filter(|&n: &i64| (-1..=1_000_000).contains(&n))
                    .ok_or("--loop wants -1, 0 or a count")?;
            }
            "--hold" => o.hold = true,
            "--unicode-placeholder" => o.unicode = true,
            "--image-id" => {
                o.image_id = Some(
                    val("--image-id")?
                        .parse()
                        .ok()
                        .filter(|&n: &u32| (1..=0xFF_FFFF).contains(&n))
                        .ok_or("--image-id wants 1..16777215")?,
                );
            }
            "--clear" => o.clear = true,
            "-x" | "--ext" => o.ext = true,
            "--" => o.files.extend(it.by_ref().cloned()),
            s if s.starts_with('-') && s != "-" => return Err(format!("unknown option `{s}`")),
            s => o.files.push(s.to_string()),
        }
    }
    if o.files.is_empty() && !o.clear {
        return Err("no image given".into());
    }
    if o.files.len() > 1 && o.image_id.is_some() {
        return Err("--image-id takes one file".into());
    }
    Ok(o)
}

/// One APC with an optional payload, chunked.
pub(crate) fn apc(out: &mut Vec<u8>, keys: &str, payload: &[u8]) {
    let b64 = b64(payload);
    if b64.len() <= CHUNK {
        out.extend_from_slice(format!("\x1b_G{keys};{b64}\x1b\\").as_bytes());
        return;
    }
    let mut chunks = b64.as_bytes().chunks(CHUNK).peekable();
    let mut first = true;
    while let Some(c) = chunks.next() {
        let m = u8::from(chunks.peek().is_some());
        let head = if first {
            format!("{keys},m={m}")
        } else {
            format!("m={m}")
        };
        first = false;
        out.extend_from_slice(b"\x1b_G");
        out.extend_from_slice(head.as_bytes());
        out.push(b';');
        out.extend_from_slice(c);
        out.extend_from_slice(b"\x1b\\");
    }
}

pub(crate) fn b64(b: &[u8]) -> String {
    const A: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(b.len().div_ceil(3) * 4);
    for c in b.chunks(3) {
        let v = u32::from(c[0]) << 16
            | u32::from(c.get(1).copied().unwrap_or(0)) << 8
            | u32::from(c.get(2).copied().unwrap_or(0));
        for i in 0..4 {
            if i <= c.len() {
                out.push(char::from(A[(v >> (18 - 6 * i) & 63) as usize]));
            } else {
                out.push('=');
            }
        }
    }
    out
}

/// The cell box for an `iw` x `ih` image: natural size, scaled down to the pane width.
pub(crate) fn fit(iw: u32, ih: u32, cell: (u32, u32), cols: u32) -> (u32, u32) {
    let (cw, ch) = (u64::from(cell.0.max(1)), u64::from(cell.1.max(1)));
    let (iw, ih) = (u64::from(iw.max(1)), u64::from(ih.max(1)));
    let nat = iw.div_ceil(cw);
    let c = nat.clamp(1, u64::from(cols.max(1)));
    let w_px = (c * cw).min(iw);
    let h_px = (w_px * ih / iw).max(1);
    (c as u32, h_px.div_ceil(ch).max(1) as u32)
}

/// The escape sequences that show one decoded file. Returns whether it animates.
#[allow(clippy::too_many_arguments)]
pub(crate) fn render(
    out: &mut Vec<u8>,
    id: u32,
    raw: &[u8],
    frames: &[(apoth_image::Image, i32)],
    o: &Opts,
    cell: (u32, u32),
    cols: u32,
) -> bool {
    let Some((root, gap0)) = frames.first() else {
        return false;
    };
    let animate = frames.len() > 1 && o.loops != 0;
    let (c, r) = match o.place {
        Some((w, h, ..)) => (w, h),
        None => fit(root.width, root.height, cell, cols),
    };
    if let Some((_, _, l, t)) = o.place {
        out.extend_from_slice(format!("\x1b7\x1b[{};{}H", t + 1, l + 1).as_bytes());
    }
    let place = if o.unicode {
        format!("U=1,c={c},r={r}")
    } else {
        let cm = if o.place.is_some() { ",C=1" } else { "" };
        format!("p=1,c={c},r={r},z={}{cm}", o.z)
    };
    if sniff(raw) == Some(Kind::Png) && !animate {
        apc(out, &format!("a=T,f=100,i={id},q=2,{place}"), raw);
    } else {
        let z = miniz_oxide::deflate::compress_to_vec_zlib(&root.rgba, 1);
        apc(
            out,
            &format!(
                "a=T,f=32,o=z,s={},v={},i={id},q=2,{place}",
                root.width, root.height
            ),
            &z,
        );
    }
    if animate {
        for (img, gap) in &frames[1..] {
            let z = miniz_oxide::deflate::compress_to_vec_zlib(&img.rgba, 1);
            let gap = if *gap <= 0 { -1 } else { *gap };
            apc(
                out,
                &format!(
                    "a=f,f=32,o=z,s={},v={},i={id},z={gap},X=1,q=2",
                    img.width, img.height
                ),
                &z,
            );
        }
        let g = if *gap0 <= 0 { 1 } else { *gap0 };
        let v = if o.loops < 0 { 1 } else { o.loops + 1 };
        out.extend_from_slice(format!("\x1b_Ga=a,i={id},r=1,z={g},q=2\x1b\\").as_bytes());
        out.extend_from_slice(format!("\x1b_Ga=a,i={id},s=3,v={v},q=2\x1b\\").as_bytes());
    }
    if o.unicode {
        let fg = format!(
            "\x1b[38;2;{};{};{}m",
            id >> 16 & 255,
            id >> 8 & 255,
            id & 255
        );
        for row in 0..r.min(DIACRITICS.len() as u32) {
            out.extend_from_slice(fg.as_bytes());
            let mut s = String::new();
            for col in 0..c.min(DIACRITICS.len() as u32) {
                s.push(char::from_u32(PLACEHOLDER).unwrap_or(' '));
                s.push(char::from_u32(DIACRITICS[row as usize]).unwrap_or(' '));
                s.push(char::from_u32(DIACRITICS[col as usize]).unwrap_or(' '));
            }
            out.extend_from_slice(s.as_bytes());
            out.extend_from_slice(b"\x1b[39m\r\n");
        }
    } else if o.place.is_none() {
        // The placement left the cursor on the row below the image.
        out.push(b'\r');
    }
    if o.place.is_some() {
        out.extend_from_slice(b"\x1b8");
    }
    animate
}

/// The tty with its signal keys off while icat waits; restored on drop.
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

    /// Cell size in pixels (from the window size the terminal reports) and columns.
    fn cells(&self) -> ((u32, u32), u32) {
        match tcgetwinsize(self.file.as_fd()) {
            Ok(w) if w.ws_col > 0 && w.ws_row > 0 && w.ws_xpixel > 0 && w.ws_ypixel > 0 => (
                (
                    u32::from(w.ws_xpixel) / u32::from(w.ws_col),
                    u32::from(w.ws_ypixel) / u32::from(w.ws_row),
                ),
                u32::from(w.ws_col),
            ),
            Ok(w) if w.ws_col > 0 => ((10, 20), u32::from(w.ws_col)),
            _ => ((10, 20), 80),
        }
    }

    /// Ctrl+C becomes a byte (no SIGINT), no echo, no line buffering.
    fn keys_raw(&mut self) {
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

    /// Wait up to `until` for a key: `Some(true)` Ctrl+C, `Some(false)` another key,
    /// `None` the time ran out.
    fn key(&mut self, until: Option<Instant>) -> Option<bool> {
        use rustix::event::{PollFd, PollFlags, Timespec, poll};
        loop {
            let left = until.map(|u| u.saturating_duration_since(Instant::now()));
            if left.is_some_and(|l| l.is_zero()) {
                return None;
            }
            let ts = left.map(|l| Timespec {
                tv_sec: l.as_secs().try_into().unwrap_or(i64::MAX),
                tv_nsec: l.subsec_nanos().into(),
            });
            let mut fds = [PollFd::new(&self.file, PollFlags::IN)];
            match poll(&mut fds, ts.as_ref()) {
                Ok(0) => return None,
                Ok(_) => {
                    let mut b = [0u8; 64];
                    match self.file.read(&mut b) {
                        Ok(0) | Err(_) => return Some(true),
                        Ok(n) => return Some(b[..n].contains(&3)),
                    }
                }
                Err(rustix::io::Errno::INTR) => {}
                Err(_) => return Some(true),
            }
        }
    }
}

impl Tty {
    /// Wait until Ctrl+C on the tty (true) or `other` becomes readable or hangs up (false).
    /// Other keys are read and dropped.
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

fn read_file(f: &str) -> std::io::Result<Vec<u8>> {
    const MAX: u64 = 256 << 20;
    let mut buf = Vec::new();
    if f == "-" {
        std::io::stdin().lock().take(MAX).read_to_end(&mut buf)?;
    } else {
        std::fs::File::open(f)?.take(MAX).read_to_end(&mut buf)?;
    }
    Ok(buf)
}

/// How long one pass of an animation takes (ms), for a finite `--loop`.
fn pass_ms(frames: &[(apoth_image::Image, i32)]) -> u64 {
    frames
        .iter()
        .map(|(_, g)| u64::try_from(*g).unwrap_or(0).max(1))
        .sum()
}

pub(crate) fn run(args: &[String]) -> i32 {
    if args.iter().any(|a| a == "-h" || a == "--help") {
        let _ = std::io::stdout().write_all(HELP.as_bytes());
        return OK;
    }
    let o = match parse(args) {
        Ok(o) => o,
        Err(m) => {
            eprintln!("apoth icat: {m}\n\n{HELP}");
            return USAGE;
        }
    };
    if o.ext {
        return ext(&o);
    }
    let Some(mut tty) = Tty::open() else {
        eprintln!("apoth icat: not in a terminal");
        return ERROR;
    };
    if o.clear {
        return if tty.write(b"\x1b_Ga=d,d=A,q=2\x1b\\") {
            OK
        } else {
            ERROR
        };
    }
    let session = std::process::id();
    let (cell, cols) = tty.cells();
    let limits = Limits::default();
    let base = o
        .image_id
        .unwrap_or_else(|| (session.wrapping_mul(2_654_435_761) >> 8 & 0xFF_FF00) | 1);
    let mut out = format!("\x1b_Ma=b,i={session},p={session}\x1b\\").into_bytes();
    let mut animated: Vec<(u32, u64)> = Vec::new();
    let mut failed = false;
    for (n, f) in o.files.iter().enumerate() {
        let raw = match read_file(f) {
            Ok(b) => b,
            Err(e) => {
                eprintln!("apoth icat: cannot read file {}: {}", n + 1, e.kind());
                failed = true;
                continue;
            }
        };
        let decoded = match decode_any(&raw, &limits, limits.animation_quota) {
            Ok(d) => d,
            Err(e) => {
                eprintln!(
                    "apoth icat: file {} is not a supported image ({e:?})",
                    n + 1
                );
                failed = true;
                continue;
            }
        };
        let id = (base + n as u32) & 0xFF_FFFF;
        let id = if id == 0 { 1 } else { id };
        if render(&mut out, id, &raw, &decoded.frames, &o, cell, cols) {
            animated.push((id, pass_ms(&decoded.frames)));
        }
        if !tty.write(&out) {
            return ERROR;
        }
        out.clear();
    }
    let interrupted = if animated.is_empty() && !o.hold {
        false
    } else {
        tty.keys_raw();
        let until = (o.loops > 0 && !o.hold).then(|| {
            let ms = animated.iter().map(|a| a.1).max().unwrap_or(0);
            let passes = u64::try_from(o.loops).unwrap_or(1);
            Instant::now() + Duration::from_millis(ms.saturating_mul(passes))
        });
        tty.key(until) == Some(true)
    };
    // Animations go when icat ends; still images stay (also on Ctrl+C they go: the user
    // interrupted the whole invocation).
    let mut end = Vec::new();
    let gone: Vec<u32> = if interrupted {
        (0..o.files.len() as u32)
            .map(|n| (base + n) & 0xFF_FFFF)
            .collect()
    } else {
        animated.iter().map(|a| a.0).collect()
    };
    for id in gone {
        end.extend_from_slice(format!("\x1b_Ga=d,d=I,i={id},q=2\x1b\\").as_bytes());
    }
    end.extend_from_slice(format!("\x1b_Ma=e,i={session}\x1b\\").as_bytes());
    tty.write(&end);
    drop(tty);
    if interrupted {
        INTERRUPTED
    } else if failed {
        ERROR
    } else {
        OK
    }
}

/// Widest media window, in cells.
const EXT_MAX_COLS: u32 = 160;
/// Raw bytes per `media.window-write` (its base64 stays well inside one wire line).
const EXT_WRITE: usize = 384 * 1024;

/// A file's bytes and its decoded frames (with their delays).
pub(crate) type Decoded = (Vec<u8>, Vec<(apoth_image::Image, i32)>);

/// Place every decoded file under the previous one, at column 0 of a fresh grid.
/// Returns the bytes and the grid (cols, rows) they need.
pub(crate) fn ext_layout(files: &[Decoded], o: &Opts, cell: (u32, u32)) -> (Vec<u8>, (u32, u32)) {
    let mut out = b"\x1b[?25l".to_vec();
    let (mut cols, mut top) = (1u32, 0u32);
    let base = o.image_id.unwrap_or(1);
    for (n, (raw, frames)) in files.iter().enumerate() {
        let Some((root, _)) = frames.first() else {
            continue;
        };
        let (c, r) = fit(root.width, root.height, cell, EXT_MAX_COLS);
        if top + r > apoth_proto::params::MAX_MEDIA_CELLS {
            break;
        }
        let one = Opts {
            place: Some((c, r, 0, top)),
            unicode: false,
            ..o.clone()
        };
        let id = (base + n as u32) & 0xFF_FFFF;
        render(&mut out, id.max(1), raw, frames, &one, cell, c);
        cols = cols.max(c);
        top += r;
    }
    (out, (cols, top.max(1)))
}

fn ext(o: &Opts) -> i32 {
    use apoth_proto::Params;
    use apoth_proto::params::{Hello, MediaWindowOpen, MediaWindowWait, MediaWindowWrite};
    let limits = Limits::default();
    let mut files = Vec::new();
    let mut failed = false;
    for (n, f) in o.files.iter().enumerate() {
        let decoded = read_file(f)
            .map_err(|e| format!("cannot read file {}: {}", n + 1, e.kind()))
            .and_then(|raw| {
                decode_any(&raw, &limits, limits.animation_quota)
                    .map(|d| (raw, d.frames))
                    .map_err(|e| format!("file {} is not a supported image ({e:?})", n + 1))
            });
        match decoded {
            Ok(d) => files.push(d),
            Err(m) => {
                eprintln!("apoth icat: {m}");
                failed = true;
            }
        }
    }
    if files.is_empty() {
        return ERROR;
    }
    let mut tty = Tty::open();
    let cell = tty.as_ref().map_or((10, 20), |t| t.cells().0);
    let (bytes, (cols, rows)) = ext_layout(&files, o, cell);
    let stream = match crate::ctl::connect(None) {
        Ok(s) => s,
        Err(crate::ctl::NoInstance(m)) => {
            eprintln!("apoth icat: {m}");
            return ERROR;
        }
    };
    let Ok(mut c) = crate::ctl::Client::new(stream) else {
        return ERROR;
    };
    let hello = Params::Hello(Hello {
        proto: apoth_proto::message::PROTO_VERSION,
        client: "apoth icat".into(),
        want: Vec::new(),
    });
    let opened = c.call(hello).and_then(|_| {
        c.call(Params::MediaWindowOpen(MediaWindowOpen {
            cols,
            rows,
            title: Some("apoth icat".into()),
        }))
    });
    let window = match opened {
        Ok(v) => v
            .get("window")
            .and_then(serde_json::Value::as_u64)
            .unwrap_or(0),
        Err(e) => {
            eprintln!("apoth icat: {}", e.message);
            return ERROR;
        }
    };
    for chunk in bytes.chunks(EXT_WRITE) {
        let w = Params::MediaWindowWrite(MediaWindowWrite {
            window,
            data: b64(chunk),
        });
        if let Err(e) = c.call(w) {
            eprintln!("apoth icat: {}", e.message);
            return ERROR;
        }
    }
    let Ok(wait) = c.send(Params::MediaWindowWait(MediaWindowWait { window })) else {
        return ERROR;
    };
    if c.unbounded().is_err() {
        return ERROR;
    }
    // Ctrl+C is a byte while we wait; the window's close is the reply.
    let interrupted = match tty.as_mut() {
        Some(t) => {
            t.keys_raw();
            !c.buffered() && t.key_or(c.stream().as_fd())
        }
        None => false,
    };
    if interrupted {
        // Dropping the connection closes the window.
        drop(c);
        drop(tty);
        return INTERRUPTED;
    }
    let closed = c.reply(wait);
    drop(tty);
    match closed {
        Ok(_) if failed => ERROR,
        Ok(_) => OK,
        Err(e) => {
            eprintln!("apoth icat: {}", e.message);
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
        let o = parse(&s(&[
            "--place", "10x5@2x3", "-z", "-1", "--loop", "2", "a.gif",
        ]))
        .unwrap();
        assert_eq!(o.place, Some((10, 5, 2, 3)));
        assert_eq!((o.z, o.loops, o.files.len()), (-1, 2, 1));
        assert!(parse(&s(&[])).is_err());
        assert!(parse(&s(&["--place", "0x5@1x1", "a"])).is_err());
        assert!(parse(&s(&["--loop", "-2", "a"])).is_err());
        assert!(parse(&s(&["--bogus", "a"])).is_err());
        assert!(parse(&s(&["--image-id", "1", "a", "b"])).is_err());
        assert!(parse(&s(&["--clear"])).unwrap().clear);
        assert_eq!(parse(&s(&["-"])).unwrap().files, ["-"]);
    }

    #[test]
    fn big_payloads_chunk_at_4096() {
        let mut out = Vec::new();
        apc(&mut out, "a=t,i=1", &[7u8; 6000]);
        let text = String::from_utf8(out).unwrap();
        let parts: Vec<&str> = text.split("\x1b\\").filter(|p| !p.is_empty()).collect();
        assert_eq!(parts.len(), 2);
        assert!(parts[0].starts_with("\x1b_Ga=t,i=1,m=1;"));
        assert!(parts[1].starts_with("\x1b_Gm=0;"));
        assert_eq!(b64(b"hi!"), "aGkh");
        assert_eq!(b64(b"h"), "aA==");
    }

    #[test]
    fn fit_keeps_natural_size_and_scales_to_the_width() {
        assert_eq!(fit(16, 32, (8, 16), 80), (2, 2));
        assert_eq!(fit(17, 16, (8, 16), 80), (3, 1));
        // 1600 px wide in a 10-column pane of 8-px cells: 80 px wide, 40 px tall.
        assert_eq!(fit(1600, 800, (8, 16), 10), (10, 3));
    }

    #[test]
    fn an_animation_sends_frames_and_starts_looping() {
        let img = |v: u8| apoth_image::Image {
            width: 2,
            height: 1,
            rgba: vec![v; 8],
        };
        let frames = vec![(img(1), 50), (img(2), 70)];
        let o = Opts {
            loops: -1,
            ..Opts::default()
        };
        let mut out = Vec::new();
        assert!(render(&mut out, 9, b"GIF89a", &frames, &o, (8, 16), 80));
        let t = String::from_utf8(out).unwrap();
        assert!(t.contains("a=T,f=32,o=z,s=2,v=1,i=9,q=2,p=1,c=1,r=1,z=0"));
        assert!(t.contains("a=f,f=32,o=z,s=2,v=1,i=9,z=70,X=1"));
        assert!(t.contains("a=a,i=9,r=1,z=50"));
        assert!(t.contains("a=a,i=9,s=3,v=1"));
        // --loop 0: the first frame only, no animation.
        let o = Opts {
            loops: 0,
            ..Opts::default()
        };
        let mut out = Vec::new();
        assert!(!render(&mut out, 9, b"GIF89a", &frames, &o, (8, 16), 80));
        assert!(!String::from_utf8(out).unwrap().contains("a=f"));
    }

    #[test]
    fn ext_stacks_files_in_a_grid_that_fits_them() {
        let img = |w: u32, h: u32| apoth_image::Image {
            width: w,
            height: h,
            rgba: vec![9; (w * h * 4) as usize],
        };
        let files = vec![
            (b"GIF89a".to_vec(), vec![(img(16, 32), 0)]),
            (b"GIF89a".to_vec(), vec![(img(24, 16), 0)]),
        ];
        let (out, grid) = ext_layout(&files, &Opts::default(), (8, 16));
        // 2x2 cells, then 3x1 under it: 3 columns, 3 rows.
        assert_eq!(grid, (3, 3));
        let t = String::from_utf8(out).unwrap();
        assert!(t.starts_with("\x1b[?25l"));
        assert!(t.contains("\x1b7\x1b[1;1H"), "{t:?}");
        assert!(t.contains("\x1b7\x1b[3;1H"), "{t:?}");
        assert!(t.contains("c=2,r=2,z=0,C=1"));
        assert!(t.contains("c=3,r=1,z=0,C=1"));
        // A grid as wide as an image is never wider than the window cap.
        let wide = vec![(b"GIF89a".to_vec(), vec![(img(8000, 8), 0)])];
        assert_eq!(
            ext_layout(&wide, &Opts::default(), (8, 16)).1.0,
            EXT_MAX_COLS
        );
    }
}
