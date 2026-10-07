// SPDX-License-Identifier: AGPL-3.0-only
//! The agent sandbox's in-process layers (VOL1 §7, VOL2 S-03, A-07 §2/§6):
//! a Landlock filesystem allow-list, a seccomp deny-list and `no_new_privs`.
//!
//! bwrap builds the namespaces and mounts; this module is the second stage.
//! The launcher runs `ec-agentd --sandbox-init <cfg> -- <entrypoint...>`
//! inside bwrap ([`init_main`]): the helper applies Landlock, then seccomp,
//! then execs the entrypoint, so the filters are on before any agent code
//! runs. It is the same binary, unprivileged: there is no root helper.
//!
//! Fail closed (VOL2 A-01 §6: no sandbox without a Landlock ruleset): when
//! the kernel has no Landlock, [`preflight`] refuses the launch and the
//! helper exits [`EXIT_SANDBOX`]. Only the `dev-unsandboxed` feature skips it.
//!
//! No crates: Landlock and seccomp are a handful of raw syscalls, and the BPF
//! program is small enough to read and to test with an interpreter.

use std::ffi::CString;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::process::CommandExt;
use std::path::{Component, Path, PathBuf};

use serde_json::{json, Value};

/// The helper's exit status when the sandbox could not be applied.
pub const EXIT_SANDBOX: i32 = 111;

/// Conservative systemd limits on the task scope (VOL1 §7: "resource limits").
pub const MEMORY_MAX: &str = "2G";
pub const TASKS_MAX: &str = "256";
pub const CPU_QUOTA: &str = "200%";

// ---- what a manifest asks for ---------------------------------------------

/// The `sandbox { ... }` block as written (A-07 §2), unresolved.
#[derive(Debug, Default, Clone, PartialEq)]
pub struct RawDecl {
    pub fs_read: Vec<String>,
    pub fs_write: Vec<String>,
    pub egress: Vec<String>,
}

/// The block after validation: absolute, existing, canonical read paths.
#[derive(Debug, Default, Clone, PartialEq)]
pub struct Decl {
    pub fs_read: Vec<PathBuf>,
}

/// Where the agent's scratch is: bwrap's tmpfs `/tmp`, also its `$HOME`.
pub const SCRATCH: &str = "/tmp";

/// Paths no declaration may reach, relative to the owner's home (VOL1 §7:
/// `~/.ssh`, `~/.gnupg`, browser profiles, password-manager data).
const FORBIDDEN_UNDER_HOME: &[&str] = &[
    ".ssh",
    ".gnupg",
    ".pki",
    ".mozilla",
    ".config/google-chrome",
    ".config/chromium",
    ".config/BraveSoftware",
    ".config/microsoft-edge",
    ".config/Bitwarden",
    ".config/1Password",
    ".config/KeePassXC",
    ".password-store",
    ".local/share/keyrings",
    ".local/share/kwalletd",
    ".local/share/eclipse",
    ".local/state/eclipse",
];

fn lexical(p: &Path) -> Option<PathBuf> {
    let mut out = PathBuf::new();
    for c in p.components() {
        match c {
            Component::RootDir => out.push("/"),
            Component::Normal(n) => out.push(n),
            Component::CurDir => {}
            _ => return None,
        }
    }
    out.is_absolute().then_some(out)
}

/// Whether `p` is, contains, or lies inside a forbidden location. Containing
/// matters: declaring `~` or `/` would hand the agent everything below it.
fn forbidden(p: &Path, home: &Path) -> bool {
    FORBIDDEN_UNDER_HOME.iter().any(|f| {
        let f = home.join(f);
        p.starts_with(&f) || f.starts_with(p)
    })
}

impl RawDecl {
    /// Validates the declaration for launch. Refusals are stable words for the
    /// log (the launch then ends `Exited{failed}`):
    /// * `egress_unavailable`: `net.egress` needs the S-09 egress proxy (M20),
    ///   which does not exist, and the agent has no network (fail closed);
    /// * `fs_path_invalid`: not absolute (after `~`) or has `..`;
    /// * `fs_read_forbidden`: reaches `~/.ssh`, `~/.gnupg` and the like;
    /// * `fs_write_outside_scratch`: A-07 §6 minimum, and the only writable
    ///   place the sandbox has.
    pub fn resolve(&self, home: Option<&Path>) -> Result<Decl, String> {
        if !self.egress.is_empty() {
            return Err("egress_unavailable".into());
        }
        let expand = |s: &str| -> Result<PathBuf, String> {
            let p = if s == "~" || s.starts_with("~/") {
                home.ok_or("fs_path_invalid")?
                    .join(s.trim_start_matches("~").trim_start_matches('/'))
            } else {
                PathBuf::from(s)
            };
            lexical(&p).ok_or_else(|| "fs_path_invalid".to_owned())
        };
        for w in &self.fs_write {
            let p = expand(w)?;
            if !p.starts_with(SCRATCH) {
                return Err("fs_write_outside_scratch".into());
            }
        }
        let mut fs_read = Vec::new();
        for r in &self.fs_read {
            let p = expand(r)?;
            if let Some(h) = home {
                if forbidden(&p, h) {
                    return Err("fs_read_forbidden".into());
                }
            }
            if p == Path::new("/") {
                return Err("fs_read_forbidden".into());
            }
            // A symlink must not lead somewhere the lexical check refused.
            let Ok(real) = std::fs::canonicalize(&p) else {
                // Missing: nothing to bind, and the agent reads nothing there.
                continue;
            };
            if real == Path::new("/") || home.is_some_and(|h| forbidden(&real, h)) {
                return Err("fs_read_forbidden".into());
            }
            if !fs_read.contains(&real) {
                fs_read.push(real);
            }
        }
        Ok(Decl { fs_read })
    }
}

// ---- the filesystem allow-list ---------------------------------------------

/// The Landlock allow-list. Everything not listed is denied.
#[derive(Debug, Default, Clone, PartialEq)]
pub struct FsPolicy {
    /// Read, list and execute: the runtime and the package.
    pub ro_exec: Vec<PathBuf>,
    /// Read and list only: declared `fs.read` paths and a few read-only nodes.
    pub ro: Vec<PathBuf>,
    /// Read, write, create, remove, no execute: the agent's own scratch.
    pub rw: Vec<PathBuf>,
    /// Read-write device nodes that carry no data.
    pub rw_dev: Vec<PathBuf>,
}

impl FsPolicy {
    /// The policy for a package mounted at `pkg_mount` with `decl`.
    pub fn for_package(pkg_mount: &Path, decl: &Decl) -> FsPolicy {
        let p = |s: &[&str]| s.iter().map(PathBuf::from).collect::<Vec<_>>();
        let mut ro = p(&[
            "/proc",
            "/etc/localtime",
            "/dev/urandom",
            "/dev/random",
            "/dev/zero",
        ]);
        ro.extend(decl.fs_read.iter().cloned());
        FsPolicy {
            ro_exec: {
                let mut v = p(&[
                    "/usr",
                    "/lib",
                    "/lib32",
                    "/lib64",
                    "/bin",
                    "/sbin",
                    "/etc/ld.so.cache",
                    "/etc/ld.so.conf",
                    "/etc/ld.so.conf.d",
                    "/etc/alternatives",
                ]);
                v.push(pkg_mount.to_path_buf());
                v
            },
            ro,
            rw: p(&[SCRATCH]),
            rw_dev: p(&["/dev/null"]),
        }
    }

    pub fn to_json(&self) -> Value {
        let l = |v: &[PathBuf]| {
            v.iter()
                .map(|p| p.to_string_lossy().into_owned())
                .collect::<Vec<String>>()
        };
        json!({"ro_exec": l(&self.ro_exec), "ro": l(&self.ro), "rw": l(&self.rw), "rw_dev": l(&self.rw_dev)})
    }

    pub fn from_json(v: &Value) -> Option<FsPolicy> {
        let l = |k: &str| -> Option<Vec<PathBuf>> {
            v.get(k)?
                .as_array()?
                .iter()
                .map(|s| s.as_str().map(PathBuf::from))
                .collect()
        };
        Some(FsPolicy {
            ro_exec: l("ro_exec")?,
            ro: l("ro")?,
            rw: l("rw")?,
            rw_dev: l("rw_dev")?,
        })
    }
}

// ---- Landlock (raw syscalls) -----------------------------------------------

mod ll {
    pub const SYS_CREATE_RULESET: libc::c_long = 444;
    pub const SYS_ADD_RULE: libc::c_long = 445;
    pub const SYS_RESTRICT_SELF: libc::c_long = 446;
    pub const CREATE_RULESET_VERSION: u32 = 1;
    pub const RULE_PATH_BENEATH: u32 = 1;

    pub const EXECUTE: u64 = 1 << 0;
    pub const WRITE_FILE: u64 = 1 << 1;
    pub const READ_FILE: u64 = 1 << 2;
    pub const READ_DIR: u64 = 1 << 3;
    pub const REMOVE_DIR: u64 = 1 << 4;
    pub const REMOVE_FILE: u64 = 1 << 5;
    pub const MAKE_CHAR: u64 = 1 << 6;
    pub const MAKE_DIR: u64 = 1 << 7;
    pub const MAKE_REG: u64 = 1 << 8;
    pub const MAKE_SOCK: u64 = 1 << 9;
    pub const MAKE_FIFO: u64 = 1 << 10;
    pub const MAKE_BLOCK: u64 = 1 << 11;
    pub const MAKE_SYM: u64 = 1 << 12;
    pub const REFER: u64 = 1 << 13; // ABI 2
    pub const TRUNCATE: u64 = 1 << 14; // ABI 3
    pub const IOCTL_DEV: u64 = 1 << 15; // ABI 5

    #[repr(C)]
    pub struct RulesetAttr {
        pub handled_access_fs: u64,
    }

    #[repr(C, packed)]
    pub struct PathBeneath {
        pub allowed_access: u64,
        pub parent_fd: i32,
    }
}

/// Every filesystem right the running kernel's Landlock ABI knows: all of them
/// are handled, so all of them are denied unless a rule grants them.
fn handled_for(abi: i32) -> u64 {
    let mut m = (1u64 << 13) - 1;
    if abi >= 2 {
        m |= ll::REFER;
    }
    if abi >= 3 {
        m |= ll::TRUNCATE;
    }
    if abi >= 5 {
        m |= ll::IOCTL_DEV;
    }
    m
}

/// The kernel's Landlock ABI version, or why there is none.
pub fn landlock_abi() -> Result<i32, String> {
    // SAFETY: a version query takes a null attribute and size 0.
    let r = unsafe {
        libc::syscall(
            ll::SYS_CREATE_RULESET,
            std::ptr::null::<ll::RulesetAttr>(),
            0usize,
            ll::CREATE_RULESET_VERSION,
        )
    };
    if r < 0 {
        let e = std::io::Error::last_os_error();
        return Err(match e.raw_os_error() {
            Some(libc::ENOSYS) => "landlock is not built into this kernel".into(),
            Some(libc::EOPNOTSUPP) => "landlock is disabled on this kernel".into(),
            _ => format!("landlock is unavailable: {e}"),
        });
    }
    Ok(r as i32)
}

/// Refuse to launch when the sandbox cannot be completed (fail closed).
/// `dev-unsandboxed` builds do not sandbox at all, so there is nothing to check.
pub fn preflight() -> Result<(), String> {
    if cfg!(feature = "dev-unsandboxed") {
        return Ok(());
    }
    landlock_abi()
        .map(drop)
        .map_err(|e| format!("sandbox_unavailable: {e}"))
}

fn rights_for(base: u64, is_dir: bool, handled: u64) -> u64 {
    let file_only = ll::EXECUTE | ll::WRITE_FILE | ll::READ_FILE | ll::TRUNCATE | ll::IOCTL_DEV;
    let a = if is_dir { base } else { base & file_only };
    a & handled
}

/// Applies the allow-list to this process (and everything it execs).
pub fn apply_landlock(p: &FsPolicy) -> Result<(), String> {
    let abi = landlock_abi()?;
    let handled = handled_for(abi);
    let attr = ll::RulesetAttr {
        handled_access_fs: handled,
    };
    // SAFETY: `attr` is a valid, live struct of the size passed.
    let fd = unsafe {
        libc::syscall(
            ll::SYS_CREATE_RULESET,
            &attr as *const ll::RulesetAttr,
            std::mem::size_of::<ll::RulesetAttr>(),
            0u32,
        )
    };
    if fd < 0 {
        return Err(format!(
            "landlock_create_ruleset: {}",
            std::io::Error::last_os_error()
        ));
    }
    let fd = fd as i32;
    let ro = ll::EXECUTE | ll::READ_FILE | ll::READ_DIR;
    let ro_noexec = ll::READ_FILE | ll::READ_DIR;
    let rw = ll::WRITE_FILE
        | ll::READ_FILE
        | ll::READ_DIR
        | ll::REMOVE_DIR
        | ll::REMOVE_FILE
        | ll::MAKE_CHAR
        | ll::MAKE_DIR
        | ll::MAKE_REG
        | ll::MAKE_SOCK
        | ll::MAKE_FIFO
        | ll::MAKE_BLOCK
        | ll::MAKE_SYM
        | ll::REFER
        | ll::TRUNCATE;
    let dev = ll::WRITE_FILE | ll::READ_FILE | ll::IOCTL_DEV;
    let groups: [(&[PathBuf], u64); 4] = [
        (&p.ro_exec, ro),
        (&p.ro, ro_noexec),
        (&p.rw, rw),
        (&p.rw_dev, dev),
    ];
    let mut result = Ok(());
    'all: for (paths, base) in groups {
        for path in paths {
            let Ok(c) = CString::new(path.as_os_str().as_bytes()) else {
                result = Err(format!("bad path {}", path.display()));
                break 'all;
            };
            // SAFETY: `c` is a valid NUL-terminated path.
            let pfd = unsafe { libc::open(c.as_ptr(), libc::O_PATH | libc::O_CLOEXEC) };
            if pfd < 0 {
                // A path this host lacks (no /lib32) grants nothing.
                continue;
            }
            let mut st: libc::stat = unsafe { std::mem::zeroed() };
            // SAFETY: `pfd` is open and `st` is a valid out-pointer.
            let is_dir =
                unsafe { libc::fstat(pfd, &mut st) } == 0 && (st.st_mode & libc::S_IFMT) == libc::S_IFDIR;
            let rule = ll::PathBeneath {
                allowed_access: rights_for(base, is_dir, handled),
                parent_fd: pfd,
            };
            // SAFETY: `rule` is a valid packed struct; `fd` is the ruleset.
            let r = unsafe {
                libc::syscall(
                    ll::SYS_ADD_RULE,
                    fd,
                    ll::RULE_PATH_BENEATH,
                    &rule as *const ll::PathBeneath,
                    0u32,
                )
            };
            let err = std::io::Error::last_os_error();
            // SAFETY: closing the fd opened above.
            unsafe { libc::close(pfd) };
            if r < 0 {
                result = Err(format!("landlock_add_rule {}: {err}", path.display()));
                break 'all;
            }
        }
    }
    if result.is_ok() {
        // SAFETY: `fd` is the ruleset; no_new_privs was set by the caller.
        if unsafe { libc::syscall(ll::SYS_RESTRICT_SELF, fd, 0u32) } < 0 {
            result = Err(format!(
                "landlock_restrict_self: {}",
                std::io::Error::last_os_error()
            ));
        }
    }
    // SAFETY: closing the ruleset fd.
    unsafe { libc::close(fd) };
    result
}

// ---- seccomp (hand-built BPF) ----------------------------------------------

/// The deny-list: syscalls an agent has no business making. Each answers
/// `EPERM`. Beyond the baseline list (VOL1 §7) it also takes the new mount
/// API (the same power as `mount`), `reboot`, swap and `io_uring` (a large
/// kernel surface that bypasses seccomp's syscall view).
pub fn denied_syscalls() -> Vec<(&'static str, i64)> {
    macro_rules! s {
        ($($n:ident),* $(,)?) => { vec![$((stringify!($n), libc::$n as i64)),*] };
    }
    #[allow(clippy::unnecessary_cast)]
    let v = s![
        SYS_ptrace,
        SYS_process_vm_readv,
        SYS_process_vm_writev,
        SYS_mount,
        SYS_umount2,
        SYS_pivot_root,
        SYS_kexec_load,
        SYS_kexec_file_load,
        SYS_add_key,
        SYS_request_key,
        SYS_keyctl,
        SYS_bpf,
        SYS_perf_event_open,
        SYS_init_module,
        SYS_finit_module,
        SYS_delete_module,
        SYS_unshare,
        SYS_setns,
        SYS_open_by_handle_at,
        SYS_userfaultfd,
        SYS_reboot,
        SYS_swapon,
        SYS_swapoff,
        SYS_io_uring_setup,
        SYS_io_uring_enter,
        SYS_io_uring_register,
        SYS_open_tree,
        SYS_move_mount,
        SYS_fsopen,
        SYS_fsconfig,
        SYS_fsmount,
        SYS_fspick,
        SYS_mount_setattr,
    ];
    v
}

/// `clone` flags that make a namespace: user, mount, pid, net, ipc, uts,
/// cgroup. `unshare` and `setns` are denied, so `clone` must be too.
pub const CLONE_NS_MASK: u32 = 0x7e02_0000;

const AF_INET: u32 = libc::AF_INET as u32;
const AF_INET6: u32 = libc::AF_INET6 as u32;
const AF_PACKET: u32 = libc::AF_PACKET as u32;
const AF_NETLINK: u32 = libc::AF_NETLINK as u32;
const NETLINK_ROUTE: u32 = libc::NETLINK_ROUTE as u32;
const SOCK_RAW: u32 = libc::SOCK_RAW as u32;

const RET_ALLOW: u32 = 0x7fff_0000;
const RET_KILL_PROCESS: u32 = 0x8000_0000;
const fn ret_errno(e: i32) -> u32 {
    0x0005_0000 | (e as u32 & 0xffff)
}

// BPF opcodes used.
const LD_W_ABS: u16 = 0x20;
const ALU_AND_K: u16 = 0x54;
const JEQ_K: u16 = 0x15;
const JSET_K: u16 = 0x45;
const RET_K: u16 = 0x06;

#[cfg(target_arch = "x86_64")]
const AUDIT_ARCH: u32 = 0xc000_003e;
#[cfg(target_arch = "aarch64")]
const AUDIT_ARCH: u32 = 0xc000_00b7;

/// A branch target while the program is being laid out.
#[derive(Clone, Copy)]
enum T {
    Next,
    Allow,
    Deny,
    Kill,
    At(usize),
}

/// The filter as `sock_filter` words. Layout: arch check, x32 check, the
/// deny-list, `clone` flags, `clone3`, `socket` types, then allow.
#[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
pub fn build_filter() -> Vec<libc::sock_filter> {
    struct I {
        code: u16,
        k: u32,
        jt: T,
        jf: T,
    }
    let st = |code, k| I {
        code,
        k,
        jt: T::Next,
        jf: T::Next,
    };
    let jmp = |code, k, jt, jf| I { code, k, jt, jf };
    // Wrong architecture (32-bit compat): nothing below would mean the same.
    let mut p: Vec<I> = vec![
        st(LD_W_ABS, 4),
        jmp(JEQ_K, AUDIT_ARCH, T::Next, T::Kill),
        st(LD_W_ABS, 0),
    ];
    #[cfg(target_arch = "x86_64")]
    p.push(jmp(JSET_K, 0x4000_0000, T::Deny, T::Next)); // x32 ABI
    for (_, nr) in denied_syscalls() {
        p.push(jmp(JEQ_K, nr as u32, T::Deny, T::Next));
    }
    // clone3 passes its flags in memory, out of the filter's sight: ENOSYS
    // makes libc fall back to clone, whose flags are checked below.
    let clone3 = p.len();
    p.push(jmp(JEQ_K, libc::SYS_clone3 as u32, T::Next, T::Next)); // patched
    let clone = p.len();
    p.push(jmp(JEQ_K, libc::SYS_clone as u32, T::Next, T::Next)); // patched
    let socket = p.len();
    p.push(jmp(JEQ_K, libc::SYS_socket as u32, T::Next, T::Allow));
    // socket(domain, type, protocol): AF_PACKET; AF_NETLINK other than
    // NETLINK_ROUTE (VOL1 C-00: "except NETLINK_ROUTE read"); SOCK_RAW on
    // AF_INET/AF_INET6. "Read" is not something seccomp can see: rtnetlink
    // writes need CAP_NET_ADMIN, which bwrap's --cap-drop ALL has taken, and
    // the namespace holds only loopback.
    p.push(st(LD_W_ABS, 16));
    p.push(jmp(JEQ_K, AF_PACKET, T::Deny, T::Next));
    let n = p.len();
    p.push(jmp(JEQ_K, AF_NETLINK, T::At(n + 6), T::Next));
    p.push(jmp(JEQ_K, AF_INET, T::At(n + 3), T::Next));
    p.push(jmp(JEQ_K, AF_INET6, T::At(n + 3), T::Allow));
    p.push(st(LD_W_ABS, 24)); // n + 3
    p.push(st(ALU_AND_K, 0xf)); // the type, without SOCK_NONBLOCK|CLOEXEC
    p.push(jmp(JEQ_K, SOCK_RAW, T::Deny, T::Allow));
    p.push(st(LD_W_ABS, 32)); // n + 6: the netlink protocol
    p.push(jmp(JEQ_K, NETLINK_ROUTE, T::Allow, T::Deny));
    // clone3 -> ENOSYS block, then clone flags block, placed after the socket
    // block; the earlier compares jump here.
    let clone3_block = p.len();
    p.push(st(RET_K, ret_errno(libc::ENOSYS)));
    let clone_block = p.len();
    p.push(st(LD_W_ABS, 16));
    p.push(jmp(JSET_K, CLONE_NS_MASK, T::Deny, T::Allow));
    p[clone3].jt = T::At(clone3_block);
    p[clone].jt = T::At(clone_block);
    // Anything else is not a clone or a socket: on to the socket compare. The
    // accumulator still holds the syscall number there.
    p[clone3].jf = T::Next;
    p[clone].jf = T::At(socket);
    let allow = p.len();
    p.push(st(RET_K, RET_ALLOW));
    let deny = p.len();
    p.push(st(RET_K, ret_errno(libc::EPERM)));
    let kill = p.len();
    p.push(st(RET_K, RET_KILL_PROCESS));

    let off = |i: usize, t: T| -> u8 {
        let target = match t {
            T::Next => i + 1,
            T::Allow => allow,
            T::Deny => deny,
            T::Kill => kill,
            T::At(x) => x,
        };
        u8::try_from(target - (i + 1)).expect("filter branch out of range")
    };
    p.iter()
        .enumerate()
        .map(|(i, x)| {
            let is_jump = x.code == JEQ_K || x.code == JSET_K;
            libc::sock_filter {
                code: x.code,
                jt: if is_jump { off(i, x.jt) } else { 0 },
                jf: if is_jump { off(i, x.jf) } else { 0 },
                k: x.k,
            }
        })
        .collect()
}

#[cfg(not(any(target_arch = "x86_64", target_arch = "aarch64")))]
pub fn build_filter() -> Vec<libc::sock_filter> {
    Vec::new()
}

/// Installs the filter on this thread and its descendants.
pub fn apply_seccomp() -> Result<(), String> {
    let mut f = build_filter();
    if f.is_empty() {
        return Err("no seccomp filter for this architecture".into());
    }
    let prog = libc::sock_fprog {
        len: f.len() as u16,
        filter: f.as_mut_ptr(),
    };
    // SAFETY: `prog` points at `f`, which lives across the call.
    let r = unsafe {
        libc::prctl(
            libc::PR_SET_SECCOMP,
            libc::SECCOMP_MODE_FILTER as libc::c_ulong,
            &prog as *const libc::sock_fprog,
            0,
            0,
        )
    };
    if r != 0 {
        return Err(format!("seccomp: {}", std::io::Error::last_os_error()));
    }
    Ok(())
}

pub fn set_no_new_privs() -> Result<(), String> {
    // SAFETY: plain prctl with integer arguments.
    if unsafe { libc::prctl(libc::PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0) } != 0 {
        return Err(format!("no_new_privs: {}", std::io::Error::last_os_error()));
    }
    Ok(())
}

// ---- the helper -------------------------------------------------------------

/// `ec-agentd --sandbox-init <cfg> -- <entrypoint> [args...]`. Never returns:
/// it execs the entrypoint, or exits [`EXIT_SANDBOX`] having said why on its
/// stderr (the launcher logs that, never the agent's own output).
pub fn init_main(cfg_path: &str, argv: &[String]) -> ! {
    fn fail(msg: &str) -> ! {
        eprintln!("ec-agentd sandbox-init: {msg}");
        std::process::exit(EXIT_SANDBOX);
    }
    let Some((prog, args)) = argv.split_first() else {
        fail("no entrypoint");
    };
    let cfg = std::fs::read_to_string(cfg_path)
        .ok()
        .and_then(|s| serde_json::from_str::<Value>(&s).ok())
        .and_then(|v| FsPolicy::from_json(&v))
        .unwrap_or_else(|| fail("unreadable sandbox config"));
    if let Err(e) = set_no_new_privs() {
        fail(&e);
    }
    if let Err(e) = apply_landlock(&cfg) {
        fail(&e);
    }
    if let Err(e) = apply_seccomp() {
        fail(&e);
    }
    // Keep the original stderr (a pipe to the launcher) on fd 100, close on
    // exec, to report an exec failure; the agent itself gets /dev/null.
    // SAFETY: plain fd calls on descriptors this process owns.
    unsafe {
        let null = libc::open(c"/dev/null".as_ptr(), libc::O_WRONLY | libc::O_CLOEXEC);
        let saved = libc::fcntl(2, libc::F_DUPFD_CLOEXEC, 100);
        if null >= 0 {
            libc::dup2(null, 2);
        }
        let err = std::process::Command::new(prog).args(args).exec();
        if saved >= 0 {
            libc::dup2(saved, 2);
        }
        fail(&format!("cannot exec the entrypoint: {err}"));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ---- a BPF interpreter, for the instruction subset the filter uses ----

    fn run(filter: &[libc::sock_filter], arch: u32, nr: u32, args: [u32; 6]) -> u32 {
        let word = |off: u32| -> u32 {
            match off {
                0 => nr,
                4 => arch,
                16..=63 if (off - 16) % 8 == 0 => args[((off - 16) / 8) as usize],
                _ => panic!("unexpected load at {off}"),
            }
        };
        let (mut a, mut pc) = (0u32, 0usize);
        loop {
            let i = &filter[pc];
            pc += 1;
            match i.code {
                LD_W_ABS => a = word(i.k),
                ALU_AND_K => a &= i.k,
                JEQ_K => pc += if a == i.k { i.jt } else { i.jf } as usize,
                JSET_K => pc += if a & i.k != 0 { i.jt } else { i.jf } as usize,
                RET_K => return i.k,
                c => panic!("unexpected opcode {c:#x}"),
            }
        }
    }

    fn verdict(nr: i64, args: [u32; 6]) -> u32 {
        run(&build_filter(), AUDIT_ARCH, nr as u32, args)
    }

    const EPERM: u32 = ret_errno(libc::EPERM);

    #[test]
    fn every_listed_syscall_is_denied() {
        let want = [
            "SYS_ptrace",
            "SYS_process_vm_readv",
            "SYS_process_vm_writev",
            "SYS_mount",
            "SYS_umount2",
            "SYS_pivot_root",
            "SYS_kexec_load",
            "SYS_kexec_file_load",
            "SYS_add_key",
            "SYS_request_key",
            "SYS_keyctl",
            "SYS_bpf",
            "SYS_perf_event_open",
            "SYS_init_module",
            "SYS_finit_module",
            "SYS_delete_module",
            "SYS_unshare",
            "SYS_setns",
            "SYS_open_by_handle_at",
            "SYS_userfaultfd",
        ];
        let denied = denied_syscalls();
        for w in want {
            let (_, nr) = denied
                .iter()
                .find(|(n, _)| *n == w)
                .unwrap_or_else(|| panic!("{w} missing"));
            assert_eq!(verdict(*nr, [0; 6]), EPERM, "{w}");
        }
    }

    #[test]
    fn ordinary_syscalls_pass() {
        for nr in [
            libc::SYS_read,
            libc::SYS_write,
            libc::SYS_openat,
            libc::SYS_execve,
            libc::SYS_mmap,
        ] {
            assert_eq!(verdict(nr, [0; 6]), RET_ALLOW, "{nr}");
        }
    }

    #[test]
    fn raw_and_packet_sockets_are_denied_and_stream_sockets_are_not() {
        let sock = |d: u32, t: u32| verdict(libc::SYS_socket, [d, t, 0, 0, 0, 0]);
        assert_eq!(sock(AF_PACKET, libc::SOCK_DGRAM as u32), EPERM);
        assert_eq!(sock(AF_PACKET, SOCK_RAW), EPERM);
        assert_eq!(sock(AF_INET, SOCK_RAW), EPERM);
        assert_eq!(sock(AF_INET6, SOCK_RAW), EPERM);
        // The type word may carry SOCK_NONBLOCK and SOCK_CLOEXEC.
        let flags = (libc::SOCK_NONBLOCK | libc::SOCK_CLOEXEC) as u32;
        assert_eq!(sock(AF_INET, SOCK_RAW | flags), EPERM);
        assert_eq!(sock(AF_INET, libc::SOCK_STREAM as u32), RET_ALLOW);
        assert_eq!(sock(AF_INET6, libc::SOCK_DGRAM as u32 | flags), RET_ALLOW);
        assert_eq!(sock(libc::AF_UNIX as u32, libc::SOCK_STREAM as u32), RET_ALLOW);
    }

    #[test]
    fn netlink_is_route_only() {
        let nl = |t: u32, proto: u32| verdict(libc::SYS_socket, [AF_NETLINK, t, proto, 0, 0, 0]);
        let raw = SOCK_RAW | libc::SOCK_CLOEXEC as u32;
        assert_eq!(nl(raw, NETLINK_ROUTE), RET_ALLOW);
        assert_eq!(nl(libc::SOCK_DGRAM as u32, NETLINK_ROUTE), RET_ALLOW);
        for proto in [
            libc::NETLINK_AUDIT,
            libc::NETLINK_KOBJECT_UEVENT,
            libc::NETLINK_SOCK_DIAG,
            libc::NETLINK_NETFILTER,
            libc::NETLINK_GENERIC,
        ] {
            assert_eq!(nl(raw, proto as u32), EPERM, "{proto}");
        }
    }

    #[test]
    fn namespace_clones_are_denied() {
        let clone = |flags: u32| verdict(libc::SYS_clone, [flags, 0, 0, 0, 0, 0]);
        assert_eq!(clone(libc::CLONE_NEWUSER as u32), EPERM);
        assert_eq!(clone(libc::CLONE_NEWNS as u32 | libc::SIGCHLD as u32), EPERM);
        assert_eq!(clone(libc::CLONE_NEWNET as u32), EPERM);
        // A thread or a fork is fine.
        assert_eq!(
            clone((libc::CLONE_VM | libc::CLONE_FS | libc::CLONE_THREAD) as u32),
            RET_ALLOW
        );
        assert_eq!(clone(libc::SIGCHLD as u32), RET_ALLOW);
        assert_eq!(verdict(libc::SYS_clone3, [0; 6]), ret_errno(libc::ENOSYS));
    }

    #[test]
    fn a_foreign_architecture_is_killed() {
        let f = build_filter();
        assert_eq!(run(&f, AUDIT_ARCH ^ 0x4000_0000, 0, [0; 6]), RET_KILL_PROCESS);
        assert_eq!(run(&f, 3, 0, [0; 6]), RET_KILL_PROCESS);
    }

    #[cfg(target_arch = "x86_64")]
    #[test]
    fn the_x32_abi_is_denied() {
        assert_eq!(run(&build_filter(), AUDIT_ARCH, 0x4000_0000 | 1, [0; 6]), EPERM);
    }

    // ---- the Landlock allow-list ----

    fn policy() -> FsPolicy {
        FsPolicy::for_package(Path::new("/opt/agent"), &Decl::default())
    }

    #[test]
    fn writable_means_scratch_and_dev_null_only() {
        let p = policy();
        assert_eq!(p.rw, [PathBuf::from("/tmp")]);
        assert_eq!(p.rw_dev, [PathBuf::from("/dev/null")]);
        // Nothing writable is also executable.
        for w in p.rw.iter().chain(&p.rw_dev) {
            assert!(
                !p.ro_exec.iter().any(|x| w.starts_with(x) || x.starts_with(w)),
                "{w:?}"
            );
        }
    }

    #[test]
    fn the_package_and_runtime_are_read_only_executable() {
        let p = policy();
        for d in ["/opt/agent", "/usr", "/lib", "/etc/ld.so.cache"] {
            assert!(p.ro_exec.contains(&PathBuf::from(d)), "{d}");
        }
        for d in ["/home", "/root", "/etc", "/run", "/var", "/sys", "/"] {
            let d = PathBuf::from(d);
            let all = || p.ro_exec.iter().chain(&p.ro).chain(&p.rw).chain(&p.rw_dev);
            assert!(!all().any(|x| *x == d), "{d:?} must not be an allowed root");
        }
    }

    #[test]
    fn policy_round_trips_through_the_config_file() {
        let d = Decl {
            fs_read: vec![PathBuf::from("/data/in")],
        };
        let p = FsPolicy::for_package(Path::new("/opt/agent"), &d);
        assert!(p.ro.contains(&PathBuf::from("/data/in")));
        assert_eq!(FsPolicy::from_json(&p.to_json()), Some(p));
    }

    #[test]
    fn declarations_are_validated() {
        let home = Path::new("/home/u");
        let raw = |r: &[&str], w: &[&str], e: &[&str]| RawDecl {
            fs_read: r.iter().map(|s| s.to_string()).collect(),
            fs_write: w.iter().map(|s| s.to_string()).collect(),
            egress: e.iter().map(|s| s.to_string()).collect(),
        };
        let err = |r: RawDecl| r.resolve(Some(home)).unwrap_err();
        assert_eq!(err(raw(&[], &[], &["api.acme.com:443"])), "egress_unavailable");
        for bad in [
            "~/.ssh",
            "~/.ssh/id_ed25519",
            "~/.gnupg",
            "~",
            "/home/u",
            "/",
            "~/.mozilla/x",
        ] {
            assert_eq!(err(raw(&[bad], &[], &[])), "fs_read_forbidden", "{bad}");
        }
        assert_eq!(err(raw(&["rel/path"], &[], &[])), "fs_path_invalid");
        assert_eq!(err(raw(&["~/a/../.ssh"], &[], &[])), "fs_path_invalid");
        assert_eq!(err(raw(&[], &["~/Documents"], &[])), "fs_write_outside_scratch");
        assert_eq!(err(raw(&[], &["/etc"], &[])), "fs_write_outside_scratch");
        // Scratch writes are what the sandbox has anyway; a missing read path
        // grants nothing.
        let ok = raw(&["/definitely/not/here"], &["/tmp/out"], &[])
            .resolve(Some(home))
            .unwrap();
        assert!(ok.fs_read.is_empty());
    }

    #[test]
    fn a_symlink_into_a_forbidden_place_is_refused() {
        let tmp = crate::scratch_dir("sbx-link");
        let home = tmp.join("home");
        std::fs::create_dir_all(home.join(".ssh")).unwrap();
        std::os::unix::fs::symlink(home.join(".ssh"), tmp.join("innocent")).unwrap();
        let r = RawDecl {
            fs_read: vec![tmp.join("innocent").to_string_lossy().into_owned()],
            ..RawDecl::default()
        };
        assert_eq!(r.resolve(Some(&home)).unwrap_err(), "fs_read_forbidden");
    }

    #[test]
    fn handled_rights_follow_the_abi() {
        assert_eq!(handled_for(1), 0x1fff);
        assert_ne!(handled_for(3) & ll::TRUNCATE, 0);
        assert_eq!(handled_for(1) & ll::TRUNCATE, 0);
        // A rule on a plain file may not carry directory rights.
        assert_eq!(
            rights_for(ll::READ_FILE | ll::READ_DIR, false, handled_for(3)),
            ll::READ_FILE
        );
    }

    #[test]
    fn the_preflight_matches_the_kernel() {
        assert_eq!(
            preflight().is_ok(),
            landlock_abi().is_ok() || cfg!(feature = "dev-unsandboxed")
        );
    }
}
