// SPDX-License-Identifier: AGPL-3.0-only
//! The stages that change the machine, reimplemented from
//! `dist/iso/airootfs/root/install-eclipseos.sh` with fixed argv: every command
//! is a [`Tool`] plus arguments built here from validated values, never a shell
//! line. Nothing in this module runs before `Confirm` has allowed (the caller
//! in `apply` orders that; `Tool::touches_disk` lets tests prove it).

use crate::catalog::{valid_pkg_name, Entry, FLOOR_UNITS};
use crate::disks::{self, DiskEntry};
use crate::env::Env;
use crate::error::{io, Error, Result};
use crate::hw::Ucode;
use crate::runner::{Cmd, Tool};
use crate::target::{self, TargetFile};
use eclipse_setup_plan::{Plan, Request};
use std::ffi::OsString;
use std::fs;
use std::io::Write;
use std::os::unix::fs::OpenOptionsExt;
use std::path::PathBuf;
use zeroize::Zeroizing;

/// Everything the pre-disk stages worked out for the later ones.
pub struct Prepared {
    /// Floor list from the medium plus catalog packages, in order, no duplicates.
    pub pkgs: Vec<String>,
    /// Fingerprint of the packaging key on the medium.
    pub keyid: String,
    /// What this machine's CPU needs loaded ahead of the initramfs.
    pub ucode: Option<Ucode>,
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

pub fn package_set(base: Vec<String>, entries: &[&Entry]) -> Vec<String> {
    let mut all = base;
    for e in entries {
        for p in e.packages {
            if !all.iter().any(|a| a == p) {
                all.push((*p).to_owned());
            }
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

pub fn limine_conf(root_uuid: &str, ucode: Option<Ucode>) -> String {
    let pstate = if ucode == Some(Ucode::Amd) {
        " amd_pstate=active"
    } else {
        ""
    };
    let ucode = ucode
        .map(|u| format!("    module_path: boot():/{}\n", u.image()))
        .unwrap_or_default();
    format!(
        "timeout: 2\n\n\
         /EclipseOS\n\
         \x20   protocol: linux\n\
         \x20   path: boot():/vmlinuz-linux\n\
         \x20   cmdline: root=UUID={root_uuid} rootfstype=ext4 rw zswap.enabled=0{pstate}\n\
         {ucode}\
         \x20   module_path: boot():/initramfs-linux.img\n\n\
         /EclipseOS (fallback initramfs)\n\
         \x20   protocol: linux\n\
         \x20   path: boot():/vmlinuz-linux\n\
         \x20   cmdline: root=UUID={root_uuid} rootfstype=ext4 rw zswap.enabled=0\n\
         \x20   module_path: boot():/initramfs-linux-fallback.img\n"
    )
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

/// Best effort; also run after a failure so the disk is not left busy.
pub fn unmount(env: &Env) -> Result<()> {
    env.runner
        .run(&Cmd::new(Tool::Umount).arg("-R").arg(&env.paths.target))
}

// ---- Pacstrap ----------------------------------------------------------------

pub fn install_pacman_conf(env: &Env) -> Result<()> {
    let p = &env.paths;
    let mut conf = fs::read_to_string(&p.live_pacman_conf).map_err(io("read pacman.conf"))?;
    // Baked into the medium by dist/iso/build-iso.sh (D-03): the database is
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

pub fn pacstrap(env: &Env, prep: &Prepared) -> Result<()> {
    let p = &env.paths;
    let r = env.runner;
    // Trust the packaging key on the live side (to verify the baked repo) and in
    // the target's own keyring (its first `pacman -Syu` needs it).
    r.run(&Cmd::new(Tool::PacmanKey).arg("--add").arg(&p.packaging_key))?;
    r.run(&Cmd::new(Tool::PacmanKey).arg("--lsign-key").arg(&prep.keyid))?;
    install_pacman_conf(env)?;
    r.run(
        &Cmd::new(Tool::Pacstrap)
            .args(["-K", "-C"])
            .arg(&p.install_pacman_conf)
            .arg(&p.target)
            .args(&prep.pkgs),
    )?;
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
    let fstab = r.capture(&Cmd::new(Tool::Genfstab).arg("-U").arg(&p.target))?;
    let mut all = target::read(&p.target, TargetFile::Fstab)?;
    all.extend_from_slice(&fstab);
    target::write(&p.target, TargetFile::Fstab, &all)
}

// ---- Configure ---------------------------------------------------------------

pub fn configure(env: &Env, plan: &Plan) -> Result<()> {
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
    target::write(t, TargetFile::SudoersWheel, SUDOERS_WHEEL.as_bytes())?;

    env.runner.run(&chroot(env).arg("locale-gen"))?;
    // Virtual machines and some firmware have no usable RTC; the disk is already
    // erased, so this must not fail the install.
    let _ = env.runner.run(&chroot(env).args(["hwclock", "--systohc"]));
    env.runner.run(&chroot(env).args(["mkinitcpio", "-P"]))
}

// ---- Bootloader --------------------------------------------------------------

pub fn bootloader(env: &Env, l: &Layout, ucode: Option<Ucode>) -> Result<()> {
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
    target::write(t, TargetFile::LimineConf, limine_conf(uuid, ucode).as_bytes())?;
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
            .resolve(&["hyperion".into(), "eclipse-toasts".into()])
            .unwrap();
        let s = package_set(vec!["base".into(), "eclipseos-hyperion".into()], &es);
        assert_eq!(s, ["base", "eclipseos-hyperion", "eclipseos-toasts"]);
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
        let c = limine_conf("abcd-1234", Some(Ucode::Amd));
        assert!(c.starts_with("timeout: 2\n\n/EclipseOS\n    protocol: linux\n"));
        assert!(c.contains("    cmdline: root=UUID=abcd-1234 rootfstype=ext4 rw zswap.enabled=0 amd_pstate=active\n    module_path: boot():/amd-ucode.img\n    module_path: boot():/initramfs-linux.img\n"));
        assert!(c.contains("/EclipseOS (fallback initramfs)"));
        assert!(!limine_conf("u", None).contains("ucode"));
        let i = limine_conf("u", Some(Ucode::Intel));
        assert!(i.contains("module_path: boot():/intel-ucode.img") && !i.contains("amd_pstate"));
        assert!(valid_uuid("1b2c-3D4e") && !valid_uuid("a b") && !valid_uuid("x\ny") && !valid_uuid(""));
    }

    #[test]
    fn pacman_options_are_uncommented_once() {
        let a = with_pacman_options("[options]\n#Color\n#ParallelDownloads = 5\n#VerbosePkgLists\n").unwrap();
        assert_eq!(a, "[options]\nColor\nParallelDownloads = 5\n#VerbosePkgLists\n");
        assert!(with_pacman_options(&a).is_none());
    }

    #[test]
    fn the_limine_hook_matches_the_reference_script() {
        let script = include_str!("../../../dist/iso/airootfs/root/install-eclipseos.sh");
        let body = script
            .split("95-limine-esp.hook <<'HOOK'\n")
            .nth(1)
            .and_then(|r| r.split("\nHOOK").next())
            .unwrap();
        assert_eq!(LIMINE_HOOK, format!("{body}\n"));
    }

    #[test]
    fn units_are_floor_plus_catalog_and_nothing_else() {
        let es = Catalog::builtin().resolve(&["hyperion".into()]).unwrap();
        assert_eq!(unit_list(&es), FLOOR_UNITS);
    }
}
