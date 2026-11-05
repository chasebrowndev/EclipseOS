// SPDX-License-Identifier: AGPL-3.0-only
//! COMP-01 §4 — deterministic GPU selection.
//!
//! Every DRM device udev reports for the seat is described from sysfs and
//! ranked. The order is fixed by the spec:
//!
//! 1. Render-capable before not (a `renderD*` node exists alongside the card).
//! 2. Discrete before integrated before virtual.
//! 3. Larger VRAM.
//! 4. PCI domain:bus:device.function ascending — a stable tiebreak, so the
//!    same machine picks the same card across boots even though udev's
//!    enumeration order is not guaranteed.
//!
//! **Discreteness is decided by position on the PCI topology, not by
//! `boot_vga`.** An integrated GPU hangs directly off the host bridge
//! (`/sys/devices/pci0000:00/0000:00:02.0`); a discrete card sits behind a
//! PCIe port bridge (`.../0000:00:01.0/0000:01:00.0`). `boot_vga` cannot carry
//! that decision on its own — on a single-GPU desktop the discrete card is
//! also the boot VGA device — so it is recorded and logged as corroboration
//! only. A device with no PCI parent at all (vkms, and some virtio setups) is
//! virtual and ranks last.
//!
//! VRAM is a *hint*: `mem_info_vram_total` where the driver exposes it
//! (amdgpu), otherwise the largest prefetchable PCI BAR. The proprietary
//! NVIDIA driver publishes neither, and without resizable BAR the aperture is
//! smaller than the real VRAM. It is only ever used to order two cards of the
//! same class, and it is deterministic, which is what the spec asks of it.

use std::{
    cmp::Reverse,
    path::{Path, PathBuf},
};

use smithay::backend::allocator::{Format, Fourcc, Modifier};

use crate::config::GpuMode;

/// Where a device sits in the machine. Ordered best-first, so it can go into
/// the sort key directly.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Class {
    Discrete,
    Integrated,
    Virtual,
}

impl Class {
    fn as_str(self) -> &'static str {
        match self {
            Class::Discrete => "discrete",
            Class::Integrated => "integrated",
            Class::Virtual => "virtual",
        }
    }
}

/// The COMP-01 §4 ranking, in sortable form: not-render-capable last, then
/// class, then VRAM descending, then PCI address, then card path.
type SortKey<'a> = (bool, Class, Reverse<u64>, (u16, u8, u8, u8), &'a Path);

/// One candidate render device.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Gpu {
    /// `/dev/dri/cardN`.
    pub card: PathBuf,
    /// `/dev/dri/renderDN`, when the device exposes one.
    pub render: Option<PathBuf>,
    pub class: Class,
    /// Bytes; see the module note — a hint, not a measurement.
    pub vram: u64,
    /// PCI domain, bus, device, function. `None` for a non-PCI device.
    pub dbdf: Option<(u16, u8, u8, u8)>,
    pub driver: String,
    pub boot_vga: bool,
    /// Every connector on this device, connected or not, from
    /// `/sys/class/drm/cardN-*`. Decides whether the device is worth opening as
    /// a secondary (a connector can be plugged in later).
    pub connectors_total: usize,
    /// Names of the connectors whose sysfs `status` is `connected`
    /// (`eDP-1`, `HDMI-A-2`, ...).
    pub connected: Vec<String>,
}

/// What a GPU does in the multi-GPU arrangement (ADR 0084).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    /// Owns explicit sync, capture and the default dmabuf feedback; renders its
    /// own outputs, and every output in `dedicated` mode.
    Primary,
    /// Renders and scans out its own connectors (`hybrid`), or scans out frames
    /// the primary rendered (`dedicated`, and as a per-frame fallback).
    Secondary,
    /// No connectors, so nothing to drive.
    Unused,
}

impl Role {
    pub fn as_str(self) -> &'static str {
        match self {
            Role::Primary => "primary",
            Role::Secondary => "secondary",
            Role::Unused => "unused",
        }
    }
}

/// Whether a connector name is a built-in panel (eDP / LVDS / DSI).
pub fn is_internal_panel(name: &str) -> bool {
    ["eDP-", "LVDS-", "DSI-"].iter().any(|p| name.starts_with(p))
}

/// ADR 0084: pick the primary GPU from a best-first ranked list; the index is
/// into `ranked`. Only render-capable devices qualify. A lone candidate is
/// taken as is; with several, `dedicated` mode takes the best discrete one,
/// otherwise the one driving a connected built-in panel wins (so the laptop
/// screen is never left dark), then the firmware's `boot_vga` device, then the
/// COMP-01 §4 ranking. `integrated` and `hybrid` pick the same primary; they
/// differ in whether the other GPU is opened ([`opens_as_secondary`]).
pub fn select_primary(ranked: &[Gpu], mode: GpuMode) -> Option<usize> {
    let mut capable = ranked.iter().enumerate().filter(|(_, g)| g.render.is_some());
    let first = capable.next()?.0;
    let capable = ranked.iter().enumerate().filter(|(_, g)| g.render.is_some());
    if capable.clone().nth(1).is_none() {
        return Some(first);
    }
    if mode == GpuMode::Dedicated {
        if let Some((i, _)) = capable.clone().find(|(_, g)| g.class == Class::Discrete) {
            return Some(i);
        }
    }
    capable
        .clone()
        .find(|(_, g)| g.has_internal_panel())
        .or_else(|| capable.clone().find(|(_, g)| g.boot_vga))
        .map(|(i, _)| i)
        .or(Some(first))
}

/// Whether a non-primary GPU is brought up at all: it needs a connector, and
/// `integrated` mode never opens a discrete GPU (it stays dark so it can power
/// off).
pub fn opens_as_secondary(g: &Gpu, mode: GpuMode) -> bool {
    g.connectors_total > 0 && !(mode == GpuMode::Integrated && g.class == Class::Discrete)
}

/// How an output's pixels reach its connector (ADR 0086, amended).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OutputRoute {
    /// Composited by the GPU that owns the connector: every primary-GPU output
    /// (so every output of a single-GPU box), and a secondary's with
    /// `secondary-compositing` on.
    Own,
    /// The primary GPU renders into a LINEAR dmabuf it allocated and the
    /// secondary's KMS imports that buffer for scanout (PRIME import +
    /// `drmModeAddFB2`). No GL context of the secondary touches a pixel.
    ImportScanout,
    /// The primary renders, reads the frame back into system memory and the
    /// secondary uploads it. Slower, but needs no cross-device buffer sharing.
    CpuCopy,
}

impl OutputRoute {
    pub fn as_str(self) -> &'static str {
        match self {
            OutputRoute::Own => "own-gpu",
            OutputRoute::ImportScanout => "import-scanout",
            OutputRoute::CpuCopy => "cpu-copy",
        }
    }

    /// The primary GPU renders this output's frames.
    pub fn renders_on_primary(self) -> bool {
        self != OutputRoute::Own
    }
}

/// Whether an output's frames are rendered on the primary rather than by its
/// own GPU (ADR 0086). Never for the primary's own outputs. In `dedicated`
/// always. Otherwise by default for a secondary, unless `secondary_compositing`
/// opts into native compositing.
pub fn wants_primary_render(mode: GpuMode, secondary: bool, secondary_compositing: bool) -> bool {
    secondary && (mode == GpuMode::Dedicated || !secondary_compositing)
}

/// The route an output takes, given what was found out at bring-up.
/// `import_ok`: a probe scanned a primary-allocated LINEAR buffer out on the
/// secondary successfully. `copy_format`: a transfer format both GPUs share.
/// Preference: import-scanout, then CPU copy, then (outside `dedicated`) the
/// output's own GPU so it still lights. `dedicated` with neither is `CpuCopy`
/// and the caller refuses the output.
pub fn select_route(
    mode: GpuMode,
    secondary: bool,
    secondary_compositing: bool,
    import_ok: bool,
    copy_format: bool,
) -> OutputRoute {
    if !wants_primary_render(mode, secondary, secondary_compositing) {
        OutputRoute::Own
    } else if import_ok {
        OutputRoute::ImportScanout
    } else if copy_format || mode == GpuMode::Dedicated {
        OutputRoute::CpuCopy
    } else {
        OutputRoute::Own
    }
}

/// Consecutive unqueueable frames after which an import-scanout output gives
/// up on importing and drops to the CPU copy for good.
pub const IMPORT_STRIKES: u32 = 3;

/// Sticky demotion: `ImportScanout` becomes `CpuCopy` after
/// [`IMPORT_STRIKES`] failed frames in a row when a copy format exists; a
/// route never moves back up, nor does any other route change.
pub fn demote(route: OutputRoute, strikes: u32, copy_format: bool) -> OutputRoute {
    if route == OutputRoute::ImportScanout && strikes >= IMPORT_STRIKES && copy_format {
        OutputRoute::CpuCopy
    } else {
        route
    }
}

/// Colour formats tried, in order, for a buffer scanned out on another GPU.
/// NVIDIA's planes take linear XRGB/ARGB8888 everywhere.
pub const IMPORT_COLOR_FORMATS: [Fourcc; 4] = [
    Fourcc::Argb8888,
    Fourcc::Xrgb8888,
    Fourcc::Abgr8888,
    Fourcc::Xbgr8888,
];

/// The formats the primary renders LINEAR that the secondary's primary plane
/// also takes (exactly, or as the opaque twin of an alpha format). Empty
/// means no import-scanout buffer can exist.
pub fn linear_scanout_formats(primary_render: &[Format], plane: &[Format]) -> Vec<Format> {
    let opaque = |c: Fourcc| match c {
        Fourcc::Argb8888 => Fourcc::Xrgb8888,
        Fourcc::Abgr8888 => Fourcc::Xbgr8888,
        o => o,
    };
    primary_render
        .iter()
        .filter(|f| f.modifier == Modifier::Linear && IMPORT_COLOR_FORMATS.contains(&f.code))
        .filter(|f| {
            plane
                .iter()
                .any(|p| p.modifier == Modifier::Linear && (p.code == f.code || p.code == opaque(f.code)))
        })
        .copied()
        .collect()
}

/// Role of every GPU in `ranked` given the chosen primary. A secondary is any
/// other device with at least one connector.
pub fn roles(ranked: &[Gpu], primary: usize) -> Vec<Role> {
    ranked
        .iter()
        .enumerate()
        .map(|(i, g)| {
            if i == primary {
                Role::Primary
            } else if g.connectors_total > 0 {
                Role::Secondary
            } else {
                Role::Unused
            }
        })
        .collect()
}

impl Gpu {
    /// A built-in panel is lit on this device.
    pub fn has_internal_panel(&self) -> bool {
        self.connected.iter().any(|n| is_internal_panel(n))
    }

    /// The COMP-01 §4 ordering, as a tuple that sorts ascending into it.
    fn key(&self) -> SortKey<'_> {
        (
            self.render.is_none(),
            self.class,
            Reverse(self.vram),
            // A non-PCI device has no address to tie-break on; sort it after
            // everything that does, then fall through to the card path.
            self.dbdf.unwrap_or((u16::MAX, u8::MAX, u8::MAX, u8::MAX)),
            &self.card,
        )
    }

    /// One line for the startup log, per "logged at startup with the full
    /// ranked list and the reason".
    pub fn describe(&self) -> String {
        let addr = match self.dbdf {
            Some((d, b, dev, f)) => format!("{d:04x}:{b:02x}:{dev:02x}.{f}"),
            None => "no-pci".to_string(),
        };
        format!(
            "{} [{} {} vram={}MiB connected=[{}] of {} render={} boot_vga={}]",
            self.card.display(),
            addr,
            self.class.as_str(),
            self.vram / (1024 * 1024),
            self.connected.join(","),
            self.connectors_total,
            if self.render.is_some() { "yes" } else { "no" },
            self.boot_vga,
        )
    }
}

/// Sort `gpus` best-first. Pure, so the ordering is unit-testable without a
/// second GPU in the machine.
pub fn rank(gpus: &mut [Gpu]) {
    gpus.sort_by(|a, b| a.key().cmp(&b.key()));
}

/// Pixel formats tried, in order, for the offscreen buffer a secondary output's
/// frame is rendered into: opaque first, because an output has no use for alpha.
const TRANSFER_FORMATS: [Fourcc; 4] = [
    Fourcc::Xrgb8888,
    Fourcc::Argb8888,
    Fourcc::Xbgr8888,
    Fourcc::Abgr8888,
];

/// ADR 0084: the format and modifiers for the buffer the primary renders a
/// secondary output into: one the primary can render to and the secondary can
/// sample from. The first of [`TRANSFER_FORMATS`] with a common modifier wins.
/// `LINEAR` is taken alone when it is common, since it is the one layout two
/// different drivers reliably agree on; otherwise every common modifier is
/// offered to the allocator.
pub fn pick_transfer_format(
    primary_render: &[Format],
    secondary_texture: &[Format],
) -> Option<(Fourcc, Vec<Modifier>)> {
    TRANSFER_FORMATS.iter().find_map(|code| {
        let mut mods: Vec<Modifier> = primary_render
            .iter()
            .filter(|f| f.code == *code && secondary_texture.contains(f))
            .map(|f| f.modifier)
            .collect();
        if mods.contains(&Modifier::Linear) {
            mods = vec![Modifier::Linear];
        }
        (!mods.is_empty()).then_some((*code, mods))
    })
}

/// Formats every set contains, in the order of the first set. Empty when there
/// are no sets.
pub fn common_formats(sets: &[&[Format]]) -> Vec<Format> {
    let Some((first, rest)) = sets.split_first() else {
        return Vec::new();
    };
    first
        .iter()
        .filter(|f| rest.iter().all(|s| s.contains(f)))
        .copied()
        .collect()
}

/// What one GPU can offer a client (ADR 0084).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FeedbackGpu {
    pub render_node: libc::dev_t,
    /// The formats its renderer can sample.
    pub texture: Vec<Format>,
}

/// One tranche of a surface's dmabuf feedback, most preferred first.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Tranche {
    pub target: libc::dev_t,
    /// A direct-scanout tranche (`zwp_linux_dmabuf_feedback_v1.tranche_flags.scanout`).
    pub scanout: bool,
    pub formats: Vec<Format>,
}

/// ADR 0084: the tranches of a surface whose main output is rendered by
/// `gpus[main]`: the output's plane formats when the surface can be scanned out
/// directly, then that GPU's own formats, then for every other GPU a tranche
/// of the formats *every* active GPU can import (LINEAR in practice), so a
/// client rendering on that GPU still allocates something this output can
/// sample. The main device is `gpus[main].render_node`.
pub fn plan_feedback(
    main: usize,
    gpus: &[FeedbackGpu],
    scanout: Option<(libc::dev_t, &[Format])>,
) -> Vec<Tranche> {
    let mut out = Vec::new();
    let Some(home) = gpus.get(main) else {
        return out;
    };
    if let Some((target, formats)) = scanout.filter(|(_, f)| !f.is_empty()) {
        out.push(Tranche {
            target,
            scanout: true,
            formats: formats.to_vec(),
        });
    }
    // The primary renders the capture and every copied frame (ADR 0086), so a
    // secondary's own tranche only offers formats the primary can also sample:
    // a buffer one of them cannot import is never advertised.
    let home_formats: Vec<Format> = match gpus.first().filter(|_| main != 0) {
        Some(primary) => home
            .texture
            .iter()
            .filter(|f| primary.texture.contains(f))
            .copied()
            .collect(),
        None => home.texture.clone(),
    };
    if !home_formats.is_empty() {
        out.push(Tranche {
            target: home.render_node,
            scanout: false,
            formats: home_formats,
        });
    }
    let sets: Vec<&[Format]> = gpus.iter().map(|g| g.texture.as_slice()).collect();
    let shared = common_formats(&sets);
    if !shared.is_empty() {
        for (i, g) in gpus.iter().enumerate() {
            if i != main && g.render_node != home.render_node {
                out.push(Tranche {
                    target: g.render_node,
                    scanout: false,
                    formats: shared.clone(),
                });
            }
        }
    }
    out
}

/// The environment that sends a client to `g` instead of the default GPU: the
/// NVIDIA PRIME render-offload variables, or `DRI_PRIME` for a Mesa device.
pub fn offload_env(g: &Gpu) -> Vec<(&'static str, String)> {
    if g.driver.starts_with("nvidia") {
        return vec![
            ("__NV_PRIME_RENDER_OFFLOAD", "1".into()),
            ("__VK_LAYER_NV_optimus", "NVIDIA_only".into()),
            ("__GLX_VENDOR_LIBRARY_NAME", "nvidia".into()),
        ];
    }
    match g.dbdf {
        Some((d, b, dev, f)) => vec![("DRI_PRIME", format!("pci-{d:04x}_{b:02x}_{dev:02x}_{f}"))],
        None => Vec::new(),
    }
}

/// Parse a PCI address as sysfs writes it: `0000:01:00.0`.
fn parse_dbdf(s: &str) -> Option<(u16, u8, u8, u8)> {
    let (domain, rest) = s.split_once(':')?;
    let (bus, rest) = rest.split_once(':')?;
    let (dev, func) = rest.split_once('.')?;
    Some((
        u16::from_str_radix(domain, 16).ok()?,
        u8::from_str_radix(bus, 16).ok()?,
        u8::from_str_radix(dev, 16).ok()?,
        u8::from_str_radix(func, 16).ok()?,
    ))
}

/// Largest prefetchable BAR, as a stand-in for VRAM. Lines of `resource` are
/// `start end flags`; bit 3 of the flags marks the region prefetchable, which
/// is what a framebuffer aperture is and what a register window is not.
fn largest_prefetchable_bar(dev: &Path) -> u64 {
    let Ok(text) = std::fs::read_to_string(dev.join("resource")) else {
        return 0;
    };
    text.lines()
        .filter_map(|l| {
            let mut it = l.split_whitespace();
            let start = u64::from_str_radix(it.next()?.trim_start_matches("0x"), 16).ok()?;
            let end = u64::from_str_radix(it.next()?.trim_start_matches("0x"), 16).ok()?;
            let flags = u64::from_str_radix(it.next()?.trim_start_matches("0x"), 16).ok()?;
            (end > start && flags & 0x8 != 0).then(|| end - start + 1)
        })
        .max()
        .unwrap_or(0)
}

fn read_u64(p: PathBuf) -> Option<u64> {
    std::fs::read_to_string(p).ok()?.trim().parse().ok()
}

/// Describe one `/dev/dri/cardN` from sysfs.
fn describe_card(card: &Path) -> Gpu {
    let name = card
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let sys = PathBuf::from("/sys/class/drm").join(&name);
    // `device` is a symlink into /sys/devices; canonicalize so the parent walk
    // below sees the real topology rather than the class-directory shortcut.
    let dev = std::fs::canonicalize(sys.join("device")).unwrap_or_else(|_| sys.join("device"));

    let render = std::fs::read_dir(dev.join("drm"))
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .find(|n| n.starts_with("renderD"))
        .map(|n| PathBuf::from("/dev/dri").join(n));

    let driver = std::fs::read_link(dev.join("driver"))
        .ok()
        .and_then(|p| p.file_name().map(|n| n.to_string_lossy().into_owned()))
        .unwrap_or_else(|| "unknown".to_string());

    let dbdf = dev.file_name().and_then(|n| parse_dbdf(&n.to_string_lossy()));

    // Integrated iff the device hangs directly off a PCI host bridge; that
    // directory is named `pciDDDD:BB`, where a PCIe port bridge is `DDDD:BB:DD.F`.
    let on_root_complex = dev
        .parent()
        .and_then(|p| p.file_name())
        .is_some_and(|n| n.to_string_lossy().starts_with("pci"));
    let class = match dbdf {
        None => Class::Virtual,
        Some(_) if on_root_complex => Class::Integrated,
        Some(_) => Class::Discrete,
    };

    let vram = read_u64(dev.join("mem_info_vram_total")).unwrap_or_else(|| {
        if dbdf.is_some() {
            largest_prefetchable_bar(&dev)
        } else {
            0
        }
    });

    // `cardN-<connector>` directories; the suffix is the kernel's connector
    // name, and `status` says whether a display is attached.
    let prefix = format!("{name}-");
    let mut connectors_total = 0;
    let mut connected = Vec::new();
    for e in std::fs::read_dir("/sys/class/drm")
        .into_iter()
        .flatten()
        .flatten()
    {
        let fname = e.file_name().to_string_lossy().into_owned();
        let Some(conn) = fname.strip_prefix(&prefix) else {
            continue;
        };
        connectors_total += 1;
        let status = std::fs::read_to_string(e.path().join("status")).unwrap_or_default();
        if status.trim() == "connected" {
            connected.push(conn.to_string());
        }
    }
    connected.sort();

    Gpu {
        card: card.to_path_buf(),
        render,
        class,
        vram,
        dbdf,
        driver,
        boot_vga: read_u64(dev.join("boot_vga")) == Some(1),
        connectors_total,
        connected,
    }
}

/// Describe and rank every DRM device udev reports for `seat`, best first.
pub fn probe_seat(seat: &str) -> std::io::Result<Vec<Gpu>> {
    Ok(probe(&smithay::backend::udev::all_gpus(seat)?))
}

/// Describe and rank the given card nodes, best first.
pub fn probe(cards: &[PathBuf]) -> Vec<Gpu> {
    let mut gpus: Vec<Gpu> = cards.iter().map(|c| describe_card(c)).collect();
    rank(&mut gpus);
    gpus
}

#[cfg(test)]
mod tests {
    use super::*;

    fn gpu(card: &str, class: Class, vram_mib: u64, dbdf: Option<(u16, u8, u8, u8)>) -> Gpu {
        Gpu {
            card: PathBuf::from(card),
            render: Some(PathBuf::from("/dev/dri/renderD128")),
            class,
            vram: vram_mib * 1024 * 1024,
            dbdf,
            driver: "test".into(),
            boot_vga: false,
            connectors_total: 1,
            connected: Vec::new(),
        }
    }

    fn with(mut g: Gpu, connected: &[&str], boot_vga: bool) -> Gpu {
        g.connected = connected.iter().map(|s| s.to_string()).collect();
        g.connectors_total = g.connectors_total.max(g.connected.len());
        g.boot_vga = boot_vga;
        g
    }

    /// The target laptop: NVIDIA (HDMI only) outranks the Intel iGPU that owns
    /// the built-in panel and is boot_vga.
    fn laptop() -> Vec<Gpu> {
        let mut v = vec![
            with(
                gpu("/dev/dri/card0", Class::Discrete, 6144, Some((0, 1, 0, 0))),
                &["HDMI-A-2"],
                false,
            ),
            with(
                gpu("/dev/dri/card1", Class::Integrated, 256, Some((0, 0, 2, 0))),
                &["eDP-1"],
                true,
            ),
        ];
        rank(&mut v);
        v
    }

    #[test]
    fn connected_internal_panel_owner_beats_discrete() {
        let v = laptop();
        assert_eq!(
            v[0].class,
            Class::Discrete,
            "ranking alone still prefers the dGPU"
        );
        let p = select_primary(&v, GpuMode::Hybrid).unwrap();
        assert_eq!(v[p].card, PathBuf::from("/dev/dri/card1"));
        assert_eq!(roles(&v, p), [Role::Secondary, Role::Primary]);
    }

    #[test]
    fn boot_vga_breaks_the_tie_when_no_panel_is_connected() {
        let mut v = vec![
            with(
                gpu("/dev/dri/card0", Class::Discrete, 6144, Some((0, 1, 0, 0))),
                &["HDMI-A-1"],
                false,
            ),
            with(
                gpu("/dev/dri/card1", Class::Integrated, 256, Some((0, 0, 2, 0))),
                &["HDMI-A-2"],
                true,
            ),
        ];
        rank(&mut v);
        let p = select_primary(&v, GpuMode::Hybrid).unwrap();
        assert_eq!(v[p].card, PathBuf::from("/dev/dri/card1"));
    }

    #[test]
    fn ranking_decides_without_a_panel_or_boot_vga() {
        let mut v = vec![
            with(
                gpu("/dev/dri/card0", Class::Integrated, 256, Some((0, 0, 2, 0))),
                &["HDMI-A-1"],
                false,
            ),
            with(
                gpu("/dev/dri/card1", Class::Discrete, 6144, Some((0, 1, 0, 0))),
                &["DP-1"],
                false,
            ),
        ];
        rank(&mut v);
        let p = select_primary(&v, GpuMode::Hybrid).unwrap();
        assert_eq!(v[p].card, PathBuf::from("/dev/dri/card1"));
    }

    #[test]
    fn a_single_gpu_is_taken_as_is() {
        let v = vec![with(
            gpu("/dev/dri/card0", Class::Discrete, 6144, Some((0, 1, 0, 0))),
            &["DP-1"],
            false,
        )];
        assert_eq!(select_primary(&v, GpuMode::Hybrid), Some(0));
        assert_eq!(roles(&v, 0), [Role::Primary]);
    }

    #[test]
    fn a_display_only_panel_owner_cannot_be_primary() {
        let mut igpu = with(
            gpu("/dev/dri/card0", Class::Integrated, 256, Some((0, 0, 2, 0))),
            &["eDP-1"],
            true,
        );
        igpu.render = None;
        let mut v = vec![
            with(
                gpu("/dev/dri/card1", Class::Discrete, 6144, Some((0, 1, 0, 0))),
                &["HDMI-A-1"],
                false,
            ),
            igpu,
        ];
        rank(&mut v);
        let p = select_primary(&v, GpuMode::Hybrid).unwrap();
        assert_eq!(v[p].card, PathBuf::from("/dev/dri/card1"));
    }

    #[test]
    fn no_render_node_anywhere_selects_nothing() {
        let mut g = gpu("/dev/dri/card0", Class::Discrete, 1, Some((0, 1, 0, 0)));
        g.render = None;
        assert_eq!(select_primary(&[g], GpuMode::Hybrid), None);
    }

    #[test]
    fn a_device_without_connectors_is_unused() {
        let mut idle = gpu("/dev/dri/card2", Class::Virtual, 0, None);
        idle.connectors_total = 0;
        let v = vec![
            with(
                gpu("/dev/dri/card0", Class::Discrete, 6144, Some((0, 1, 0, 0))),
                &["DP-1"],
                false,
            ),
            idle,
        ];
        assert_eq!(roles(&v, 0), [Role::Primary, Role::Unused]);
    }

    fn fmt(code: Fourcc, modifier: Modifier) -> Format {
        Format { code, modifier }
    }

    #[test]
    fn transfer_format_prefers_opaque_linear() {
        let primary = [
            fmt(Fourcc::Argb8888, Modifier::Linear),
            fmt(Fourcc::Xrgb8888, Modifier::Invalid),
            fmt(Fourcc::Xrgb8888, Modifier::Linear),
        ];
        let secondary = [
            fmt(Fourcc::Xrgb8888, Modifier::Linear),
            fmt(Fourcc::Xrgb8888, Modifier::Invalid),
            fmt(Fourcc::Argb8888, Modifier::Linear),
        ];
        assert_eq!(
            pick_transfer_format(&primary, &secondary),
            Some((Fourcc::Xrgb8888, vec![Modifier::Linear]))
        );
    }

    #[test]
    fn transfer_format_falls_back_to_alpha_and_to_non_linear() {
        // Only the alpha variant is shared.
        let p = [
            fmt(Fourcc::Xrgb8888, Modifier::Linear),
            fmt(Fourcc::Argb8888, Modifier::Linear),
        ];
        let s = [fmt(Fourcc::Argb8888, Modifier::Linear)];
        assert_eq!(
            pick_transfer_format(&p, &s),
            Some((Fourcc::Argb8888, vec![Modifier::Linear]))
        );
        // Shared, but not linear: every common modifier is offered.
        let m1 = Modifier::from(0x0100_0000_0000_0001);
        let m2 = Modifier::from(0x0100_0000_0000_0002);
        let p = [fmt(Fourcc::Xrgb8888, m1), fmt(Fourcc::Xrgb8888, m2)];
        let s = [fmt(Fourcc::Xrgb8888, m2), fmt(Fourcc::Xrgb8888, m1)];
        assert_eq!(
            pick_transfer_format(&p, &s),
            Some((Fourcc::Xrgb8888, vec![m1, m2]))
        );
    }

    #[test]
    fn transfer_format_is_none_without_an_intersection() {
        let p = [fmt(Fourcc::Xrgb8888, Modifier::Linear)];
        let s = [
            fmt(Fourcc::Xrgb8888, Modifier::Invalid),
            fmt(Fourcc::Rgb565, Modifier::Linear),
        ];
        assert_eq!(pick_transfer_format(&p, &s), None);
        assert_eq!(pick_transfer_format(&[], &s), None);
    }

    #[test]
    fn dedicated_takes_the_discrete_gpu_even_with_the_panel_elsewhere() {
        let v = laptop();
        let p = select_primary(&v, GpuMode::Dedicated).unwrap();
        assert_eq!(v[p].card, PathBuf::from("/dev/dri/card0"));
        // The other two modes keep the panel owner.
        for mode in [GpuMode::Hybrid, GpuMode::Integrated] {
            let p = select_primary(&v, mode).unwrap();
            assert_eq!(v[p].card, PathBuf::from("/dev/dri/card1"));
        }
    }

    #[test]
    fn dedicated_without_a_discrete_gpu_falls_back_to_the_usual_pick() {
        let v = vec![with(
            gpu("/dev/dri/card0", Class::Integrated, 256, Some((0, 0, 2, 0))),
            &["eDP-1"],
            true,
        )];
        assert_eq!(select_primary(&v, GpuMode::Dedicated), Some(0));
    }

    #[test]
    fn integrated_never_opens_the_discrete_secondary() {
        let v = laptop();
        let dgpu = v.iter().find(|g| g.class == Class::Discrete).unwrap();
        let igpu = v.iter().find(|g| g.class == Class::Integrated).unwrap();
        assert!(!opens_as_secondary(dgpu, GpuMode::Integrated));
        assert!(opens_as_secondary(dgpu, GpuMode::Hybrid));
        assert!(opens_as_secondary(dgpu, GpuMode::Dedicated));
        assert!(opens_as_secondary(igpu, GpuMode::Integrated));
        let mut idle = dgpu.clone();
        idle.connectors_total = 0;
        assert!(!opens_as_secondary(&idle, GpuMode::Hybrid));
    }

    #[test]
    fn route_selection() {
        use GpuMode::*;
        use OutputRoute::*;
        // Primary-GPU outputs, and so every output of a single-GPU box, never
        // take a cross-GPU route, whatever the probes or options say.
        for m in [Integrated, Hybrid, Dedicated] {
            for opt in [false, true] {
                for import in [false, true] {
                    for fmt in [false, true] {
                        assert!(!wants_primary_render(m, false, opt));
                        assert_eq!(select_route(m, false, opt, import, fmt), Own);
                    }
                }
            }
        }
        // A secondary prefers importing, then the CPU copy ...
        assert_eq!(select_route(Hybrid, true, false, true, true), ImportScanout);
        assert_eq!(select_route(Integrated, true, false, true, false), ImportScanout);
        assert_eq!(select_route(Hybrid, true, false, false, true), CpuCopy);
        // ... native compositing is opt-in ...
        assert_eq!(select_route(Hybrid, true, true, true, true), Own);
        // ... with nothing shared it lights natively rather than staying dark ...
        assert_eq!(select_route(Hybrid, true, false, false, false), Own);
        // ... and dedicated never composites on the secondary.
        assert_eq!(select_route(Dedicated, true, true, true, true), ImportScanout);
        assert_eq!(select_route(Dedicated, true, false, false, false), CpuCopy);
    }

    #[test]
    fn demotion_is_sticky_and_one_way() {
        use OutputRoute::*;
        assert_eq!(demote(ImportScanout, IMPORT_STRIKES - 1, true), ImportScanout);
        assert_eq!(demote(ImportScanout, IMPORT_STRIKES, true), CpuCopy);
        // No copy format: stay put rather than strand the output.
        assert_eq!(demote(ImportScanout, 99, false), ImportScanout);
        // Nothing climbs back, and the other routes never move.
        assert_eq!(demote(CpuCopy, 0, true), CpuCopy);
        assert_eq!(demote(Own, 99, true), Own);
    }

    #[test]
    fn linear_scanout_intersection() {
        let tiled = Modifier::from(0x0100_0000_0000_0001);
        let primary = [
            fmt(Fourcc::Argb8888, tiled),
            fmt(Fourcc::Argb8888, Modifier::Linear),
            fmt(Fourcc::Nv12, Modifier::Linear),
        ];
        // The plane takes only the opaque twin: still importable.
        let plane = [
            fmt(Fourcc::Xrgb8888, Modifier::Linear),
            fmt(Fourcc::Xrgb8888, tiled),
        ];
        assert_eq!(
            linear_scanout_formats(&primary, &plane),
            vec![fmt(Fourcc::Argb8888, Modifier::Linear)]
        );
        // A plane with only a vendor-tiled layout cannot take our buffer.
        assert!(linear_scanout_formats(&primary, &[fmt(Fourcc::Xrgb8888, tiled)]).is_empty());
        // A primary that cannot render LINEAR has nothing to offer.
        assert!(linear_scanout_formats(&[fmt(Fourcc::Argb8888, tiled)], &plane).is_empty());
    }

    fn feedback_gpus() -> Vec<FeedbackGpu> {
        let m = Modifier::from(0x0100_0000_0000_0001);
        vec![
            // Intel: tiled and linear.
            FeedbackGpu {
                render_node: 100,
                texture: vec![
                    fmt(Fourcc::Xrgb8888, m),
                    fmt(Fourcc::Xrgb8888, Modifier::Linear),
                    fmt(Fourcc::Argb8888, Modifier::Linear),
                ],
            },
            // NVIDIA: its own layout and linear.
            FeedbackGpu {
                render_node: 200,
                texture: vec![
                    fmt(Fourcc::Xrgb8888, Modifier::Linear),
                    fmt(Fourcc::Xrgb8888, Modifier::Invalid),
                ],
            },
        ]
    }

    #[test]
    fn common_formats_are_what_every_set_has() {
        let g = feedback_gpus();
        let sets: Vec<&[Format]> = g.iter().map(|g| g.texture.as_slice()).collect();
        assert_eq!(common_formats(&sets), [fmt(Fourcc::Xrgb8888, Modifier::Linear)]);
        assert!(common_formats(&[]).is_empty());
    }

    #[test]
    fn a_surface_on_the_igpu_is_led_by_the_igpu() {
        let g = feedback_gpus();
        let plan = plan_feedback(0, &g, None);
        assert_eq!(plan[0].target, 100);
        assert_eq!(plan[0].formats, g[0].texture);
        // The NVIDIA device gets the shared formats only.
        assert_eq!(plan.len(), 2);
        assert_eq!(plan[1].target, 200);
        assert_eq!(plan[1].formats, [fmt(Fourcc::Xrgb8888, Modifier::Linear)]);
        assert!(plan.iter().all(|t| !t.scanout));
    }

    #[test]
    fn a_surface_on_the_dgpu_is_led_by_the_dgpu() {
        let g = feedback_gpus();
        let plan = plan_feedback(1, &g, None);
        assert_eq!(plan[0].target, 200);
        assert_eq!(plan[1].target, 100);
        assert_eq!(plan[1].formats, [fmt(Fourcc::Xrgb8888, Modifier::Linear)]);
    }

    #[test]
    fn a_secondary_tranche_offers_only_what_the_primary_can_sample() {
        let mut g = feedback_gpus();
        // A tiled layout only the dGPU understands must not be advertised.
        let tiled = fmt(Fourcc::Xrgb8888, Modifier::from(0x0300_0000_0000_0001u64));
        g[1].texture.push(tiled);
        let plan = plan_feedback(1, &g, None);
        assert_eq!(plan[0].target, 200);
        assert!(!plan[0].formats.contains(&tiled));
        assert!(plan.iter().all(|t| !t.formats.contains(&tiled)));
    }

    #[test]
    fn the_scanout_tranche_comes_first_and_one_gpu_has_no_fallback() {
        let g = feedback_gpus();
        let planes = [fmt(Fourcc::Xrgb8888, Modifier::Linear)];
        let plan = plan_feedback(1, &g, Some((300, &planes)));
        assert!(plan[0].scanout);
        assert_eq!(plan[0].target, 300);
        assert_eq!(plan[1].target, 200);
        // A single active GPU: its tranche, no fallback.
        let plan = plan_feedback(0, &g[..1], None);
        assert_eq!(plan.len(), 1);
        // Nothing in common: no fallback tranche to offer.
        let disjoint = vec![
            g[0].clone(),
            FeedbackGpu {
                render_node: 200,
                texture: vec![fmt(Fourcc::Rgb565, Modifier::Linear)],
            },
        ];
        assert_eq!(plan_feedback(0, &disjoint, None).len(), 1);
        assert!(plan_feedback(5, &g, None).is_empty());
    }

    #[test]
    fn one_gpu_is_the_pre_hybrid_shape() {
        // Single GPU: [scanout, its own formats], no shared tranche; without a
        // scanout candidate just its own formats (what #176 and before sent).
        let g = feedback_gpus();
        let one = &g[..1];
        let planes = [fmt(Fourcc::Xrgb8888, Modifier::Linear)];
        let plan = plan_feedback(0, one, Some((300, &planes)));
        assert_eq!(plan.len(), 2);
        assert!(plan[0].scanout && plan[0].target == 300 && plan[0].formats == planes);
        assert!(!plan[1].scanout && plan[1].target == 100 && plan[1].formats == one[0].texture);
        let plan = plan_feedback(0, one, None);
        assert_eq!(plan.len(), 1);
        assert!(!plan[0].scanout && plan[0].formats == one[0].texture);
    }

    #[test]
    fn offload_env_follows_the_driver() {
        let mut g = gpu("/dev/dri/card0", Class::Discrete, 6144, Some((0, 1, 0, 0)));
        g.driver = "nvidia".into();
        let env = offload_env(&g);
        assert!(env.contains(&("__NV_PRIME_RENDER_OFFLOAD", "1".to_string())));
        g.driver = "amdgpu".into();
        g.dbdf = Some((0, 0x03, 0, 0));
        assert_eq!(offload_env(&g), [("DRI_PRIME", "pci-0000_03_00_0".to_string())]);
        g.dbdf = None;
        assert!(offload_env(&g).is_empty());
    }

    #[test]
    fn recognises_built_in_panels() {
        assert!(is_internal_panel("eDP-1"));
        assert!(is_internal_panel("LVDS-1"));
        assert!(is_internal_panel("DSI-2"));
        assert!(!is_internal_panel("HDMI-A-1"));
        assert!(!is_internal_panel("DP-1"));
    }

    /// The hybrid-laptop shape: the iGPU is boot_vga and enumerates first, and
    /// must still lose to the discrete card.
    #[test]
    fn discrete_beats_integrated_regardless_of_enumeration_order() {
        let mut v = vec![
            gpu("/dev/dri/card0", Class::Integrated, 256, Some((0, 0, 2, 0))),
            gpu("/dev/dri/card1", Class::Discrete, 256, Some((0, 1, 0, 0))),
        ];
        rank(&mut v);
        assert_eq!(v[0].card, PathBuf::from("/dev/dri/card1"));
    }

    /// A virtual device (vkms) ranks below real hardware even though its VRAM
    /// hint of 0 never gets consulted.
    #[test]
    fn a_virtual_device_ranks_last() {
        let mut v = vec![
            gpu("/dev/dri/card2", Class::Virtual, 0, None),
            gpu("/dev/dri/card1", Class::Discrete, 8192, Some((0, 1, 0, 0))),
            gpu("/dev/dri/card0", Class::Integrated, 256, Some((0, 0, 2, 0))),
        ];
        rank(&mut v);
        let order: Vec<_> = v.iter().map(|g| g.class).collect();
        assert_eq!(order, [Class::Discrete, Class::Integrated, Class::Virtual]);
    }

    /// Two discrete cards: VRAM decides.
    #[test]
    fn more_vram_wins_within_a_class() {
        let mut v = vec![
            gpu("/dev/dri/card0", Class::Discrete, 4096, Some((0, 1, 0, 0))),
            gpu("/dev/dri/card1", Class::Discrete, 16384, Some((0, 2, 0, 0))),
        ];
        rank(&mut v);
        assert_eq!(v[0].vram, 16384 * 1024 * 1024);
    }

    /// Identical cards tie-break on PCI address, ascending, so the choice is
    /// the same on every boot.
    #[test]
    fn identical_cards_tiebreak_on_pci_address() {
        let mut v = vec![
            gpu("/dev/dri/card1", Class::Discrete, 8192, Some((0, 0x41, 0, 0))),
            gpu("/dev/dri/card0", Class::Discrete, 8192, Some((0, 0x01, 0, 0))),
        ];
        rank(&mut v);
        assert_eq!(v[0].dbdf, Some((0, 0x01, 0, 0)));
        // ...and reversing the input does not change the answer.
        v.reverse();
        rank(&mut v);
        assert_eq!(v[0].dbdf, Some((0, 0x01, 0, 0)));
    }

    /// A card with no render node loses to one that has it, whatever else it
    /// scores — a display-only device cannot be the render device.
    #[test]
    fn render_capability_outranks_everything() {
        let mut display_only = gpu("/dev/dri/card0", Class::Discrete, 24576, Some((0, 1, 0, 0)));
        display_only.render = None;
        let mut v = vec![display_only, gpu("/dev/dri/card1", Class::Virtual, 0, None)];
        rank(&mut v);
        assert_eq!(v[0].card, PathBuf::from("/dev/dri/card1"));
    }

    #[test]
    fn parses_a_sysfs_pci_address() {
        assert_eq!(parse_dbdf("0000:01:00.0"), Some((0, 1, 0, 0)));
        assert_eq!(parse_dbdf("10a0:ff:1f.7"), Some((0x10a0, 0xff, 0x1f, 7)));
        assert_eq!(parse_dbdf("vkms"), None);
    }
}
