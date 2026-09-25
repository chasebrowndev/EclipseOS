// SPDX-License-Identifier: AGPL-3.0-only
//! The `apply` orchestrator (D-07 §6, §8). Order is the safety property:
//!
//! 1. `Validate` and `Preflight` fail without touching the disk: guards, plan
//!    validators, the medium's own files, free space, mirror reachability.
//! 2. `Confirm` asks a [`Confirm`] whether to erase this disk. The answer is
//!    "no" unless something positively says yes; an error is "no".
//! 3. Only then `Partition`, `Pacstrap`, `Configure`, `Bootloader`, `User`,
//!    `Seed`, `Units`, and `Done`.
//!
//! The first failed stage is reported (`failed: true`) and ends the run.

use crate::disks;
use crate::env::{Env, Paths};
use crate::error::{io, Error, Result};
use crate::hw;
use crate::network;
use crate::runner::{Cmd, Tool};
use crate::seed;
use crate::stages::{self, Prepared};
use crate::validate::{self, Validated};
use eclipse_setup_plan::{Progress, Request, Stage};
use std::fs;
use std::io::Write;
use std::os::unix::fs::OpenOptionsExt;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

/// Smaller than this cannot hold the package set plus a usable home.
pub const MIN_DISK_BYTES: u64 = 16 * 1024 * 1024 * 1024;

/// Sized for the Abyss desktop plus the installer running from RAM. `MemTotal`
/// reads a little under the fitted amount, so this is roughly a 2 GB machine.
pub const MIN_MEM_KIB: u64 = 1_800_000;

/// Arch's own packages come from the mirrors; check before touching the disk.
const MIRROR_PROBE: &str = "https://geo.mirror.pkgbuild.com/lastupdate";

/// Bounds a package list file the medium supplies.
const MAX_PKG_LIST_BYTES: u64 = 256 * 1024;

struct Reporter<'a> {
    out: &'a mut dyn Write,
    /// The same lines, kept for the failure screen and the installed system.
    /// They are fixed-vocabulary and never echo content, so this is not secret.
    log: Option<fs::File>,
}

fn open_log(p: &Paths) -> Option<fs::File> {
    let _ = fs::remove_file(&p.install_log);
    fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .custom_flags(rustix::fs::OFlags::NOFOLLOW.bits() as i32)
        .mode(0o644)
        .open(&p.install_log)
        .ok()
}

impl Reporter<'_> {
    fn line(&mut self, stage: Stage, pct: u8, msg: &str, failed: bool) {
        let p = Progress {
            stage,
            pct,
            msg: msg.to_owned(),
            failed,
        };
        // A closed stdout must not stop an install that is already under way.
        if let Ok(mut s) = serde_json::to_string(&p) {
            s.push('\n');
            let _ = self.out.write_all(s.as_bytes());
            let _ = self.out.flush();
            if let Some(f) = &mut self.log {
                let ts = SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .map_or(0, |d| d.as_secs());
                let _ = writeln!(f, "{ts} {}", s.trim_end());
            }
        }
    }
}

/// Everything that can fail without touching a disk, after `validate`.
pub fn preflight(env: &Env, v: &Validated) -> Result<Prepared> {
    let p = &env.paths;
    if v.disk.disk.size_bytes < MIN_DISK_BYTES {
        return Err(Error::Refused("disk too small"));
    }
    // The medium must carry what the reference script assumed it did (D-03).
    if !p.repo.is_dir() {
        return Err(Error::Refused("medium has no baked package repo"));
    }
    if !p.live_pacman_conf.is_file() {
        return Err(Error::Refused("medium has no pacman.conf"));
    }
    let md = fs::metadata(&p.pkg_list).map_err(io("read package list"))?;
    if !md.is_file() || md.len() > MAX_PKG_LIST_BYTES {
        return Err(Error::Refused("package list on the medium"));
    }
    let text = fs::read_to_string(&p.pkg_list).map_err(io("read package list"))?;
    let mut pkgs = stages::package_set(stages::parse_pkg_list(&text)?, &v.entries);

    let machine = hw::detect(p);
    if machine.mem_kib < MIN_MEM_KIB {
        return Err(Error::Refused("not enough memory"));
    }
    for extra in machine.packages() {
        if !pkgs.iter().any(|q| q == extra) {
            pkgs.push(extra.to_owned());
        }
    }
    let mut warnings = Vec::new();
    if machine.on_battery {
        warnings.push("running on battery: plug in before installing");
    }

    if !p.packaging_key.is_file() {
        return Err(Error::Refused("medium has no packaging key"));
    }
    let mut gpg = Cmd::new(Tool::Gpg)
        .args(["--show-keys", "--with-colons"])
        .arg(&p.packaging_key);
    gpg.silence_stderr = true;
    let colons = String::from_utf8(env.runner.capture(&gpg)?).map_err(|_| Error::Refused("packaging key"))?;
    let keyid = stages::parse_fpr(&colons).ok_or(Error::Refused("packaging key"))?;

    // Nothing may already be mounted at the target or from the chosen disk.
    let mounts = disks::read_mounts(p)?;
    let target = p.target.to_string_lossy();
    if mounts.iter().any(|m| m.point == target) {
        return Err(Error::Refused("target already mounted"));
    }
    if disks::in_use(&mounts, &v.disk) {
        return Err(Error::Refused("disk has mounted partitions"));
    }
    if let Some(why) = disks::busy_reason(p, &v.disk) {
        return Err(Error::Refused(why));
    }

    let probe = Cmd::new(Tool::Curl)
        .args(["-fsS", "--max-time", "10", "-o", "/dev/null"])
        .arg(MIRROR_PROBE);
    env.runner
        .run(&probe)
        .map_err(|_| Error::Refused("no network: package mirror unreachable"))?;

    // The signatures on the packages are checked against the clock.
    if !wait_for_clock(p) {
        return Err(Error::Refused("system clock not synchronised"));
    }
    // Everything the install will download must resolve now: after the erase
    // there is no going back to the old system.
    stages::install_pacman_conf(env)?;
    let conf = &p.install_pacman_conf;
    let pacman = |args: &[&str]| {
        Cmd::new(Tool::Pacman)
            .arg("--config")
            .arg(conf)
            .args(args.iter().copied())
    };
    env.runner
        .run(&pacman(&["-Sy", "--noconfirm", "archlinux-keyring"]))
        .map_err(|_| Error::Refused("could not refresh the package keyring"))?;
    env.runner
        .run(&Cmd::new(Tool::PacmanKey).args(["--populate", "archlinux"]))
        .map_err(|_| Error::Refused("could not refresh the package keyring"))?;
    env.runner
        .run(&pacman(&["-Sp", "--noconfirm"]).args(&pkgs))
        .map_err(|_| Error::Refused("a package could not be resolved"))?;

    Ok(Prepared {
        pkgs,
        keyid,
        ucode: machine.ucode(),
        warnings,
    })
}

/// True once timesyncd has set the clock, waiting up to `clock_wait` for it.
fn wait_for_clock(p: &Paths) -> bool {
    let end = Instant::now() + p.clock_wait;
    loop {
        if p.ntp_synced.exists() {
            return true;
        }
        if Instant::now() >= end {
            return false;
        }
        std::thread::sleep(Duration::from_millis(500));
    }
}

struct Run<'a, 'b> {
    env: &'a Env<'a>,
    req: &'a Request,
    rep: Reporter<'b>,
    mounted: bool,
}

impl Run<'_, '_> {
    /// Announce a stage, run it, and turn its error into the failed line.
    fn stage<T>(
        &mut self,
        stage: Stage,
        pct: u8,
        msg: &str,
        f: impl FnOnce(&mut Self) -> Result<T>,
    ) -> Result<T> {
        self.rep.line(stage, pct, msg, false);
        match f(self) {
            Ok(v) => Ok(v),
            Err(e) => {
                self.rep.line(stage, pct, &e.to_string(), true);
                Err(e)
            }
        }
    }

    fn go(&mut self) -> Result<()> {
        let env = self.env;
        let req = self.req;

        let v = self.stage(Stage::Validate, 2, "validating the plan", |_| {
            validate::validate(env, req)
        })?;
        let prep = self.stage(
            Stage::Preflight,
            8,
            "checking the machine and the network",
            |_| preflight(env, &v),
        )?;
        for w in &prep.warnings {
            self.rep.line(Stage::Preflight, 10, w, false);
        }

        self.stage(
            Stage::Confirm,
            12,
            "waiting for confirmation to erase the disk",
            |_| {
                // Err is a "no": a transport that cannot be reached must not erase.
                match env.confirm.confirm_erase(&v.disk.disk) {
                    Ok(true) => Ok(()),
                    Ok(false) => Err(Error::Refused("erase not confirmed")),
                    Err(_) => Err(Error::Refused("erase could not be confirmed")),
                }
            },
        )?;

        let layout = self.stage(Stage::Partition, 20, "partitioning the disk", |s| {
            let l = stages::partition(env, &v.disk)?;
            // Set before the mount so a half-done mount is still unwound.
            s.mounted = true;
            stages::mount(env, &l)?;
            Ok(l)
        })?;
        self.stage(Stage::Pacstrap, 30, "installing packages", |_| {
            stages::pacstrap(env, &prep)
        })?;
        self.stage(Stage::Configure, 75, "configuring the system", |_| {
            stages::configure(env, &req.plan)
        })?;
        self.stage(Stage::Bootloader, 85, "installing the bootloader", |_| {
            stages::bootloader(env, &layout, prep.ucode)
        })?;
        self.stage(Stage::User, 90, "creating the user", |_| stages::user(env, req))?;
        let outcome = self.stage(Stage::Seed, 94, "carrying over settings", |_| {
            seed::run(env, &req.plan.username)
        })?;
        self.rep.line(Stage::Seed, 95, &outcome.message(), false);
        if req.plan.carry_network {
            let n = network::carry(env);
            self.rep.line(
                Stage::Seed,
                95,
                if n == 0 {
                    "no network connections carried"
                } else {
                    "network connections carried"
                },
                false,
            );
        }
        self.stage(Stage::Units, 97, "enabling services", |_| {
            stages::units(env, &v.entries)
        })?;
        // Last, so the copy holds everything but the closing lines. Best effort.
        let _ = stages::keep_log(env);
        Ok(())
    }
}

/// Run the whole install, writing one [`Progress`] JSON line per event to `out`.
/// Returns whether it finished. The password is only ever read by `validate`
/// (for its shape) and `stages::user` (into `chpasswd`'s stdin).
pub fn apply(env: &Env, req: &Request, out: &mut dyn Write) -> bool {
    let mut run = Run {
        env,
        req,
        rep: Reporter {
            out,
            log: open_log(&env.paths),
        },
        mounted: false,
    };
    let ok = run.go().is_ok();
    if run.mounted {
        // The disk must not be left busy either way. A failure to unmount after
        // success is a failure to finish: the user would reboot into a dirty fs.
        let un = stages::unmount(env);
        if ok && un.is_err() {
            run.rep
                .line(Stage::Done, 99, "could not unmount the target", true);
            return false;
        }
    }
    if ok {
        run.rep.line(Stage::Done, 100, "done", false);
    }
    ok
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::confirm::{AllowConfirm, Confirm, DenyConfirm};
    use crate::env::Paths;
    use crate::runner::Runner;
    use crate::testutil::{live, my_uid, req, FakeRunner, LSBLK};
    use eclipse_setup_plan::Disk;
    use std::collections::BTreeSet;
    use std::path::Path;

    fn progress(buf: &[u8]) -> Vec<Progress> {
        std::str::from_utf8(buf)
            .unwrap()
            .lines()
            .map(|l| serde_json::from_str(l).unwrap())
            .collect()
    }

    /// Big enough for preflight; the fixture's nvme disk is 1 TB already.
    fn machine_runner(p: &Paths) -> FakeRunner {
        let target = p.target.clone();
        let keyfpr = format!("fpr:::::::::{}:\n", "AB".repeat(20));
        FakeRunner::new(move |c| {
            let has = |s: &str| c.args.iter().any(|a| a == s);
            match c.tool {
                Tool::Lsblk => Ok(LSBLK.as_bytes().to_vec()),
                Tool::Gpg => Ok(keyfpr.clone().into_bytes()),
                Tool::Blkid => Ok(b"1234-ABCD\n".to_vec()),
                Tool::Genfstab => Ok(b"# fstab\nUUID=1 / ext4 rw 0 1\n".to_vec()),
                Tool::Mount if has("--mkdir") => {
                    fs::create_dir_all(target.join("boot")).unwrap();
                    Ok(Vec::new())
                }
                Tool::Pacstrap => {
                    // What the packages put in the tree.
                    for d in ["etc/pacman.d/gnupg", "usr/share/limine", "home", "boot"] {
                        fs::create_dir_all(target.join(d)).unwrap();
                    }
                    fs::write(target.join("usr/share/limine/BOOTX64.EFI"), b"MZ").unwrap();
                    fs::write(
                        target.join("etc/locale.gen"),
                        "#en_US.UTF-8 UTF-8\n#de_DE.UTF-8 UTF-8\n",
                    )
                    .unwrap();
                    fs::write(target.join("etc/pacman.conf"), "[options]\n").unwrap();
                    fs::write(target.join("etc/passwd"), "root:x:0:0::/root:/bin/bash\n").unwrap();
                    Ok(Vec::new())
                }
                Tool::ArchChroot if has("useradd") => {
                    let name = c.args.last().unwrap().to_string_lossy().into_owned();
                    fs::create_dir_all(target.join("home").join(&name)).unwrap();
                    let mut pw = fs::read_to_string(target.join("etc/passwd")).unwrap();
                    pw.push_str(&format!(
                        "{name}:x:{}:{}::/home/{name}:/bin/bash\n",
                        my_uid(),
                        rustix::process::getgid().as_raw()
                    ));
                    fs::write(target.join("etc/passwd"), pw).unwrap();
                    Ok(Vec::new())
                }
                _ => Ok(Vec::new()),
            }
        })
    }

    fn env<'a>(p: Paths, r: &'a dyn Runner, c: &'a dyn Confirm, hint: Option<String>) -> Env<'a> {
        Env {
            paths: p,
            euid: 0,
            caller_uid_hint: hint,
            runner: r,
            confirm: c,
        }
    }

    struct Failing;
    impl Confirm for Failing {
        fn confirm_erase(&self, _: &Disk) -> Result<bool> {
            Err(Error::Io("no seat"))
        }
    }

    struct Recording(std::cell::RefCell<Vec<Disk>>);
    impl Confirm for Recording {
        fn confirm_erase(&self, d: &Disk) -> Result<bool> {
            self.0.borrow_mut().push(d.clone());
            Ok(true)
        }
    }

    fn file_set(root: &Path) -> BTreeSet<String> {
        fn walk(dir: &Path, root: &Path, out: &mut BTreeSet<String>) {
            for e in fs::read_dir(dir).unwrap() {
                let e = e.unwrap();
                let path = e.path();
                let md = fs::symlink_metadata(&path).unwrap();
                if md.is_dir() {
                    walk(&path, root, out);
                } else {
                    out.insert(path.strip_prefix(root).unwrap().to_string_lossy().into_owned());
                }
            }
        }
        let mut s = BTreeSet::new();
        walk(root, root, &mut s);
        s
    }

    #[test]
    fn deny_confirm_stops_before_any_disk_command() {
        let (_t, p) = live();
        let r = machine_runner(&p);
        let mut out = Vec::new();
        let ok = apply(&env(p, &r, &DenyConfirm, None), &req(), &mut out);
        assert!(!ok);
        let lines = progress(&out);
        let last = lines.last().unwrap();
        assert_eq!((last.stage, last.failed), (Stage::Confirm, true));
        // Everything before Confirm ran (network probe included), nothing after.
        assert!(r.issued(Tool::Curl));
        for t in r.tools() {
            assert!(!t.touches_disk(), "{t:?} issued before confirmation");
        }
        assert!(!r.issued(Tool::Blkid));
        // Refreshing the live keyring is fine; trusting the packaging key is pacstrap's.
        assert!(!r
            .log
            .borrow()
            .iter()
            .any(|l| l.tool == Tool::PacmanKey && l.args.iter().any(|a| a == "--add" || a == "--lsign-key")));
        assert!(!lines.iter().any(|l| l.stage == Stage::Partition));
    }

    #[test]
    fn an_erroring_confirm_is_a_no() {
        let (_t, p) = live();
        let r = machine_runner(&p);
        let mut out = Vec::new();
        assert!(!apply(&env(p, &r, &Failing, None), &req(), &mut out));
        assert!(r.tools().iter().all(|t| !t.touches_disk()));
    }

    #[test]
    fn confirm_is_shown_the_helpers_own_description_of_the_disk() {
        let (_t, p) = live();
        let r = machine_runner(&p);
        let c = Recording(Default::default());
        let mut out = Vec::new();
        let _ = apply(&env(p, &r, &c, None), &req(), &mut out);
        let seen = c.0.borrow();
        assert_eq!(seen.len(), 1);
        assert_eq!(seen[0].by_id, "nvme-Some_NVMe_1");
        assert_eq!(seen[0].size_bytes, 1_000_204_886_016);
    }

    fn refuses(mutate: impl FnOnce(&mut Request, &Paths), stage: Stage) {
        let (_t, p) = live();
        let r = machine_runner(&p);
        let mut q = req();
        mutate(&mut q, &p);
        let mut out = Vec::new();
        assert!(!apply(&env(p, &r, &AllowConfirm, None), &q, &mut out));
        let lines = progress(&out);
        let last = lines.last().unwrap();
        assert!(last.failed);
        assert_eq!(last.stage, stage);
        assert!(r.tools().iter().all(|t| !t.touches_disk()));
    }

    #[test]
    fn validate_failures_touch_nothing() {
        refuses(|q, _| q.plan.disk_by_id = "usb-Stick_1".into(), Stage::Validate);
        refuses(|q, _| q.plan.candidates = vec!["waybar".into()], Stage::Validate);
        refuses(|q, _| q.plan.username = "root".into(), Stage::Validate);
        refuses(
            |q, _| q.password = zeroize::Zeroizing::new("a\nb".into()),
            Stage::Validate,
        );
        refuses(|_, p| fs::remove_dir_all(&p.archiso).unwrap(), Stage::Validate);
    }

    #[test]
    fn preflight_failures_touch_nothing() {
        refuses(|_, p| fs::remove_dir_all(&p.repo).unwrap(), Stage::Preflight);
        refuses(|_, p| fs::remove_file(&p.pkg_list).unwrap(), Stage::Preflight);
        refuses(
            |_, p| fs::write(&p.pkg_list, "base\n$(reboot)\n").unwrap(),
            Stage::Preflight,
        );
        refuses(
            |_, p| fs::remove_file(&p.packaging_key).unwrap(),
            Stage::Preflight,
        );
    }

    #[test]
    fn machine_and_package_checks_refuse_before_the_wipe() {
        refuses(
            |_, p| fs::write(&p.proc_meminfo, "MemTotal: 900000 kB\n").unwrap(),
            Stage::Preflight,
        );
        refuses(|_, p| fs::remove_file(&p.ntp_synced).unwrap(), Stage::Preflight);
        refuses(
            |_, p| {
                fs::write(
                    &p.proc_swaps,
                    "Filename Type Size Used Priority\n/dev/nvme0n1 partition 1 0 -2\n",
                )
                .unwrap()
            },
            Stage::Preflight,
        );
    }

    #[test]
    fn a_failing_keyring_or_closure_check_refuses_before_the_wipe() {
        for bad in [Tool::PacmanKey, Tool::Pacman] {
            let (_t, p) = live();
            let inner = machine_runner(&p);
            let r = FakeRunner::new(move |c| {
                if c.tool == bad {
                    return Err(Error::Command {
                        tool: "x",
                        code: Some(1),
                    });
                }
                inner.capture(c)
            });
            let mut out = Vec::new();
            assert!(!apply(&env(p, &r, &AllowConfirm, None), &req(), &mut out));
            assert_eq!(progress(&out).pop().unwrap().stage, Stage::Preflight);
            assert!(r.tools().iter().all(|t| !t.touches_disk()));
        }
    }

    #[test]
    fn a_missing_rtc_does_not_fail_the_run() {
        let (_t, p) = live();
        let inner = machine_runner(&p);
        let r = FakeRunner::new(move |c| {
            if c.args.iter().any(|a| a == "hwclock") {
                return Err(Error::Command {
                    tool: "arch-chroot",
                    code: Some(1),
                });
            }
            inner.capture(c)
        });
        let mut out = Vec::new();
        assert!(apply(&env(p, &r, &AllowConfirm, None), &req(), &mut out));
    }

    #[test]
    fn stale_signatures_are_wiped_after_confirm_and_before_partitioning() {
        let (_t, p) = live();
        let r = machine_runner(&p);
        let mut out = Vec::new();
        assert!(apply(&env(p, &r, &AllowConfirm, None), &req(), &mut out));
        let t = r.tools();
        let w = t.iter().position(|t| *t == Tool::Wipefs).expect("wipefs");
        let s = t.iter().position(|t| *t == Tool::Sgdisk).unwrap();
        assert!(w < s);
    }

    #[test]
    fn no_network_is_a_preflight_refusal() {
        let (_t, p) = live();
        let inner = machine_runner(&p);
        let r = FakeRunner::new(move |c| {
            if c.tool == Tool::Curl {
                return Err(Error::Command {
                    tool: "curl",
                    code: Some(6),
                });
            }
            inner.capture(c)
        });
        let mut out = Vec::new();
        assert!(!apply(&env(p, &r, &AllowConfirm, None), &req(), &mut out));
        let last = progress(&out).pop().unwrap();
        assert_eq!(last.stage, Stage::Preflight);
        assert!(last.msg.contains("no network"));
        assert!(r.tools().iter().all(|t| !t.touches_disk()));
    }

    #[test]
    fn a_mounted_target_or_disk_is_a_preflight_refusal() {
        for extra in ["/mnt", "nvme"] {
            let (_t, p) = live();
            let mut mi = fs::read_to_string(&p.mountinfo).unwrap();
            if extra == "/mnt" {
                mi.push_str(&format!(
                    "40 25 8:1 / {} rw - ext4 /dev/x rw\n",
                    p.target.display()
                ));
            } else {
                mi.push_str("40 25 259:1 / /somewhere rw - ext4 /dev/nvme0n1 rw\n");
            }
            fs::write(&p.mountinfo, mi).unwrap();
            let r = machine_runner(&p);
            let mut out = Vec::new();
            assert!(
                !apply(&env(p, &r, &AllowConfirm, None), &req(), &mut out),
                "{extra}"
            );
            assert_eq!(progress(&out).last().unwrap().stage, Stage::Preflight, "{extra}");
            assert!(r.tools().iter().all(|t| !t.touches_disk()), "{extra}");
        }
    }

    #[test]
    fn a_too_small_disk_is_refused() {
        // sdb-like: 8 GB; give the request the small non-boot disk by rewriting lsblk.
        let (_t, p) = live();
        let small = LSBLK.replace("1000204886016", "8000000000");
        let inner = machine_runner(&p);
        let r = FakeRunner::new(move |c| {
            if c.tool == Tool::Lsblk {
                return Ok(small.clone().into_bytes());
            }
            inner.capture(c)
        });
        let mut out = Vec::new();
        assert!(!apply(&env(p, &r, &AllowConfirm, None), &req(), &mut out));
        let last = progress(&out).pop().unwrap();
        assert_eq!(
            (last.stage, last.msg.as_str()),
            (Stage::Preflight, "refused: disk too small")
        );
    }

    #[test]
    fn full_run_issues_fixed_commands_and_stays_inside_the_write_surface() {
        let (t, p) = live();
        // The live user's seed, with a command-bearing key that must not travel.
        let home = t.path().join("home/liveuser/.config/eclipse");
        fs::create_dir_all(&home).unwrap();
        fs::write(
            home.join("abyss.kdl"),
            "mode \"wm\"\nbind \"SUPER\" \"x\" { spawn \"evil\"; }\n",
        )
        .unwrap();
        let target = p.target.clone();
        let r = machine_runner(&p);
        let mut out = Vec::new();
        let ok = apply(
            &env(p, &r, &AllowConfirm, Some(my_uid().to_string())),
            &req(),
            &mut out,
        );
        assert!(ok, "{:?}", progress(&out).last());

        let lines = progress(&out);
        assert_eq!(lines.last().unwrap().stage, Stage::Done);
        assert_eq!(lines.last().unwrap().pct, 100);
        assert!(lines.iter().all(|l| !l.failed));
        let stages: Vec<Stage> = lines.iter().map(|l| l.stage).collect();
        let first = |s| stages.iter().position(|x| *x == s).unwrap();
        let order = [
            Stage::Validate,
            Stage::Preflight,
            Stage::Confirm,
            Stage::Partition,
            Stage::Pacstrap,
            Stage::Configure,
            Stage::Bootloader,
            Stage::User,
            Stage::Seed,
            Stage::Units,
            Stage::Done,
        ];
        for w in order.windows(2) {
            assert!(first(w[0]) < first(w[1]), "{:?} before {:?}", w[0], w[1]);
        }
        assert!(lines.windows(2).all(|w| w[0].pct <= w[1].pct));

        // Disk commands are fixed argv on the validated disk only.
        let log = r.log.borrow();
        let sg: Vec<_> = log.iter().filter(|l| l.tool == Tool::Sgdisk).collect();
        assert_eq!(sg.len(), 2);
        assert!(sg[0]
            .args
            .iter()
            .all(|a| a == "--zap-all" || a.ends_with("/dev/nvme0n1")));
        let mk: Vec<_> = log
            .iter()
            .filter(|l| matches!(l.tool, Tool::MkfsFat | Tool::MkfsExt4))
            .collect();
        assert!(mk[0].args.last().unwrap().ends_with("nvme0n1p1"));
        assert!(mk[1].args.last().unwrap().ends_with("nvme0n1p2"));
        // Nothing ever touched the boot medium's disk.
        for l in log.iter() {
            assert!(l.args.iter().all(|a| !a.contains("sdb")), "{:?}", l.tool);
        }
        // Package names came from the medium's list and the catalog, in one pacstrap.
        let ps: Vec<_> = log.iter().filter(|l| l.tool == Tool::Pacstrap).collect();
        assert_eq!(ps.len(), 1);
        for want in [
            "base",
            "linux",
            "limine",
            "eclipseos-meta",
            "eclipseos-hyperion",
            "eclipseos-toasts",
        ] {
            assert!(ps[0].args.iter().any(|a| a == want), "{want}");
        }
        // Units: the floor, in one call.
        let en = log
            .iter()
            .find(|l| l.tool == Tool::ArchChroot && l.args.iter().any(|a| a == "systemctl"))
            .unwrap();
        assert!(en.args.iter().any(|a| a == "greetd") && en.args.iter().any(|a| a == "NetworkManager"));
        // The target was unmounted last.
        assert_eq!(log.last().unwrap().tool, Tool::Umount);

        // The password reached chpasswd's stdin, and only there.
        let pw = b"chase:correct horse\n";
        for l in log.iter() {
            let via_stdin = l.tool == Tool::ArchChroot && l.args.iter().any(|a| a == "chpasswd");
            assert_eq!(l.stdin.as_deref() == Some(pw.as_slice()), via_stdin);
            assert!(l.args.iter().all(|a| !a.contains("correct horse")));
        }
        drop(log);
        let raw = std::str::from_utf8(&out).unwrap();
        assert!(!raw.contains("correct horse") && !raw.contains("horse"));

        // Config on the target.
        let rd = |f: &str| fs::read_to_string(target.join(f)).unwrap();
        assert_eq!(rd("etc/hostname"), "eclipse\n");
        assert_eq!(rd("etc/locale.conf"), "LANG=en_US.UTF-8\n");
        assert!(rd("etc/locale.gen").starts_with("en_US.UTF-8 UTF-8\n#de_DE"));
        assert_eq!(rd("etc/sudoers.d/wheel"), "%wheel ALL=(ALL:ALL) ALL\n");
        assert!(rd("etc/pacman.conf").contains("eclipseos.conf"));
        assert!(rd("etc/fstab").contains("UUID=1"));
        assert!(rd("boot/limine.conf").contains("root=UUID=1234-ABCD rootfstype=ext4 rw"));
        assert_eq!(
            fs::read_link(target.join("etc/localtime")).unwrap(),
            Path::new("/usr/share/zoneinfo/America/New_York")
        );
        let seed = rd("home/chase/.config/eclipse/abyss.kdl");
        assert!(seed.contains("mode") && !seed.contains("evil") && !seed.contains("bind"));

        // The target holds only what the fake packages put there plus the D-07 §6 surface.
        let allowed: BTreeSet<&str> = [
            // the fake pacstrap and useradd
            "usr/share/limine/BOOTX64.EFI",
            "etc/passwd",
            // the write surface (`etc/localtime` is a link)
            "etc/localtime",
            "etc/hostname",
            "etc/locale.gen",
            "etc/locale.conf",
            "etc/fstab",
            "etc/pacman.conf",
            "etc/sudoers.d/wheel",
            "boot/limine.conf",
            "boot/EFI/BOOT/BOOTX64.EFI",
            "etc/pacman.d/hooks/95-limine-esp.hook",
            "etc/vconsole.conf",
            "etc/systemd/zram-generator.conf",
            "var/log/eclipseos-install/install.log",
            "home/chase/.config/eclipse/abyss.kdl",
        ]
        .into();
        for f in file_set(&target) {
            assert!(allowed.contains(f.as_str()), "unexpected file on target: {f}");
        }
        // `etc/localtime` is a link, which `file_set` reports too.
        assert!(fs::symlink_metadata(target.join("etc/localtime")).is_ok());
    }

    #[test]
    fn a_failing_stage_stops_the_run_and_unmounts() {
        let (_t, p) = live();
        let inner = machine_runner(&p);
        let r = FakeRunner::new(move |c| {
            if c.tool == Tool::Pacstrap {
                return Err(Error::Command {
                    tool: "pacstrap",
                    code: Some(1),
                });
            }
            inner.capture(c)
        });
        let mut out = Vec::new();
        assert!(!apply(&env(p, &r, &AllowConfirm, None), &req(), &mut out));
        let lines = progress(&out);
        let last = lines.last().unwrap();
        assert_eq!((last.stage, last.failed), (Stage::Pacstrap, true));
        assert!(!lines.iter().any(|l| l.stage == Stage::Configure));
        assert!(!r.issued(Tool::ArchChroot));
        assert_eq!(r.tools().last(), Some(&Tool::Umount));
    }

    #[test]
    fn a_failing_password_command_never_reports_the_password() {
        let (_t, p) = live();
        let inner = machine_runner(&p);
        let r = FakeRunner::new(move |c| {
            if c.stdin.is_some() {
                return Err(Error::Command {
                    tool: "arch-chroot",
                    code: Some(1),
                });
            }
            inner.capture(c)
        });
        let mut out = Vec::new();
        assert!(!apply(&env(p, &r, &AllowConfirm, None), &req(), &mut out));
        let raw = String::from_utf8(out).unwrap();
        assert!(raw.contains("\"stage\":\"user\"") && raw.contains("\"failed\":true"));
        assert!(!raw.contains("horse"));
        let dbg = format!(
            "{:?}",
            r.log.borrow().iter().map(|l| l.args.clone()).collect::<Vec<_>>()
        );
        assert!(!dbg.contains("horse"));
    }

    #[test]
    fn seed_problems_do_not_fail_the_install() {
        let (_t, p) = live();
        // No seed at all for the caller.
        let target = p.target.clone();
        let r = machine_runner(&p);
        let mut out = Vec::new();
        assert!(apply(
            &env(p, &r, &AllowConfirm, Some(my_uid().to_string())),
            &req(),
            &mut out
        ));
        assert!(!target.join("home/chase/.config").exists());
        let lines = progress(&out);
        assert!(lines
            .iter()
            .any(|l| l.stage == Stage::Seed && l.msg == "seed absent"));
    }
}
