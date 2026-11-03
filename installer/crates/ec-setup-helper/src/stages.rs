// SPDX-License-Identifier: AGPL-3.0-only
//! The stages that change the machine, reimplemented from
//! `packaging/iso/airootfs/root/install-eclipseos.sh` with fixed argv: every command
//! is a [`Tool`] plus arguments built here from validated values, never a shell
//! line. Nothing in this module runs before `Confirm` has allowed (the caller
//! in `apply` orders that; `Tool::touches_disk` lets tests prove it).

use crate::catalog::{valid_pkg_name, Entry, AGENTS_PACKAGE, FLOOR_UNITS};
use crate::disks::{self, DiskEntry};
use crate::env::Env;
use crate::error::{io, Error, Result};
use crate::hw::Ucode;
use crate::runner::{Cmd, Tool};
use crate::target::{self, TargetFile};
use ec_setup_plan::{Plan, Request};
use std::ffi::OsString;
use std::fs;
use std::io::Write;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::time::Duration;
use zeroize::Zeroizing;

/// Everything the pre-disk stages worked out for the later ones.
pub struct Prepared {
    /// Floor list from the medium plus catalog packages, in order, no duplicates.
    pub pkgs: Vec<String>,
    /// Fingerprint of the packaging key on the medium.
    pub keyid: String,
    /// What this machine's CPU needs loaded ahead of the initramfs.
    pub ucode: Option<Ucode>,
    /// Turing-or-newer NVIDIA present: DKMS module, cmdline and initramfs setup.
    pub nvidia: bool,
    /// An earlier attempt left mounts at or under the target; they are
    /// unmounted once the erase is confirmed.
    pub stale_mounts: bool,
    /// Non-fatal findings, reported to the user before the erase.
    pub warnings: Vec<&'static str>,
}

pub struct Layout {
    pub disk: PathBuf,
    pub esp: PathBuf,
    pub root: PathBuf,
}

const MAX_PKGS: usize = 2048;

/// `nvme0n1` -> `nvme0n1p1`; `sda` -> `sda1`.
pub fn part_name(kernel: &str, n: u8) -> String {
    if kernel.ends_with(|c: char| c.is_ascii_digit()) {
        format!("{kernel}p{n}")
    } else {
        format!("{kernel}{n}")
    }
}

/// The medium's package list: one name per line, `#` comments and blanks skipped.
/// Each name is held to the argv-safe pattern; a bad line refuses the install
/// rather than being skipped, since a silently missing package is a broken system.
pub fn parse_pkg_list(text: &str) -> Result<Vec<String>> {
    let mut out = Vec::new();
    for l in text.lines() {
        let l = l.trim();
        if l.is_empty() || l.starts_with('#') {
            continue;
        }
        if !valid_pkg_name(l) {
            return Err(Error::Refused("package list on the medium"));
        }
        out.push(l.to_owned());
        if out.len() > MAX_PKGS {
            return Err(Error::Refused("package list on the medium"));
        }
    }
    if out.is_empty() {
        return Err(Error::Refused("package list on the medium"));
    }
    Ok(out)
}

/// Floor, then catalog packages, then the agent add-on if the plan asked for it.
pub fn package_set(base: Vec<String>, entries: &[&Entry], agents: bool) -> Vec<String> {
    let mut all = base;
    let extra = entries.iter().flat_map(|e| e.packages.iter());
    let agents = agents.then_some(&AGENTS_PACKAGE);
    for p in extra.chain(agents) {
        if !all.iter().any(|a| a == p) {
            all.push((*p).to_owned());
        }
    }
    all
}

/// First `fpr:` record of `gpg --with-colons`: 40 (v4) to 64 (v5) hex digits.
pub fn parse_fpr(colons: &str) -> Option<String> {
    let f = colons
        .lines()
        .find(|l| l.starts_with("fpr:"))?
        .split(':')
        .nth(9)?;
    let ok = (40..=64).contains(&f.len()) && f.bytes().all(|c| c.is_ascii_hexdigit());
    ok.then(|| f.to_ascii_uppercase())
}

pub fn valid_uuid(s: &str) -> bool {
    !s.is_empty() && s.len() <= 64 && s.bytes().all(|c| c.is_ascii_alphanumeric() || c == b'-')
}

/// Uncomment `NAME UTF-8` in a `locale.gen`. `None` if the locale is not there.
/// (Already uncommented counts as there.)
pub fn enable_locale(text: &str, locale: &str) -> Option<String> {
    let mut found = false;
    let mut out = String::with_capacity(text.len());
    for l in text.lines() {
        let body = l.strip_prefix('#').unwrap_or(l);
        let mut t = body.split_whitespace();
        if t.next() == Some(locale) && t.next() == Some("UTF-8") && t.next().is_none() {
            found = true;
            out.push_str(body.trim_start());
        } else {
            out.push_str(l);
        }
        out.push('\n');
    }
    found.then_some(out)
}

/// The D-02 include line, added if `eclipseos.conf` is not already referenced.
/// `None` when nothing needs adding.
pub fn with_eclipseos_include(text: &str) -> Option<String> {
    if text.contains("eclipseos.conf") {
        return None;
    }
    let mut s = text.to_owned();
    if !s.ends_with('\n') && !s.is_empty() {
        s.push('\n');
    }
    s.push_str("\n# EclipseOS packages (D-02)\nInclude = /etc/pacman.d/eclipseos.conf\n");
    Some(s)
}

/// Nothing else refreshes the stub on the ESP, so without this a `limine`
/// upgrade would leave the machine booting last release's stub (D-03 §4).
pub const LIMINE_HOOK: &str = "[Trigger]\n\
Type = Path\n\
Operation = Install\n\
Operation = Upgrade\n\
Target = usr/share/limine/BOOTX64.EFI\n\n\
[Action]\n\
Description = Copying the Limine stub to the ESP...\n\
When = PostTransaction\n\
Exec = /usr/bin/install -Dm0644 /usr/share/limine/BOOTX64.EFI /boot/EFI/BOOT/BOOTX64.EFI\n";

/// Compressed swap in RAM (D-07 §6): no partition, nothing to size.
pub const ZRAM_CONF: &str = "[zram0]\nzram-size = min(ram / 2, 4096)\ncompression-algorithm = zstd\n";

/// Uncomment the two pacman options an installed system wants: coloured output
/// and parallel downloads. `None` when there is nothing to change.
pub fn with_pacman_options(text: &str) -> Option<String> {
    let mut changed = false;
    let mut out = String::with_capacity(text.len());
    for l in text.lines() {
        let t = l.trim();
        if t == "#Color" || t == "#ParallelDownloads = 5" {
            out.push_str(&t[1..]);
            changed = true;
        } else {
            out.push_str(l);
        }
        out.push('\n');
    }
    changed.then_some(out)
}

pub const SUDOERS_WHEEL: &str = "%wheel ALL=(ALL:ALL) ALL\n";

/// Limine's config, byte for byte what the reference script wrote. The ESP is
/// mounted at /boot, so `boot():/` is where the kernel and initramfs sit.
/// Fonts the chosen locale needs on the installed system. The live medium
/// carries them for its own wizard; the target gets them only when used.
pub fn locale_packages(locale: &str) -> &'static [&'static str] {
    match locale.split(['_', '.']).next() {
        Some("ja" | "zh" | "ko") => &["noto-fonts-cjk"],
        _ => &[],
    }
}

/// Kernel parameters for the proprietary open module (D-01 §2.3): set
/// explicitly rather than inherited from a default upstream may flip.
pub const NVIDIA_CMDLINE: &str = " nvidia-drm.modeset=1 nvidia-drm.fbdev=1";

/// Drop-in for `mkinitcpio` on the NVIDIA path (D-01 §2.3): the modules load
/// in early userspace. `+=`, so the stock hooks (`kms` included) stay: the
/// `modconf` hook carries `nvidia-utils`' nouveau blacklist into the initramfs.
pub const MKINITCPIO_NVIDIA: &str = "MODULES+=(nvidia nvidia_modeset nvidia_uvm nvidia_drm)\n";

pub fn limine_conf(root_uuid: &str, ucode: Option<Ucode>, nvidia: bool) -> String {
    let pstate = if ucode == Some(Ucode::Amd) {
        " amd_pstate=active"
    } else {
        ""
    };
    let nv = if nvidia { NVIDIA_CMDLINE } else { "" };
    let ucode = ucode
        .map(|u| format!("    module_path: boot():/{}\n", u.image()))
        .unwrap_or_default();
    format!(
        "timeout: 2\n\n\
         /EclipseOS\n\
         \x20   protocol: linux\n\
         \x20   path: boot():/vmlinuz-linux\n\
         \x20   cmdline: root=UUID={root_uuid} rootfstype=ext4 rw zswap.enabled=0{pstate}{nv}\n\
         {ucode}\
         \x20   module_path: boot():/initramfs-linux.img\n\n\
         /EclipseOS (fallback initramfs)\n\
         \x20   protocol: linux\n\
         \x20   path: boot():/vmlinuz-linux\n\
         \x20   cmdline: root=UUID={root_uuid} rootfstype=ext4 rw zswap.enabled=0{nv}\n\
         \x20   module_path: boot():/initramfs-linux-fallback.img\n\n\
         /EclipseOS (LTS kernel)\n\
         \x20   protocol: linux\n\
         \x20   path: boot():/vmlinuz-linux-lts\n\
         \x20   cmdline: root=UUID={root_uuid} rootfstype=ext4 rw zswap.enabled=0{pstate}{nv}\n\
         {ucode}\
         \x20   module_path: boot():/initramfs-linux-lts.img\n"
    )
}

/// True when every installed kernel (a directory under `usr/lib/modules` that
/// holds a `vmlinuz`) has the NVIDIA module DKMS built for it. Reads the target
/// tree only; runs nothing.
pub fn nvidia_modules_built(modules_dir: &std::path::Path) -> bool {
    let Ok(rd) = fs::read_dir(modules_dir) else {
        return false;
    };
    let mut kernels = 0;
    for k in rd
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.join("vmlinuz").is_file())
    {
        kernels += 1;
        let dkms = k.join("updates/dkms");
        let built = ["nvidia.ko", "nvidia.ko.zst", "nvidia.ko.xz", "nvidia.ko.gz"]
            .iter()
            .any(|n| dkms.join(n).is_file());
        if !built {
            return false;
        }
    }
    kernels > 0
}

/// After pacstrap: the DKMS pacman hook has run in the target, so a missing
/// module means the install would boot to a black screen. Fail now, loudly.
pub fn verify_nvidia(env: &Env) -> Result<()> {
    if nvidia_modules_built(&env.paths.target.join("usr/lib/modules")) {
        Ok(())
    } else {
        Err(Error::Refused(
            "the NVIDIA kernel module did not build for every installed kernel; \
             this machine cannot be installed with the NVIDIA driver",
        ))
    }
}

fn chroot(env: &Env) -> Cmd {
    Cmd::new(Tool::ArchChroot).arg(&env.paths.target)
}

// ---- Partition ---------------------------------------------------------------

fn queue_attr(env: &Env, kernel: &str, name: &str) -> Option<u64> {
    fs::read_to_string(env.paths.sys_class_block.join(kernel).join("queue").join(name))
        .ok()?
        .trim()
        .parse()
        .ok()
}

fn discards(env: &Env, kernel: &str) -> bool {
    queue_attr(env, kernel, "discard_max_bytes").is_some_and(|n| n > 0)
}

/// Logical sector size, for `mkfs.fat -S` (a 4Kn disk needs it to match).
fn sector_size(env: &Env, kernel: &str) -> Option<u64> {
    queue_attr(env, kernel, "logical_block_size").filter(|s| matches!(s, 512 | 1024 | 2048 | 4096))
}

pub fn partition(env: &Env, d: &DiskEntry) -> Result<Layout> {
    let p = &env.paths;
    // The link is re-resolved at the point of use: it must still name the disk
    // that was validated and confirmed.
    if disks::resolve_by_id(p, &d.disk.by_id).as_deref() != Some(d.kernel.as_str()) {
        return Err(Error::Refused("disk changed since it was confirmed"));
    }
    let dev = p.dev.join(&d.kernel);
    let esp = p.dev.join(part_name(&d.kernel, 1));
    let root = p.dev.join(part_name(&d.kernel, 2));
    let r = env.runner;
    // Old RAID, LVM, LUKS and ZFS labels survive `sgdisk --zap-all` and get
    // re-assembled by udev, so clear signatures first: partitions, then the disk.
    for part in &d.parts {
        r.run(&Cmd::new(Tool::Wipefs).arg("-a").arg(p.dev.join(part)))?;
    }
    r.run(&Cmd::new(Tool::Wipefs).arg("-a").arg(&dev))?;
    // Best effort: not every device can discard, and the wipe above already
    // made the old contents unreachable.
    if discards(env, &d.kernel) {
        let _ = r.run(&Cmd::new(Tool::Blkdiscard).arg("-f").arg(&dev));
    }
    r.run(&Cmd::new(Tool::Sgdisk).arg("--zap-all").arg(&dev))?;
    r.run(
        &Cmd::new(Tool::Sgdisk)
            .args(["-n1:0:+1G", "-t1:ef00", "-c1:EFI"])
            .args(["-n2:0:0", "-t2:8304", "-c2:eclipseos"])
            .arg(&dev),
    )?;
    r.run(&Cmd::new(Tool::Partprobe).arg(&dev))?;
    r.run(&Cmd::new(Tool::Udevadm).arg("settle"))?;
    let mut mkfs = Cmd::new(Tool::MkfsFat).args(["-F32", "-n", "ECLIPSE_ESP"]);
    if let Some(ss) = sector_size(env, &d.kernel) {
        mkfs = mkfs.arg("-S").arg(ss.to_string());
    }
    r.run(&mkfs.arg(&esp))?;
    r.run(
        &Cmd::new(Tool::MkfsExt4)
            .args(["-F", "-L", "eclipseos"])
            .arg(&root),
    )?;
    Ok(Layout { disk: dev, esp, root })
}

pub fn mount(env: &Env, l: &Layout) -> Result<()> {
    let t = &env.paths.target;
    env.runner.run(&Cmd::new(Tool::Mount).arg(&l.root).arg(t))?;
    // `genfstab` copies these, so the installed system keeps the ESP (kernel,
    // initramfs) readable by root only.
    env.runner.run(
        &Cmd::new(Tool::Mount)
            .args(["--mkdir", "-o", "fmask=0177,dmask=0077"])
            .arg(&l.esp)
            .arg(t.join("boot")),
    )
}

/// Attempts at `umount -R` before giving up (about 5 s with the 1 s backoff).
const UMOUNT_TRIES: u32 = 5;

/// The daemons gpg starts on demand for a keyring.
const GPG_DAEMONS: [&str; 4] = ["gpg-agent", "dirmngr", "keyboxd", "scdaemon"];

/// Pids under `proc` of gpg daemons that belong to the target: chrooted into
/// it, or given a `--homedir` inside it under any spelling. pacstrap spells it
/// `<root>//etc/pacman.d/gnupg`, and with no `/run/user/0` gpg keeps its socket
/// in the homedir, so two spellings share one socket and the agent that lost it
/// cannot be reached by `gpgconf --kill`. Hence finding them by process.
pub fn target_gpg_pids(proc: &Path, target: &Path) -> Vec<i32> {
    let Ok(dir) = fs::read_dir(proc) else {
        return Vec::new();
    };
    let mut pids = Vec::new();
    for e in dir.flatten() {
        let Some(pid) = e.file_name().to_str().and_then(|s| s.parse::<i32>().ok()) else {
            continue;
        };
        let p = e.path();
        let comm = fs::read_to_string(p.join("comm")).unwrap_or_default();
        if !GPG_DAEMONS.contains(&comm.trim_end()) {
            continue;
        }
        let rooted =
            fs::read_link(p.join("root")).is_ok_and(|r| r != Path::new("/") && r.starts_with(target));
        let cmdline = fs::read(p.join("cmdline")).unwrap_or_default();
        let mut args = cmdline.split(|b| *b == 0);
        let homed = std::iter::from_fn(|| args.next())
            .skip_while(|a| *a != b"--homedir")
            .nth(1)
            .is_some_and(|h| Path::new(std::ffi::OsStr::from_bytes(h)).starts_with(target));
        if rooted || homed {
            pids.push(pid);
        }
    }
    pids
}

/// Stop the daemons the keyring stages left running against the target's
/// gnupg home (pacstrap -K, and the host-side `--gpgdir` calls). They hold the
/// mount busy until they exit on their own. TERM, a short wait, then KILL.
fn stop_target_daemons(env: &Env) {
    use rustix::process::{kill_process, Pid, Signal};
    let p = &env.paths;
    for sig in [Signal::TERM, Signal::KILL] {
        let pids = target_gpg_pids(&p.proc, &p.target);
        if pids.is_empty() {
            return;
        }
        for pid in pids.iter().filter_map(|&n| Pid::from_raw(n)) {
            let _ = kill_process(pid, sig);
        }
        for _ in 0..20 {
            if target_gpg_pids(&p.proc, &p.target).is_empty() {
                return;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
    }
}

/// Append what is still mounted under the target to the install log, so a busy
/// mount is diagnosable. Mount paths and device names only.
fn log_busy_mounts(env: &Env) {
    let Ok(out) = env
        .runner
        .capture(&Cmd::new(Tool::Findmnt).arg("-R").arg(&env.paths.target))
    else {
        return;
    };
    if let Ok(mut f) = fs::OpenOptions::new().append(true).open(&env.paths.install_log) {
        let _ = writeln!(f, "umount of the target failed; still mounted:");
        let _ = f.write_all(&out);
    }
}

/// Stop the target's key daemons, flush, then `umount -R`, retrying briefly:
/// a process that is on its way out can hold a mount for a moment. Never a lazy
/// unmount. Also run after a failure so the disk is not left busy.
pub fn unmount(env: &Env) -> Result<()> {
    stop_target_daemons(env);
    let _ = env.runner.run(&Cmd::new(Tool::Sync));
    let mut res = Ok(());
    for i in 0..UMOUNT_TRIES {
        if i > 0 {
            std::thread::sleep(env.paths.umount_backoff);
        }
        res = env
            .runner
            .run(&Cmd::new(Tool::Umount).arg("-R").arg(&env.paths.target));
        if res.is_ok() {
            return res;
        }
    }
    log_busy_mounts(env);
    res
}

// ---- Pacstrap ----------------------------------------------------------------

/// Download settings for the install-time pacman.conf: no low-speed abort (a
/// slow or filtered link must not kill a transaction) and few parallel
/// downloads (many streams over a bad link starve each other). Replaces any
/// existing or commented-out setting, then puts ours right under `[options]`.
pub fn with_download_options(text: &str) -> String {
    const OURS: &str = "DisableDownloadTimeout\nParallelDownloads = 3\n";
    let mut out = String::with_capacity(text.len() + OURS.len() + 16);
    let mut placed = false;
    for l in text.lines() {
        let key = l.trim().trim_start_matches('#').trim();
        let key = key.split(['=', ' ']).next().unwrap_or("");
        if key == "DisableDownloadTimeout" || key == "ParallelDownloads" {
            continue;
        }
        out.push_str(l);
        out.push('\n');
        if !placed && l.trim() == "[options]" {
            out.push_str(OURS);
            placed = true;
        }
    }
    if placed {
        out
    } else {
        format!("[options]\n{OURS}\n{out}")
    }
}

/// Why a pacstrap run failed, from the tail of its stderr.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FailClass {
    Network,
    DiskFull,
    Signature,
    Other,
}

impl FailClass {
    /// Only a download failure can plausibly go away by trying again. A bad
    /// signature, a full disk or anything unrecognised must not be retried
    /// into success.
    pub fn retryable(self) -> bool {
        self == FailClass::Network
    }

    /// The one line the user sees: what happened and what to do next.
    pub fn message(self) -> &'static str {
        match self {
            FailClass::Network => {
                "Downloads stalled on every mirror — check the connection (a filtered network or \
                 captive portal can do this), then Try again."
            }
            FailClass::DiskFull => "The target disk ran out of space — pick a larger disk, then Try again.",
            FailClass::Signature => {
                "Package signature check failed — check that the clock is right and the network is \
                 not altering downloads (captive portal, proxy), then Try again."
            }
            FailClass::Other => {
                "Installing packages failed — see the install log for the reason, then Try again."
            }
        }
    }
}

/// Classify pacstrap's stderr. Disk and signature win over network: a "failed
/// retrieving ... .sig" line is a download problem, but "invalid or corrupted
/// package (PGP signature)" is a verification one, and a full disk surfaces as
/// a failed transaction that would otherwise look like a download. The text
/// is only matched against fixed phrases; none of it is kept or shown.
pub fn classify_pacstrap(stderr: &[u8]) -> FailClass {
    let t = String::from_utf8_lossy(stderr).to_ascii_lowercase();
    let any = |needles: &[&str]| needles.iter().any(|n| t.contains(n));
    if any(&[
        "no space left on device",
        "not enough free disk space",
        "too full",
    ]) {
        FailClass::DiskFull
    } else if any(&[
        "pgp signature",
        "signature from",
        "is unknown trust",
        "is marginal trust",
        "key is disabled",
        "required key missing",
        "keyring is not writable",
        "could not be looked up remotely",
        "invalid or corrupted database",
    ]) {
        FailClass::Signature
    } else if any(&[
        "failed retrieving file",
        "operation too slow",
        "operation timed out",
        "connection timed out",
        "could not resolve",
        "failed to connect",
        "connection reset",
        "connection refused",
        "ssl connect error",
        "failed to synchronize",
        "invalid or corrupted package (checksum)",
        "unexpected error",
    ]) {
        FailClass::Network
    } else {
        FailClass::Other
    }
}

/// Move the first `Server` line to the end of the list, so the mirror that just
/// failed is tried last. `None` when there are fewer than two servers.
pub fn rotate_mirrorlist(text: &str) -> Option<String> {
    let is_server = |l: &&str| l.trim_start().starts_with("Server");
    let servers = text.lines().filter(is_server).count();
    if servers < 2 {
        return None;
    }
    let first = text.lines().position(|l| is_server(&l))?;
    let mut out = String::with_capacity(text.len() + 1);
    let mut moved = "";
    for (i, l) in text.lines().enumerate() {
        if i == first {
            moved = l;
        } else {
            out.push_str(l);
            out.push('\n');
        }
    }
    out.push_str(moved);
    out.push('\n');
    Some(out)
}

/// Best effort: a mirror list that cannot be rotated just means the retry
/// starts where the last attempt did. Written beside the list and renamed over
/// it, so a link at the path is replaced, never written through.
fn rotate_live_mirrorlist(env: &Env) {
    let path = &env.paths.mirrorlist;
    let Ok(text) = fs::read_to_string(path) else {
        return;
    };
    let Some(rotated) = rotate_mirrorlist(&text) else {
        return;
    };
    let mut tmp = path.clone().into_os_string();
    tmp.push(".new");
    let tmp = PathBuf::from(tmp);
    let _ = fs::remove_file(&tmp);
    let written = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o644)
        .open(&tmp)
        .and_then(|mut f| f.write_all(rotated.as_bytes()));
    if written.is_ok() && fs::rename(&tmp, path).is_ok() {
        return;
    }
    let _ = fs::remove_file(&tmp);
}

/// Attempts at pacstrap before giving up on a download failure.
pub const PACSTRAP_TRIES: u32 = 3;

pub fn install_pacman_conf(env: &Env) -> Result<()> {
    let p = &env.paths;
    let conf = fs::read_to_string(&p.live_pacman_conf).map_err(io("read pacman.conf"))?;
    let mut conf = with_download_options(&conf);
    // Baked into the medium by packaging/iso/build-iso.sh (D-03): the database is
    // unsigned, the packages in it are signed.
    conf.push_str(&format!(
        "\n[eclipseos]\nSigLevel = PackageRequired DatabaseOptional\nServer = file://{}\n",
        p.repo.display()
    ));
    let mut f = fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(&p.install_pacman_conf)
        .map_err(io("write install pacman.conf"))?;
    f.write_all(conf.as_bytes())
        .map_err(io("write install pacman.conf"))
}

/// `note` receives a progress line for each retry. Downloaded packages stay in
/// the target's cache (pacstrap's default), so a retry fetches only what is left.
pub fn pacstrap(env: &Env, prep: &Prepared, note: &mut dyn FnMut(&str)) -> Result<()> {
    let p = &env.paths;
    let r = env.runner;
    // Trust the packaging key on the live side (to verify the baked repo) and in
    // the target's own keyring (its first `pacman -Syu` needs it).
    r.run(&Cmd::new(Tool::PacmanKey).arg("--add").arg(&p.packaging_key))?;
    r.run(&Cmd::new(Tool::PacmanKey).arg("--lsign-key").arg(&prep.keyid))?;
    install_pacman_conf(env)?;
    let strap = Cmd::new(Tool::Pacstrap)
        .args(["-K", "-C"])
        .arg(&p.install_pacman_conf)
        .arg(&p.target)
        .args(&prep.pkgs);
    let mut attempt = 1;
    loop {
        let (res, stderr) = r.run_diag(&strap);
        if res.is_ok() {
            break;
        }
        let class = classify_pacstrap(&stderr);
        if !class.retryable() || attempt >= PACSTRAP_TRIES {
            return Err(Error::Failed(class.message()));
        }
        note(match attempt {
            1 => "downloads failed; retrying on a different mirror (attempt 2 of 3)",
            _ => "downloads failed; retrying on a different mirror (attempt 3 of 3)",
        });
        // The failed run's key daemons would hold the target's keyring.
        stop_target_daemons(env);
        rotate_live_mirrorlist(env);
        std::thread::sleep(p.retry_backoff * attempt);
        attempt += 1;
    }
    let ring = p.target.join("etc/pacman.d/gnupg");
    r.run(
        &Cmd::new(Tool::PacmanKey)
            .arg("--gpgdir")
            .arg(&ring)
            .arg("--add")
            .arg(&p.packaging_key),
    )?;
    r.run(
        &Cmd::new(Tool::PacmanKey)
            .arg("--gpgdir")
            .arg(&ring)
            .arg("--lsign-key")
            .arg(&prep.keyid),
    )?;
    // pacstrap synced `[eclipseos]` from the medium's baked repo, whose
    // database build-iso.sh re-signs, so it differs from the published one but
    // is newer. The target's first `pacman -Sy` would keep that db (the server
    // answers 304) and fetch the published signature: a bad signature until a
    // later sync. Drop the pair so the first sync fetches both.
    target::remove(&p.target, TargetFile::SyncDb)?;
    target::remove(&p.target, TargetFile::SyncDbSig)?;
    let fstab = r.capture(&Cmd::new(Tool::Genfstab).arg("-U").arg(&p.target))?;
    let mut all = target::read(&p.target, TargetFile::Fstab)?;
    all.extend_from_slice(&fstab);
    target::write(&p.target, TargetFile::Fstab, &all)
}

// ---- Configure ---------------------------------------------------------------

pub fn configure(env: &Env, plan: &Plan, nvidia: bool) -> Result<()> {
    let t = &env.paths.target;
    target::write(t, TargetFile::Hostname, format!("{}\n", plan.hostname).as_bytes())?;
    target::link_localtime(t, &plan.timezone)?;

    let lg = String::from_utf8(target::read(t, TargetFile::LocaleGen)?)
        .map_err(|_| Error::Refused("target locale.gen"))?;
    let mut lg = enable_locale(&lg, &plan.locale).ok_or(Error::Refused("locale not in target"))?;
    // Programs that assume English messages keep working next to the chosen locale.
    if let Some(both) = enable_locale(&lg, "en_US.UTF-8") {
        lg = both;
    }
    target::write(t, TargetFile::LocaleGen, lg.as_bytes())?;
    target::write(
        t,
        TargetFile::LocaleConf,
        format!("LANG={}\n", plan.locale).as_bytes(),
    )?;

    let pc = String::from_utf8(target::read(t, TargetFile::PacmanConf)?)
        .map_err(|_| Error::Refused("target pacman.conf"))?;
    let pc = with_pacman_options(&pc).unwrap_or(pc);
    let pc = with_eclipseos_include(&pc).unwrap_or(pc);
    target::write(t, TargetFile::PacmanConf, pc.as_bytes())?;
    // Before `mkinitcpio`, whose `keymap` hook bakes this into the initramfs.
    target::write(
        t,
        TargetFile::Vconsole,
        format!("KEYMAP={}\n", plan.keymap).as_bytes(),
    )?;
    target::write(t, TargetFile::ZramGenerator, ZRAM_CONF.as_bytes())?;
    if nvidia {
        target::write(t, TargetFile::MkinitcpioNvidia, MKINITCPIO_NVIDIA.as_bytes())?;
    }
    target::write(t, TargetFile::SudoersWheel, SUDOERS_WHEEL.as_bytes())?;

    env.runner.run(&chroot(env).arg("locale-gen"))?;
    // Virtual machines and some firmware have no usable RTC; the disk is already
    // erased, so this must not fail the install.
    let _ = env.runner.run(&chroot(env).args(["hwclock", "--systohc"]));
    env.runner.run(&chroot(env).args(["mkinitcpio", "-P"]))
}

// ---- Bootloader --------------------------------------------------------------

pub fn bootloader(env: &Env, l: &Layout, ucode: Option<Ucode>, nvidia: bool) -> Result<()> {
    let t = &env.paths.target;
    // Limine's UEFI install is two files: the stub at the removable fallback
    // path, which needs no NVRAM entry, and a config that says what it does.
    let stub = target::read_limine_stub(t)?;
    target::write(t, TargetFile::LimineStub, &stub)?;
    target::write(t, TargetFile::LimineHook, LIMINE_HOOK.as_bytes())?;

    let out = env.runner.capture(
        &Cmd::new(Tool::Blkid)
            .args(["-s", "UUID", "-o", "value"])
            .arg(&l.root),
    )?;
    let uuid = String::from_utf8(out).map_err(|_| Error::Refused("root uuid"))?;
    let uuid = uuid.trim();
    if !valid_uuid(uuid) {
        return Err(Error::Refused("root uuid"));
    }
    let ucode = ucode.filter(|u| t.join("boot").join(u.image()).is_file());
    target::write(
        t,
        TargetFile::LimineConf,
        limine_conf(uuid, ucode, nvidia).as_bytes(),
    )?;
    // A firmware entry as well as the removable path, so machines that prefer
    // NVRAM entries boot it. Best effort: the fallback path is what must work.
    let _ = env.runner.run(
        &Cmd::new(Tool::Efibootmgr)
            .arg("--create")
            .arg("--disk")
            .arg(&l.disk)
            .args(["--part", "1", "--label", "EclipseOS", "--loader"])
            .arg("\\EFI\\BOOT\\BOOTX64.EFI"),
    );
    Ok(())
}

/// Keep the redacted progress log on the installed system.
pub fn keep_log(env: &Env) -> Result<()> {
    let bytes = target::read_bounded(&env.paths.install_log)?;
    target::write(&env.paths.target, TargetFile::InstallLog, &bytes)
}

// ---- User --------------------------------------------------------------------

pub fn user(env: &Env, req: &Request) -> Result<()> {
    let name = &req.plan.username;
    env.runner.run(
        &chroot(env)
            .args(["useradd", "-m", "-G", "wheel", "-s", "/bin/bash"])
            .arg(name),
    )?;
    // `user:password\n` on the child's stdin, its stderr discarded. Names and
    // password were checked for `:`-free names and no `\n`, `\r`, NUL.
    let mut line = Zeroizing::new(Vec::with_capacity(name.len() + req.password.len() + 2));
    line.extend_from_slice(name.as_bytes());
    line.push(b':');
    line.extend_from_slice(req.password.as_bytes());
    line.push(b'\n');
    env.runner.run(&chroot(env).arg("chpasswd").stdin(line))?;
    // One password in the plan and it is the user's; root stays locked.
    env.runner.run(&chroot(env).args(["passwd", "-l", "root"]))
}

// ---- Units -------------------------------------------------------------------

/// System units to enable: the floor plus what the resolved catalog entries list.
pub fn unit_list(entries: &[&Entry]) -> Vec<&'static str> {
    let mut u: Vec<&'static str> = FLOOR_UNITS.to_vec();
    for e in entries {
        for s in e.system_units {
            if !u.contains(s) {
                u.push(s);
            }
        }
    }
    u
}

pub fn units(env: &Env, entries: &[&Entry]) -> Result<()> {
    let list = unit_list(entries);
    if !list.iter().all(|u| valid_pkg_name(u)) {
        return Err(Error::Refused("unit name"));
    }
    let args: Vec<OsString> = ["systemctl", "enable"]
        .iter()
        .chain(list.iter())
        .map(OsString::from)
        .collect();
    env.runner.run(&chroot(env).args(args))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn target_gpg_daemons_are_found_under_any_spelling_or_chroot() {
        let t = crate::testutil::TempDir::new();
        let proc = t.path().join("proc");
        let fake = |pid: &str, comm: &str, cmdline: &[&str], root: &str| {
            let d = proc.join(pid);
            fs::create_dir_all(&d).unwrap();
            fs::write(d.join("comm"), format!("{comm}\n")).unwrap();
            fs::write(d.join("cmdline"), cmdline.join("\0") + "\0").unwrap();
            std::os::unix::fs::symlink(root, d.join("root")).unwrap();
        };
        let agent = |h: &'static str| ["gpg-agent", "--homedir", h, "--daemon"];
        fake("10", "gpg-agent", &agent("/mnt//etc/pacman.d/gnupg"), "/");
        fake("11", "gpg-agent", &agent("/mnt/etc/pacman.d/gnupg"), "/");
        fake(
            "12",
            "keyboxd",
            &["keyboxd", "--homedir", "/etc/pacman.d/gnupg"],
            "/mnt",
        );
        fake("20", "gpg-agent", &agent("/etc/pacman.d/gnupg"), "/");
        fake("21", "zsh", &["zsh", "--homedir", "/mnt/x"], "/");
        fake("22", "gpg-agent", &agent("/mntx/etc/pacman.d/gnupg"), "/");
        fs::create_dir_all(proc.join("self")).unwrap();
        let mut got = target_gpg_pids(&proc, Path::new("/mnt"));
        got.sort();
        assert_eq!(got, vec![10, 11, 12]);
    }
    use crate::catalog::Catalog;

    #[test]
    fn partition_names() {
        assert_eq!(part_name("sda", 1), "sda1");
        assert_eq!(part_name("nvme0n1", 2), "nvme0n1p2");
        assert_eq!(part_name("mmcblk0", 1), "mmcblk0p1");
        assert_eq!(part_name("vda", 2), "vda2");
    }

    #[test]
    fn package_list_is_strict() {
        assert_eq!(
            parse_pkg_list("# c\n\n base \nlinux\n").unwrap(),
            ["base", "linux"]
        );
        for bad in ["", "# only\n", "a b\n", "-rf\n", "$(x)\n", "a;b\n", "../x\n"] {
            assert!(parse_pkg_list(bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn package_set_adds_catalog_packages_once() {
        let es = Catalog::builtin()
            .resolve(&["ec-hyperion-bar".into(), "ec-toasts".into()])
            .unwrap();
        let s = package_set(vec!["base".into(), "eclipseos-hyperion".into()], &es, false);
        assert_eq!(s, ["base", "eclipseos-hyperion", "eclipseos-toasts"]);
    }

    #[test]
    fn agents_package_follows_the_flag_once() {
        let base = || vec!["base".to_owned()];
        assert_eq!(package_set(base(), &[], true), ["base", "eclipseos-agents"]);
        assert_eq!(package_set(base(), &[], false), ["base"]);
        let had = vec!["base".to_owned(), "eclipseos-agents".to_owned()];
        assert_eq!(package_set(had, &[], true), ["base", "eclipseos-agents"]);
    }

    #[test]
    fn fingerprint_parse() {
        let f = "A".repeat(40);
        let ok = format!("tru::1:0:0:3:1:5\npub:-:255:22:AAAA:1:::-:::scESC:::\nfpr:::::::::{f}:\n");
        assert_eq!(parse_fpr(&ok).unwrap(), f);
        assert!(parse_fpr("fpr:::::::::zz:\n").is_none());
        assert!(parse_fpr("nothing\n").is_none());
        assert!(parse_fpr(&format!("fpr:::::::::{}:\n", "g".repeat(40))).is_none());
    }

    #[test]
    fn locale_is_uncommented_only_when_present() {
        let lg = "# header\n#en_US.UTF-8 UTF-8\n#de_DE.UTF-8 UTF-8\n#en_US ISO-8859-1\n";
        let out = enable_locale(lg, "en_US.UTF-8").unwrap();
        assert_eq!(
            out,
            "# header\nen_US.UTF-8 UTF-8\n#de_DE.UTF-8 UTF-8\n#en_US ISO-8859-1\n"
        );
        assert!(enable_locale(lg, "xx_XX.UTF-8").is_none());
        assert!(enable_locale(lg, "en_US").is_none());
        assert!(enable_locale("en_US.UTF-8 UTF-8\n", "en_US.UTF-8").is_some());
    }

    #[test]
    fn pacman_include_is_added_once() {
        let a = with_eclipseos_include("[options]\n").unwrap();
        assert!(a.contains("Include = /etc/pacman.d/eclipseos.conf"));
        assert!(with_eclipseos_include(&a).is_none());
    }

    #[test]
    fn cjk_fonts_follow_the_locale() {
        assert_eq!(locale_packages("ja_JP.UTF-8"), ["noto-fonts-cjk"]);
        assert_eq!(locale_packages("zh_CN.UTF-8"), ["noto-fonts-cjk"]);
        assert!(locale_packages("en_US.UTF-8").is_empty() && locale_packages("de_DE.UTF-8").is_empty());
    }

    #[test]
    fn limine_config_matches_the_reference() {
        let c = limine_conf("abcd-1234", Some(Ucode::Amd), false);
        assert!(c.starts_with("timeout: 2\n\n/EclipseOS\n    protocol: linux\n"));
        assert!(c.contains("    cmdline: root=UUID=abcd-1234 rootfstype=ext4 rw zswap.enabled=0 amd_pstate=active\n    module_path: boot():/amd-ucode.img\n    module_path: boot():/initramfs-linux.img\n"));
        assert!(c.contains("/EclipseOS (fallback initramfs)"));
        assert!(!limine_conf("u", None, false).contains("ucode"));
        let i = limine_conf("u", Some(Ucode::Intel), false);
        assert!(i.contains("module_path: boot():/intel-ucode.img") && !i.contains("amd_pstate"));
        assert!(valid_uuid("1b2c-3D4e") && !valid_uuid("a b") && !valid_uuid("x\ny") && !valid_uuid(""));
    }

    #[test]
    fn nvidia_cmdline_is_on_every_entry_only_for_turing_plus() {
        let on = limine_conf("u", Some(Ucode::Amd), true);
        assert_eq!(on.matches("nvidia-drm.modeset=1 nvidia-drm.fbdev=1").count(), 3);
        assert_eq!(on.matches("cmdline:").count(), 3);
        assert!(on.contains("path: boot():/vmlinuz-linux-lts") && on.contains("initramfs-linux-lts.img"));
        assert!(!limine_conf("u", Some(Ucode::Amd), false).contains("nvidia"));
    }

    #[test]
    fn nvidia_initramfs_appends_modules_and_leaves_hooks_alone() {
        assert_eq!(
            MKINITCPIO_NVIDIA,
            "MODULES+=(nvidia nvidia_modeset nvidia_uvm nvidia_drm)\n"
        );
        assert!(!MKINITCPIO_NVIDIA.contains("HOOKS"));
    }

    #[test]
    fn nvidia_build_is_checked_for_every_kernel() {
        let t = crate::testutil::TempDir::new();
        let m = t.path().join("usr/lib/modules");
        assert!(!nvidia_modules_built(&m), "no modules dir");
        let kernel = |name: &str, built: bool| {
            let d = m.join(name);
            fs::create_dir_all(d.join("updates/dkms")).unwrap();
            fs::write(d.join("vmlinuz"), b"k").unwrap();
            if built {
                fs::write(d.join("updates/dkms/nvidia.ko.zst"), b"m").unwrap();
            }
        };
        kernel("6.1-arch1-1", true);
        assert!(nvidia_modules_built(&m));
        kernel("6.2-lts", false);
        assert!(!nvidia_modules_built(&m), "one kernel missing the module");
        kernel("6.2-lts", true);
        assert!(nvidia_modules_built(&m));
        // A leftover dir with no kernel image is not a kernel.
        fs::create_dir_all(m.join("extramodules-old")).unwrap();
        assert!(nvidia_modules_built(&m));
    }

    #[test]
    fn pacman_options_are_uncommented_once() {
        let a = with_pacman_options("[options]\n#Color\n#ParallelDownloads = 5\n#VerbosePkgLists\n").unwrap();
        assert_eq!(a, "[options]\nColor\nParallelDownloads = 5\n#VerbosePkgLists\n");
        assert!(with_pacman_options(&a).is_none());
    }

    #[test]
    fn the_limine_hook_matches_the_reference_script() {
        let script = include_str!("../../../../packaging/iso/airootfs/root/install-eclipseos.sh");
        let body = script
            .split("95-limine-esp.hook <<'HOOK'\n")
            .nth(1)
            .and_then(|r| r.split("\nHOOK").next())
            .unwrap();
        assert_eq!(LIMINE_HOOK, format!("{body}\n"));
    }

    #[test]
    fn units_are_floor_plus_catalog_and_nothing_else() {
        let es = Catalog::builtin().resolve(&["ec-hyperion-bar".into()]).unwrap();
        assert_eq!(unit_list(&es), FLOOR_UNITS);
    }

    #[test]
    fn pacstrap_failures_are_classified() {
        use FailClass::*;
        let c = |t: &str| classify_pacstrap(t.as_bytes());
        assert_eq!(
            c(
                "error: failed retrieving file 'x.sig' from m : Operation too slow. Less than 1 bytes/sec\n\
               error: failed to commit transaction (unexpected error)"
            ),
            Network
        );
        assert_eq!(
            c("curl: (6) Could not resolve host: geo.mirror.pkgbuild.com"),
            Network
        );
        assert_eq!(
            c("error: failed retrieving file 'a' : Connection timed out after 10001"),
            Network
        );
        assert_eq!(c("error: x: signature from \"A\" is unknown trust"), Signature);
        assert_eq!(
            c("error: x: invalid or corrupted package (PGP signature)"),
            Signature
        );
        assert_eq!(
            c("error: failed to commit transaction (No space left on device)"),
            DiskFull
        );
        assert_eq!(c("error: Partition / too full: 10 blocks needed"), DiskFull);
        assert_eq!(c("error: target not found: nope"), Other);
        assert_eq!(c(""), Other);
        assert!(Network.retryable());
        assert!(![DiskFull, Signature, Other].iter().any(|k| k.retryable()));
        for k in [Network, DiskFull, Signature, Other] {
            assert!(k.message().ends_with("Try again."));
        }
    }

    #[test]
    fn mirrorlist_rotation_puts_the_failed_mirror_last() {
        let t = "# c\nServer = https://a\n#Server = https://off\nServer = https://b\n";
        let r = rotate_mirrorlist(t).unwrap();
        assert_eq!(
            r,
            "# c\n#Server = https://off\nServer = https://b\nServer = https://a\n"
        );
        assert_eq!(rotate_mirrorlist("Server = https://a\n"), None);
        assert_eq!(rotate_mirrorlist(""), None);
    }

    #[test]
    fn download_options_replace_existing_settings_under_options() {
        let t = "[options]\nHoldPkg = pacman\n#ParallelDownloads = 5\nParallelDownloads = 8\n\n[core]\nInclude = x\n";
        let r = with_download_options(t);
        assert_eq!(
            r,
            "[options]\nDisableDownloadTimeout\nParallelDownloads = 3\nHoldPkg = pacman\n\n[core]\nInclude = x\n"
        );
        assert_eq!(with_download_options(&r), r);
        assert!(with_download_options("[core]\n").starts_with("[options]\nDisableDownloadTimeout"));
    }
}
