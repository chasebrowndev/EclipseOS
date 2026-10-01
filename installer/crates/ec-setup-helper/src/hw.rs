// SPDX-License-Identifier: AGPL-3.0-only
//! What the machine is, read from `/proc` and `/sys` only (D-03 §2, D-07 §6):
//! CPU vendor for microcode, PCI display class for the graphics stack, RAM and
//! battery for preflight. Nothing here runs a program.

use crate::env::Paths;
use std::fs;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Ucode {
    Amd,
    Intel,
}

impl Ucode {
    pub const fn package(self) -> &'static str {
        match self {
            Ucode::Amd => "amd-ucode",
            Ucode::Intel => "intel-ucode",
        }
    }

    /// The image name under `/boot`, which Limine loads ahead of the initramfs.
    pub const fn image(self) -> &'static str {
        match self {
            Ucode::Amd => "amd-ucode.img",
            Ucode::Intel => "intel-ucode.img",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Gpu {
    Amd,
    Intel,
    Nvidia,
}

impl Gpu {
    /// Open-source Mesa stack only: nothing out-of-tree, so no DKMS and no
    /// kernel-upgrade step that can fail after reboot (D-03 §2).
    pub const fn packages(self) -> &'static [&'static str] {
        match self {
            Gpu::Amd => &["mesa", "vulkan-radeon"],
            Gpu::Intel => &["mesa", "vulkan-intel", "intel-media-driver"],
            Gpu::Nvidia => &["mesa", "vulkan-nouveau"],
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
pub struct Hardware {
    pub cpu: Option<Ucode>,
    pub vm: bool,
    pub gpus: Vec<Gpu>,
    pub mem_kib: u64,
    pub on_battery: bool,
}

fn cpuinfo(text: &str) -> (Option<Ucode>, bool) {
    let mut cpu = None;
    let mut vm = false;
    for l in text.lines() {
        let Some((k, v)) = l.split_once(':') else { continue };
        match k.trim() {
            "vendor_id" if cpu.is_none() => {
                cpu = match v.trim() {
                    "AuthenticAMD" => Some(Ucode::Amd),
                    "GenuineIntel" => Some(Ucode::Intel),
                    _ => None,
                }
            }
            "flags" => vm |= v.split_whitespace().any(|f| f == "hypervisor"),
            _ => {}
        }
    }
    (cpu, vm)
}

fn mem_kib(text: &str) -> u64 {
    text.lines()
        .find_map(|l| l.strip_prefix("MemTotal:"))
        .and_then(|v| v.split_whitespace().next())
        .and_then(|n| n.parse().ok())
        .unwrap_or(0)
}

fn hex(p: &std::path::Path) -> Option<u32> {
    let s = fs::read_to_string(p).ok()?;
    u32::from_str_radix(s.trim().strip_prefix("0x")?, 16).ok()
}

fn gpus(p: &Paths) -> Vec<Gpu> {
    let mut out = Vec::new();
    let Ok(rd) = fs::read_dir(&p.sys_pci) else {
        return out;
    };
    let mut dirs: Vec<_> = rd.flatten().map(|e| e.path()).collect();
    dirs.sort();
    for d in dirs {
        // Base class 0x03: VGA, 3D and other display controllers.
        if hex(&d.join("class")).is_none_or(|c| c >> 16 != 0x03) {
            continue;
        }
        let g = match hex(&d.join("vendor")) {
            Some(0x1002) => Gpu::Amd,
            Some(0x8086) => Gpu::Intel,
            Some(0x10de) => Gpu::Nvidia,
            _ => continue,
        };
        if !out.contains(&g) {
            out.push(g);
        }
    }
    out
}

fn on_battery(p: &Paths) -> bool {
    let Ok(rd) = fs::read_dir(&p.power_supply) else {
        return false;
    };
    rd.flatten().any(|e| {
        let d = e.path();
        fs::read_to_string(d.join("type")).is_ok_and(|t| t.trim() == "Battery")
            && fs::read_to_string(d.join("status")).is_ok_and(|s| s.trim() == "Discharging")
    })
}

pub fn detect(p: &Paths) -> Hardware {
    let (cpu, vm) = fs::read_to_string(&p.proc_cpuinfo)
        .map(|t| cpuinfo(&t))
        .unwrap_or((None, false));
    Hardware {
        cpu,
        vm,
        gpus: gpus(p),
        mem_kib: fs::read_to_string(&p.proc_meminfo)
            .map(|t| mem_kib(&t))
            .unwrap_or(0),
        on_battery: on_battery(p),
    }
}

impl Hardware {
    /// Microcode does nothing in a guest.
    pub fn ucode(&self) -> Option<Ucode> {
        if self.vm {
            None
        } else {
            self.cpu
        }
    }

    /// Packages this machine needs beyond the medium's list, in a stable order.
    pub fn packages(&self) -> Vec<&'static str> {
        let mut out: Vec<&'static str> = Vec::new();
        let mut add = |p: &'static str| {
            if !out.contains(&p) {
                out.push(p);
            }
        };
        if let Some(u) = self.ucode() {
            add(u.package());
        }
        for g in &self.gpus {
            for p in g.packages() {
                add(p);
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::{fake_paths, TempDir};

    fn pci(p: &Paths, slot: &str, class: &str, vendor: &str) {
        let d = p.sys_pci.join(slot);
        fs::create_dir_all(&d).unwrap();
        fs::write(d.join("class"), format!("{class}\n")).unwrap();
        fs::write(d.join("vendor"), format!("{vendor}\n")).unwrap();
    }

    #[test]
    fn cpu_vendor_and_hypervisor() {
        let amd = "processor : 0\nvendor_id\t: AuthenticAMD\nflags\t: fpu sse\n";
        assert_eq!(cpuinfo(amd), (Some(Ucode::Amd), false));
        let intel_vm = "vendor_id : GenuineIntel\nflags : fpu hypervisor\n";
        assert_eq!(cpuinfo(intel_vm), (Some(Ucode::Intel), true));
        assert_eq!(cpuinfo("vendor_id : HygonGenuine\n"), (None, false));
        assert_eq!(cpuinfo(""), (None, false));
    }

    #[test]
    fn a_guest_gets_no_microcode() {
        let h = Hardware {
            cpu: Some(Ucode::Intel),
            vm: true,
            gpus: vec![],
            mem_kib: 0,
            on_battery: false,
        };
        assert_eq!(h.ucode(), None);
        assert!(h.packages().is_empty());
    }

    #[test]
    fn meminfo() {
        assert_eq!(mem_kib("MemTotal:  8000000 kB\nMemFree: 1 kB\n"), 8_000_000);
        assert_eq!(mem_kib("junk"), 0);
    }

    #[test]
    fn display_controllers_by_vendor() {
        let t = TempDir::new();
        let p = fake_paths(&t);
        pci(&p, "0000:00:02.0", "0x030000", "0x8086");
        pci(&p, "0000:01:00.0", "0x030200", "0x10de");
        pci(&p, "0000:c1:00.0", "0x038000", "0x1002");
        // Not display class: an Intel audio device, an unknown-vendor VGA.
        pci(&p, "0000:00:1f.3", "0x040300", "0x8086");
        pci(&p, "0000:02:00.0", "0x030000", "0x1234");
        let mut got = gpus(&p);
        got.sort();
        assert_eq!(got, [Gpu::Amd, Gpu::Intel, Gpu::Nvidia]);
    }

    #[test]
    fn packages_are_deduplicated_and_open_source() {
        let h = Hardware {
            cpu: Some(Ucode::Intel),
            vm: false,
            gpus: vec![Gpu::Intel, Gpu::Nvidia, Gpu::Amd],
            mem_kib: 0,
            on_battery: false,
        };
        assert_eq!(
            h.packages(),
            [
                "intel-ucode",
                "mesa",
                "vulkan-intel",
                "intel-media-driver",
                "vulkan-nouveau",
                "vulkan-radeon"
            ]
        );
        assert!(h
            .packages()
            .iter()
            .all(|p| !p.contains("nvidia") && !p.contains("dkms")));
    }

    #[test]
    fn only_a_discharging_battery_counts() {
        let t = TempDir::new();
        let p = fake_paths(&t);
        assert!(!on_battery(&p));
        for (n, ty, st) in [("AC", "Mains", "Charging"), ("BAT0", "Battery", "Discharging")] {
            let d = p.power_supply.join(n);
            fs::create_dir_all(&d).unwrap();
            fs::write(d.join("type"), ty).unwrap();
            fs::write(d.join("status"), st).unwrap();
        }
        assert!(on_battery(&p));
        fs::write(p.power_supply.join("BAT0/status"), "Charging\n").unwrap();
        assert!(!on_battery(&p));
    }
}
