// SPDX-License-Identifier: AGPL-3.0-only
//! Test scaffolding: a tempdir "machine", a recording command runner. No real
//! disk, mount or package manager is ever touched.

use crate::env::Paths;
use crate::error::Result;
use crate::runner::{Cmd, Runner, Tool};
use std::cell::RefCell;
use std::os::unix::fs::symlink;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};

pub struct TempDir(PathBuf);

impl TempDir {
    pub fn new() -> Self {
        static N: AtomicU32 = AtomicU32::new(0);
        let p = std::env::temp_dir().join(format!(
            "eclipse-setup-helper-test-{}-{}",
            std::process::id(),
            N.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&p).unwrap();
        TempDir(p)
    }
    pub fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// All host paths under `t`. The live-medium markers (`archiso`, `efi`) are
/// left absent: a test that wants them creates them.
pub fn fake_paths(t: &TempDir) -> Paths {
    let r = t.path();
    for d in ["dev", "sys/class", "proc", "etc", "root", "mnt", "usr/share"] {
        std::fs::create_dir_all(r.join(d)).unwrap();
    }
    Paths {
        archiso: r.join("run/archiso"),
        bootmnt: r.join("run/archiso/bootmnt"),
        efi: r.join("sys/firmware/efi"),
        dev: r.join("dev"),
        dev_by_id: r.join("dev/disk/by-id"),
        sys_class_block: r.join("sys/class/block"),
        mountinfo: r.join("proc/mountinfo"),
        zoneinfo: r.join("usr/share/zoneinfo"),
        locale_gen: r.join("etc/locale.gen"),
        passwd: r.join("etc/passwd"),
        target: r.join("mnt"),
        repo: r.join("root/eclipseos-repo"),
        pkg_list: r.join("root/eclipseos-packages.txt"),
        packaging_key: r.join("root/eclipseos-packaging.asc"),
        live_pacman_conf: r.join("etc/pacman.conf"),
        install_pacman_conf: r.join("root/eclipseos-install-pacman.conf"),
        uid_min: 1,
    }
}

pub struct Logged {
    pub tool: Tool,
    pub args: Vec<String>,
    pub stdin: Option<Vec<u8>>,
}

type Behaviour = Box<dyn Fn(&Cmd) -> Result<Vec<u8>>>;

pub struct FakeRunner {
    f: Behaviour,
    pub log: RefCell<Vec<Logged>>,
}

impl FakeRunner {
    pub fn new(f: impl Fn(&Cmd) -> Result<Vec<u8>> + 'static) -> Self {
        FakeRunner {
            f: Box::new(f),
            log: RefCell::new(Vec::new()),
        }
    }

    fn record(&self, c: &Cmd) {
        self.log.borrow_mut().push(Logged {
            tool: c.tool,
            args: c.args.iter().map(|a| a.to_string_lossy().into_owned()).collect(),
            stdin: c.stdin.as_ref().map(|s| s.to_vec()),
        });
    }

    pub fn tools(&self) -> Vec<Tool> {
        self.log.borrow().iter().map(|l| l.tool).collect()
    }

    pub fn issued(&self, tool: Tool) -> bool {
        self.tools().contains(&tool)
    }
}

impl Runner for FakeRunner {
    fn run(&self, cmd: &Cmd) -> Result<()> {
        self.record(cmd);
        (self.f)(cmd).map(|_| ())
    }
    fn capture(&self, cmd: &Cmd) -> Result<Vec<u8>> {
        self.record(cmd);
        (self.f)(cmd)
    }
}

pub const LSBLK: &str = r#"{"blockdevices":[
  {"name":"loop0","type":"loop","size":123456,"model":null,"ro":false},
  {"name":"sr0","type":"rom","size":1000000,"model":"QEMU DVD","ro":true},
  {"name":"sda","type":"disk","size":"500107862016","model":"Samsung SSD","ro":"0",
   "children":[{"name":"sda1","type":"part","size":1073741824,"fstype":"vfat","label":"ESP"},
               {"name":"sda2","type":"part","size":499000000000,"fstype":"ext4","label":"evil\u001b[2Jname"}]},
  {"name":"sdb","type":"disk","size":8000000000,"model":"USB Stick","ro":false,
   "children":[{"name":"sdb1","type":"part","size":8000000000,"fstype":"iso9660","label":"ARCHISO"}]},
  {"name":"nvme0n1","type":"disk","size":1000204886016,"model":null,"ro":false,"children":null},
  {"name":"zram0","type":"disk","size":4000000000,"model":null,"ro":false}
]}"#;

/// sda, sdb (the medium, via sdb1), nvme0n1 with by-id links; sysfs and
/// mountinfo say the medium is /dev/sdb1.
pub fn machine() -> (TempDir, Paths) {
    let t = TempDir::new();
    let p = fake_paths(&t);
    std::fs::create_dir_all(&p.dev_by_id).unwrap();
    for k in ["sda", "sdb", "nvme0n1", "sda1", "sdb1"] {
        std::fs::write(p.dev.join(k), b"").unwrap();
    }
    for (id, k) in [
        ("ata-Samsung_SSD_S1", "sda"),
        ("wwn-0x5000", "sda"),
        ("ata-Samsung_SSD_S1-part1", "sda1"),
        ("usb-Stick_1", "sdb"),
        ("nvme-Some_NVMe_1", "nvme0n1"),
    ] {
        symlink(format!("../../{k}"), p.dev_by_id.join(id)).unwrap();
    }
    // sysfs: sdb1 is a partition of sdb.
    let blk = p.sys_class_block.parent().unwrap().join("devices/block");
    std::fs::create_dir_all(blk.join("sdb/sdb1")).unwrap();
    std::fs::write(blk.join("sdb/sdb1/partition"), b"1").unwrap();
    std::fs::create_dir_all(&p.sys_class_block).unwrap();
    symlink(blk.join("sdb/sdb1"), p.sys_class_block.join("sdb1")).unwrap();
    let bm = p.bootmnt.display();
    std::fs::write(
        &p.mountinfo,
        format!("30 25 8:17 / {bm} ro - iso9660 /dev/sdb1 ro\n31 25 0:5 / /proc rw - proc proc rw\n"),
    )
    .unwrap();
    (t, p)
}

pub fn runner() -> FakeRunner {
    FakeRunner::new(|c| match c.tool {
        Tool::Lsblk => Ok(LSBLK.as_bytes().to_vec()),
        Tool::Gpg => Ok(format!("fpr:::::::::{}:\n", "AB".repeat(20)).into_bytes()),
        _ => Ok(Vec::new()),
    })
}

/// The uid tests run as, standing in for `liveuser`.
pub fn my_uid() -> u32 {
    rustix::process::getuid().as_raw()
}

/// Adds what a live UEFI medium has: archiso, EFI, tzdata, locale.gen, the baked
/// repo and package list, and a passwd with a `liveuser`.
pub fn make_live(p: &Paths) {
    std::fs::create_dir_all(&p.archiso).unwrap();
    std::fs::create_dir_all(&p.efi).unwrap();
    for z in ["America/New_York", "UTC", "posix/UTC"] {
        let f = p.zoneinfo.join(z);
        std::fs::create_dir_all(f.parent().unwrap()).unwrap();
        std::fs::write(f, b"TZif2\0\0\0").unwrap();
    }
    std::fs::write(p.zoneinfo.join("zone.tab"), b"# zones\n").unwrap();
    std::fs::write(
        &p.locale_gen,
        "# Configuration file for locale-gen\n#\n#  Some prose UTF-8\n\
         #en_US.UTF-8 UTF-8\n#de_DE.UTF-8 UTF-8\n#de_DE ISO-8859-1\n",
    )
    .unwrap();
    std::fs::write(&p.live_pacman_conf, "[options]\nHoldPkg = pacman glibc\n").unwrap();
    std::fs::create_dir_all(&p.repo).unwrap();
    std::fs::write(&p.pkg_list, "# base\n\nbase\nlinux\nlimine\neclipseos-meta\n").unwrap();
    std::fs::write(&p.packaging_key, b"KEYDATA").unwrap();
    let home = p.target.parent().unwrap().join("home/liveuser");
    std::fs::create_dir_all(&home).unwrap();
    std::fs::write(
        &p.passwd,
        format!(
            "root:x:0:0:root:/root:/bin/bash\nliveuser:x:{}:{}::{}:/bin/bash\n",
            my_uid(),
            my_uid(),
            home.display()
        ),
    )
    .unwrap();
}

pub fn live() -> (TempDir, Paths) {
    let (t, p) = machine();
    make_live(&p);
    (t, p)
}

pub fn req() -> eclipse_setup_plan::Request {
    use eclipse_setup_plan::{Plan, Profile, Request};
    Request {
        plan: Plan {
            disk_by_id: "nvme-Some_NVMe_1".into(),
            hostname: "eclipse".into(),
            username: "chase".into(),
            locale: "en_US.UTF-8".into(),
            timezone: "America/New_York".into(),
            profile: Profile::Standard,
            candidates: vec!["hyperion".into(), "eclipse-toasts".into()],
        },
        password: zeroize::Zeroizing::new("correct horse".into()),
    }
}
