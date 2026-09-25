// SPDX-License-Identifier: AGPL-3.0-only
//! Every host path the helper reads or writes, in one struct, so tests can point
//! all of it at a tempdir. `Paths::system()` is the only production value and
//! nothing in it comes from the caller.

use crate::confirm::Confirm;
use crate::runner::Runner;
use std::path::PathBuf;
use std::time::Duration;

pub struct Paths {
    /// Present only on the live medium (D-07 §6: refuse without it).
    pub archiso: PathBuf,
    /// The mount point whose source device is the medium's own disk.
    pub bootmnt: PathBuf,
    /// UEFI only (D-07 §4.2).
    pub efi: PathBuf,
    pub dev: PathBuf,
    pub dev_by_id: PathBuf,
    pub sys_class_block: PathBuf,
    pub mountinfo: PathBuf,
    pub zoneinfo: PathBuf,
    /// Locale source: the file `locale-gen` itself consumes.
    pub locale_gen: PathBuf,
    pub passwd: PathBuf,
    /// Where the target is assembled (`pacstrap`'s root).
    pub target: PathBuf,
    /// Baked into the medium by `build-iso.sh` (D-03, D-07 §2.2).
    pub repo: PathBuf,
    pub pkg_list: PathBuf,
    pub packaging_key: PathBuf,
    pub live_pacman_conf: PathBuf,
    /// The live-side pacman.conf handed to `pacstrap -C`. Not part of the target.
    pub install_pacman_conf: PathBuf,
    /// Lowest uid accepted as "a real user" for the seed.
    pub uid_min: u32,
    pub proc_cpuinfo: PathBuf,
    pub proc_meminfo: PathBuf,
    pub proc_swaps: PathBuf,
    pub sys_pci: PathBuf,
    pub power_supply: PathBuf,
    /// kbd's keymap tree; a console keymap is valid if a `<name>.map.gz` is in it.
    pub keymaps: PathBuf,
    /// Present once systemd-timesyncd has set the clock from the network.
    pub ntp_synced: PathBuf,
    /// How long `Preflight` waits for the clock before refusing.
    pub clock_wait: Duration,
    /// The live medium's saved connections (D-07 §4.3).
    pub nm_connections: PathBuf,
    /// Progress log; not secret by construction, so world-readable for the GUI.
    pub install_log: PathBuf,
}

impl Paths {
    pub fn system() -> Self {
        Paths {
            archiso: "/run/archiso".into(),
            bootmnt: "/run/archiso/bootmnt".into(),
            efi: "/sys/firmware/efi".into(),
            dev: "/dev".into(),
            dev_by_id: "/dev/disk/by-id".into(),
            sys_class_block: "/sys/class/block".into(),
            mountinfo: "/proc/self/mountinfo".into(),
            zoneinfo: "/usr/share/zoneinfo".into(),
            locale_gen: "/etc/locale.gen".into(),
            passwd: "/etc/passwd".into(),
            target: "/mnt".into(),
            repo: "/root/eclipseos-repo".into(),
            pkg_list: "/root/eclipseos-packages.txt".into(),
            packaging_key: "/root/eclipseos-packaging.asc".into(),
            live_pacman_conf: "/etc/pacman.conf".into(),
            install_pacman_conf: "/root/eclipseos-install-pacman.conf".into(),
            uid_min: 1000,
            proc_cpuinfo: "/proc/cpuinfo".into(),
            proc_meminfo: "/proc/meminfo".into(),
            proc_swaps: "/proc/swaps".into(),
            sys_pci: "/sys/bus/pci/devices".into(),
            power_supply: "/sys/class/power_supply".into(),
            keymaps: "/usr/share/kbd/keymaps".into(),
            ntp_synced: "/run/systemd/timesync/synchronized".into(),
            clock_wait: Duration::from_secs(30),
            nm_connections: "/etc/NetworkManager/system-connections".into(),
            install_log: "/run/eclipseos-install.log".into(),
        }
    }
}

pub struct Env<'a> {
    pub paths: Paths,
    /// Effective uid of this process. A field so tests can be "root" without being it.
    pub euid: u32,
    /// `PKEXEC_UID` as the caller's environment gave it. A hint only: the seed
    /// re-derives everything from passwd (D-07 §5).
    pub caller_uid_hint: Option<String>,
    pub runner: &'a dyn Runner,
    pub confirm: &'a dyn Confirm,
}
