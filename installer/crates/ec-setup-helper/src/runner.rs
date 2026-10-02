// SPDX-License-Identifier: AGPL-3.0-only
//! Every external program the helper can run, and the only way to run one.
//!
//! No shell, ever. A [`Tool`] is a closed enum resolving to an absolute path;
//! arguments are a `Vec<OsString>` built by the caller from validated values;
//! there is no string interpolation into a command line anywhere. The trait
//! exists so tests can assert which commands were (not) issued.

use crate::error::{Error, Result};
use std::ffi::{OsStr, OsString};
use std::fmt;
use std::io::Write;
use std::process::{Command, Stdio};
use zeroize::Zeroizing;

/// The complete list of programs the helper may execute.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tool {
    Lsblk,
    Curl,
    Gpg,
    PacmanKey,
    Sgdisk,
    Partprobe,
    Udevadm,
    MkfsFat,
    MkfsExt4,
    Mount,
    Umount,
    Pacstrap,
    Genfstab,
    ArchChroot,
    Blkid,
    Pacman,
    Wipefs,
    Blkdiscard,
    Efibootmgr,
}

impl Tool {
    pub const fn path(self) -> &'static str {
        match self {
            Tool::Lsblk => "/usr/bin/lsblk",
            Tool::Curl => "/usr/bin/curl",
            Tool::Gpg => "/usr/bin/gpg",
            Tool::PacmanKey => "/usr/bin/pacman-key",
            Tool::Sgdisk => "/usr/bin/sgdisk",
            Tool::Partprobe => "/usr/bin/partprobe",
            Tool::Udevadm => "/usr/bin/udevadm",
            Tool::MkfsFat => "/usr/bin/mkfs.fat",
            Tool::MkfsExt4 => "/usr/bin/mkfs.ext4",
            Tool::Mount => "/usr/bin/mount",
            Tool::Umount => "/usr/bin/umount",
            Tool::Pacstrap => "/usr/bin/pacstrap",
            Tool::Genfstab => "/usr/bin/genfstab",
            Tool::ArchChroot => "/usr/bin/arch-chroot",
            Tool::Blkid => "/usr/bin/blkid",
            Tool::Pacman => "/usr/bin/pacman",
            Tool::Wipefs => "/usr/bin/wipefs",
            Tool::Blkdiscard => "/usr/bin/blkdiscard",
            Tool::Efibootmgr => "/usr/bin/efibootmgr",
        }
    }

    pub const fn name(self) -> &'static str {
        match self {
            Tool::Lsblk => "lsblk",
            Tool::Curl => "curl",
            Tool::Gpg => "gpg",
            Tool::PacmanKey => "pacman-key",
            Tool::Sgdisk => "sgdisk",
            Tool::Partprobe => "partprobe",
            Tool::Udevadm => "udevadm",
            Tool::MkfsFat => "mkfs.fat",
            Tool::MkfsExt4 => "mkfs.ext4",
            Tool::Mount => "mount",
            Tool::Umount => "umount",
            Tool::Pacstrap => "pacstrap",
            Tool::Genfstab => "genfstab",
            Tool::ArchChroot => "arch-chroot",
            Tool::Blkid => "blkid",
            Tool::Pacman => "pacman",
            Tool::Wipefs => "wipefs",
            Tool::Blkdiscard => "blkdiscard",
            Tool::Efibootmgr => "efibootmgr",
        }
    }

    /// Tools whose use means the disk (or the firmware's boot entries) is being
    /// changed. Nothing here may run before `Confirm` has allowed. (`Genfstab`
    /// and `ArchChroot` only make sense on an already-mounted target. `Pacman`
    /// is not here: preflight runs it against the live medium's own root.)
    pub const fn touches_disk(self) -> bool {
        matches!(
            self,
            Tool::Sgdisk
                | Tool::Wipefs
                | Tool::Blkdiscard
                | Tool::Efibootmgr
                | Tool::Partprobe
                | Tool::MkfsFat
                | Tool::MkfsExt4
                | Tool::Mount
                | Tool::Umount
                | Tool::Pacstrap
                | Tool::Genfstab
                | Tool::ArchChroot
        )
    }
}

pub struct Cmd {
    pub tool: Tool,
    pub args: Vec<OsString>,
    /// Fed to the child's stdin, then closed. The only channel for the password.
    pub stdin: Option<Zeroizing<Vec<u8>>>,
    /// Discard the child's stderr instead of passing it on. Set for anything
    /// that handles the password.
    pub silence_stderr: bool,
}

impl Cmd {
    pub fn new(tool: Tool) -> Self {
        Cmd {
            tool,
            args: Vec::new(),
            stdin: None,
            silence_stderr: false,
        }
    }

    pub fn arg(mut self, a: impl AsRef<OsStr>) -> Self {
        self.args.push(a.as_ref().to_owned());
        self
    }

    pub fn args<I, S>(mut self, it: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: AsRef<OsStr>,
    {
        self.args.extend(it.into_iter().map(|a| a.as_ref().to_owned()));
        self
    }

    pub fn stdin(mut self, data: Zeroizing<Vec<u8>>) -> Self {
        self.stdin = Some(data);
        self.silence_stderr = true;
        self
    }
}

/// Hand-written so a stray `{:?}` can never print the stdin payload.
impl fmt::Debug for Cmd {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Cmd")
            .field("tool", &self.tool.name())
            .field("args", &self.args)
            .field("has_stdin", &self.stdin.is_some())
            .finish()
    }
}

pub trait Runner {
    /// Run to completion; stdout is discarded. Non-zero exit is an error.
    fn run(&self, cmd: &Cmd) -> Result<()>;
    /// Run to completion and return stdout.
    fn capture(&self, cmd: &Cmd) -> Result<Vec<u8>>;
}

/// stdout of a captured command is bounded: lsblk on a big machine is tens of KiB.
const MAX_CAPTURE: usize = 4 * 1024 * 1024;

/// Runs the real programs with a scrubbed environment: nothing the caller put in
/// its own environment reaches a child.
pub struct SysRunner;

impl SysRunner {
    fn exec(cmd: &Cmd, capture: bool) -> Result<Vec<u8>> {
        let tool = cmd.tool.name();
        let fail = |code| Error::Command { tool, code };
        let mut c = Command::new(cmd.tool.path());
        c.args(&cmd.args)
            .env_clear()
            .env("PATH", "/usr/bin")
            .env("LC_ALL", "C")
            .stdin(if cmd.stdin.is_some() {
                Stdio::piped()
            } else {
                Stdio::null()
            })
            .stdout(if capture { Stdio::piped() } else { Stdio::null() })
            .stderr(if cmd.silence_stderr {
                Stdio::null()
            } else {
                Stdio::inherit()
            });
        let mut child = c.spawn().map_err(|_| fail(None))?;
        if let (Some(data), Some(mut si)) = (&cmd.stdin, child.stdin.take()) {
            // A child that exits early makes this fail; its exit status decides.
            let _ = si.write_all(data);
        }
        let out = child.wait_with_output().map_err(|_| fail(None))?;
        if !out.status.success() {
            return Err(fail(out.status.code()));
        }
        if out.stdout.len() > MAX_CAPTURE {
            return Err(Error::Io("command output too large"));
        }
        Ok(out.stdout)
    }
}

impl Runner for SysRunner {
    fn run(&self, cmd: &Cmd) -> Result<()> {
        Self::exec(cmd, false).map(|_| ())
    }
    fn capture(&self, cmd: &Cmd) -> Result<Vec<u8>> {
        Self::exec(cmd, true)
    }
}
