// SPDX-License-Identifier: AGPL-3.0-only
//! The helper's own disk listing (D-07 §4.2). This, and nothing the caller
//! sends, is what a disk id is checked against.
//!
//! A disk is listed only if it has a `/dev/disk/by-id` name (that is how it is
//! addressed, never as a free path), is a whole non-read-only `disk` (so no
//! loop, no optical), and is not the disk the live medium booted from.

use crate::env::Paths;
use crate::error::{io, Error, Result};
use crate::runner::{Cmd, Runner, Tool};
use ec_setup_plan::{valid_by_id, Disk, Partition};
use serde_json::Value;
use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

pub struct DiskEntry {
    pub disk: Disk,
    /// Kernel name, e.g. `nvme0n1`. Never leaves the helper.
    pub kernel: String,
    /// Kernel names of its partitions.
    pub parts: Vec<String>,
    /// The disk the live medium is running from.
    pub is_boot: bool,
}

pub struct Mount {
    pub point: String,
    pub source: String,
}

/// Model and label strings come off hardware and filesystems an attacker may
/// have prepared, and are shown in the trusted prompt. Strip control
/// characters and bound the length.
fn clean(s: &str, max: usize) -> String {
    s.chars()
        .filter(|c| !c.is_control())
        .take(max)
        .collect::<String>()
        .trim()
        .to_owned()
}

pub fn valid_kernel_name(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 32
        && s.bytes()
            .all(|c| c.is_ascii_alphanumeric() || c == b'-' || c == b'_')
}

fn unescape_mountinfo(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'\\' && i + 3 < b.len() && b[i + 1..=i + 3].iter().all(|c| (b'0'..=b'7').contains(c)) {
            let v =
                u32::from(b[i + 1] - b'0') * 64 + u32::from(b[i + 2] - b'0') * 8 + u32::from(b[i + 3] - b'0');
            out.push(v as u8);
            i += 4;
        } else {
            out.push(b[i]);
            i += 1;
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

pub fn parse_mountinfo(text: &str) -> Vec<Mount> {
    text.lines()
        .filter_map(|l| {
            let (left, right) = l.split_once(" - ")?;
            let point = left.split(' ').nth(4)?;
            let source = right.split(' ').nth(1)?;
            Some(Mount {
                point: unescape_mountinfo(point),
                source: unescape_mountinfo(source),
            })
        })
        .collect()
}

pub fn read_mounts(paths: &Paths) -> Result<Vec<Mount>> {
    let text = fs::read_to_string(&paths.mountinfo).map_err(io("read mountinfo"))?;
    Ok(parse_mountinfo(&text))
}

fn file_name_of(p: &Path) -> Option<&str> {
    p.file_name()?.to_str()
}

fn disks_of(paths: &Paths, name: &str, depth: u8, out: &mut Vec<String>) -> Result<()> {
    if depth > 4 || !valid_kernel_name(name) {
        return Err(Error::Refused("boot medium device unrecognised"));
    }
    let node = paths.sys_class_block.join(name);
    if !node.exists() {
        return Err(Error::Refused("boot medium not found in sysfs"));
    }
    // A stacked device (device-mapper, e.g. a booted loop or Ventoy image):
    // the medium is whatever sits underneath.
    if let Ok(rd) = fs::read_dir(node.join("slaves")) {
        let mut any = false;
        for e in rd.flatten() {
            any = true;
            let n = e.file_name();
            let n = n
                .to_str()
                .ok_or(Error::Refused("boot medium device unrecognised"))?;
            disks_of(paths, n, depth + 1, out)?;
        }
        if any {
            return Ok(());
        }
    }
    if node.join("partition").exists() {
        let real = fs::canonicalize(&node).map_err(io("resolve boot partition"))?;
        let parent = real
            .parent()
            .and_then(file_name_of)
            .ok_or(Error::Refused("boot medium device unrecognised"))?;
        if !valid_kernel_name(parent) {
            return Err(Error::Refused("boot medium device unrecognised"));
        }
        out.push(parent.to_owned());
    } else {
        out.push(name.to_owned());
    }
    Ok(())
}

/// Kernel names of the disk(s) under `/run/archiso/bootmnt`. If the medium's
/// mount is there but its source cannot be traced to a disk, that is an error:
/// an unidentifiable boot medium must not be offered as a target. If nothing is
/// mounted at `bootmnt` (copytoram) there is nothing to exclude.
pub fn boot_disks(paths: &Paths, mounts: &[Mount]) -> Result<Vec<String>> {
    let bm = paths.bootmnt.to_str().ok_or(Error::Refused("bad bootmnt path"))?;
    let Some(m) = mounts.iter().find(|m| m.point == bm) else {
        return Ok(Vec::new());
    };
    let rel = m
        .source
        .strip_prefix("/dev/")
        .ok_or(Error::Refused("boot medium source unrecognised"))?;
    let real = fs::canonicalize(paths.dev.join(rel)).map_err(io("resolve boot medium"))?;
    let name = file_name_of(&real).ok_or(Error::Refused("boot medium device unrecognised"))?;
    let mut out = Vec::new();
    disks_of(paths, name, 0, &mut out)?;
    Ok(out)
}

/// What `/dev/disk/by-id/<id>` points at right now, as a kernel name.
pub fn resolve_by_id(paths: &Paths, by_id: &str) -> Option<String> {
    if !valid_by_id(by_id) {
        return None;
    }
    let t = fs::read_link(paths.dev_by_id.join(by_id)).ok()?;
    let n = file_name_of(&t)?;
    valid_kernel_name(n).then(|| n.to_owned())
}

/// Is any mount's source this disk or one of its partitions?
pub fn in_use(mounts: &[Mount], e: &DiskEntry) -> bool {
    mounts.iter().any(|m| {
        m.source
            .strip_prefix("/dev/")
            .and_then(|s| s.rsplit('/').next())
            .is_some_and(|n| n == e.kernel || e.parts.iter().any(|p| p == n))
    })
}

/// Why a disk that nothing has mounted is still busy: a device-mapper or md
/// device sits on it (or a partition of it), or it holds swap.
pub fn busy_reason(paths: &Paths, e: &DiskEntry) -> Option<&'static str> {
    let names = std::iter::once(&e.kernel).chain(e.parts.iter());
    for n in names {
        let held =
            fs::read_dir(paths.sys_class_block.join(n).join("holders")).is_ok_and(|mut r| r.next().is_some());
        if held {
            return Some("disk is in use by RAID, LVM or an encrypted volume");
        }
    }
    let swaps = fs::read_to_string(&paths.proc_swaps).unwrap_or_default();
    let on_disk = swaps
        .lines()
        .skip(1)
        .filter_map(|l| l.split_whitespace().next())
        .any(|f| {
            f.strip_prefix("/dev/")
                .is_some_and(|n| n == e.kernel || e.parts.iter().any(|p| p == n))
        });
    on_disk.then_some("disk is in use as swap")
}

struct RawPart {
    kernel: String,
    fs: String,
    label: String,
    size: u64,
}

struct Raw {
    kernel: String,
    model: String,
    size: u64,
    parts: Vec<RawPart>,
}

fn str_of<'a>(v: &'a Value, k: &str) -> Option<&'a str> {
    v.get(k)?.as_str()
}

// lsblk prints numbers as numbers or as strings depending on util-linux version.
fn u64_of(v: &Value, k: &str) -> Option<u64> {
    match v.get(k)? {
        Value::Number(n) => n.as_u64(),
        Value::String(s) => s.parse().ok(),
        _ => None,
    }
}

fn truthy(v: &Value, k: &str) -> bool {
    match v.get(k) {
        Some(Value::Bool(b)) => *b,
        Some(Value::String(s)) => s == "1",
        Some(Value::Number(n)) => n.as_u64() == Some(1),
        _ => false,
    }
}

fn parse_lsblk(json: &[u8]) -> Result<Vec<Raw>> {
    let v: Value = serde_json::from_slice(json).map_err(io("lsblk output"))?;
    let devs = v
        .get("blockdevices")
        .and_then(Value::as_array)
        .ok_or(Error::Io("lsblk output"))?;
    let mut out = Vec::new();
    for d in devs {
        if str_of(d, "type") != Some("disk") || truthy(d, "ro") {
            continue;
        }
        let (Some(kernel), Some(size)) = (str_of(d, "name"), u64_of(d, "size")) else {
            continue;
        };
        if size == 0 || !valid_kernel_name(kernel) {
            continue;
        }
        let mut parts = Vec::new();
        for c in d.get("children").and_then(Value::as_array).into_iter().flatten() {
            if str_of(c, "type") != Some("part") {
                continue;
            }
            let Some(pk) = str_of(c, "name").filter(|n| valid_kernel_name(n)) else {
                continue;
            };
            parts.push(RawPart {
                kernel: pk.to_owned(),
                fs: clean(str_of(c, "fstype").unwrap_or(""), 32),
                label: clean(str_of(c, "label").unwrap_or(""), 64),
                size: u64_of(c, "size").unwrap_or(0),
            });
        }
        out.push(Raw {
            kernel: kernel.to_owned(),
            model: clean(str_of(d, "model").unwrap_or(""), 64),
            size,
            parts,
        });
    }
    Ok(out)
}

/// Whole-disk `by-id` names grouped by the kernel device they point at.
/// Partition links (`…-part1`) point at partitions, so they never match a disk.
fn by_id_index(paths: &Paths) -> BTreeMap<String, Vec<String>> {
    let mut m: BTreeMap<String, Vec<String>> = BTreeMap::new();
    let Ok(rd) = fs::read_dir(&paths.dev_by_id) else {
        return m;
    };
    for e in rd.flatten() {
        let Some(name) = e.file_name().to_str().map(str::to_owned) else {
            continue;
        };
        if let Some(k) = resolve_by_id(paths, &name) {
            m.entry(k).or_default().push(name);
        }
    }
    m
}

/// Stable, human-recognisable name first; `wwn-`/`eui.` aliases last.
fn preferred(names: &[String]) -> Option<&String> {
    names
        .iter()
        .min_by_key(|n| (n.starts_with("wwn-") || n.contains("eui."), n.len(), (*n).clone()))
}

/// Every addressable disk, boot medium included and flagged.
pub fn list_all(paths: &Paths, runner: &dyn Runner) -> Result<Vec<DiskEntry>> {
    let cmd = Cmd::new(Tool::Lsblk).args(["-J", "-b", "-o", "NAME,TYPE,SIZE,MODEL,FSTYPE,LABEL,RO"]);
    let raws = parse_lsblk(&runner.capture(&cmd)?)?;
    let mounts = read_mounts(paths)?;
    let boot = boot_disks(paths, &mounts)?;
    let ids = by_id_index(paths);
    let mut out = Vec::new();
    for r in raws {
        let Some(by_id) = ids.get(&r.kernel).and_then(|n| preferred(n)) else {
            continue;
        };
        let is_boot = boot.contains(&r.kernel);
        out.push(DiskEntry {
            disk: Disk {
                by_id: by_id.clone(),
                model: if r.model.is_empty() {
                    "Unknown".into()
                } else {
                    r.model
                },
                size_bytes: r.size,
                partitions: r
                    .parts
                    .iter()
                    .map(|p| Partition {
                        fs: p.fs.clone(),
                        label: p.label.clone(),
                        size_bytes: p.size,
                    })
                    .collect(),
            },
            parts: r.parts.into_iter().map(|p| p.kernel).collect(),
            kernel: r.kernel,
            is_boot,
        });
    }
    out.sort_by(|a, b| a.disk.by_id.cmp(&b.disk.by_id));
    Ok(out)
}

/// What `list-disks` prints: every disk except the boot medium's.
pub fn list_offered(paths: &Paths, runner: &dyn Runner) -> Result<Vec<Disk>> {
    Ok(list_all(paths, runner)?
        .into_iter()
        .filter(|e| !e.is_boot)
        .map(|e| e.disk)
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::{machine, runner};

    #[test]
    fn lists_by_id_disks_and_excludes_the_boot_medium() {
        let (_t, p) = machine();
        let r = runner();
        let all = list_all(&p, &r).unwrap();
        let names: Vec<_> = all.iter().map(|e| e.disk.by_id.as_str()).collect();
        // loop, rom and the id-less zram device are absent; wwn alias lost to the ata name.
        assert_eq!(names, ["ata-Samsung_SSD_S1", "nvme-Some_NVMe_1", "usb-Stick_1"]);
        assert!(all.iter().find(|e| e.kernel == "sdb").unwrap().is_boot);

        let offered = list_offered(&p, &r).unwrap();
        assert_eq!(offered.len(), 2);
        assert!(offered.iter().all(|d| d.by_id != "usb-Stick_1"));
    }

    #[test]
    fn strips_control_characters_from_labels_and_models() {
        let (_t, p) = machine();
        let d = list_offered(&p, &runner()).unwrap();
        let sda = d.iter().find(|d| d.by_id.starts_with("ata-")).unwrap();
        assert_eq!(sda.partitions[1].label, "evil[2Jname");
        assert_eq!(sda.partitions[0].fs, "vfat");
        assert_eq!(sda.size_bytes, 500_107_862_016);
        assert_eq!(
            d.iter().find(|d| d.by_id.starts_with("nvme")).unwrap().model,
            "Unknown"
        );
    }

    #[test]
    fn unidentifiable_boot_medium_is_an_error_not_a_free_pass() {
        let (_t, p) = machine();
        let bm = p.bootmnt.display();
        std::fs::write(
            &p.mountinfo,
            format!("30 25 8:17 / {bm} ro - overlay overlay ro\n"),
        )
        .unwrap();
        assert!(list_all(&p, &runner()).is_err());
    }

    #[test]
    fn no_bootmnt_means_nothing_to_exclude() {
        let (_t, p) = machine();
        std::fs::write(&p.mountinfo, "31 25 0:5 / /proc rw - proc proc rw\n").unwrap();
        assert_eq!(list_offered(&p, &runner()).unwrap().len(), 3);
    }

    #[test]
    fn mountinfo_octal_escapes_and_in_use() {
        let m = parse_mountinfo("1 0 8:1 / /mnt/my\\040disk rw - ext4 /dev/sda2 rw\n");
        assert_eq!(m[0].point, "/mnt/my disk");
        let (_t, p) = machine();
        let all = list_all(&p, &runner()).unwrap();
        let sda = all.iter().find(|e| e.kernel == "sda").unwrap();
        assert!(in_use(&m, sda));
        let nv = all.iter().find(|e| e.kernel == "nvme0n1").unwrap();
        assert!(!in_use(&m, nv));
    }

    #[test]
    fn by_id_resolution_follows_the_link_and_rejects_paths() {
        let (_t, p) = machine();
        assert_eq!(resolve_by_id(&p, "usb-Stick_1").as_deref(), Some("sdb"));
        assert_eq!(resolve_by_id(&p, "../sda"), None);
        assert_eq!(resolve_by_id(&p, "nope"), None);
    }
}
