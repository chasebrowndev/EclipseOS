// SPDX-License-Identifier: AGPL-3.0-only
//! System usage: CPU, memory, GPU and disk, sampled for the taskbar's
//! `system-usage` widget (ADR 0065).
//!
//! Read from `/proc`, `statvfs` and GPU sysfs on one thread at
//! [`UsageConfig::interval`]. A source the machine does not have is `None`
//! and the widget hides it; it is never reported as zero. Same shape as
//! [`crate::status`]: an `mpsc` feed the GUI drains with [`Handle::try_recv`].
//!
//! A sample is sent only when some value moved by at least
//! [`EMIT_THRESHOLD`] (half a percentage point) or a source appeared or
//! vanished; an unchanged reading is repeated at most every [`HEARTBEAT`].
//! The first reading lands one interval after [`spawn`], because CPU load is
//! a delta between two `/proc/stat` reads.
//!
//! GPU: the busiest of every GPU that can be read — amdgpu sysfs, Intel idle
//! residency, and NVIDIA through NVML loaded at runtime when the driver ships
//! it. A runtime-suspended GPU reads as idle and is never woken. Sources are
//! probed at spawn and on [`Handle::reconfigure`], not every tick; see
//! [`gpu`].

use std::ffi::CString;
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::time::{Duration, Instant};

mod gpu;

/// Smallest change, as a fraction, that is worth a new sample.
const EMIT_THRESHOLD: f32 = 0.005;
/// Longest an unchanged reading goes unrepeated.
const HEARTBEAT: Duration = Duration::from_secs(5);

/// What to sample, and how often. Mirrors `bar.widgets.system-usage.*`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UsageConfig {
    pub interval: Duration,
    /// The filesystem whose fill level `disk` reports.
    pub disk_path: PathBuf,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Update {
    Sample(Sample),
}

/// One reading. Every field is a fraction `0.0..=1.0`; `None` means the
/// source is unavailable on this machine and the widget hides it.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Sample {
    pub cpu: f32,
    pub mem: Option<f32>,
    pub gpu: Option<f32>,
    pub disk: Option<f32>,
}

#[derive(Debug)]
enum Command {
    Reconfigure(UsageConfig),
}

/// The GUI's end of the sampler.
pub struct Handle {
    updates: Receiver<Update>,
    commands: Sender<Command>,
}

impl Handle {
    /// The next sample, if one is waiting. Never blocks.
    pub fn try_recv(&self) -> Option<Update> {
        self.updates.try_recv().ok()
    }

    /// Change the interval or disk path, e.g. after a config reload. Takes
    /// effect at once: the sampler re-probes the GPU and sends a fresh
    /// sample.
    pub fn reconfigure(&self, cfg: UsageConfig) {
        let _ = self.commands.send(Command::Reconfigure(cfg));
    }
}

/// Start sampling. The thread lives until the [`Handle`] is dropped.
pub fn spawn(cfg: UsageConfig) -> Handle {
    let (updates_tx, updates) = mpsc::channel();
    let (commands, commands_rx) = mpsc::channel();
    let _ = std::thread::Builder::new()
        .name("eclipse-usage".into())
        .spawn(move || run(cfg, updates_tx, commands_rx));
    Handle { updates, commands }
}

/// The sampler's state between ticks.
struct Sampler {
    cfg: UsageConfig,
    gpus: gpu::Gpus,
    prev_cpu: Option<CpuTimes>,
    cpu: f32,
    last_sent: Option<(Sample, Instant)>,
}

impl Sampler {
    fn new(cfg: UsageConfig) -> Self {
        Self {
            cfg,
            gpus: gpu::Gpus::probe(Path::new(gpu::SYS), gpu::NVML_LIB.as_ref()),
            prev_cpu: read_to_string("/proc/stat").and_then(|s| parse_cpu(&s)),
            cpu: 0.0,
            last_sent: None,
        }
    }

    fn reconfigure(&mut self, cfg: UsageConfig) {
        self.cfg = cfg;
        self.gpus.reprobe(Path::new(gpu::SYS), gpu::NVML_LIB.as_ref());
    }

    fn sample(&mut self, now: Instant) -> Sample {
        if let Some(now) = read_to_string("/proc/stat").and_then(|s| parse_cpu(&s)) {
            if let Some(prev) = self.prev_cpu {
                if let Some(f) = cpu_fraction(prev, now) {
                    self.cpu = f;
                }
            }
            self.prev_cpu = Some(now);
        }
        Sample {
            cpu: self.cpu,
            mem: read_to_string("/proc/meminfo").and_then(|s| parse_meminfo(&s)),
            gpu: self.gpus.read(now),
            disk: disk_fraction(&self.cfg.disk_path),
        }
    }

    /// The sample to send now, if any. `force` bypasses the change filter.
    fn tick(&mut self, now: Instant, force: bool) -> Option<Sample> {
        let s = self.sample(now);
        let due = match self.last_sent {
            None => true,
            Some((last, at)) => force || changed(&last, &s) || now.duration_since(at) >= HEARTBEAT,
        };
        if due {
            self.last_sent = Some((s, now));
            Some(s)
        } else {
            None
        }
    }
}

fn run(cfg: UsageConfig, updates: Sender<Update>, commands: Receiver<Command>) {
    let mut sampler = Sampler::new(cfg);
    let mut next = Instant::now() + sampler.cfg.interval;
    loop {
        let wait = next.saturating_duration_since(Instant::now());
        let force = match commands.recv_timeout(wait) {
            Ok(Command::Reconfigure(cfg)) => {
                sampler.reconfigure(cfg);
                true
            }
            Err(RecvTimeoutError::Timeout) => false,
            Err(RecvTimeoutError::Disconnected) => return,
        };
        let now = Instant::now();
        next = now + sampler.cfg.interval;
        if let Some(s) = sampler.tick(now, force) {
            if updates.send(Update::Sample(s)).is_err() {
                return;
            }
        }
    }
}

/// Whether `b` differs from `a` enough to be worth sending.
fn changed(a: &Sample, b: &Sample) -> bool {
    fn opt(a: Option<f32>, b: Option<f32>) -> bool {
        match (a, b) {
            (Some(x), Some(y)) => (x - y).abs() >= EMIT_THRESHOLD,
            (None, None) => false,
            _ => true,
        }
    }
    (a.cpu - b.cpu).abs() >= EMIT_THRESHOLD || opt(a.mem, b.mem) || opt(a.gpu, b.gpu) || opt(a.disk, b.disk)
}

fn read_to_string(path: impl AsRef<Path>) -> Option<String> {
    std::fs::read_to_string(path).ok()
}

/// Cumulative jiffies from the aggregate `cpu` line of `/proc/stat`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct CpuTimes {
    busy: u64,
    total: u64,
}

/// The aggregate `cpu` line of `/proc/stat`. Busy is
/// user+nice+system+irq+softirq+steal; idle and iowait count as idle. Guest
/// time is already inside user/nice, so it is not added again.
fn parse_cpu(stat: &str) -> Option<CpuTimes> {
    let line = stat.lines().find(|l| l.starts_with("cpu "))?;
    let mut f = [0u64; 8];
    let mut fields = line.split_ascii_whitespace().skip(1);
    for slot in &mut f[..4] {
        *slot = fields.next()?.parse().ok()?;
    }
    // iowait, irq, softirq and steal are absent on very old kernels.
    for slot in &mut f[4..] {
        match fields.next() {
            Some(v) => *slot = v.parse().ok()?,
            None => break,
        }
    }
    let [user, nice, system, idle, iowait, irq, softirq, steal] = f;
    let busy = user + nice + system + irq + softirq + steal;
    Some(CpuTimes {
        busy,
        total: busy + idle + iowait,
    })
}

/// Busy fraction between two readings; `None` if no time passed or the
/// counters went backwards.
fn cpu_fraction(prev: CpuTimes, now: CpuTimes) -> Option<f32> {
    let total = now.total.checked_sub(prev.total)?;
    let busy = now.busy.checked_sub(prev.busy)?;
    if total == 0 {
        return None;
    }
    Some((busy as f64 / total as f64).clamp(0.0, 1.0) as f32)
}

/// `1 - MemAvailable/MemTotal`; `None` if either is missing or garbage.
fn parse_meminfo(meminfo: &str) -> Option<f32> {
    let field = |key: &str| -> Option<u64> {
        let line = meminfo.lines().find(|l| l.starts_with(key))?;
        let rest = line[key.len()..].strip_prefix(':')?;
        rest.split_ascii_whitespace().next()?.parse().ok()
    };
    let total = field("MemTotal")?;
    let available = field("MemAvailable")?;
    if total == 0 {
        return None;
    }
    Some((1.0 - available as f64 / total as f64).clamp(0.0, 1.0) as f32)
}

/// Used fraction of the filesystem holding `path`, as `df` reports it:
/// used / (used + available to unprivileged users).
#[allow(clippy::useless_conversion, reason = "fsblkcnt_t is not u64 on every target")]
fn disk_fraction(path: &Path) -> Option<f32> {
    let c = CString::new(path.as_os_str().as_bytes()).ok()?;
    let mut st = std::mem::MaybeUninit::<libc::statvfs>::uninit();
    // SAFETY: `c` is a valid NUL-terminated string and `st` is a writable
    // buffer of the right type; statvfs fully initializes it on success.
    let rc = unsafe { libc::statvfs(c.as_ptr(), st.as_mut_ptr()) };
    if rc != 0 {
        return None;
    }
    // SAFETY: rc == 0, so statvfs initialized `st`.
    let st = unsafe { st.assume_init() };
    // fsblkcnt_t is u32 on some 32-bit targets; `from` widens either way.
    let used = u64::from(st.f_blocks).saturating_sub(u64::from(st.f_bfree));
    let denom = used + u64::from(st.f_bavail);
    if denom == 0 {
        return None;
    }
    Some((used as f64 / denom as f64).clamp(0.0, 1.0) as f32)
}

#[cfg(test)]
mod tests {
    use super::*;

    const STAT_A: &str = "cpu  100 10 50 800 40 5 5 0 0 0\n\
cpu0 50 5 25 400 20 2 3 0 0 0\n\
intr 12345\nctxt 999\n";
    const STAT_B: &str = "cpu  160 10 70 880 60 10 10 0 0 0\n\
cpu0 80 5 35 440 30 5 5 0 0 0\n";

    #[test]
    fn stat_delta() {
        let a = parse_cpu(STAT_A).unwrap();
        assert_eq!(
            a,
            CpuTimes {
                busy: 170,
                total: 1010
            }
        );
        let b = parse_cpu(STAT_B).unwrap();
        // busy +90 (user 60, system 20, irq 5, softirq 5); idle +80, iowait +20.
        assert_eq!(cpu_fraction(a, b), Some(90.0 / 190.0));
    }

    #[test]
    fn stat_no_progress_or_backwards() {
        let a = parse_cpu(STAT_A).unwrap();
        assert_eq!(cpu_fraction(a, a), None);
        let b = parse_cpu(STAT_B).unwrap();
        assert_eq!(cpu_fraction(b, a), None);
    }

    #[test]
    fn stat_old_kernel_four_fields() {
        assert_eq!(parse_cpu("cpu 1 2 3 4\n"), Some(CpuTimes { busy: 6, total: 10 }));
    }

    #[test]
    fn stat_garbage() {
        assert_eq!(parse_cpu(""), None);
        assert_eq!(parse_cpu("not a stat file\n"), None);
        assert_eq!(parse_cpu("cpu  1 2 x 4 5\n"), None);
        assert_eq!(parse_cpu("cpu0 1 2 3 4\n"), None);
    }

    #[test]
    fn meminfo() {
        let m = "MemTotal:       16000000 kB\n\
MemFree:         1000000 kB\n\
MemAvailable:    4000000 kB\n\
Buffers:          100000 kB\n";
        assert_eq!(parse_meminfo(m), Some(0.75));
    }

    #[test]
    fn meminfo_missing_available() {
        let m = "MemTotal:       16000000 kB\nMemFree:         1000000 kB\n";
        assert_eq!(parse_meminfo(m), None);
    }

    #[test]
    fn meminfo_garbage() {
        assert_eq!(parse_meminfo(""), None);
        assert_eq!(parse_meminfo("\u{0}\u{1}binary junk"), None);
        assert_eq!(parse_meminfo("MemTotal: lots kB\nMemAvailable: 1 kB\n"), None);
        assert_eq!(parse_meminfo("MemTotal: 0 kB\nMemAvailable: 0 kB\n"), None);
        // A prefix match must not satisfy the key.
        assert_eq!(parse_meminfo("MemTotalX: 10 kB\nMemAvailable: 1 kB\n"), None);
    }

    #[test]
    fn change_filter() {
        let a = Sample {
            cpu: 0.10,
            mem: Some(0.5),
            gpu: None,
            disk: Some(0.3),
        };
        assert!(!changed(&a, &Sample { cpu: 0.104, ..a }));
        assert!(changed(&a, &Sample { cpu: 0.106, ..a }));
        assert!(changed(&a, &Sample { gpu: Some(0.0), ..a }));
        assert!(changed(&a, &Sample { disk: None, ..a }));
    }

    #[test]
    fn disk_of_root_and_missing() {
        let f = disk_fraction(Path::new("/")).unwrap();
        assert!((0.0..=1.0).contains(&f));
        assert_eq!(disk_fraction(Path::new("/no/such/path/here")), None);
    }

    #[test]
    fn spawn_samples_and_reconfigures() {
        let cfg = UsageConfig {
            interval: Duration::from_secs(60),
            disk_path: PathBuf::from("/"),
        };
        let h = spawn(cfg.clone());
        assert_eq!(h.try_recv(), None);
        // Reconfigure applies at once rather than waiting out the interval.
        h.reconfigure(cfg);
        let deadline = Instant::now() + Duration::from_secs(5);
        let got = loop {
            if let Some(u) = h.try_recv() {
                break u;
            }
            assert!(Instant::now() < deadline, "no sample after reconfigure");
            std::thread::sleep(Duration::from_millis(10));
        };
        let Update::Sample(s) = got;
        assert!((0.0..=1.0).contains(&s.cpu));
    }
}
