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
    /// Connectors on this device, from `/sys/class/drm/cardN-*`.
    pub connectors: usize,
}

impl Gpu {
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
            "{} [{} {} vram={}MiB connectors={} render={} boot_vga={}]",
            self.card.display(),
            addr,
            self.class.as_str(),
            self.vram / (1024 * 1024),
            self.connectors,
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

    let connectors = std::fs::read_dir("/sys/class/drm")
        .into_iter()
        .flatten()
        .flatten()
        .filter(|e| e.file_name().to_string_lossy().starts_with(&format!("{name}-")))
        .count();

    Gpu {
        card: card.to_path_buf(),
        render,
        class,
        vram,
        dbdf,
        driver,
        boot_vga: read_u64(dev.join("boot_vga")) == Some(1),
        connectors,
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
            connectors: 1,
        }
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
