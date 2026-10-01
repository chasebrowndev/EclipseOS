// SPDX-License-Identifier: AGPL-3.0-only

//! `ec-fog-bench frames`: scrolling frame times of the real ec-fog-ui over a
//! 10,000-entry folder (FOG §Performance model: 60 fps while scrolling).
//!
//! ec-fogd runs in-process on a private runtime directory; ec-fog-ui is spawned
//! against it with `FOG_UI_BENCH_SCROLL`, scrolls a fixed step per frame and
//! prints its frame intervals as one JSON line. Run twice: the list alone,
//! and with the blurred palette sheet held open over it.
//!
//! Needs a Wayland display. Run it inside a headless compositor (cage with
//! `WLR_BACKENDS=headless`) in CI; without `WAYLAND_DISPLAY` it skips.

use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{bail, Context, Result};
use ec_fog_daemon::{bind, serve, Cache, Daemon, LocalBackend};

const USAGE: &str = "usage: ec-fog-bench frames [-n FRAMES] [--ui PATH] [--json]";
const ENTRIES: usize = 10_000;
/// A frame is late when it missed a 60 Hz vblank: 1.5 frame periods.
const LATE_MS: f64 = 1000.0 / 60.0 * 1.5;
/// ec-fog-ui's startup and warm-up, on top of the timed frames.
const GRACE: Duration = Duration::from_secs(60);

pub fn main(mut args: impl Iterator<Item = String>) -> Result<()> {
    let mut frames = 600usize;
    let mut ui = None;
    let mut json = false;
    while let Some(a) = args.next() {
        match a.as_str() {
            "-n" | "--frames" => {
                frames = args
                    .next()
                    .context(USAGE)?
                    .parse()
                    .context("frames must be a positive integer")?;
            }
            "--ui" => ui = Some(PathBuf::from(args.next().context(USAGE)?)),
            "--json" => json = true,
            "-h" | "--help" => {
                println!("{USAGE}");
                return Ok(());
            }
            _ => bail!("unknown argument {a:?}\n{USAGE}"),
        }
    }
    let Some(display) = display() else {
        println!("ec-fog-bench frames: skipped, no WAYLAND_DISPLAY");
        return Ok(());
    };
    let ui = match ui {
        Some(p) => p,
        None => std::env::current_exe()?
            .parent()
            .context("ec-fog-bench has no directory")?
            .join("ec-fog-ui"),
    };

    let root = tempfile::Builder::new()
        .prefix("ec-fog-bench.")
        .tempdir_in("/tmp")?;
    let dir = root.path().join(format!("d{ENTRIES}"));
    crate::populate(&dir, ENTRIES)?;
    let runtime = root.path().join("run");
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;
    let _guard = rt.enter();
    let daemon = Arc::new(Daemon::new(Box::new(LocalBackend), Cache::default()));
    let server = rt.spawn(serve(bind(&runtime.join("fog/fogd.sock"))?, daemon));

    let mut rows = Vec::new();
    for sheet in [false, true] {
        let mut child = Command::new(&ui)
            .arg(&dir)
            .env("XDG_RUNTIME_DIR", &runtime)
            .env("WAYLAND_DISPLAY", &display)
            .env("FOG_UI_BENCH_SCROLL", frames.to_string())
            .env("FOG_UI_BENCH_SHEET", if sheet { "1" } else { "0" })
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .with_context(|| format!("spawning {}", ui.display()))?;
        let deadline =
            std::time::Instant::now() + GRACE + Duration::from_millis(frames as u64 * 50);
        let status = loop {
            if let Some(s) = child.try_wait()? {
                break s;
            }
            if std::time::Instant::now() > deadline {
                child.kill()?;
                bail!("ec-fog-ui did not finish its run");
            }
            std::thread::sleep(Duration::from_millis(100));
        };
        let mut out = String::new();
        std::io::Read::read_to_string(&mut child.stdout.take().context("no stdout")?, &mut out)?;
        let line = out
            .lines()
            .rev()
            .find(|l| l.starts_with("{\"frames\""))
            .with_context(|| format!("ec-fog-ui printed no report ({status})"))?;
        rows.push(line.to_owned());
    }
    server.abort();

    let mut ok = true;
    for r in &rows {
        let p95 = field(r, "p95_ms").unwrap_or(f64::INFINITY);
        ok &= p95 <= LATE_MS;
    }
    if json {
        println!(
            "{{\"entries\":{ENTRIES},\"late_ms\":{LATE_MS:.2},\"runs\":[{}],\"pass\":{ok}}}",
            rows.join(",")
        );
    } else {
        println!(
            "ec-fog-bench frames: ec-fog-ui scrolling {ENTRIES} entries, {frames} frames per run"
        );
        for r in &rows {
            println!("{r}");
        }
        println!(
            "pass = p95 interval within {LATE_MS:.1} ms (no missed 60 Hz vblank): {}",
            if ok { "pass" } else { "FAIL" }
        );
    }
    if !ok {
        std::process::exit(1);
    }
    Ok(())
}

/// The compositor socket as an absolute path: the child gets its own
/// `XDG_RUNTIME_DIR` for ec-fogd, so a relative name would not resolve.
fn display() -> Option<PathBuf> {
    let d = PathBuf::from(std::env::var_os("WAYLAND_DISPLAY").filter(|d| !d.is_empty())?);
    if d.is_absolute() {
        return Some(d);
    }
    Some(PathBuf::from(std::env::var_os("XDG_RUNTIME_DIR")?).join(d))
}

/// A number field of ec-fog-ui's flat JSON report.
fn field(line: &str, key: &str) -> Option<f64> {
    let rest = &line[line.find(&format!("\"{key}\":"))? + key.len() + 3..];
    rest[..rest.find([',', '}'])?].parse().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_report_fields() {
        let r = "{\"frames\":3,\"sheet\":false,\"p50_ms\":16.00,\"p95_ms\":16.90,\"late\":0}";
        assert_eq!(field(r, "p95_ms"), Some(16.9));
        assert_eq!(field(r, "late"), Some(0.0));
        assert_eq!(field(r, "p99_ms"), None);
    }
}
