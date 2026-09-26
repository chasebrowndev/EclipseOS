// SPDX-License-Identifier: AGPL-3.0-only
//! One thread per running widget: the exec loop and the stream supervisor.
//!
//! A worker owns its child outright: it spawns it (so the child's
//! parent-death signal is tied to this thread), kills its group and reaps it
//! before the thread exits. Each child's stdout is drained by a short-lived
//! reader thread that reports back on the worker's own channel, tagged with
//! the run's generation so a late line from a killed run is dropped.

use std::process::{Child, ExitStatus};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use super::{child, parse, Output, Update};

/// The runner's clocks. Tests shrink them; [`Timing::DEFAULT`] is ADR 0065's.
#[derive(Debug, Clone, Copy)]
pub(super) struct Timing {
    /// The longest one exec run may take (further capped by its interval).
    pub exec_cap: Duration,
    /// The first stream restart delay; doubles per failure.
    pub backoff_min: Duration,
    pub backoff_max: Duration,
    /// A stream run that lasted this long with output resets the backoff.
    pub healthy: Duration,
    /// How often a waiting worker checks its child.
    pub tick: Duration,
}

impl Timing {
    pub(super) const DEFAULT: Timing = Timing {
        exec_cap: Duration::from_secs(10),
        backoff_min: Duration::from_secs(1),
        backoff_max: Duration::from_secs(60),
        healthy: Duration::from_secs(60),
        tick: Duration::from_millis(20),
    };
}

/// What a worker runs: the runner-relevant part of a spec.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum Job {
    Exec { argv: Vec<String>, interval: Duration },
    Stream { argv: Vec<String> },
}

enum Msg {
    Stop,
    /// One stdout line of stream run `gen`.
    Line {
        gen: u64,
        raw: Vec<u8>,
    },
    /// Stream run `gen`'s stdout closed.
    Eof {
        gen: u64,
    },
    /// Exec run `gen`'s stdout closed; its last non-blank line.
    ExecDone {
        gen: u64,
        last: Option<Vec<u8>>,
    },
}

/// The runner's end of a worker. Dropping it stops the worker, which kills
/// and reaps its child before the join returns.
pub(super) struct Worker {
    pub name: String,
    pub job: Job,
    tx: Sender<Msg>,
    thread: Option<JoinHandle<()>>,
}

impl Worker {
    pub(super) fn start(name: String, job: Job, updates: Sender<Update>, t: Timing) -> Worker {
        let (tx, rx) = mpsc::channel();
        let ctx = Ctx {
            name: name.clone(),
            updates,
            tx: tx.clone(),
            rx,
            t,
        };
        let job2 = job.clone();
        let thread = std::thread::Builder::new()
            .name("eclipse-custom-widget".into())
            .spawn(move || match job2 {
                Job::Exec { argv, interval } => ctx.exec_loop(&argv, interval),
                Job::Stream { argv } => ctx.stream_loop(&argv),
            });
        let thread = match thread {
            Ok(h) => Some(h),
            Err(_) => {
                let _ = tx.send(Msg::Stop);
                None
            }
        };
        Worker {
            name,
            job,
            tx,
            thread,
        }
    }

    /// Asks the worker to stop without waiting; the drop then joins. Lets the
    /// runner stop many workers in parallel.
    pub(super) fn signal_stop(&self) {
        let _ = self.tx.send(Msg::Stop);
    }
}

impl Drop for Worker {
    fn drop(&mut self) {
        let _ = self.tx.send(Msg::Stop);
        if let Some(h) = self.thread.take() {
            let _ = h.join();
        }
    }
}

struct Ctx {
    name: String,
    updates: Sender<Update>,
    /// Handed to reader threads.
    tx: Sender<Msg>,
    rx: Receiver<Msg>,
    t: Timing,
}

/// Whether a wait ended normally or the worker was told to stop.
enum Flow<T> {
    Go(T),
    Stop,
}

impl Ctx {
    fn output(&self, out: Output) {
        let _ = self.updates.send(Update::Output {
            name: self.name.clone(),
            out,
        });
    }

    fn failed(&self, reason: String) {
        let _ = self.updates.send(Update::Failed {
            name: self.name.clone(),
            reason,
        });
    }

    fn line(&self, raw: &[u8]) {
        match parse::parse_line(raw) {
            Ok(out) => self.output(out),
            Err(reason) => self.failed(reason.into()),
        }
    }

    /// Waits until `until`, dropping stale reader messages. `Stop` on a stop
    /// request.
    fn sleep_until(&self, until: Instant) -> Flow<()> {
        loop {
            let now = Instant::now();
            if now >= until {
                return Flow::Go(());
            }
            match self.rx.recv_timeout(until - now) {
                Ok(Msg::Stop) | Err(RecvTimeoutError::Disconnected) => return Flow::Stop,
                Ok(_) => {}
                Err(RecvTimeoutError::Timeout) => return Flow::Go(()),
            }
        }
    }

    // ---- exec -----------------------------------------------------------

    fn exec_loop(&self, argv: &[String], interval: Duration) {
        let timeout = interval.min(self.t.exec_cap);
        let mut gen = 0u64;
        loop {
            gen += 1;
            let start = Instant::now();
            match self.exec_once(argv, timeout, gen) {
                Flow::Stop => return,
                Flow::Go(Ok(out)) => self.output(out),
                Flow::Go(Err(reason)) => self.failed(reason),
            }
            // Runs never overlap: the next starts one interval after this
            // one started, or at once if this one ran that long.
            if let Flow::Stop = self.sleep_until(start + interval) {
                return;
            }
        }
    }

    fn exec_once(&self, argv: &[String], timeout: Duration, gen: u64) -> Flow<Result<Output, String>> {
        let deadline = Instant::now() + timeout;
        let mut proc = match child::spawn(argv, true) {
            Ok(c) => c,
            Err(e) => return Flow::Go(Err(format!("could not start: {e}"))),
        };
        let Some(stdout) = proc.stdout.take() else {
            child::kill_and_reap(&mut proc);
            return Flow::Go(Err("no stdout".into()));
        };
        let tx = self.tx.clone();
        let reader = std::thread::Builder::new()
            .name("eclipse-custom-read".into())
            .spawn(move || {
                let mut last: Option<Vec<u8>> = None;
                let _ = parse::read_lines(stdout, |l| {
                    if !parse::is_blank(l) {
                        let buf = last.get_or_insert_with(Vec::new);
                        buf.clear();
                        buf.extend_from_slice(l);
                    }
                });
                let _ = tx.send(Msg::ExecDone { gen, last });
            });
        if reader.is_err() {
            child::kill_and_reap(&mut proc);
            return Flow::Go(Err("could not start a reader".into()));
        }

        let mut status: Option<ExitStatus> = None;
        let mut done: Option<Option<Vec<u8>>> = None;
        loop {
            if status.is_none() {
                if let Ok(Some(s)) = proc.try_wait() {
                    status = Some(s);
                }
            }
            if let (Some(s), Some(last)) = (status, done.as_ref()) {
                if !s.success() {
                    return Flow::Go(Err(describe(s)));
                }
                return Flow::Go(match last {
                    Some(raw) => parse::parse_line(raw).map_err(String::from),
                    None => Ok(Output {
                        text: String::new(),
                        detail: Vec::new(),
                        tooltip: None,
                        state: None,
                    }),
                });
            }
            let now = Instant::now();
            if now >= deadline {
                // Kill the group even if the leader has exited: a background
                // job holding stdout open is what kept us waiting.
                reap_or_kill(&mut proc, status);
                return Flow::Go(Err(format!("timed out after {} ms", timeout.as_millis())));
            }
            match self.rx.recv_timeout((deadline - now).min(self.t.tick)) {
                Ok(Msg::Stop) | Err(RecvTimeoutError::Disconnected) => {
                    reap_or_kill(&mut proc, status);
                    return Flow::Stop;
                }
                Ok(Msg::ExecDone { gen: g, last }) if g == gen => done = Some(last),
                Ok(_) | Err(RecvTimeoutError::Timeout) => {}
            }
        }
    }

    // ---- stream ---------------------------------------------------------

    fn stream_loop(&self, argv: &[String]) {
        let mut backoff = self.t.backoff_min;
        let mut gen = 0u64;
        loop {
            gen += 1;
            let started = Instant::now();
            let reason = match self.stream_once(argv, gen) {
                Flow::Stop => return,
                Flow::Go(Ok((reason, got_output))) => {
                    if got_output && started.elapsed() >= self.t.healthy {
                        backoff = self.t.backoff_min;
                    }
                    reason
                }
                Flow::Go(Err(reason)) => reason,
            };
            self.failed(reason);
            if let Flow::Stop = self.sleep_until(Instant::now() + backoff) {
                return;
            }
            backoff = (backoff * 2).min(self.t.backoff_max);
        }
    }

    /// One stream run, to its exit. `Ok((reason, got_output))` once it has
    /// exited; `Err` if it never started.
    fn stream_once(&self, argv: &[String], gen: u64) -> Flow<Result<(String, bool), String>> {
        let mut proc = match child::spawn(argv, true) {
            Ok(c) => c,
            Err(e) => return Flow::Go(Err(format!("could not start: {e}"))),
        };
        let Some(stdout) = proc.stdout.take() else {
            child::kill_and_reap(&mut proc);
            return Flow::Go(Err("no stdout".into()));
        };
        let tx = self.tx.clone();
        let reader = std::thread::Builder::new()
            .name("eclipse-custom-read".into())
            .spawn(move || {
                let _ = parse::read_lines(stdout, |l| {
                    let _ = tx.send(Msg::Line { gen, raw: l.to_vec() });
                });
                let _ = tx.send(Msg::Eof { gen });
            });
        if reader.is_err() {
            child::kill_and_reap(&mut proc);
            return Flow::Go(Err("could not start a reader".into()));
        }

        let mut got_output = false;
        let mut eof = false;
        let status = loop {
            match self.rx.recv_timeout(self.t.tick) {
                Ok(Msg::Stop) | Err(RecvTimeoutError::Disconnected) => {
                    child::kill_and_reap(&mut proc);
                    return Flow::Stop;
                }
                Ok(Msg::Line { gen: g, raw }) if g == gen => {
                    got_output = true;
                    self.line(&raw);
                }
                Ok(Msg::Eof { gen: g }) if g == gen => eof = true,
                Ok(_) | Err(RecvTimeoutError::Timeout) => {}
            }
            if let Ok(Some(s)) = proc.try_wait() {
                break s;
            }
        };
        // The leader is gone; take any stragglers in its group with it, then
        // forward what it printed last, briefly, until its stdout closes.
        child::kill_group(&proc);
        let grace = Instant::now() + self.t.tick * 10;
        while !eof {
            let now = Instant::now();
            if now >= grace {
                break;
            }
            match self.rx.recv_timeout(grace - now) {
                Ok(Msg::Stop) | Err(RecvTimeoutError::Disconnected) => return Flow::Stop,
                Ok(Msg::Line { gen: g, raw }) if g == gen => {
                    got_output = true;
                    self.line(&raw);
                }
                Ok(Msg::Eof { gen: g }) if g == gen => eof = true,
                Ok(_) => {}
                Err(RecvTimeoutError::Timeout) => break,
            }
        }
        Flow::Go(Ok((describe(status), got_output)))
    }
}

/// Kills `proc`'s group and reaps it unless `status` says it is reaped.
fn reap_or_kill(proc: &mut Child, status: Option<ExitStatus>) {
    if status.is_some() {
        child::kill_group(proc);
    } else {
        child::kill_and_reap(proc);
    }
}

/// Our words for how a command ended. Never includes its output.
fn describe(s: ExitStatus) -> String {
    use std::os::unix::process::ExitStatusExt;
    match (s.code(), s.signal()) {
        (Some(c), _) => format!("exited with status {c}"),
        (None, Some(sig)) => format!("killed by signal {sig}"),
        (None, None) => "exited".into(),
    }
}
