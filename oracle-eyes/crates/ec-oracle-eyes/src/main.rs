// SPDX-License-Identifier: AGPL-3.0-only

//! The Oracle-Eyes daemon (COMP-18, ADR 0041).
//!
//! One loop: wait on the control socket, act on a chord, hold the answer on
//! screen for as long as it takes to read, take it down. The model call runs
//! on a worker so the loop never goes deaf (see `daemon`). Every stage's
//! failure is shown rather than logged away (§2.1).

mod answer;
mod beacon;
mod capture;
mod choice;
mod classify;
mod config;
mod daemon;
mod fault;
mod focus;
mod frame;
mod hud;
mod logging;
mod logsafe;
mod ocr;
mod pipeline;
mod redact;
#[cfg(test)]
mod tests_loop;

use std::process::ExitCode;
use std::time::{Duration, Instant};

use ec_ipc::{Client, EventKind};
use serde_json::Value;

use beacon::Beacon;
use daemon::Daemon;
use hud::Control;
use pipeline::Pipeline;

fn main() -> ExitCode {
    let debug = config::debug_requested(
        std::env::args().skip(1),
        std::env::var("OE_DEBUG").ok().as_deref(),
    );
    // Held to the end of `main` so the debug log is flushed on every exit.
    let log = logging::init(debug);
    match run(debug, &log) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            tracing::error!("{e}");
            ExitCode::FAILURE
        }
    }
}

/// The control socket, noting whether a call has been made since the event
/// queue was last drained. `Client::call` keeps any event that arrives while
/// it waits for its reply in a queue of its own, which `poll` on the socket
/// cannot see: a chord that landed during a HUD call made after the drain sat
/// there until something else woke the loop — with nothing scheduled, never.
struct Link {
    client: Client,
    called: bool,
}

impl Control for Link {
    fn call(&mut self, method: &str, params: Value) -> Result<Value, String> {
        self.called = true;
        Control::call(&mut self.client, method, params)
    }
}

/// The file config with the compositor's `oracle-eyes` block laid over it.
/// The file is the fallback: an unreachable or refused `get_config` leaves it
/// as it is.
fn effective(base: &config::Config, c: &mut impl Control) -> config::Config {
    let mut cfg = base.clone();
    match c.call("get_config", serde_json::json!({ "file": "abyss" })) {
        Ok(reply) => {
            let (n, errors) = config::apply_compositor(&mut cfg, &reply);
            for e in &errors {
                tracing::warn!("settings: {e}");
            }
            tracing::debug!(applied = n, "settings: compositor block applied");
        }
        Err(e) => tracing::warn!("settings: get_config: {e}, using the file config"),
    }
    cfg
}

fn run(flag_debug: bool, log: &logging::Guard) -> Result<(), String> {
    let (base, errors) = config::load();
    for e in &errors {
        // A bad config line is never fatal: the default it failed to
        // override is still a working daemon.
        tracing::warn!("config: {e}");
    }
    let mut client = Client::connect().map_err(|e| format!("control socket: {e}"))?;
    if let Err(e) = client.subscribe(&[EventKind::Keybind, EventKind::Config]) {
        // An older compositor without the config event: settings then apply
        // at the next start, not live.
        tracing::warn!("subscribe to config: {e}, settings changes apply on restart");
        client
            .subscribe(&[EventKind::Keybind])
            .map_err(|e| format!("subscribe to keybind: {e}"))?;
    }
    let cfg = effective(&base, &mut client);
    tracing::debug!(?cfg, "effective config");
    let (auto, fail_ms, settle_ms) = (cfg.auto, cfg.fail_indicator_ms, cfg.settle_ms);
    let mut debug = flag_debug || cfg.debug;
    log.set_debug(debug);
    tracing::info!(
        auto,
        "connected, listening for annotation chords{}",
        if auto { " (automatic mode on)" } else { "" }
    );
    let mut link = Link {
        client,
        called: false,
    };

    let pipeline = Pipeline::new(cfg);
    let mut daemon = Daemon::new(
        pipeline,
        Beacon::bind(debug),
        auto,
        fail_ms,
        settle_ms,
        Instant::now(),
    );

    loop {
        let timeout = if link.called {
            Some(Duration::ZERO)
        } else {
            daemon.wake_in(Instant::now())
        };
        wait_readable(link.client.as_raw_fd(), timeout)?;
        link.called = false;

        loop {
            let event = match link.client.poll_event() {
                Ok(Some(e)) => e,
                Ok(None) => break,
                Err(e) => return Err(format!("control socket: {e}")),
            };
            if event.kind == EventKind::Keybind {
                daemon.chord(&mut link, &event.data, Instant::now());
            } else if event.kind == EventKind::Config {
                let cfg = effective(&base, &mut link);
                let now_debug = flag_debug || cfg.debug;
                if now_debug != debug {
                    debug = now_debug;
                    log.set_debug(debug);
                    daemon.set_debug(debug);
                }
                tracing::info!("settings changed, reapplied");
                daemon.reconfigure(cfg, Instant::now());
            }
        }
        daemon.step(&mut link, Instant::now());
    }
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
    use super::daemon::region_of;
    use super::frame::Region;
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
        assert_eq!(
            region_of(&json!({"region":{"x":0,"y":0,"w":0,"h":40}})),
            None
        );
    }
}
