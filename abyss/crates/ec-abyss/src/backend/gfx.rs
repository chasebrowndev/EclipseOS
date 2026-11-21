// SPDX-License-Identifier: AGPL-3.0-only
//! Recommended graphics profile (COMP-02 §9, COMP-14): what this PC can draw.
//!
//! Three parts, none of them on the input or policy path:
//!
//! - [`decide`]: a pure, table-driven function from [`Inputs`] (GL renderer
//!   string, RAM, CPU count, output pixels, VM hint) to a [`GfxProfile`] and a
//!   reason. Every rule only ever *lowers* the profile; the start is `Full`.
//! - [`probe`]: reads those inputs. The renderer string comes from the backend
//!   (the only place that holds a GL context); the rest from `/proc` and `/sys`.
//! - [`Governor`]: a pure state machine on a caller-supplied clock that steps
//!   an `auto` profile down one level for the session after sustained missed
//!   frames. It never steps up and never writes the config.
//!
//! The result reaches the config through `ec_abyss_config::gfx`: `auto`
//! follows the detected profile, and keys written in `abyss.kdl` still win.

use std::time::Duration;

use crate::config::gfx::{self, GfxProfile, ProfileSetting};
use crate::state::AbyssState;

/// What the decision looks at.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Inputs {
    /// `GL_RENDERER`, as reported by the driver. Empty when unknown.
    pub renderer: String,
    /// Total RAM in MiB; 0 when unreadable (then no RAM rule applies).
    pub ram_mb: u64,
    /// Logical CPUs available to the process; 0 when unknown.
    pub cpus: u32,
    /// Total pixels of the enabled outputs; 0 when no output exists yet.
    pub pixels: u64,
    /// A hypervisor was detected (DMI, `/sys/hypervisor` or cpuinfo flag).
    pub vm: bool,
}

/// The profile and the one-line reason for it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Decision {
    pub profile: GfxProfile,
    pub reason: &'static str,
}

/// Pixels of a 4K (3840x2160) output, less a margin for 3840x1600 ultrawides.
pub const FOUR_K_PIXELS: u64 = 7_000_000;
const LOW_RAM_MB: u64 = 2_048;
const MODEST_RAM_MB: u64 = 4_096;

/// GPUs that rasterise on the CPU.
const SOFTWARE: &[&str] = &[
    "llvmpipe",
    "softpipe",
    "swrast",
    "software rasterizer",
    "swr ",
    "lavapipe",
];
/// Paravirtual GPUs: a VM's display adapter, however fast the host is.
const VIRTUAL: &[&str] = &[
    "virgl",
    "virtio",
    "svga3d",
    "vmware",
    "qxl",
    "vbox",
    "virtualbox",
    "bochs",
    "cirrus",
    "microsoft basic",
];
/// Intel Mesa codenames and families of generations that struggle with a
/// blur chain (Gen 4 to 8, Atom).
const INTEL_WEAK: &[&str] = &[
    " (ilk",
    " (snb",
    " (ivb",
    " (hsw",
    " (byt",
    " (bdw",
    " (chv",
    " (bxt",
    " (glk",
    " (cln",
    " (elk",
    "hd graphics 2000",
    "hd graphics 3000",
    "hd graphics 4000",
    "hd graphics 4600",
    "hd graphics 5000",
    "ironlake",
    "sandybridge",
    "ivybridge",
    "haswell",
    "bay trail",
    "braswell",
    "pineview",
    "gma ",
    "mesa dri intel",
];
/// Single-board-computer and phone GPUs.
const EMBEDDED: &[&str] = &[
    "panfrost",
    "mali",
    "v3d",
    "videocore",
    "vc4",
    "adreno",
    "lima",
    "etnaviv",
    "tegra",
];
const DISCRETE: &[&str] = &[
    "nvidia",
    "geforce",
    "quadro",
    "rtx",
    "gtx",
    "radeon",
    "amd",
    "arc(tm)",
    "arc ",
    "iris(r) xe",
    " (tgl",
    " (adl",
    " (rkl",
    " (dg",
    " (mtl",
    " (rpl",
];

fn has(hay: &str, needles: &[&str]) -> bool {
    needles.iter().any(|n| hay.contains(n))
}

type Rule = fn(&Inputs, &str) -> Option<Decision>;

fn lower(profile: GfxProfile, reason: &'static str) -> Option<Decision> {
    Some(Decision { profile, reason })
}

/// What the GPU alone allows, before size, RAM and CPU. Lowercased renderer.
fn gpu_class(_: &Inputs, r: &str) -> Option<Decision> {
    use GfxProfile::*;
    if r.is_empty() {
        return lower(Balanced, "graphics driver not identified");
    }
    if has(r, SOFTWARE) {
        return lower(Minimal, "software rendering (no GPU acceleration)");
    }
    if has(r, VIRTUAL) {
        return lower(Low, "virtual machine graphics adapter");
    }
    if has(r, EMBEDDED) {
        return lower(Low, "low-power embedded GPU");
    }
    if r.contains("intel") || r.contains("mesa dri") {
        if has(r, DISCRETE) {
            return None;
        }
        if has(r, INTEL_WEAK) {
            return lower(Low, "older Intel integrated graphics");
        }
        // Gen 9 to 11 (Skylake to Ice Lake): fine at 1080p, heavy at 4K.
        return lower(Balanced, "Intel integrated graphics");
    }
    if has(r, DISCRETE) {
        return None;
    }
    lower(Balanced, "unrecognised graphics hardware")
}

fn ram(i: &Inputs, _: &str) -> Option<Decision> {
    match i.ram_mb {
        0 => None,
        m if m < LOW_RAM_MB => lower(GfxProfile::Minimal, "under 2 GiB of RAM"),
        m if m < MODEST_RAM_MB => lower(GfxProfile::Low, "under 4 GiB of RAM"),
        _ => None,
    }
}

fn cpus(i: &Inputs, _: &str) -> Option<Decision> {
    match i.cpus {
        0 => None,
        1 => lower(GfxProfile::Minimal, "a single CPU core"),
        2 => lower(GfxProfile::Low, "only two CPU cores"),
        _ => None,
    }
}

/// An unidentified adapter inside a hypervisor is treated as virtual.
fn vm_unknown(i: &Inputs, r: &str) -> Option<Decision> {
    (i.vm && r.is_empty()).then_some(Decision {
        profile: GfxProfile::Low,
        reason: "virtual machine with unidentified graphics",
    })
}

const RULES: &[Rule] = &[gpu_class, vm_unknown, ram, cpus];

/// The recommended profile for `i`: the lowest any rule allows, then one step
/// lighter when a 4K-class output sits on a GPU that is not known to be fast.
/// Pure; the first rule that reaches the lowest profile supplies the reason.
pub fn decide(i: &Inputs) -> Decision {
    let r = i.renderer.to_ascii_lowercase();
    let mut d = Decision {
        profile: GfxProfile::Full,
        reason: "capable graphics hardware",
    };
    for rule in RULES {
        if let Some(n) = rule(i, &r) {
            if n.profile < d.profile {
                d = n;
            }
        }
    }
    if i.pixels >= FOUR_K_PIXELS && d.profile < GfxProfile::Full {
        let lighter = match d.profile {
            GfxProfile::Balanced => GfxProfile::Low,
            _ => GfxProfile::Minimal,
        };
        d = Decision {
            profile: lighter,
            reason: "4K display on modest graphics hardware",
        };
    }
    d
}

/// Total RAM in MiB from a `/proc/meminfo` body.
pub fn parse_meminfo(text: &str) -> u64 {
    text.lines()
        .find_map(|l| l.strip_prefix("MemTotal:"))
        .and_then(|v| v.split_whitespace().next())
        .and_then(|kb| kb.parse::<u64>().ok())
        .map_or(0, |kb| kb / 1024)
}

fn vm_hint() -> bool {
    const DMI: &[&str] = &[
        "qemu",
        "kvm",
        "virtualbox",
        "vmware",
        "innotek",
        "bochs",
        "xen",
        "virtual machine",
        "parallels",
        "bhyve",
    ];
    let dmi = ["sys_vendor", "product_name"].iter().any(|f| {
        std::fs::read_to_string(format!("/sys/class/dmi/id/{f}"))
            .map(|s| has(&s.to_ascii_lowercase(), DMI))
            .unwrap_or(false)
    });
    dmi || std::path::Path::new("/sys/hypervisor/type").exists()
        || std::fs::read_to_string("/proc/cpuinfo").is_ok_and(|s| {
            s.lines()
                .any(|l| l.starts_with("flags") && l.contains(" hypervisor"))
        })
}

/// Read the machine's inputs. `ECLIPSE_GFX_FAKE_RENDERER`, `_RAM_MB`, `_CPUS`,
/// `_PIXELS` and `_VM` replace what they name, so every row of the decision
/// table can be exercised without that hardware.
pub fn probe(renderer: &str, pixels: u64) -> Inputs {
    let env = |k: &str| std::env::var(format!("ECLIPSE_GFX_FAKE_{k}")).ok();
    let num = |k: &str| env(k).and_then(|v| v.parse::<u64>().ok());
    Inputs {
        renderer: env("RENDERER").unwrap_or_else(|| renderer.to_owned()),
        ram_mb: num("RAM_MB")
            .unwrap_or_else(|| std::fs::read_to_string("/proc/meminfo").map_or(0, |t| parse_meminfo(&t))),
        cpus: num("CPUS").map_or_else(
            || std::thread::available_parallelism().map_or(0, |n| n.get() as u32),
            |n| n as u32,
        ),
        pixels: num("PIXELS").unwrap_or(pixels),
        vm: env("VM").map_or_else(vm_hint, |v| v == "1"),
    }
}

// ----------------------------------------------------------------- governor

/// No decision for this long after start: shader compiles and first-map work
/// miss frames on every machine.
pub const WARMUP_MS: u64 = 30_000;
/// One observation window.
pub const WINDOW_MS: u64 = 10_000;
/// A window with fewer frames than this says nothing (an idle desktop only
/// draws on damage) and resets the run.
pub const MIN_FRAMES: u32 = 120;
/// Percent of a window's frames that must miss for it to count as bad.
pub const BAD_PCT: u32 = 50;
/// Consecutive bad windows before stepping down.
pub const BAD_WINDOWS: u32 = 2;
/// After a step, wait this long before judging again.
pub const COOLDOWN_MS: u64 = 30_000;

/// Steps an `auto` profile down after sustained misses. Pure: the caller
/// supplies `now_ms`, so tests drive it with a fake clock.
#[derive(Debug, Clone, Default)]
pub struct Governor {
    start_ms: Option<u64>,
    window_start_ms: u64,
    frames: u32,
    misses: u32,
    bad: u32,
    hold_until_ms: u64,
}

impl Governor {
    /// Account one frame of `frame` against `period`, with `current` the
    /// profile in force. Returns the profile to step down to, if any; the
    /// caller applies it. Allocation-free.
    pub fn observe(
        &mut self,
        now_ms: u64,
        frame: Duration,
        period: Duration,
        current: GfxProfile,
    ) -> Option<GfxProfile> {
        let start = *self.start_ms.get_or_insert(now_ms);
        let target = current.step_down()?;
        if now_ms < start + WARMUP_MS || now_ms < self.hold_until_ms {
            self.reset_window(now_ms);
            return None;
        }
        if now_ms.saturating_sub(self.window_start_ms) >= WINDOW_MS {
            let bad = self.frames >= MIN_FRAMES && self.misses * 100 >= self.frames * BAD_PCT;
            self.bad = if bad { self.bad + 1 } else { 0 };
            self.reset_window(now_ms);
            if self.bad >= BAD_WINDOWS {
                self.bad = 0;
                self.hold_until_ms = now_ms + COOLDOWN_MS;
                return Some(target);
            }
        }
        self.frames += 1;
        if frame > period {
            self.misses += 1;
        }
        None
    }

    fn reset_window(&mut self, now_ms: u64) {
        self.window_start_ms = now_ms;
        self.frames = 0;
        self.misses = 0;
    }
}

// -------------------------------------------------------------------- state

/// Detection result and governor, owned by `AbyssState`.
#[derive(Debug, Default)]
pub struct GfxState {
    /// `GL_RENDERER` stashed by the backend once the renderer exists.
    renderer: Option<String>,
    /// Detection ran (once, on the first frame, when outputs exist).
    settled: bool,
    pub detected: Option<Decision>,
    pub inputs: Inputs,
    /// Session ceiling set by the governor, with the profile it fell from.
    pub stepped: Option<(GfxProfile, GfxProfile)>,
    governor: Governor,
}

/// Called by a backend right after its renderer exists. `renderer` is
/// `GL_RENDERER`. Detection itself waits for the first frame, when the outputs
/// (and so the pixel count) are known.
pub fn set_renderer(state: &mut AbyssState, renderer: String) {
    state.gfx.renderer = Some(renderer);
}

fn output_pixels(state: &AbyssState) -> u64 {
    state
        .outputs
        .iter()
        .filter(|e| e.enabled)
        .filter_map(|e| e.output.current_mode())
        .map(|m| m.size.w.max(0) as u64 * m.size.h.max(0) as u64)
        .sum()
}

fn settle(state: &mut AbyssState) {
    let Some(renderer) = state.gfx.renderer.clone() else {
        return;
    };
    let pixels = output_pixels(state);
    if pixels == 0 {
        return;
    }
    state.gfx.settled = true;
    let inputs = probe(&renderer, pixels);
    let d = decide(&inputs);
    tracing::info!(
        profile = d.profile.name(),
        reason = d.reason,
        renderer = %inputs.renderer,
        ram_mb = inputs.ram_mb,
        cpus = inputs.cpus,
        pixels = inputs.pixels,
        vm = inputs.vm,
        setting = state.config.gfx.setting.name(),
        "recommended graphics profile"
    );
    state.gfx.inputs = inputs;
    state.gfx.detected = Some(d);
    gfx::set_detected(Some(d.profile));
    refresh(state);
}

/// Re-resolve the visual defaults after detection or a step-down moved the
/// profile, and tell what depends on blur being on.
fn refresh(state: &mut AbyssState) {
    let before = state.config.gfx.effective;
    state.config.resolve_gfx_defaults();
    if state.config.gfx.effective != before {
        tracing::info!(
            from = before.name(),
            to = state.config.gfx.effective.name(),
            "graphics profile in force"
        );
    }
    crate::protocols::standard::background_effect::update_capabilities(state);
    crate::protocols::standard::glass::update_capabilities(state);
    crate::backend::damage_all(state);
}

/// Per composited frame, after `stats.record`: settles detection on the first
/// frame and feeds the governor. `frame` is the CPU time of the frame and
/// `period` the output's refresh period.
pub fn on_frame(state: &mut AbyssState, frame: Duration, period: Duration) {
    if !state.gfx.settled {
        settle(state);
        return;
    }
    let cfg = &state.config.gfx;
    if cfg.setting != ProfileSetting::Auto || !cfg.governor {
        return;
    }
    let now_ms = state.start_time.elapsed().as_millis() as u64;
    let current = cfg.effective;
    if let Some(next) = state.gfx.governor.observe(now_ms, frame, period, current) {
        tracing::warn!(
            from = current.name(),
            to = next.name(),
            "sustained missed frames: graphics profile lowered for this session (config untouched)"
        );
        state.gfx.stepped = Some((current, next));
        gfx::set_session_cap(Some(next));
        refresh(state);
    }
}

/// The `graphics` object of `get_metrics`: what `ec-ctl graphics` and
/// Settings show.
pub fn status_json(state: &AbyssState) -> serde_json::Value {
    let c = &state.config.gfx;
    let rec = state.gfx.detected;
    serde_json::json!({
        "setting": c.setting.name(),
        "effective": c.effective.name(),
        "recommended": rec.map(|d| d.profile.name()),
        "reason": rec.map(|d| d.reason),
        "description": c.effective.describe(),
        "governor": c.governor,
        "stepped_down": state.gfx.stepped.map(|(from, to)| serde_json::json!({"from": from.name(), "to": to.name()})),
        "inputs": {
            "renderer": state.gfx.inputs.renderer,
            "ram_mb": state.gfx.inputs.ram_mb,
            "cpus": state.gfx.inputs.cpus,
            "pixels": state.gfx.inputs.pixels,
            "vm": state.gfx.inputs.vm,
        },
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use GfxProfile::*;

    fn gpu(renderer: &str) -> Inputs {
        Inputs {
            renderer: renderer.into(),
            ram_mb: 16_384,
            cpus: 8,
            pixels: 1920 * 1080,
            vm: false,
        }
    }

    #[test]
    fn decision_table() {
        let uhd4k = |mut i: Inputs| {
            i.pixels = 3840 * 2160;
            i
        };
        let ram = |mut i: Inputs, mb| {
            i.ram_mb = mb;
            i
        };
        let cpus = |mut i: Inputs, n| {
            i.cpus = n;
            i
        };
        let table: Vec<(&str, Inputs, GfxProfile)> = vec![
            ("llvmpipe", gpu("llvmpipe (LLVM 17.0.6, 256 bits)"), Minimal),
            ("softpipe", gpu("softpipe"), Minimal),
            ("swrast", gpu("Mesa Software Rasterizer"), Minimal),
            ("virgl vm", gpu("virgl (NVIDIA GeForce RTX 3060)"), Low),
            ("vmware svga", gpu("SVGA3D; build: RELEASE; LLVM;"), Low),
            ("qxl", gpu("Mesa QXL"), Low),
            ("virtio venus", gpu("Virtio-GPU Venus (Intel)"), Low),
            ("ivy bridge", gpu("Mesa Intel(R) HD Graphics 4000 (IVB GT2)"), Low),
            (
                "sandy bridge",
                gpu("Mesa Intel(R) HD Graphics 3000 (SNB GT2)"),
                Low,
            ),
            (
                "kaby lake",
                gpu("Mesa Intel(R) UHD Graphics 620 (KBL GT2)"),
                Balanced,
            ),
            ("tiger lake", gpu("Mesa Intel(R) Xe Graphics (TGL GT2)"), Full),
            ("alder lake", gpu("Mesa Intel(R) Graphics (ADL GT2)"), Full),
            ("arc", gpu("Mesa Intel(R) Arc(tm) A380 Graphics (DG2)"), Full),
            ("nvidia", gpu("NVIDIA GeForce RTX 4070/PCIe/SSE2"), Full),
            (
                "amd",
                gpu("AMD Radeon RX 6700 XT (radeonsi, navi22, LLVM 17)"),
                Full,
            ),
            ("embedded", gpu("Mali-G52 MC2 (Panfrost)"), Low),
            ("unknown", gpu("Frobnicator 9000"), Balanced),
            ("no driver string", gpu(""), Balanced),
            (
                "4k on kaby lake",
                uhd4k(gpu("Mesa Intel(R) UHD Graphics 620 (KBL GT2)")),
                Low,
            ),
            (
                "4k on ivy bridge",
                uhd4k(gpu("Mesa Intel(R) HD Graphics 4000 (IVB GT2)")),
                Minimal,
            ),
            (
                "4k on discrete",
                uhd4k(gpu("NVIDIA GeForce RTX 4070/PCIe/SSE2")),
                Full,
            ),
            (
                "low ram discrete",
                ram(gpu("NVIDIA GeForce GTX 1050"), 1536),
                Minimal,
            ),
            (
                "modest ram discrete",
                ram(gpu("NVIDIA GeForce GTX 1050"), 3072),
                Low,
            ),
            ("8 gib is fine", ram(gpu("NVIDIA GeForce GTX 1050"), 8192), Full),
            ("two cores", cpus(gpu("NVIDIA GeForce GTX 1050"), 2), Low),
            ("one core", cpus(gpu("NVIDIA GeForce GTX 1050"), 1), Minimal),
        ];
        for (name, inputs, want) in table {
            let d = decide(&inputs);
            assert_eq!(d.profile, want, "{name}: {}", d.reason);
            assert!(!d.reason.is_empty(), "{name}");
        }
    }

    #[test]
    fn vm_with_a_real_gpu_is_not_penalised() {
        let mut i = gpu("NVIDIA GeForce RTX 4070/PCIe/SSE2");
        i.vm = true;
        assert_eq!(decide(&i).profile, Full, "GPU passthrough");
        i.renderer = String::new();
        assert_eq!(decide(&i).profile, Low, "unidentified adapter in a VM");
    }

    #[test]
    fn reasons_name_the_binding_cause() {
        assert!(decide(&gpu("llvmpipe")).reason.contains("software"));
        assert!(decide(&gpu("Mesa QXL")).reason.contains("virtual machine"));
        let mut i = gpu("NVIDIA GeForce GTX 1050");
        i.ram_mb = 1024;
        assert!(decide(&i).reason.contains("RAM"));
        let mut k = gpu("Mesa Intel(R) UHD Graphics 620 (KBL GT2)");
        k.pixels = 3840 * 2160;
        assert!(decide(&k).reason.contains("4K"));
    }

    #[test]
    fn rules_only_lower() {
        // Software rendering with lots of RAM and cores stays Minimal.
        assert_eq!(decide(&gpu("llvmpipe")).profile, Minimal);
        // Nothing in the table can raise a weak GPU above its class.
        let mut i = gpu("Mesa Intel(R) HD Graphics 4000 (IVB GT2)");
        i.ram_mb = 64_000;
        i.cpus = 32;
        assert_eq!(decide(&i).profile, Low);
    }

    #[test]
    fn meminfo_parses() {
        assert_eq!(
            parse_meminfo("MemTotal:       16318156 kB\nMemFree: 1 kB\n"),
            15935
        );
        assert_eq!(parse_meminfo("garbage"), 0);
    }

    // ---- governor, fake clock

    const P: Duration = Duration::from_millis(16);
    const SLOW: Duration = Duration::from_millis(40);
    const FAST: Duration = Duration::from_millis(5);

    /// Feed `fps` frames a second of `frame` from `from_ms` for `secs`.
    fn feed(
        g: &mut Governor,
        from_ms: u64,
        secs: u64,
        fps: u64,
        frame: Duration,
        cur: GfxProfile,
    ) -> Option<GfxProfile> {
        let mut out = None;
        for n in 0..secs * fps {
            let now = from_ms + n * 1000 / fps;
            if let Some(p) = g.observe(now, frame, P, cur) {
                out = Some(p);
            }
        }
        out
    }

    #[test]
    fn steps_down_one_after_sustained_misses() {
        let mut g = Governor::default();
        // Warm-up (30 s) ignores everything, then two bad windows.
        let got = feed(&mut g, 0, 60, 30, SLOW, Full);
        assert_eq!(got, Some(Balanced));
    }

    #[test]
    fn warmup_never_steps() {
        let mut g = Governor::default();
        assert_eq!(feed(&mut g, 0, 29, 60, SLOW, Full), None);
    }

    #[test]
    fn a_healthy_run_never_steps() {
        let mut g = Governor::default();
        assert_eq!(feed(&mut g, 0, 300, 60, FAST, Full), None);
    }

    #[test]
    fn one_bad_window_is_forgiven() {
        let mut g = Governor::default();
        assert_eq!(feed(&mut g, 0, 30, 30, FAST, Full), None);
        assert_eq!(feed(&mut g, 30_000, 10, 30, SLOW, Full), None);
        assert_eq!(
            feed(&mut g, 40_000, 40, 30, FAST, Full),
            None,
            "the run was broken"
        );
    }

    #[test]
    fn an_idle_desktop_says_nothing() {
        let mut g = Governor::default();
        // Slow, but only 5 frames a second: under MIN_FRAMES per window.
        assert_eq!(feed(&mut g, 0, 300, 5, SLOW, Full), None);
    }

    #[test]
    fn cooldown_then_second_step_and_floor_at_low() {
        let mut g = Governor::default();
        assert_eq!(feed(&mut g, 0, 60, 30, SLOW, Full), Some(Balanced));
        // The step lands around 50 s; the next judgement waits 30 s, then
        // needs two more bad windows.
        assert_eq!(feed(&mut g, 60_000, 20, 30, SLOW, Balanced), None, "cooling down");
        let next = feed(&mut g, 80_000, 60, 30, SLOW, Balanced);
        assert_eq!(next, Some(Low));
        // Low and Minimal are never stepped further.
        assert_eq!(feed(&mut g, 140_000, 300, 30, SLOW, Low), None);
        assert_eq!(feed(&mut g, 440_000, 300, 30, SLOW, Minimal), None);
    }

    #[test]
    fn never_steps_up() {
        let mut g = Governor::default();
        for p in GfxProfile::ALL {
            let got = feed(&mut g, 0, 120, 30, FAST, p);
            assert!(got.is_none_or(|n| n < p));
        }
    }
}
