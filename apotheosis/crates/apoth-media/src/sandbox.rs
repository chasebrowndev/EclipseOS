// SPDX-License-Identifier: AGPL-3.0-only
//! The helper's own sandbox (T-16 §4; not the TCB agentd sandbox, which it shares no code
//! with). Order: `PR_SET_NO_NEW_PRIVS`, resource limits, a Landlock ruleset that handles
//! every filesystem access and adds no rule (an empty view: any path open fails), then a
//! seccomp allow-list. It fails closed: a ruleset that is not fully enforced, or a failed
//! `prctl`/seccomp load, is an error and the helper exits before it reads input.
//!
//! The allow-list covers what the demuxers, decoders, the Rust runtime and rav1d's worker
//! threads use: reads and writes on fds it already holds, `recvmsg`/`sendmsg` on its socket,
//! memory management, threads (`clone` only with `CLONE_THREAD`; `clone3` answers `ENOSYS`
//! so the C library falls back to `clone`, whose flags can be checked), futexes, clocks.
//! There is no `open*`, `socket`, `connect`, `execve`, `ioctl` or `ptrace`; `tgkill` reaches
//! only this process (abort). Anything else fails with `EPERM`.

use std::collections::BTreeMap;

use landlock::{ABI, Access, AccessFs, Ruleset, RulesetAttr, RulesetStatus};
use seccompiler::{
    BpfProgram, SeccompAction, SeccompCmpArgLen, SeccompCmpOp, SeccompCondition, SeccompFilter,
    SeccompRule, TargetArch,
};

/// Address-space cap (`RLIMIT_AS`) per helper mode.
pub const AS_MEDIA: u64 = 3 << 30;
pub const AS_IMAGE: u64 = 3 << 29;
/// Open fds at most (it inherits stdin and gets two more).
const NOFILE: u64 = 16;
/// The Landlock ABI whose every filesystem right is handled.
const ABI_FS: ABI = ABI::V3;

/// Why the sandbox could not be put in force.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SandboxError {
    NoNewPrivs,
    Rlimit,
    /// Landlock missing or only partly enforced.
    Landlock,
    Seccomp,
}

/// Test knob (tighten only): treat the Landlock ruleset as partly enforced, as a kernel
/// without the full ABI would report it.
#[derive(Debug, Clone, Copy, Default)]
pub struct Knobs {
    pub simulate_partial_landlock: bool,
}

/// Put the sandbox in force for the calling (single-threaded) process.
pub fn enter(address_space: u64, knobs: Knobs) -> Result<(), SandboxError> {
    rustix::thread::set_no_new_privs(true).map_err(|_| SandboxError::NoNewPrivs)?;
    use rustix::process::{Resource, Rlimit, setrlimit};
    let lim = |r, v: u64| {
        setrlimit(
            r,
            Rlimit {
                current: Some(v),
                maximum: Some(v),
            },
        )
    };
    lim(Resource::As, address_space).map_err(|_| SandboxError::Rlimit)?;
    lim(Resource::Nofile, NOFILE).map_err(|_| SandboxError::Rlimit)?;
    lim(Resource::Core, 0).map_err(|_| SandboxError::Rlimit)?;
    landlock_empty(knobs)?;
    for p in programs(rustix::process::getpid().as_raw_nonzero().get())? {
        seccompiler::apply_filter(&p).map_err(|_| SandboxError::Seccomp)?;
    }
    Ok(())
}

fn landlock_empty(knobs: Knobs) -> Result<(), SandboxError> {
    let status = Ruleset::default()
        .handle_access(AccessFs::from_all(ABI_FS))
        .and_then(|r| r.create())
        .and_then(|r| r.restrict_self())
        .map_err(|_| SandboxError::Landlock)?;
    if status.ruleset != RulesetStatus::FullyEnforced || knobs.simulate_partial_landlock {
        return Err(SandboxError::Landlock);
    }
    Ok(())
}

fn arch() -> Result<TargetArch, SandboxError> {
    TargetArch::try_from(std::env::consts::ARCH).map_err(|_| SandboxError::Seccomp)
}

/// The syscalls allowed without argument checks.
fn plain() -> Vec<i64> {
    use libc::*;
    let mut v = vec![
        SYS_read,
        SYS_write,
        SYS_readv,
        SYS_writev,
        SYS_pread64,
        SYS_pwrite64,
        SYS_lseek,
        SYS_close,
        SYS_fstat,
        SYS_newfstatat,
        SYS_statx,
        SYS_fcntl,
        SYS_recvmsg,
        SYS_recvfrom,
        SYS_sendmsg,
        SYS_sendto,
        SYS_mmap,
        SYS_munmap,
        SYS_mremap,
        SYS_mprotect,
        SYS_madvise,
        SYS_brk,
        SYS_futex,
        SYS_set_robust_list,
        SYS_rseq,
        SYS_sched_yield,
        SYS_sched_getaffinity,
        SYS_exit,
        SYS_exit_group,
        SYS_rt_sigaction,
        SYS_rt_sigprocmask,
        SYS_rt_sigreturn,
        SYS_sigaltstack,
        SYS_getrandom,
        SYS_ppoll,
        SYS_clock_gettime,
        SYS_clock_nanosleep,
        SYS_nanosleep,
        SYS_gettid,
        SYS_getpid,
        SYS_restart_syscall,
    ];
    #[cfg(target_arch = "x86_64")]
    v.extend_from_slice(&[SYS_poll, SYS_arch_prctl]);
    v
}

fn cond(arg: u8, op: SeccompCmpOp, value: u64) -> Result<SeccompCondition, SandboxError> {
    SeccompCondition::new(arg, SeccompCmpArgLen::Qword, op, value)
        .map_err(|_| SandboxError::Seccomp)
}

fn rule(c: Vec<SeccompCondition>) -> Result<SeccompRule, SandboxError> {
    SeccompRule::new(c).map_err(|_| SandboxError::Seccomp)
}

/// The BPF programs for this process (`pid` bounds `tgkill`), in load order: first
/// `clone3` → `ENOSYS` (everything else allowed, so the second load still works), then the
/// allow-list (everything else `EPERM`). `clone3` takes its flags by pointer, which BPF
/// cannot read; the allow-list lets it through and the first filter's `ENOSYS` wins (errno
/// outranks allow), so the C library falls back to `clone`, whose flags are checked. The
/// allow-list has neither `seccomp` nor `prctl` beyond thread names, so nothing loads after it.
pub fn programs(pid: i32) -> Result<[BpfProgram; 2], SandboxError> {
    let mut rules: BTreeMap<i64, Vec<SeccompRule>> =
        plain().into_iter().map(|s| (s, Vec::new())).collect();
    // Answered by the first filter.
    rules.insert(libc::SYS_clone3, Vec::new());
    // Threads only: `clone` with CLONE_THREAD set.
    let thread = libc::CLONE_THREAD as u64;
    rules.insert(
        libc::SYS_clone,
        vec![rule(vec![cond(
            0,
            SeccompCmpOp::MaskedEq(thread),
            thread,
        )?])?],
    );
    // Thread names only.
    rules.insert(
        libc::SYS_prctl,
        vec![rule(vec![cond(
            0,
            SeccompCmpOp::Eq,
            libc::PR_SET_NAME as u64,
        )?])?],
    );
    // Signals to this process only (abort).
    rules.insert(
        libc::SYS_tgkill,
        vec![rule(vec![cond(0, SeccompCmpOp::Eq, pid as u64)?])?],
    );
    let allow = SeccompFilter::new(
        rules,
        SeccompAction::Errno(libc::EPERM as u32),
        SeccompAction::Allow,
        arch()?,
    )
    .map_err(|_| SandboxError::Seccomp)?;
    let clone3 = SeccompFilter::new(
        BTreeMap::from([(libc::SYS_clone3, Vec::new())]),
        SeccompAction::Allow,
        SeccompAction::Errno(libc::ENOSYS as u32),
        arch()?,
    )
    .map_err(|_| SandboxError::Seccomp)?;
    Ok([
        clone3.try_into().map_err(|_| SandboxError::Seccomp)?,
        allow.try_into().map_err(|_| SandboxError::Seccomp)?,
    ])
}
