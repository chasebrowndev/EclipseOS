// SPDX-License-Identifier: AGPL-3.0-only
//! The `Validate` stage: every reason to refuse that needs no disk access and no
//! network (D-07 §4.2, §4.3, §6, §8). Nothing in the request is trusted because
//! the UI checked it.

use crate::catalog::{Catalog, Entry};
use crate::disks::{self, DiskEntry};
use crate::env::{Env, Paths};
use crate::error::{io, Error, Result};
use eclipse_setup_plan::{
    check_password, valid_by_id, valid_hostname, valid_keymap, valid_username, Plan, Request,
};
use std::fs;
use std::io::Read;

pub struct Validated {
    pub disk: DiskEntry,
    pub entries: Vec<&'static Entry>,
}

/// Locales the target can generate: `NAME.UTF-8 UTF-8` lines of `/etc/locale.gen`,
/// commented or not. (`locale -a` lists only what is already generated on the
/// live medium, a handful; `locale.gen` is the list `locale-gen` itself uses, so
/// it is the list the Configure stage can honour.)
pub fn locale_names(text: &str) -> impl Iterator<Item = &str> {
    text.lines().filter_map(|l| {
        let l = l.trim_start_matches('#').trim();
        let mut t = l.split_whitespace();
        let (name, charset) = (t.next()?, t.next()?);
        let ok = t.next().is_none()
            && charset == "UTF-8"
            && name.ends_with(".UTF-8")
            && name.len() <= 64
            && name
                .bytes()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, b'_' | b'.' | b'@' | b'-'));
        ok.then_some(name)
    })
}

pub fn valid_locale(locale_gen: &str, s: &str) -> bool {
    locale_names(locale_gen).any(|n| n == s)
}

/// A tzdata zone: `Region/City` (or `UTC`), each component a plain name, the
/// file present under the zoneinfo root and actually a TZif file (so `zone.tab`,
/// `tzdata.zi` and friends do not qualify). `posix/` and `right/` are duplicate
/// trees, not offered.
pub fn valid_timezone(paths: &Paths, s: &str) -> bool {
    if s.is_empty() || s.len() > 64 {
        return false;
    }
    let mut first = true;
    for comp in s.split('/') {
        let ok = !comp.is_empty()
            && comp.len() <= 32
            && comp
                .bytes()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, b'_' | b'+' | b'-'))
            && !(first && matches!(comp, "posix" | "right"));
        if !ok {
            return false;
        }
        first = false;
    }
    let path = paths.zoneinfo.join(s);
    let Ok(md) = fs::metadata(&path) else {
        return false;
    };
    if !md.is_file() {
        return false;
    }
    let mut magic = [0u8; 4];
    fs::File::open(&path)
        .and_then(|mut f| f.read_exact(&mut magic))
        .is_ok()
        && &magic == b"TZif"
}

/// A kbd keymap that is actually installed: `<name>.map.gz` somewhere under the
/// keymap tree (bounded walk, no links followed).
pub fn keymap_installed(paths: &Paths, name: &str) -> bool {
    fn walk(dir: &std::path::Path, want: &str, depth: u8) -> bool {
        let Ok(rd) = fs::read_dir(dir) else { return false };
        rd.flatten().any(|e| {
            let Ok(ft) = e.file_type() else { return false };
            if ft.is_dir() {
                depth < 4 && walk(&e.path(), want, depth + 1)
            } else {
                ft.is_file() && e.file_name().to_str() == Some(want)
            }
        })
    }
    valid_keymap(name) && walk(&paths.keymaps, &format!("{name}.map.gz"), 0)
}

fn check_plan_fields(paths: &Paths, plan: &Plan, password: &str) -> Result<()> {
    if !valid_hostname(&plan.hostname) {
        return Err(Error::Refused("hostname"));
    }
    if !valid_username(&plan.username) {
        return Err(Error::Refused("username"));
    }
    // Names the field, never the value.
    if !check_password(password) {
        return Err(Error::Refused("password"));
    }
    let lg = fs::read_to_string(&paths.locale_gen).map_err(io("read locale.gen"))?;
    if !valid_locale(&lg, &plan.locale) {
        return Err(Error::Refused("locale"));
    }
    if !valid_timezone(paths, &plan.timezone) {
        return Err(Error::Refused("timezone"));
    }
    if !keymap_installed(paths, &plan.keymap) {
        return Err(Error::Refused("keymap"));
    }
    Ok(())
}

pub fn validate(env: &Env, req: &Request) -> Result<Validated> {
    let p = &env.paths;
    if env.euid != 0 {
        return Err(Error::Refused("not root"));
    }
    // An installed system cannot be pointed at its own disk, even if the polkit
    // rule that allows this action leaked there (D-07 §6).
    if !p.archiso.exists() {
        return Err(Error::Refused("not running from the live medium"));
    }
    // UEFI only (D-07 §4.2).
    if !p.efi.exists() {
        return Err(Error::Refused("not booted in UEFI mode"));
    }
    check_plan_fields(p, &req.plan, &req.password)?;

    let entries = Catalog::builtin().resolve(&req.plan.candidates)?;

    if !valid_by_id(&req.plan.disk_by_id) {
        return Err(Error::Refused("disk id"));
    }
    let disk = disks::list_all(p, env.runner)?
        .into_iter()
        .find(|e| e.disk.by_id == req.plan.disk_by_id)
        .ok_or(Error::Refused("disk not in the helper's listing"))?;
    if disk.is_boot {
        return Err(Error::Refused("disk is the boot medium"));
    }
    Ok(Validated { disk, entries })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::confirm::DenyConfirm;
    use crate::testutil::{live, req, runner, FakeRunner};
    use zeroize::Zeroizing;

    fn env<'a>(p: Paths, r: &'a FakeRunner, euid: u32) -> Env<'a> {
        Env {
            paths: p,
            euid,
            caller_uid_hint: None,
            runner: r,
            confirm: &DenyConfirm,
        }
    }

    fn refused_with(p: Paths, mutate: impl FnOnce(&mut Request), euid: u32) -> Error {
        let r = runner();
        let mut q = req();
        mutate(&mut q);
        match validate(&env(p, &r, euid), &q) {
            Err(e) => e,
            Ok(_) => panic!("expected refusal"),
        }
    }

    #[test]
    fn accepts_a_good_request() {
        let (_t, p) = live();
        let r = runner();
        let v = validate(&env(p, &r, 0), &req()).unwrap();
        assert_eq!(v.disk.kernel, "nvme0n1");
        assert_eq!(v.entries.len(), 2);
    }

    #[test]
    fn refuses_non_root() {
        let (_t, p) = live();
        assert_eq!(refused_with(p, |_| {}, 1000), Error::Refused("not root"));
    }

    #[test]
    fn refuses_without_archiso() {
        let (_t, p) = live();
        std::fs::remove_dir_all(&p.archiso).unwrap();
        assert_eq!(
            refused_with(p, |_| {}, 0),
            Error::Refused("not running from the live medium")
        );
    }

    #[test]
    fn refuses_bios() {
        let (_t, p) = live();
        std::fs::remove_dir_all(&p.efi).unwrap();
        assert_eq!(
            refused_with(p, |_| {}, 0),
            Error::Refused("not booted in UEFI mode")
        );
    }

    #[test]
    fn refuses_names_outside_the_patterns() {
        for bad in ["", "Bad", "a b", "-x", "a;b", "a\nb"] {
            let (_t, p) = live();
            assert_eq!(
                refused_with(p, |q| q.plan.hostname = bad.into(), 0),
                Error::Refused("hostname"),
                "{bad:?}"
            );
        }
        for bad in ["", "root", "Chase", "1abc", "a:b", "a\nroot"] {
            let (_t, p) = live();
            assert_eq!(
                refused_with(p, |q| q.plan.username = bad.into(), 0),
                Error::Refused("username"),
                "{bad:?}"
            );
        }
    }

    #[test]
    fn refuses_passwords_with_newline_cr_nul_or_empty() {
        for bad in ["a\nroot:x1234", "abcdefg\rb", "abcdefg\0b", ""] {
            let (_t, p) = live();
            let e = refused_with(p, |q| q.password = Zeroizing::new(bad.into()), 0);
            assert_eq!(e, Error::Refused("password"));
            // The refusal text names the field, never the value.
            assert!(!e.to_string().contains("root:x"));
        }
    }

    #[test]
    fn refuses_locale_and_timezone_not_in_the_system_lists() {
        for bad in ["xx_XX.UTF-8", "en_US.UTF-8\n", "C", "en_US", "de_DE.ISO-8859-1"] {
            let (_t, p) = live();
            assert_eq!(
                refused_with(p, |q| q.plan.locale = bad.into(), 0),
                Error::Refused("locale"),
                "{bad:?}"
            );
        }
        for bad in [
            "",
            "Mars/Olympus",
            "../../etc/passwd",
            "America/../../etc/passwd",
            "/etc/passwd",
            "America//New_York",
            "zone.tab",
            "posix/UTC",
            "America",
            "America/New_York\n",
        ] {
            let (_t, p) = live();
            assert_eq!(
                refused_with(p, |q| q.plan.timezone = bad.into(), 0),
                Error::Refused("timezone"),
                "{bad:?}"
            );
        }
    }

    #[test]
    fn keymaps_must_be_installed() {
        let (_t, p) = live();
        assert!(keymap_installed(&p, "us") && keymap_installed(&p, "de-latin1"));
        for bad in ["", "xx", "../us", "us.map.gz", "i386"] {
            assert!(!keymap_installed(&p, bad), "{bad:?}");
        }
        let (_t, p) = live();
        assert_eq!(
            refused_with(p, |q| q.plan.keymap = "nope".into(), 0),
            Error::Refused("keymap")
        );
    }

    #[test]
    fn refuses_short_passwords() {
        let (_t, p) = live();
        assert_eq!(
            refused_with(p, |q| q.password = Zeroizing::new("short".into()), 0),
            Error::Refused("password")
        );
    }

    #[test]
    fn refuses_candidates_not_in_the_catalog() {
        for bad in [
            "firefox",
            "eclipseos-hyperion",
            "systemctl poweroff",
            "hyperion;x",
        ] {
            let (_t, p) = live();
            assert_eq!(
                refused_with(p, |q| q.plan.candidates = vec![bad.into()], 0),
                Error::Refused("candidate not in catalog"),
                "{bad:?}"
            );
        }
    }

    #[test]
    fn refuses_a_disk_not_from_the_listing() {
        for bad in [
            "nvme-Does_Not_Exist",
            "../../dev/sda",
            "/dev/nvme0n1",
            "ata-Samsung_SSD_S1-part1", // a partition link, not a disk
            "",
        ] {
            let (_t, p) = live();
            assert!(
                matches!(
                    refused_with(p, |q| q.plan.disk_by_id = bad.into(), 0),
                    Error::Refused(_)
                ),
                "{bad:?}"
            );
        }
    }

    #[test]
    fn refuses_the_boot_medium_by_name() {
        let (_t, p) = live();
        assert_eq!(
            refused_with(p, |q| q.plan.disk_by_id = "usb-Stick_1".into(), 0),
            Error::Refused("disk is the boot medium")
        );
    }
}
