// SPDX-License-Identifier: AGPL-3.0-only
//! Terminal side of the helper (T-16 §4): start `apoth-media-helper` with a socket as its
//! only inherited fd and a clean environment, wait for its sandbox report, and kill it when
//! the handle drops. The helper is found next to the running executable, or at
//! `$APOTH_MEDIA_HELPER` (tests).

use std::io;
use std::os::fd::OwnedFd;
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};

use crate::chan::Chan;
use crate::wire::{Fail, FromHelper};

/// How long a helper may take to report its sandbox.
const START_MS: i32 = 5_000;

/// Which loop the helper runs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    Media,
    Image,
}

/// Where the helper binary is.
pub fn helper_path() -> Option<PathBuf> {
    if let Some(p) = std::env::var_os("APOTH_MEDIA_HELPER") {
        return Some(PathBuf::from(p));
    }
    let exe = std::env::current_exe().ok()?;
    let p = exe.parent()?.join("apoth-media-helper");
    p.is_file().then_some(p)
}

/// The helper process: killed and reaped when dropped.
#[derive(Debug)]
pub struct Proc {
    child: Child,
}

/// A running, sandboxed helper.
#[derive(Debug)]
pub struct Helper {
    pub proc: Proc,
    /// Messages from the helper (and the fds with them).
    pub rx: Chan,
    /// Messages to the helper.
    pub tx: Chan,
}

/// Why a helper is not available.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StartError {
    /// No helper binary, or it could not start.
    Missing,
    /// The helper started but reported this (`sandbox-unavailable`) or broke protocol.
    Failed(Fail),
}

impl Helper {
    /// Start a helper and wait for `Ready`. `test_args` only tighten (sandbox knobs).
    pub fn start(mode: Mode, test_args: &[&str], max_in: usize) -> Result<Helper, StartError> {
        let path = helper_path().ok_or(StartError::Missing)?;
        Helper::start_at(&path, mode, test_args, max_in)
    }

    /// [`Helper::start`] with an explicit binary (tests).
    pub fn start_at(
        path: &std::path::Path,
        mode: Mode,
        test_args: &[&str],
        max_in: usize,
    ) -> Result<Helper, StartError> {
        let (ours, theirs) = UnixStream::pair().map_err(|_| StartError::Missing)?;
        let child = Command::new(path)
            .arg(match mode {
                Mode::Media => "media",
                Mode::Image => "image",
            })
            .args(test_args)
            .env_clear()
            .stdin(Stdio::from(OwnedFd::from(theirs)))
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|_| StartError::Missing)?;
        let rx_stream = ours.try_clone().map_err(|_| StartError::Missing)?;
        let mut h = Helper {
            proc: Proc { child },
            rx: Chan::new(rx_stream, max_in),
            tx: Chan::new(ours, 1 << 10),
        };
        if !h.rx.readable(START_MS) {
            return Err(StartError::Failed(Fail::Protocol));
        }
        match h.rx.recv() {
            Ok(Some((tag, body))) => match FromHelper::decode(tag, &body) {
                Ok(FromHelper::Ready) => Ok(h),
                Ok(FromHelper::Failed(f)) => Err(StartError::Failed(f)),
                _ => Err(StartError::Failed(Fail::Protocol)),
            },
            _ => Err(StartError::Failed(Fail::Protocol)),
        }
    }
}

impl Proc {
    /// The helper's pid (watchdog, tests).
    pub fn pid(&self) -> u32 {
        self.child.id()
    }

    /// Its exit status if it has exited (signal as `128 + n`).
    pub fn try_status(&mut self) -> Option<i32> {
        use std::os::unix::process::ExitStatusExt;
        std::thread::sleep(std::time::Duration::from_millis(50));
        let s = self.child.try_wait().ok()??;
        s.code().or(s.signal().map(|n| 128 + n))
    }

    /// Kill it now (protocol violation, watchdog).
    pub fn kill(&mut self) {
        let _ = self.child.kill();
    }

    /// Wait for it to exit after `Quit`; kill it if it does not within `ms`.
    pub fn finish(mut self, ms: u64) -> io::Result<Option<i32>> {
        let until = std::time::Instant::now() + std::time::Duration::from_millis(ms);
        loop {
            if let Some(s) = self.child.try_wait()? {
                return Ok(s.code());
            }
            if std::time::Instant::now() >= until {
                self.kill();
                return Ok(None);
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
    }
}

impl Drop for Proc {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}
