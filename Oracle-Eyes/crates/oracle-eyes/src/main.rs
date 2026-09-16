// SPDX-License-Identifier: AGPL-3.0-only

//! The Oracle-Eyes daemon (COMP-18, ADR 0041).
//!
//! One thread, one loop: wait on the control socket, act on a chord, hold
//! the answer on screen for as long as it takes to read, take it down.
//! Every stage's failure is shown rather than logged away (§2.1).

mod answer;
mod capture;
mod classify;
mod config;
mod frame;
mod hud;
mod ocr;
mod pipeline;
mod redact;

use std::process::ExitCode;
use std::time::{Duration, Instant};

use eclipse_ipc::{Client, EventKind};
use frame::Region;
use hud::{Anchor, Hud};
use pipeline::{Answer, Pipeline};

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("oracle-eyes: {e}");
            ExitCode::FAILURE
        }
    }
}

fn run() -> Result<(), String> {
    let (cfg, errors) = config::load();
    for e in &errors {
        // A bad config line is never fatal: the default it failed to
        // override is still a working daemon.
        eprintln!("oracle-eyes: config: {e}");
    }
    let auto = cfg.auto;
    let fail_ms = cfg.fail_indicator_ms;
    let poll_every = Duration::from_millis(cfg.settle_ms.max(1));

    let mut client = Client::connect().map_err(|e| format!("control socket: {e}"))?;
    client
        .subscribe(&[EventKind::Keybind])
        .map_err(|e| format!("subscribe to keybind: {e}"))?;
    eprintln!(
        "oracle-eyes: connected, listening for annotation chords{}",
        if auto { " (automatic mode on)" } else { "" }
    );

    let mut pipeline = Pipeline::new(cfg);
    let mut hud = Hud::new();
    // When the current annotation has outstayed its welcome.
    let mut until: Option<Instant> = None;
    // Automatic mode walks the outputs one per tick rather than capturing
    // the whole desktop at once; a screen's worth of OCR per tick is the
    // cost we are trying not to pay.
    let mut next_output = 0usize;
    let mut next_tick = Instant::now() + poll_every;

    loop {
        let now = Instant::now();
        let mut wake = until;
        if auto {
            wake = Some(match wake {
                Some(w) => w.min(next_tick),
                None => next_tick,
            });
        }
        let timeout = wake.map(|w| w.saturating_duration_since(now));
        wait_readable(client.as_raw_fd(), timeout)?;

        loop {
            let event = match client.poll_event() {
                Ok(Some(e)) => e,
                Ok(None) => break,
                Err(e) => return Err(format!("control socket: {e}")),
            };
            if event.kind != EventKind::Keybind {
                continue;
            }
            let action = event.data.get("action").and_then(|v| v.as_str());
            let outcome = match action {
                Some("annotation-select") => match region_of(&event.data) {
                    Some(r) => Some(pipeline.select(r)),
                    // The compositor only sends the chord with a region on
                    // commit, so this is a protocol mismatch, not a cancel.
                    None => Some(Err("select event carried no region".to_string())),
                },
                Some("annotation-expand") => Some(pipeline.expand()),
                Some("annotation-dismiss") => {
                    until = None;
                    if let Err(e) = hud.dismiss(&mut client) {
                        eprintln!("oracle-eyes: {e}");
                    }
                    None
                }
                _ => None,
            };
            if let Some(result) = outcome {
                until = present(&mut hud, &mut client, result, fail_ms);
            }
        }

        let now = Instant::now();
        if until.is_some_and(|u| now >= u) {
            until = None;
            if let Err(e) = hud.dismiss(&mut client) {
                eprintln!("oracle-eyes: {e}");
            }
        }

        if auto && now >= next_tick {
            next_tick = now + poll_every;
            let regions = pipeline.output_regions();
            if !regions.is_empty() {
                let region = regions[next_output % regions.len()];
                next_output = next_output.wrapping_add(1);
                // Nothing on screen means nothing to look at: automatic mode
                // never interrupts an answer the user is still reading.
                if until.is_none() {
                    match pipeline.auto(region, ms_since_epoch()) {
                        // The gate declined. That is the common case.
                        Ok(None) => {}
                        Ok(Some(a)) => until = present(&mut hud, &mut client, Ok(a), fail_ms),
                        // Automatic mode is unprompted, so its failures are
                        // logged, not thrown on screen — the user did not ask
                        // for anything and should not be told it failed.
                        Err(e) => eprintln!("oracle-eyes: auto: {e}"),
                    }
                }
            }
        }
    }
}

/// Put an answer — or the reason there isn't one — on screen, and say when
/// it should come down.
fn present(
    hud: &mut Hud,
    client: &mut Client,
    result: Result<Answer, String>,
    fail_ms: u64,
) -> Option<Instant> {
    let (anchor, text, hold) = match result {
        Ok(a) => (a.anchor, a.text, a.hold_ms),
        Err(e) => {
            eprintln!("oracle-eyes: {e}");
            (FAIL_ANCHOR, format!("no answer — {e}"), fail_ms)
        }
    };
    match hud.show(client, anchor, &text) {
        Ok(_) => Some(Instant::now() + Duration::from_millis(hold)),
        Err(e) => {
            // If the compositor will not draw for us there is nowhere left
            // to complain but the journal.
            eprintln!("oracle-eyes: {e}");
            None
        }
    }
}

/// Where a failure with no region of its own goes. A select that failed
/// before it knew where to point still has to say so somewhere.
const FAIL_ANCHOR: Anchor = Anchor {
    x: 64,
    y: 64,
    w: 480,
    h: 96,
};

fn region_of(data: &serde_json::Value) -> Option<Region> {
    let r = data.get("region")?;
    let get = |k: &str| r.get(k)?.as_i64();
    let (x, y, w, h) = (get("x")?, get("y")?, get("w")?, get("h")?);
    if w <= 0 || h <= 0 {
        return None;
    }
    Some(Region {
        x: x as i32,
        y: y as i32,
        w: w as i32,
        h: h as i32,
    })
}

fn ms_since_epoch() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// Block until the socket has something, or `timeout` elapses. `None` waits
/// forever, which is what a daemon with nothing scheduled should do.
fn wait_readable(fd: std::os::fd::RawFd, timeout: Option<Duration>) -> Result<(), String> {
    let ms = match timeout {
        None => -1,
        Some(d) => i32::try_from(d.as_millis()).unwrap_or(i32::MAX),
    };
    let mut pfd = libc::pollfd {
        fd,
        events: libc::POLLIN,
        revents: 0,
    };
    loop {
        let n = unsafe { libc::poll(&mut pfd, 1, ms) };
        if n >= 0 {
            return Ok(());
        }
        let err = std::io::Error::last_os_error();
        if err.kind() == std::io::ErrorKind::Interrupted {
            continue;
        }
        return Err(format!("poll: {err}"));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn a_committed_selection_parses() {
        let r = region_of(&json!({"action":"annotation-select",
                                  "region":{"x":10,"y":20,"w":300,"h":100}}));
        assert_eq!(
            r,
            Some(Region {
                x: 10,
                y: 20,
                w: 300,
                h: 100
            })
        );
    }

    #[test]
    fn a_chord_without_a_region_is_not_a_selection() {
        assert_eq!(region_of(&json!({"action": "annotation-dismiss"})), None);
    }

    #[test]
    fn a_zero_area_region_is_rejected_rather_than_captured() {
        assert_eq!(region_of(&json!({"region":{"x":0,"y":0,"w":0,"h":40}})), None);
    }
}
