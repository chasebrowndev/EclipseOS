// SPDX-License-Identifier: AGPL-3.0-only

//! fog-bench: cold/warm listing latency against the budgets in
//! FOG §Performance model; M0's exit criterion (FOG §Milestones).
//!
//! fogd runs in-process on a tokio runtime and is measured over its real
//! Unix socket with `fog_proto` framing, from sending `ListDir` to the
//! decoded reply.
//!
//! "Cold" means a cold daemon cache, not a cold page cache: dropping the
//! page cache needs root, which Fog never has.
//!
//! TODO(FOG §Performance model, "Benchmarks in CI"): frame times while
//! scrolling, once fog-ui exists.

use std::fs;
use std::os::unix::ffi::OsStrExt;
use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{bail, ensure, Context, Result};
use fog_daemon::{bind, serve, Daemon};
use fog_proto::{read_frame, write_frame, Reply, Request};
use tokio::net::UnixStream;

const SIZES: [usize; 3] = [1_000, 10_000, 100_000];
/// One entry in this many is a directory.
const DIR_EVERY: usize = 50;
const FRAME: Duration = Duration::from_micros(16_700);
const MS50: Duration = Duration::from_millis(50);
const S1: Duration = Duration::from_secs(1);

const USAGE: &str = "usage: fog-bench [-n ITERATIONS] [--json]";

#[derive(Clone, Copy, PartialEq)]
enum Metric {
    ColdFirst,
    ColdComplete,
    WarmFirst,
}

impl Metric {
    fn name(self) -> &'static str {
        match self {
            Metric::ColdFirst => "cold_first",
            Metric::ColdComplete => "cold_complete",
            Metric::WarmFirst => "warm_first",
        }
    }
}

struct Case {
    entries: usize,
    metric: Metric,
    /// Sorted ascending.
    samples: Vec<Duration>,
    budget: Option<Duration>,
}

impl Case {
    fn pct(&self, p: f64) -> Duration {
        let i = ((self.samples.len() - 1) as f64 * p).round() as usize;
        self.samples[i]
    }

    fn median(&self) -> Duration {
        self.pct(0.5)
    }

    fn pass(&self) -> Option<bool> {
        self.budget.map(|b| self.median() <= b)
    }
}

/// Budgets, checked against medians (FOG §Performance model, latency table).
fn budget(entries: usize, metric: Metric) -> Option<Duration> {
    match (entries, metric) {
        (1_000, Metric::ColdComplete) => Some(MS50),
        (10_000 | 100_000, Metric::ColdFirst) => Some(MS50),
        (100_000, Metric::ColdComplete) => Some(S1),
        (1_000 | 10_000, Metric::WarmFirst) => Some(FRAME),
        _ => None,
    }
}

fn main() -> Result<()> {
    let mut iters = 20usize;
    let mut json = false;
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        match a.as_str() {
            "-n" | "--iterations" => {
                iters = args
                    .next()
                    .context(USAGE)?
                    .parse()
                    .context("iterations must be a positive integer")?;
                ensure!(iters > 0, "iterations must be positive");
            }
            "--json" => json = true,
            "-h" | "--help" => {
                println!("{USAGE}");
                return Ok(());
            }
            _ => bail!("unknown argument {a:?}\n{USAGE}"),
        }
    }

    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;
    let cases = rt.block_on(run(iters))?;

    if json {
        print_json(&cases, iters);
    } else {
        print_table(&cases, iters);
    }
    if cases.iter().any(|c| c.pass() == Some(false)) {
        std::process::exit(1);
    }
    Ok(())
}

async fn run(iters: usize) -> Result<Vec<Case>> {
    // Under /tmp so the socket path fits in sun_path (108 bytes).
    let root = tempfile::Builder::new()
        .prefix("fog-bench.")
        .tempdir_in("/tmp")?;
    let sock = root.path().join("fogd.sock");
    let daemon = Arc::new(Daemon::local());
    let server = tokio::spawn(serve(bind(&sock)?, daemon.clone()));
    let mut conn = UnixStream::connect(&sock).await?;

    let mut cases = Vec::new();
    for n in SIZES {
        let dir = root.path().join(format!("d{n}"));
        populate(&dir, n)?;
        let raw = dir.as_os_str().as_bytes().to_vec();

        let mut first = Vec::with_capacity(iters);
        let mut complete = Vec::with_capacity(iters);
        // One untimed pass first: warms the page and dentry caches.
        for i in 0..=iters {
            let (f, c) = list_cold(&mut conn, &daemon, &raw, n).await?;
            if i > 0 {
                first.push(f);
                complete.push(c);
            }
        }

        // The last cold pass left the listing cached.
        let settle = complete.iter().max().copied().unwrap_or_default() * 2;
        let mut warm = Vec::with_capacity(iters);
        for _ in 0..iters {
            warm.push(list_warm(&mut conn, &raw, n).await?);
            // A cached ListDir rescans in the background and replies only if
            // something changed. Nothing does, so no reply marks the end of
            // the rescan: give it time to finish before the next sample.
            tokio::time::sleep(settle).await;
        }
        daemon.cache().remove(&raw);

        for (metric, mut samples) in [
            (Metric::ColdFirst, first),
            (Metric::ColdComplete, complete),
            (Metric::WarmFirst, warm),
        ] {
            samples.sort_unstable();
            cases.push(Case {
                entries: n,
                metric,
                samples,
                budget: budget(n, metric),
            });
        }
    }
    server.abort();
    Ok(cases)
}

/// `n` empty entries named `file{i}`, one in [`DIR_EVERY`] a directory.
fn populate(dir: &Path, n: usize) -> Result<()> {
    fs::create_dir(dir)?;
    for i in 0..n {
        let p = dir.join(format!("file{i}"));
        if i % DIR_EVERY == 0 {
            fs::create_dir(&p)?;
        } else {
            fs::File::create(&p)?;
        }
    }
    Ok(())
}

/// Wait until the listing lands in the cache: the daemon inserts it only
/// after sending the completing reply.
async fn wait_cached(daemon: &Daemon, raw: &[u8]) {
    while daemon.cache().peek(raw).is_none() {
        tokio::time::sleep(Duration::from_micros(100)).await;
    }
}

async fn recv(conn: &mut UnixStream) -> Result<Reply> {
    match read_frame(conn).await? {
        Some(Reply::Error { errno, .. }) => bail!("fogd error: errno {errno}"),
        Some(r) => Ok(r),
        None => bail!("fogd closed the connection"),
    }
}

/// Cold daemon cache: (time to first snapshot, time to complete listing).
async fn list_cold(
    conn: &mut UnixStream,
    daemon: &Daemon,
    raw: &[u8],
    n: usize,
) -> Result<(Duration, Duration)> {
    daemon.cache().remove(raw);
    let t0 = Instant::now();
    write_frame(conn, &Request::ListDir { path: raw.to_vec() }).await?;
    let Reply::DirSnapshot {
        entries,
        order,
        complete,
        ..
    } = recv(conn).await?
    else {
        bail!("expected DirSnapshot first");
    };
    let first = t0.elapsed();
    ensure!(
        order.len() == entries.len(),
        "order/entries length mismatch"
    );
    let mut total = entries.len();
    let mut done = complete;
    while !done {
        let Reply::DirDiff {
            added,
            order,
            complete,
            ..
        } = recv(conn).await?
        else {
            bail!("expected DirDiff");
        };
        total += added.len();
        ensure!(order.len() == total, "order/entries length mismatch");
        done = complete;
    }
    let all = t0.elapsed();
    ensure!(total == n, "listed {total} entries, expected {n}");
    wait_cached(daemon, raw).await;
    Ok((first, all))
}

/// Warm daemon cache: time to the cached snapshot.
async fn list_warm(conn: &mut UnixStream, raw: &[u8], n: usize) -> Result<Duration> {
    let t0 = Instant::now();
    write_frame(conn, &Request::ListDir { path: raw.to_vec() }).await?;
    let Reply::DirSnapshot {
        entries, complete, ..
    } = recv(conn).await?
    else {
        bail!("expected DirSnapshot");
    };
    let t = t0.elapsed();
    ensure!(
        complete && entries.len() == n,
        "cached snapshot incomplete ({} of {n})",
        entries.len()
    );
    Ok(t)
}

fn ms(d: Duration) -> f64 {
    d.as_secs_f64() * 1e3
}

fn print_table(cases: &[Case], iters: usize) {
    println!("fog-bench: ListDir over fogd.sock, {iters} iterations, pass/fail on medians");
    println!("cold = cold fogd cache, warm page cache (dropping caches needs root)");
    println!(
        "{:>7}  {:<13} {:>9} {:>9} {:>9}  result",
        "entries", "metric", "median", "p95", "budget"
    );
    for c in cases {
        let budget = c
            .budget
            .map_or_else(|| "-".to_string(), |b| format!("{:.1}", ms(b)));
        let result = match c.pass() {
            Some(true) => "pass",
            Some(false) => "FAIL",
            None => "-",
        };
        println!(
            "{:>7}  {:<13} {:>9.2} {:>9.2} {:>9}  {result}",
            c.entries,
            c.metric.name(),
            ms(c.median()),
            ms(c.pct(0.95)),
            budget,
        );
    }
    println!("times in ms; scrolling frame times: TODO (FOG §Performance model)");
}

fn print_json(cases: &[Case], iters: usize) {
    let rows: Vec<String> = cases
        .iter()
        .map(|c| {
            let budget = c
                .budget
                .map_or_else(|| "null".to_string(), |b| format!("{:.3}", ms(b)));
            let pass = c
                .pass()
                .map_or_else(|| "null".to_string(), |p| p.to_string());
            format!(
                "{{\"entries\":{},\"metric\":\"{}\",\"median_ms\":{:.3},\"p95_ms\":{:.3},\"budget_ms\":{budget},\"pass\":{pass}}}",
                c.entries,
                c.metric.name(),
                ms(c.median()),
                ms(c.pct(0.95)),
            )
        })
        .collect();
    println!(
        "{{\"iterations\":{iters},\"cold\":\"daemon cache\",\"cases\":[{}]}}",
        rows.join(",")
    );
}
