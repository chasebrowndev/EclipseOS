// SPDX-License-Identifier: AGPL-3.0-only

//! Screenshot: one-shot capture to `$XDG_PICTURES_DIR/Screenshots/*.png`.

use std::fs::{self, OpenOptions};
use std::io::BufWriter;
use std::path::{Path, PathBuf};
use std::time::Duration;

use ecl_portal_proto::Source;

use crate::session::CursorMode;
use crate::source::{CaptureError, Capturer, Frame};

#[derive(Debug, thiserror::Error)]
pub enum ScreenshotError {
    #[error(transparent)]
    Capture(#[from] CaptureError),
    #[error("no frame within timeout")]
    Timeout,
    #[error("malformed frame")]
    BadFrame,
    #[error("png: {0}")]
    Png(#[from] png::EncodingError),
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
}

/// Take one frame from `source` (the first frame is unconditional).
pub fn capture_once(
    capturer: &dyn Capturer,
    source: &Source,
    cursor: CursorMode,
    timeout: Duration,
) -> Result<Frame, ScreenshotError> {
    let mut src = capturer.open(source, cursor)?;
    let deadline = std::time::Instant::now() + timeout;
    loop {
        let left = deadline.saturating_duration_since(std::time::Instant::now());
        if left.is_zero() {
            return Err(ScreenshotError::Timeout);
        }
        if let Some(f) = src.next_frame(left)? {
            return Ok(f);
        }
    }
}

/// `$XDG_PICTURES_DIR`, else `~/.config/user-dirs.dirs`, else `$HOME/Pictures`.
pub fn pictures_dir() -> Option<PathBuf> {
    let home = std::env::var_os("HOME").map(PathBuf::from);
    if let Some(d) = std::env::var_os("XDG_PICTURES_DIR").filter(|d| !d.is_empty()) {
        return Some(PathBuf::from(d));
    }
    let cfg = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| home.as_ref().map(|h| h.join(".config")))?;
    if let Ok(t) = fs::read_to_string(cfg.join("user-dirs.dirs")) {
        if let Some(d) = parse_user_dirs(&t, home.as_deref()) {
            return Some(d);
        }
    }
    Some(home?.join("Pictures"))
}

fn parse_user_dirs(text: &str, home: Option<&Path>) -> Option<PathBuf> {
    let line = text
        .lines()
        .map(str::trim)
        .find(|l| l.starts_with("XDG_PICTURES_DIR="))?;
    let v = line.split_once('=')?.1.trim().trim_matches('"');
    match v.strip_prefix("$HOME") {
        Some(rest) => Some(home?.join(rest.trim_start_matches('/'))),
        None if v.starts_with('/') => Some(PathBuf::from(v)),
        None => None,
    }
}

/// `Screenshot-YYYYMMDD-HHMMSS.png` (UTC) for a unix time.
pub fn file_name(unix_secs: u64) -> String {
    let days = (unix_secs / 86_400) as i64;
    let sod = unix_secs % 86_400;
    // Howard Hinnant's civil_from_days.
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!(
        "Screenshot-{y:04}{m:02}{d:02}-{:02}{:02}{:02}.png",
        sod / 3600,
        sod % 3600 / 60,
        sod % 60
    )
}

/// Write `frame` as PNG into `<dir>/Screenshots/`. Never overwrites: an
/// existing name gets a `-N` suffix (`O_EXCL`). Returns the path written.
pub fn save_png(frame: &Frame, dir: &Path, unix_secs: u64) -> Result<PathBuf, ScreenshotError> {
    let rgba = frame.to_rgba().ok_or(ScreenshotError::BadFrame)?;
    let shots = dir.join("Screenshots");
    fs::create_dir_all(&shots)?;
    let base = file_name(unix_secs);
    let stem = base.trim_end_matches(".png");
    for n in 0..1000u32 {
        let name = if n == 0 {
            base.clone()
        } else {
            format!("{stem}-{n}.png")
        };
        let path = shots.join(name);
        match OpenOptions::new().write(true).create_new(true).open(&path) {
            Ok(f) => {
                let mut enc = png::Encoder::new(BufWriter::new(f), frame.width, frame.height);
                enc.set_color(png::ColorType::Rgba);
                enc.set_depth(png::BitDepth::Eight);
                let mut w = enc.write_header()?;
                w.write_image_data(&rgba)?;
                w.finish()?;
                return Ok(path);
            }
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(e.into()),
        }
    }
    Err(std::io::Error::other("too many screenshots with the same timestamp").into())
}

/// `file://` URI with RFC 3986 percent-encoding of everything outside the
/// unreserved set and `/`.
pub fn file_uri(path: &Path) -> String {
    use std::os::unix::ffi::OsStrExt;
    let mut s = String::from("file://");
    for &b in path.as_os_str().as_bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.' | b'_' | b'~' | b'/') {
            s.push(b as char);
        } else {
            s.push_str(&format!("%{b:02X}"));
        }
    }
    s
}

/// Capture + save + URI: the whole non-interactive Screenshot backend.
pub fn take(
    capturer: &dyn Capturer,
    source: &Source,
    cursor: CursorMode,
) -> Result<String, ScreenshotError> {
    let frame = capture_once(capturer, source, cursor, Duration::from_secs(3))?;
    let dir = pictures_dir().ok_or_else(|| std::io::Error::other("no pictures dir"))?;
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    Ok(file_uri(&save_png(&frame, &dir, secs)?))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::source::mock::MockCapturer;

    fn out() -> Source {
        Source::Output {
            name: "DP-1".into(),
            width: 4,
            height: 3,
        }
    }

    #[test]
    fn names_and_uris() {
        assert_eq!(file_name(0), "Screenshot-19700101-000000.png");
        assert_eq!(file_name(1_700_000_000), "Screenshot-20231114-221320.png");
        assert_eq!(
            file_uri(Path::new("/home/a b/Pic#1.png")),
            "file:///home/a%20b/Pic%231.png"
        );
    }

    #[test]
    fn user_dirs_parse() {
        let t = "# c\nXDG_PICTURES_DIR=\"$HOME/Bilder\"\n";
        assert_eq!(
            parse_user_dirs(t, Some(Path::new("/h"))),
            Some(PathBuf::from("/h/Bilder"))
        );
        assert_eq!(
            parse_user_dirs("XDG_PICTURES_DIR=\"/x/p\"", None),
            Some(PathBuf::from("/x/p"))
        );
        assert_eq!(parse_user_dirs("XDG_PICTURES_DIR=\"rel\"", None), None);
    }

    #[test]
    fn saves_decodable_png_without_clobbering() {
        let cap = MockCapturer { frames: 1 };
        let f = capture_once(&cap, &out(), CursorMode::Hidden, Duration::from_secs(1)).unwrap();
        let d = tempfile::tempdir().unwrap();
        let a = save_png(&f, d.path(), 100).unwrap();
        let b = save_png(&f, d.path(), 100).unwrap();
        assert_ne!(a, b);
        assert!(a.starts_with(d.path().join("Screenshots")));
        let dec = png::Decoder::new(std::fs::File::open(&a).unwrap());
        let mut r = dec.read_info().unwrap();
        let mut buf = vec![0; r.output_buffer_size()];
        let info = r.next_frame(&mut buf).unwrap();
        assert_eq!((info.width, info.height), (4, 3));
        // mock pixel B=1,G=2,R=3 -> RGBA 3,2,1,255
        assert_eq!(&buf[..4], &[3, 2, 1, 255]);
    }

    #[test]
    fn timeout_when_no_frames() {
        let cap = MockCapturer { frames: 0 };
        let e = capture_once(&cap, &out(), CursorMode::Hidden, Duration::from_millis(5));
        assert!(matches!(e, Err(ScreenshotError::Timeout)));
    }
}
