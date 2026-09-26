// SPDX-License-Identifier: AGPL-3.0-only
//! GPU busyness for [`super::Sample::gpu`]: the busiest of every GPU that can
//! be read (ADR 0065).
//!
//! Sources, probed per DRM card and per PCI device at spawn and on
//! reconfigure, never per tick:
//!
//! - amdgpu: `device/gpu_busy_percent`.
//! - Intel: i915 and xe expose engine busyness only through the perf PMU or
//!   DRM fdinfo on an open client fd, neither of which the sampler takes. What
//!   they do expose to anyone is idle residency (i915 `rc6_residency_ms`, xe
//!   `gtidle/idle_residency_ms`); busy is read as `1 - idle/wall` over a tick.
//!   It is a proxy, not engine time.
//! - NVIDIA (proprietary driver only): NVML, `dlopen`ed from
//!   `libnvidia-ml.so.1` by `nvml-wrapper`. Never linked. A missing library
//!   or a failed `nvmlInit` (no driver, nouveau) leaves NVIDIA out, silently.
//!
//! A GPU whose PCI device is runtime-suspended reads as idle (`0.0`) without
//! being touched: a query would resume it, and on a hybrid laptop that keeps
//! the dGPU awake and drains the battery. For the same reason NVML is not
//! even initialized while every NVIDIA GPU sleeps; it is initialized on the
//! first tick one of them is awake, once, and kept until the next probe.

use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};
use std::time::Instant;

use nvml_wrapper::{Device, Nvml};
use nvml_wrapper_sys::bindings::nvmlDevice_t;

/// sysfs root in production; tests pass a tempdir.
pub(super) const SYS: &str = "/sys";
/// NVML's soname, as the driver installs it.
pub(super) const NVML_LIB: &str = "libnvidia-ml.so.1";

/// Every GPU source found by the last probe.
pub(super) struct Gpus {
    sysfs: Vec<Source>,
    nvidia: Nvidia,
}

/// One GPU read from sysfs.
struct Source {
    /// The PCI device's `power/runtime_status`.
    power: PathBuf,
    kind: Kind,
}

enum Kind {
    /// amdgpu `gpu_busy_percent`.
    Busy(PathBuf),
    /// A cumulative idle-residency counter in ms (Intel).
    Idle {
        counter: PathBuf,
        /// The last reading and when it was taken.
        prev: Option<(u64, Instant)>,
        /// Repeated when two reads land too close together to divide.
        last: f32,
    },
}

/// NVML device handles, each with its PCI device's `power/runtime_status`
/// (`None` if the bus id was unparseable).
type NvDevices = Vec<(nvmlDevice_t, Option<PathBuf>)>;

enum Nvidia {
    /// No GPU driven by `nvidia`, or NVML would not load or init.
    Absent,
    /// NVIDIA GPUs exist but all slept at probe: NVML not yet initialized.
    Pending {
        /// `power/runtime_status` of each NVIDIA PCI device.
        power: Vec<PathBuf>,
        pci: PathBuf,
        lib: OsString,
    },
    Ready {
        /// Handles from `nvml`.
        devices: NvDevices,
        /// Boxed: the loaded symbol table is large.
        nvml: Box<Nvml>,
    },
}

impl Gpus {
    /// Find every GPU source under `sys`. NVML is loaded from `lib` only if a
    /// PCI GPU bound to `nvidia` exists and at least one is awake.
    pub(super) fn probe(sys: &Path, lib: &OsStr) -> Self {
        let now = Instant::now();
        let mut sysfs = probe_cards(&sys.join("class/drm"));
        for s in &mut sysfs {
            if let Kind::Idle { counter, prev, .. } = &mut s.kind {
                if !asleep(&s.power) {
                    *prev = read_u64(counter).map(|v| (v, now));
                }
            }
        }
        let pci = sys.join("bus/pci/devices");
        let power = nvidia_devices(&pci);
        let mut nvidia = if power.is_empty() {
            Nvidia::Absent
        } else {
            Nvidia::Pending {
                power,
                pci,
                lib: lib.to_owned(),
            }
        };
        nvidia.init_if_awake();
        Self { sysfs, nvidia }
    }

    /// Drop the old sources (shutting NVML down) and probe again. Idle
    /// counters keep their last reading so the next tick still has a delta.
    pub(super) fn reprobe(&mut self, sys: &Path, lib: &OsStr) {
        let seeds: Vec<(PathBuf, (u64, Instant), f32)> = self
            .sysfs
            .drain(..)
            .filter_map(|s| match s.kind {
                Kind::Idle {
                    counter,
                    prev: Some(p),
                    last,
                } => Some((counter, p, last)),
                _ => None,
            })
            .collect();
        self.nvidia = Nvidia::Absent;
        *self = Self::probe(sys, lib);
        for s in &mut self.sysfs {
            if let Kind::Idle { counter, prev, last } = &mut s.kind {
                if let Some((_, p, l)) = seeds.iter().find(|(c, _, _)| c == counter) {
                    *prev = Some(*p);
                    *last = *l;
                }
            }
        }
    }

    /// The busiest GPU now, `0.0..=1.0`; `None` if none could be read.
    pub(super) fn read(&mut self, now: Instant) -> Option<f32> {
        let sysfs = self.sysfs.iter_mut().map(|s| s.read(now));
        let nvidia = self.nvidia.read();
        max_of(sysfs.chain(nvidia))
    }
}

impl Source {
    fn read(&mut self, now: Instant) -> Option<f32> {
        let asleep = asleep(&self.power);
        match &mut self.kind {
            Kind::Busy(_) if asleep => Some(0.0),
            Kind::Busy(path) => read_to_string(path).and_then(|s| parse_gpu_busy(&s)),
            Kind::Idle { prev, .. } if asleep => {
                // The counter is not read while asleep, so the delta across a
                // suspend would be meaningless: start over on wake.
                *prev = None;
                Some(0.0)
            }
            Kind::Idle { counter, prev, last } => {
                let Some(v) = read_u64(counter) else {
                    *prev = None;
                    return None;
                };
                match *prev {
                    // First read after probe failed or after a wake: seed.
                    None => {
                        *prev = Some((v, now));
                        *last = 0.0;
                    }
                    Some((p, at)) => match idle_busy(p, at, v, now) {
                        Residency::Busy(f) => {
                            *prev = Some((v, now));
                            *last = f;
                        }
                        Residency::TooSoon => {}
                        Residency::Reset => {
                            *prev = Some((v, now));
                            *last = 0.0;
                        }
                    },
                }
                Some(*last)
            }
        }
    }
}

impl Nvidia {
    /// Initialize NVML if it is pending and some NVIDIA GPU is awake. On
    /// failure NVIDIA is out until the next probe: no per-tick retry.
    fn init_if_awake(&mut self) {
        let Nvidia::Pending { power, pci, lib } = self else {
            return;
        };
        if power.iter().all(|p| asleep(p)) {
            return;
        }
        *self = match open_nvml(lib, pci) {
            Some((nvml, devices)) => Nvidia::Ready { devices, nvml },
            None => Nvidia::Absent,
        };
    }

    /// One reading per NVIDIA GPU.
    fn read(&mut self) -> Vec<Option<f32>> {
        self.init_if_awake();
        match self {
            Nvidia::Absent => Vec::new(),
            // Still pending, so every NVIDIA GPU sleeps.
            Nvidia::Pending { power, .. } => vec![Some(0.0); power.len()],
            Nvidia::Ready { devices, nvml } => devices
                .iter()
                .map(|(handle, power)| {
                    if power.as_deref().is_some_and(asleep) {
                        return Some(0.0);
                    }
                    // SAFETY: `handle` came from `device_by_index` on this
                    // same `nvml`, which is still initialized (it is dropped,
                    // calling nvmlShutdown, only together with `devices`).
                    // NVML device handles stay valid until nvmlShutdown.
                    let device = unsafe { Device::new(*handle, nvml) };
                    let u = device.utilization_rates().ok()?;
                    Some(u.gpu.min(100) as f32 / 100.0)
                })
                .collect(),
        }
    }
}

/// Load and init NVML from `lib` and list its devices. `None`, silently, if
/// the library is missing, init fails or it reports no devices.
fn open_nvml(lib: &OsStr, pci: &Path) -> Option<(Box<Nvml>, NvDevices)> {
    let nvml = Nvml::builder().lib_path(lib).init().ok()?;
    let count = nvml.device_count().ok()?;
    let mut devices = Vec::new();
    for i in 0..count {
        let Ok(device) = nvml.device_by_index(i) else {
            continue;
        };
        let power = device
            .pci_info()
            .ok()
            .and_then(|p| sysfs_bdf(&p.bus_id))
            .map(|bdf| pci.join(bdf).join("power/runtime_status"));
        // SAFETY: only copies the raw handle out; it is used again only
        // through `Device::new` with this same `nvml` (see `Nvidia::read`).
        devices.push((unsafe { device.handle() }, power));
    }
    (!devices.is_empty()).then(|| (Box::new(nvml), devices))
}

/// NVML's bus id (`00000000:01:00.0`, 8-digit domain) as sysfs names the PCI
/// device (`0000:01:00.0`), lower-cased.
fn sysfs_bdf(bus_id: &str) -> Option<String> {
    let bus_id = bus_id.trim().to_ascii_lowercase();
    let (domain, rest) = bus_id.split_once(':')?;
    let domain = u32::from_str_radix(domain, 16).ok()?;
    let (bus, devfn) = rest.split_once(':')?;
    let (dev, func) = devfn.split_once('.')?;
    let hex = |s: &str, n: usize| s.len() == n && s.bytes().all(|b| b.is_ascii_hexdigit());
    (hex(bus, 2) && hex(dev, 2) && hex(func, 1)).then(|| format!("{domain:04x}:{rest}"))
}

/// Whether a PCI device's `runtime_status` says it is (going) to sleep. A
/// missing or unreadable file means it is not runtime-managed: awake.
fn asleep(runtime_status: &Path) -> bool {
    read_to_string(runtime_status).is_some_and(|s| matches!(s.trim(), "suspended" | "suspending"))
}

/// The largest reading, or `None` if there were none.
fn max_of(readings: impl IntoIterator<Item = Option<f32>>) -> Option<f32> {
    readings.into_iter().flatten().reduce(f32::max)
}

enum Residency {
    Busy(f32),
    /// Less than a millisecond passed: keep the previous reading.
    TooSoon,
    /// The counter went backwards (driver reset): reseed.
    Reset,
}

/// Busy fraction from two idle-residency readings (ms) and when they were
/// taken: `1 - idle/wall`, clamped.
fn idle_busy(prev: u64, prev_at: Instant, now: u64, now_at: Instant) -> Residency {
    let wall = now_at.saturating_duration_since(prev_at).as_secs_f64() * 1000.0;
    if wall < 1.0 {
        return Residency::TooSoon;
    }
    let Some(idle) = now.checked_sub(prev) else {
        return Residency::Reset;
    };
    Residency::Busy((1.0 - idle as f64 / wall).clamp(0.0, 1.0) as f32)
}

/// amdgpu's `gpu_busy_percent`: an integer 0..=100.
fn parse_gpu_busy(s: &str) -> Option<f32> {
    let pct: u32 = s.trim().parse().ok()?;
    (pct <= 100).then(|| pct as f32 / 100.0)
}

/// Every GPU source under `drm` (`/sys/class/drm`), per card, by card name.
/// Presence is checked with `stat` only, so a sleeping GPU is not woken.
/// Connector entries (`card1-eDP-1`) are skipped.
fn probe_cards(drm: &Path) -> Vec<Source> {
    let mut cards: Vec<PathBuf> = subdirs(drm)
        .into_iter()
        .filter(|c| {
            c.file_name()
                .and_then(|n| n.to_str())
                .and_then(|n| n.strip_prefix("card"))
                .is_some_and(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()))
        })
        .collect();
    cards.sort();
    let mut out = Vec::new();
    for card in cards {
        let dev = card.join("device");
        let power = dev.join("power/runtime_status");
        let mut add = |kind| {
            out.push(Source {
                power: power.clone(),
                kind,
            })
        };
        let idle = |counter| Kind::Idle {
            counter,
            prev: None,
            last: 0.0,
        };
        // amdgpu
        let busy = dev.join("gpu_busy_percent");
        if busy.exists() {
            add(Kind::Busy(busy));
        }
        // xe: device/tile*/gt*/gtidle/idle_residency_ms
        for tile in prefixed(&dev, "tile") {
            for gt in prefixed(&tile, "gt") {
                let c = gt.join("gtidle/idle_residency_ms");
                if c.exists() {
                    add(idle(c));
                }
            }
        }
        // i915: gt/gt*/rc6_residency_ms, or power/rc6_residency_ms on
        // kernels from before the per-GT directories.
        let gts: Vec<PathBuf> = prefixed(&card.join("gt"), "gt")
            .into_iter()
            .map(|gt| gt.join("rc6_residency_ms"))
            .filter(|c| c.exists())
            .collect();
        if gts.is_empty() {
            let c = card.join("power/rc6_residency_ms");
            if c.exists() {
                add(idle(c));
            }
        }
        for c in gts {
            add(idle(c));
        }
    }
    out
}

/// `power/runtime_status` of each PCI display controller bound to the
/// proprietary `nvidia` driver. nouveau-driven cards are left out: NVML cannot
/// read them, so there is no point loading it.
fn nvidia_devices(pci: &Path) -> Vec<PathBuf> {
    let mut devs: Vec<PathBuf> = subdirs(pci)
        .into_iter()
        .filter(|d| {
            let vendor = read_to_string(d.join("vendor"));
            let class = read_to_string(d.join("class"));
            let driver = std::fs::read_link(d.join("driver")).ok();
            vendor.is_some_and(|v| v.trim() == "0x10de")
                && class.is_some_and(|c| c.trim().starts_with("0x03"))
                && driver.is_some_and(|l| l.file_name() == Some(OsStr::new("nvidia")))
        })
        .map(|d| d.join("power/runtime_status"))
        .collect();
    devs.sort();
    devs
}

/// Entries of `dir` (following symlinks, as sysfs needs).
fn subdirs(dir: &Path) -> Vec<PathBuf> {
    std::fs::read_dir(dir)
        .map(|r| r.filter_map(|e| e.ok()).map(|e| e.path()).collect())
        .unwrap_or_default()
}

/// Subdirectories of `dir` named `<prefix><digits>`.
fn prefixed(dir: &Path, prefix: &str) -> Vec<PathBuf> {
    let mut v: Vec<PathBuf> = subdirs(dir)
        .into_iter()
        .filter(|p| {
            p.file_name()
                .and_then(|n| n.to_str())
                .and_then(|n| n.strip_prefix(prefix))
                .is_some_and(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()))
        })
        .collect();
    v.sort();
    v
}

fn read_to_string(path: impl AsRef<Path>) -> Option<String> {
    std::fs::read_to_string(path).ok()
}

fn read_u64(path: &Path) -> Option<u64> {
    read_to_string(path)?.trim().parse().ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    /// A throwaway sysfs root, removed on drop.
    struct FakeSys(PathBuf);

    impl FakeSys {
        fn new(tag: &str) -> Self {
            let dir = std::env::temp_dir().join(format!(
                "eclipse-usage-gpu-{tag}-{}-{:?}",
                std::process::id(),
                std::thread::current().id()
            ));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).unwrap();
            Self(dir)
        }

        fn write(&self, rel: &str, contents: &str) {
            let p = self.0.join(rel);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(p, contents).unwrap();
        }

        fn mkdir(&self, rel: &str) {
            std::fs::create_dir_all(self.0.join(rel)).unwrap();
        }

        /// An NVIDIA display controller at `bdf`, bound to `driver`.
        fn nvidia(&self, bdf: &str, driver: &str, status: &str) {
            let d = format!("bus/pci/devices/{bdf}");
            self.write(&format!("{d}/vendor"), "0x10de\n");
            self.write(&format!("{d}/class"), "0x030000\n");
            self.write(&format!("{d}/power/runtime_status"), status);
            self.mkdir(&format!("bus/pci/drivers/{driver}"));
            std::os::unix::fs::symlink(
                self.0.join(format!("bus/pci/drivers/{driver}")),
                self.0.join(format!("{d}/driver")),
            )
            .unwrap();
        }
    }

    impl Drop for FakeSys {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    const NO_LIB: &str = "libeclipse-no-such-nvml.so.1";

    #[test]
    fn gpu_busy() {
        assert_eq!(parse_gpu_busy("37\n"), Some(0.37));
        assert_eq!(parse_gpu_busy("0\n"), Some(0.0));
        assert_eq!(parse_gpu_busy("101\n"), None);
        assert_eq!(parse_gpu_busy("garbage"), None);
        assert_eq!(parse_gpu_busy(""), None);
    }

    #[test]
    fn max_across_sources() {
        assert_eq!(max_of([]), None);
        assert_eq!(max_of([None, None]), None);
        assert_eq!(max_of([Some(0.0)]), Some(0.0));
        assert_eq!(max_of([Some(0.2), None, Some(0.7), Some(0.1)]), Some(0.7));
    }

    #[test]
    fn bus_id_to_sysfs() {
        assert_eq!(sysfs_bdf("00000000:01:00.0").as_deref(), Some("0000:01:00.0"));
        assert_eq!(sysfs_bdf("00000000:0A:00.1\n").as_deref(), Some("0000:0a:00.1"));
        assert_eq!(sysfs_bdf("0001:C1:00.0").as_deref(), Some("0001:c1:00.0"));
        assert_eq!(sysfs_bdf(""), None);
        assert_eq!(sysfs_bdf("garbage"), None);
        assert_eq!(sysfs_bdf("00000000:01:00"), None);
        assert_eq!(sysfs_bdf("zz:01:00.0"), None);
    }

    #[test]
    fn suspended_gate() {
        let s = FakeSys::new("gate");
        let f = |name: &str, v: &str| {
            s.write(name, v);
            s.0.join(name)
        };
        assert!(asleep(&f("a", "suspended\n")));
        assert!(asleep(&f("b", "suspending\n")));
        assert!(!asleep(&f("c", "active\n")));
        assert!(!asleep(&f("d", "resuming\n")));
        assert!(!asleep(&f("e", "unsupported\n")));
        assert!(!asleep(&s.0.join("missing")));
    }

    #[test]
    fn idle_residency() {
        let t0 = Instant::now();
        let t1 = t0 + Duration::from_millis(1000);
        match idle_busy(5000, t0, 5750, t1) {
            Residency::Busy(f) => assert!((f - 0.25).abs() < 1e-6),
            _ => panic!("expected busy"),
        }
        // More idle than wall (jitter) clamps to 0.
        assert!(matches!(idle_busy(0, t0, 1100, t1), Residency::Busy(0.0)));
        assert!(matches!(idle_busy(5000, t0, 4000, t1), Residency::Reset));
        assert!(matches!(idle_busy(5000, t0, 5000, t0), Residency::TooSoon));
    }

    #[test]
    fn probe_per_card() {
        let s = FakeSys::new("cards");
        // card0: amdgpu iGPU, awake. card1: amdgpu dGPU, suspended.
        s.write("class/drm/card0/device/gpu_busy_percent", "12\n");
        s.write("class/drm/card0/device/power/runtime_status", "active\n");
        s.write("class/drm/card1/device/gpu_busy_percent", "garbage-if-read\n");
        s.write("class/drm/card1/device/power/runtime_status", "suspended\n");
        // Connector entries are not cards.
        s.write("class/drm/card0-eDP-1/device/gpu_busy_percent", "99\n");
        // card2: xe with two GTs. card3: i915 per-GT. card4: old i915.
        s.write("class/drm/card2/device/tile0/gt0/gtidle/idle_residency_ms", "0\n");
        s.write("class/drm/card2/device/tile0/gt1/gtidle/idle_residency_ms", "0\n");
        s.write("class/drm/card3/gt/gt0/rc6_residency_ms", "0\n");
        s.write("class/drm/card3/power/rc6_residency_ms", "0\n");
        s.write("class/drm/card4/power/rc6_residency_ms", "0\n");
        // card5: nothing readable (e.g. nvidia-drm).
        s.mkdir("class/drm/card5/device");

        let cards = probe_cards(&s.0.join("class/drm"));
        let got: Vec<(String, bool)> = cards
            .iter()
            .map(|c| {
                let (p, busy) = match &c.kind {
                    Kind::Busy(p) => (p, true),
                    Kind::Idle { counter, .. } => (counter, false),
                };
                (p.strip_prefix(&s.0).unwrap().display().to_string(), busy)
            })
            .collect();
        let want: Vec<(String, bool)> = [
            ("class/drm/card0/device/gpu_busy_percent", true),
            ("class/drm/card1/device/gpu_busy_percent", true),
            ("class/drm/card2/device/tile0/gt0/gtidle/idle_residency_ms", false),
            ("class/drm/card2/device/tile0/gt1/gtidle/idle_residency_ms", false),
            ("class/drm/card3/gt/gt0/rc6_residency_ms", false),
            ("class/drm/card4/power/rc6_residency_ms", false),
        ]
        .into_iter()
        .map(|(p, b)| (p.to_owned(), b))
        .collect();
        assert_eq!(got, want);
        assert_eq!(probe_cards(Path::new("/no/such/drm")).len(), 0);
    }

    #[test]
    fn read_is_max_and_gated() {
        let s = FakeSys::new("read");
        s.write("class/drm/card0/device/gpu_busy_percent", "12\n");
        s.write("class/drm/card0/device/power/runtime_status", "active\n");
        // A suspended dGPU whose file would read 90 if touched: must be 0.
        s.write("class/drm/card1/device/gpu_busy_percent", "90\n");
        s.write("class/drm/card1/device/power/runtime_status", "suspended\n");
        let mut g = Gpus::probe(&s.0, OsStr::new(NO_LIB));
        assert!(matches!(g.nvidia, Nvidia::Absent));
        assert_eq!(g.read(Instant::now()), Some(0.12));
        // Waking it makes it the busiest.
        s.write("class/drm/card1/device/power/runtime_status", "active\n");
        assert_eq!(g.read(Instant::now()), Some(0.9));
    }

    #[test]
    fn read_idle_residency_over_ticks() {
        let s = FakeSys::new("idle");
        s.write("class/drm/card0/gt/gt0/rc6_residency_ms", "1000\n");
        let mut g = Gpus::probe(&s.0, OsStr::new(NO_LIB));
        let Kind::Idle { prev, .. } = &mut g.sysfs[0].kind else {
            panic!("expected an idle source")
        };
        let t0 = prev.unwrap().1;
        s.write("class/drm/card0/gt/gt0/rc6_residency_ms", "1400\n");
        let got = g.read(t0 + Duration::from_millis(1000)).unwrap();
        assert!((got - 0.6).abs() < 1e-3, "{got}");
        // Reprobe keeps the seed, so a forced tick right after still reads.
        g.reprobe(&s.0, OsStr::new(NO_LIB));
        s.write("class/drm/card0/gt/gt0/rc6_residency_ms", "2400\n");
        let got = g.read(t0 + Duration::from_millis(2000)).unwrap();
        assert!(got.abs() < 1e-3, "{got}");
    }

    #[test]
    fn no_source_is_none() {
        let s = FakeSys::new("none");
        s.mkdir("class/drm/card0/device");
        let mut g = Gpus::probe(&s.0, OsStr::new(NO_LIB));
        assert_eq!(g.read(Instant::now()), None);
    }

    #[test]
    fn nvidia_detection() {
        let s = FakeSys::new("nvdetect");
        s.nvidia("0000:01:00.0", "nvidia", "active\n");
        s.nvidia("0000:02:00.0", "nouveau", "active\n");
        // Not a display controller (the dGPU's HDMI audio function).
        s.write("bus/pci/devices/0000:01:00.1/vendor", "0x10de\n");
        s.write("bus/pci/devices/0000:01:00.1/class", "0x040300\n");
        let got = nvidia_devices(&s.0.join("bus/pci/devices"));
        assert_eq!(
            got,
            vec![s.0.join("bus/pci/devices/0000:01:00.0/power/runtime_status")]
        );
    }

    /// The path this host and CI take: an NVIDIA GPU is present, the library
    /// is not. NVML falls away silently and the sysfs reading stands.
    #[test]
    fn nvidia_without_library_falls_back() {
        let s = FakeSys::new("nvnolib");
        s.write("class/drm/card0/device/gpu_busy_percent", "30\n");
        s.nvidia("0000:01:00.0", "nvidia", "active\n");
        let mut g = Gpus::probe(&s.0, OsStr::new(NO_LIB));
        assert!(matches!(g.nvidia, Nvidia::Absent));
        assert_eq!(g.read(Instant::now()), Some(0.3));
        assert!(open_nvml(OsStr::new(NO_LIB), &s.0).is_none());
    }

    /// While every NVIDIA GPU sleeps NVML is not loaded at all, and the GPU
    /// reads as idle. The first awake tick tries it once.
    #[test]
    fn nvidia_asleep_defers_nvml() {
        let s = FakeSys::new("nvasleep");
        s.nvidia("0000:01:00.0", "nvidia", "suspended\n");
        let mut g = Gpus::probe(&s.0, OsStr::new(NO_LIB));
        assert!(matches!(g.nvidia, Nvidia::Pending { .. }));
        assert_eq!(g.read(Instant::now()), Some(0.0));
        assert!(matches!(g.nvidia, Nvidia::Pending { .. }));
        s.write("bus/pci/devices/0000:01:00.0/power/runtime_status", "active\n");
        assert_eq!(g.read(Instant::now()), None);
        assert!(matches!(g.nvidia, Nvidia::Absent));
    }
}
