// SPDX-License-Identifier: AGPL-3.0-only
//! PTY pair, child spawn in a new session, pidfd exit detection, resize, termios and
//! foreground-group queries, escalating close (T-18 §7, SECURITY-28, VTCORE-48/49/50/52).

use std::ffi::OsString;
use std::io;
use std::os::fd::{AsFd, BorrowedFd, OwnedFd};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use rustix::event::{PollFd, PollFlags, Timespec, poll};
use rustix::fs::{OFlags, fcntl_getfl, fcntl_setfl};
use rustix::io::Errno;
use rustix::process::{
    Pid, PidfdFlags, Signal, WaitId, WaitIdOptions, kill_process_group, pidfd_open, waitid,
};
use rustix::pty::{OpenptFlags, grantpt, ioctl_tiocgptpeer, openpt, ptsname, unlockpt};
use rustix::termios::{
    InputModes, LocalModes, OptionalActions, Winsize, tcgetattr, tcgetpgrp, tcgetwinsize,
    tcsetattr, tcsetwinsize,
};

use crate::outq::{Enqueue, OutQueue};
use crate::procinfo;

fn io_err(e: Errno) -> io::Error {
    io::Error::from_raw_os_error(e.raw_os_error())
}

/// Terminal size as the kernel stores it. Pixels are physical buffer pixels with
/// padding excluded: `xpixel = cols * cell_w`, `ypixel = rows * cell_h` (VTCORE-49).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PtySize {
    /// Rows.
    pub rows: u16,
    /// Columns.
    pub cols: u16,
    /// `ws_xpixel`.
    pub xpixel: u16,
    /// `ws_ypixel`.
    pub ypixel: u16,
}

impl PtySize {
    /// Derive the pixel fields from the cell size (saturating at `u16::MAX`).
    pub fn from_cells(rows: u16, cols: u16, cell_w: u32, cell_h: u32) -> Self {
        let sat = |n: u16, cell: u32| {
            u16::try_from(u32::from(n).saturating_mul(cell)).unwrap_or(u16::MAX)
        };
        Self {
            rows,
            cols,
            xpixel: sat(cols, cell_w),
            ypixel: sat(rows, cell_h),
        }
    }

    fn ws(self) -> Winsize {
        Winsize {
            ws_row: self.rows,
            ws_col: self.cols,
            ws_xpixel: self.xpixel,
            ws_ypixel: self.ypixel,
        }
    }
}

/// How the child ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExitStatus {
    /// `exit(code)`.
    Code(i32),
    /// Terminated by signal number.
    Signal(i32),
}

/// Grace and wait durations for [`Pty::close`]. SECURITY-28 gives 1 s grace; the
/// post-SIGKILL reap bound has no spec number (2 s here, reported as a gap).
#[derive(Debug, Clone, Copy)]
pub struct CloseTimings {
    /// Wait after SIGHUP.
    pub hup_grace: Duration,
    /// Wait after SIGTERM.
    pub term_grace: Duration,
    /// Wait for the reap after SIGKILL.
    pub kill_reap: Duration,
}

impl Default for CloseTimings {
    fn default() -> Self {
        Self {
            hup_grace: Duration::from_secs(1),
            term_grace: Duration::from_secs(1),
            kill_reap: Duration::from_secs(2),
        }
    }
}

/// Everything needed to start a child. `argv` is an array, never a shell string.
#[derive(Clone)]
pub struct SpawnConfig {
    /// `argv[0]` is the program (resolved by the caller; looked up via PATH if bare).
    pub argv: Vec<OsString>,
    /// Prefix `argv[0]` with `-` so the shell runs as a login shell.
    pub login: bool,
    /// Working directory; inherited when `None`.
    pub cwd: Option<PathBuf>,
    /// Final environment (see [`crate::build_env`]); the child gets exactly this.
    pub env: Vec<(OsString, OsString)>,
    /// Initial window size, set before the child starts.
    pub size: PtySize,
    /// Set IUTF8 on the pty termios (config `pty.iutf8`, default true; VTCORE-48).
    pub iutf8: bool,
}

// Manual Debug: argv is the command line and env may hold secrets; counts only.
impl std::fmt::Debug for SpawnConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SpawnConfig")
            .field("argc", &self.argv.len())
            .field("login", &self.login)
            .field("has_cwd", &self.cwd.is_some())
            .field("env_vars", &self.env.len())
            .field("size", &self.size)
            .field("iutf8", &self.iutf8)
            .finish()
    }
}

/// ECHO / ICANON state of the pty line discipline.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TermiosState {
    /// `ECHO` set.
    pub echo: bool,
    /// `ICANON` set.
    pub icanon: bool,
}

impl TermiosState {
    /// ADR 0115 credential signal: ECHO off AND ICANON on. Both off (vim, htop) is not.
    pub fn is_credential(self) -> bool {
        !self.echo && self.icanon
    }
}

/// One pane's PTY master plus its child.
#[derive(Debug)]
pub struct Pty {
    master: OwnedFd,
    pidfd: OwnedFd,
    pid: Pid,
    size: PtySize,
    queue: OutQueue,
    status: Option<ExitStatus>,
}

/// Raw `close_range` flag: mark as close-on-exec instead of closing.
const CLOSE_RANGE_CLOEXEC: u32 = 4;

/// Runs in the forked child before exec. Async-signal-safe: raw syscalls only, no
/// allocation (errors are `from_raw_os_error`).
#[allow(unsafe_code)]
fn child_setup() -> io::Result<()> {
    rustix::process::setsid().map_err(io_err)?;
    // SAFETY: std dup2s the slave onto fd 0..=2 before running pre_exec hooks (its
    // documented order: stdio setup, then hooks, then exec; we rely on that), so fd 0 is
    // open and stays open for the duration of this borrow.
    #[allow(unsafe_code)]
    let fd0 = unsafe { BorrowedFd::borrow_raw(0) };
    rustix::process::ioctl_tiocsctty(fd0).map_err(io_err)?;
    // Everything above fd 2 stops being inherited. CLOEXEC rather than close so that
    // std's exec-error pipe (itself CLOEXEC) still reports a failed exec. Kernels
    // before 5.11 lack the call; every fd we create is already CLOEXEC, so ignore.
    //
    #[allow(unsafe_code)]
    // SAFETY: raw `close_range(3, ~0, CLOSE_RANGE_CLOEXEC)` has no memory arguments and
    // is async-signal-safe; the result is deliberately ignored.
    unsafe {
        libc::syscall(libc::SYS_close_range, 3u32, u32::MAX, CLOSE_RANGE_CLOEXEC);
    }
    Ok(())
}

impl Pty {
    /// Open a pty, apply IUTF8 and the initial size, and spawn the child in a new session
    /// with the slave as controlling tty and stdio. The master is `O_NONBLOCK` + CLOEXEC.
    pub fn spawn(cfg: &SpawnConfig) -> io::Result<Pty> {
        let Some(program) = cfg.argv.first() else {
            return Err(io::Error::new(io::ErrorKind::InvalidInput, "empty argv"));
        };
        let flags = OpenptFlags::RDWR | OpenptFlags::NOCTTY | OpenptFlags::CLOEXEC;
        let master = openpt(flags).map_err(io_err)?;
        grantpt(&master).map_err(io_err)?;
        unlockpt(&master).map_err(io_err)?;
        let slave = match ioctl_tiocgptpeer(&master, flags) {
            Ok(f) => f,
            Err(_) => {
                let name = ptsname(&master, Vec::new()).map_err(io_err)?;
                rustix::fs::open(
                    name.as_c_str(),
                    OFlags::RDWR | OFlags::NOCTTY | OFlags::CLOEXEC,
                    rustix::fs::Mode::empty(),
                )
                .map_err(io_err)?
            }
        };

        // Line discipline: IUTF8 on request; IXON/IXOFF are left to the discipline (VTCORE-52).
        let mut t = tcgetattr(&slave).map_err(io_err)?;
        t.input_modes.set(InputModes::IUTF8, cfg.iutf8);
        tcsetattr(&slave, OptionalActions::Now, &t).map_err(io_err)?;
        tcsetwinsize(&slave, cfg.size.ws()).map_err(io_err)?;

        let mut cmd = Command::new(program);
        if cfg.login {
            let base = Path::new(program)
                .file_name()
                .unwrap_or(program.as_os_str());
            let mut a0 = OsString::from("-");
            a0.push(base);
            cmd.arg0(a0);
        }
        cmd.args(&cfg.argv[1..]);
        cmd.env_clear();
        cmd.envs(cfg.env.iter().map(|(k, v)| (k, v)));
        if let Some(d) = &cfg.cwd {
            cmd.current_dir(d);
        }
        cmd.stdin(Stdio::from(slave.try_clone()?));
        cmd.stdout(Stdio::from(slave.try_clone()?));
        cmd.stderr(Stdio::from(slave.try_clone()?));
        #[allow(unsafe_code)]
        // SAFETY: the hook only issues raw syscalls (setsid, ioctl, close_range) and
        // touches no allocator or lock, so it is sound between fork and exec.
        unsafe {
            cmd.pre_exec(child_setup);
        }
        let mut child = cmd.spawn()?;
        drop(cmd);
        drop(slave); // parent must not hold the slave or EIO/HUP never arrives

        let pid = Pid::from_child(&child);
        let pidfd = match pidfd_open(pid, PidfdFlags::empty()) {
            Ok(f) => f,
            Err(e) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(io_err(e));
            }
        };
        // Reaping goes through the pidfd (waitid); `Child` is just dropped (no wait on drop).
        drop(child);

        let fl = fcntl_getfl(&master).map_err(io_err)?;
        fcntl_setfl(&master, fl | OFlags::NONBLOCK).map_err(io_err)?;

        Ok(Pty {
            master,
            pidfd,
            pid,
            size: cfg.size,
            queue: OutQueue::default(),
            status: None,
        })
    }

    /// The master fd (register for POLLIN; POLLOUT only while [`Pty::wants_pollout`]).
    pub fn master_fd(&self) -> BorrowedFd<'_> {
        self.master.as_fd()
    }

    /// The pidfd (becomes readable when the child exits; T-18 §7).
    pub fn pidfd(&self) -> BorrowedFd<'_> {
        self.pidfd.as_fd()
    }

    /// Child pid (also its pgid and sid).
    pub fn pid(&self) -> i32 {
        self.pid.as_raw_pid()
    }

    /// Non-blocking read of pty output. `WouldBlock` = drained; `EIO` = slave closed
    /// (the caller drains fully on HUP, PERFORMANCE-59).
    pub fn read(&self, buf: &mut [u8]) -> io::Result<usize> {
        rustix::io::read(&self.master, buf).map_err(io_err)
    }

    fn write_master(master: &OwnedFd, b: &[u8]) -> io::Result<usize> {
        rustix::io::write(master, b).map_err(io_err)
    }

    /// Submit one whole ordinary unit (a reply or a key's bytes). Bounded at 1 MiB of
    /// pending bytes, a paste in flight excepted (T-00 §4.2).
    pub fn send(&mut self, unit: Vec<u8>) -> io::Result<Enqueue> {
        let m = &self.master;
        self.queue.send(unit, &mut |b| Self::write_master(m, b))
    }

    /// Submit a whole paste with its bracket wrapper: the paste in flight rides on top of
    /// the 1 MiB bound; behind other units it counts (T-00 §4.2).
    pub fn send_paste(&mut self, unit: Vec<u8>) -> io::Result<Enqueue> {
        let m = &self.master;
        self.queue
            .send_paste(unit, &mut |b| Self::write_master(m, b))
    }

    /// Drain the queue on POLLOUT. `Ok(true)` = empty, disarm write interest.
    pub fn drain_out(&mut self) -> io::Result<bool> {
        let m = &self.master;
        self.queue.drain(&mut |b| Self::write_master(m, b))
    }

    /// True while the outbound queue is non-empty: arm POLLOUT only then.
    pub fn wants_pollout(&self) -> bool {
        self.queue.wants_write()
    }

    /// Units dropped on queue overflow (counter only).
    pub fn dropped_units(&self) -> u64 {
        self.queue.dropped_units()
    }

    /// Bytes waiting in the outbound queue.
    pub fn pending_out_bytes(&self) -> usize {
        self.queue.pending_bytes()
    }

    /// Push a new size. `Ok(false)` if nothing changed (a pixel-only change still goes
    /// through). The kernel delivers SIGWINCH; we never `kill()` for resize (VTCORE-50).
    /// Callers log an error and carry on.
    pub fn resize(&mut self, size: PtySize) -> io::Result<bool> {
        if size == self.size {
            return Ok(false);
        }
        tcsetwinsize(&self.master, size.ws()).map_err(io_err)?;
        self.size = size;
        Ok(true)
    }

    /// The size the kernel currently holds.
    pub fn kernel_winsize(&self) -> io::Result<PtySize> {
        let w = tcgetwinsize(&self.master).map_err(io_err)?;
        Ok(PtySize {
            rows: w.ws_row,
            cols: w.ws_col,
            xpixel: w.ws_xpixel,
            ypixel: w.ws_ypixel,
        })
    }

    /// Inspect ECHO/ICANON (ADR 0115). The caller samples on output-idle edges and
    /// before content leaves the process.
    pub fn termios_state(&self) -> io::Result<TermiosState> {
        let t = tcgetattr(&self.master).map_err(io_err)?;
        Ok(TermiosState {
            echo: t.local_modes.contains(LocalModes::ECHO),
            icanon: t.local_modes.contains(LocalModes::ICANON),
        })
    }

    /// Foreground process group of the pty (`tcgetpgrp` on the master).
    pub fn foreground_pgid(&self) -> io::Result<i32> {
        tcgetpgrp(&self.master)
            .map(|p| p.as_raw_pid())
            .map_err(io_err)
    }

    /// `/proc/<pgid leader>/cwd`, falling back to the shell pid when the group query or
    /// the leader is gone (OSC 7 is primary and handled by the caller).
    pub fn foreground_cwd(&self) -> io::Result<PathBuf> {
        let shell = self.pid.as_raw_pid();
        match self.foreground_pgid() {
            Ok(pg) => procinfo::cwd(pg).or_else(|_| procinfo::cwd(shell)),
            Err(_) => procinfo::cwd(shell),
        }
    }

    /// Remote-session check from the foreground process group, falling back to the
    /// shell's group when the query fails (T-18 §10).
    pub fn foreground_is_remote(&self, user_names: &[String]) -> bool {
        let pgid = self.foreground_pgid().unwrap_or(self.pid.as_raw_pid());
        procinfo::is_remote(&procinfo::ProcFs, pgid, user_names)
    }

    /// Reap the child if it has exited (by pidfd, never SIGCHLD or polling a pid).
    pub fn try_wait(&mut self) -> io::Result<Option<ExitStatus>> {
        if self.status.is_some() {
            return Ok(self.status);
        }
        loop {
            match waitid(
                WaitId::PidFd(self.pidfd.as_fd()),
                WaitIdOptions::EXITED | WaitIdOptions::NOHANG,
            ) {
                Ok(Some(st)) => {
                    let s = match st.terminating_signal() {
                        Some(sig) => ExitStatus::Signal(sig),
                        None => ExitStatus::Code(st.exit_status().unwrap_or(0)),
                    };
                    self.status = Some(s);
                    return Ok(Some(s));
                }
                Ok(None) => return Ok(None),
                Err(Errno::INTR) => {}
                Err(e) => return Err(io_err(e)),
            }
        }
    }

    /// Signal the child's whole process group. A vanished group is not an error.
    ///
    /// Only while the leader is unreaped: an exited-but-unreaped leader is a zombie that
    /// still holds its pid, so the pgid cannot have been reused. After the reap the group
    /// id is nobody's to signal and this does nothing.
    pub fn signal_group(&self, sig: Signal) -> io::Result<()> {
        if self.status.is_some() {
            return Ok(());
        }
        match kill_process_group(self.pid, sig) {
            Ok(()) | Err(Errno::SRCH) => Ok(()),
            Err(e) => Err(io_err(e)),
        }
    }

    /// Wait until the leader has exited (pidfd readable) without reaping it, or `timeout`
    /// passes. True if it has exited.
    fn wait_exited(&self, timeout: Duration) -> io::Result<bool> {
        let deadline = Instant::now() + timeout;
        loop {
            if self.status.is_some() {
                return Ok(true);
            }
            let left = deadline.saturating_duration_since(Instant::now());
            let ts = Timespec::try_from(left).unwrap_or(Timespec {
                tv_sec: 1,
                tv_nsec: 0,
            });
            let mut fds = [PollFd::new(&self.pidfd, PollFlags::IN)];
            match poll(&mut fds, Some(&ts)) {
                Ok(_) if !fds[0].revents().is_empty() => return Ok(true),
                Ok(_) | Err(Errno::INTR) => {}
                Err(e) => return Err(io_err(e)),
            }
            if left.is_zero() {
                return Ok(false);
            }
        }
    }

    /// Escalate SIGHUP -> SIGTERM -> SIGKILL on the process group with bounded waits
    /// (T-18 §7, SECURITY-28: 1 s grace per step, bounded post-KILL reap, signals reach
    /// descendants). Blocks up to the sum of the timings, so call it off the main loop.
    /// `Ok(None)` if the child is still unreaped after the post-KILL bound.
    ///
    /// Exit is observed on the pidfd *without reaping*, so the leader stays a zombie while
    /// the final SIGKILL goes to the group: descendants die and the pgid cannot have been
    /// reused. The reap comes last. If the leader was already reaped through
    /// [`Pty::try_wait`], no group signal is sent (see [`Pty::signal_group`]).
    pub fn close(&mut self, t: &CloseTimings) -> io::Result<Option<ExitStatus>> {
        self.queue.clear();
        if self.status.is_none() {
            let mut exited = false;
            for (sig, wait) in [
                (Signal::HUP, t.hup_grace),
                (Signal::TERM, t.term_grace),
                (Signal::KILL, t.kill_reap),
            ] {
                self.signal_group(sig)?;
                if self.wait_exited(wait)? {
                    exited = true;
                    break;
                }
            }
            if exited {
                // Group members outlive the leader; its zombie still pins the pgid.
                self.signal_group(Signal::KILL)?;
            }
            self.try_wait()?;
        }
        Ok(self.status)
    }
}

impl Drop for Pty {
    fn drop(&mut self) {
        // Non-blocking courtesy: closing the master already HUPs the session. If the
        // child is still ours, nudge its group; the owner should have called `close`.
        if self.status.is_none() {
            let _ = kill_process_group(self.pid, Signal::HUP);
            let _ = self.try_wait();
        }
    }
}
