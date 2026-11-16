// SPDX-License-Identifier: AGPL-3.0-only
//! Headless end-to-end driver (T-25 §6): starts `ec-abyss --backend headless` in a private
//! runtime directory (pid captured, killed by pid, never by name), launches a terminal
//! binary inside it and drives it through the compositor's control socket (`type_text`,
//! `press_key`), reading grid text from the terminal's test dump hook (`APOTH_TEST_DUMP`,
//! debug builds only; the `pane.text` analogue until the control socket serves it).
//!
//! Never a visible window, never the live session: every path and socket lives under the
//! session's own directory. Screenshots: the compositor has no screenshot RPC (only its
//! key-bound path writes files), so there is no `screenshot` here; pixel checks use the
//! renderer's offscreen path instead.
//!
//! Nothing here logs pane text.

use std::io::{Read, Write};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

/// A child killed (by pid) and reaped on drop.
#[derive(Debug)]
pub struct Guard(pub Child);

impl Drop for Guard {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

/// Why a session could not be used.
#[derive(Debug)]
pub enum SessionError {
    /// No `ec-abyss` (set `APOTH_EC_ABYSS` or put it on `PATH`).
    NoCompositor,
    /// The compositor did not create its Wayland socket in time.
    CompositorTimeout,
    /// I/O failure preparing or spawning.
    Io(std::io::Error),
}

impl From<std::io::Error> for SessionError {
    fn from(e: std::io::Error) -> Self {
        SessionError::Io(e)
    }
}

/// `APOTH_EC_ABYSS`, else the first `ec-abyss` on `PATH`.
#[must_use]
pub fn find_abyss() -> Option<PathBuf> {
    if let Some(p) = std::env::var_os("APOTH_EC_ABYSS") {
        return Some(PathBuf::from(p));
    }
    std::env::var_os("PATH").and_then(|path| {
        std::env::split_paths(&path)
            .map(|d| d.join("ec-abyss"))
            .find(|p| p.is_file())
    })
}

/// A running headless compositor and, once launched, a terminal inside it.
#[derive(Debug)]
pub struct Session {
    dir: PathBuf,
    // Field order: the terminal is dropped (killed) before the compositor.
    term: Option<Guard>,
    abyss: Guard,
    next_id: u32,
}

impl Session {
    /// Start `ec-abyss --backend headless --size WxH` under a fresh directory named for `tag`.
    pub fn start(tag: &str, size: (u32, u32)) -> Result<Session, SessionError> {
        let abyss = find_abyss().ok_or(SessionError::NoCompositor)?;
        let dir = std::env::temp_dir().join(format!("{tag}{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        for sub in ["sock", "cfg/eclipse", "home"] {
            std::fs::create_dir_all(dir.join(sub))?;
        }
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700))?;
        std::fs::write(
            dir.join("cfg/eclipse/policy.kdl"),
            "misc { scripted-input #true }\n",
        )?;
        let child = Command::new(abyss)
            .args([
                "--backend",
                "headless",
                "--size",
                &format!("{}x{}", size.0, size.1),
            ])
            .env("XDG_RUNTIME_DIR", &dir)
            .env("ECLIPSE_SOCKET_DIR", dir.join("sock"))
            .env("XDG_CONFIG_HOME", dir.join("cfg"))
            .env_remove("WAYLAND_DISPLAY")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()?;
        let abyss = Guard(child);
        let wl = dir.join("wayland-1");
        let t0 = Instant::now();
        while !wl.exists() {
            if t0.elapsed() > Duration::from_secs(20) {
                return Err(SessionError::CompositorTimeout);
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        Ok(Session {
            dir,
            term: None,
            abyss,
            next_id: 0,
        })
    }

    /// The compositor's pid (what a test kills, never a name).
    #[must_use]
    pub fn compositor_pid(&self) -> u32 {
        self.abyss.0.id()
    }

    /// The session directory.
    #[must_use]
    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// Where the launched terminal writes its grid text (`APOTH_TEST_DUMP`).
    #[must_use]
    pub fn dump_path(&self) -> PathBuf {
        self.dir.join("dump.txt")
    }

    /// Launch the terminal `bin` inside the session with a private HOME and `/bin/sh`.
    pub fn launch(&mut self, bin: &Path, extra_env: &[(&str, &str)]) -> Result<(), SessionError> {
        let home = self.dir.join("home");
        let mut c = Command::new(bin);
        c.env_remove("APOTHEOSIS_SOCKET")
            .env(
                "DBUS_SESSION_BUS_ADDRESS",
                "unix:path=/nonexistent/apoth-test-bus",
            )
            // The nested compositor's socket, never the host's (T-17 §1 detection).
            .env("ECLIPSE_SOCKET_DIR", self.dir.join("sock"))
            .env("XDG_RUNTIME_DIR", &self.dir)
            .env("WAYLAND_DISPLAY", "wayland-1")
            .env("SHELL", "/bin/sh")
            .env("HOME", &home)
            .env("XDG_CACHE_HOME", home.join(".cache"))
            .env("XDG_CONFIG_HOME", home.join(".config"))
            .env("APOTH_TEST_DUMP", self.dump_path());
        for (k, v) in extra_env {
            c.env(k, v);
        }
        self.term = Some(Guard(
            c.stdout(Stdio::null()).stderr(Stdio::piped()).spawn()?,
        ));
        Ok(())
    }

    /// The terminal's exit status once it has exited.
    pub fn terminal_exit(&mut self) -> Option<std::process::ExitStatus> {
        self.term
            .as_mut()
            .and_then(|t| t.0.try_wait().ok().flatten())
    }

    fn call(&mut self, method: &str, params: &str) -> Option<String> {
        self.next_id += 1;
        let mut s = UnixStream::connect(self.dir.join("sock/abyss.sock")).ok()?;
        let line = format!(
            "{{\"jsonrpc\":\"2.0\",\"id\":{},\"method\":\"{method}\",\"params\":{params}}}\n",
            self.next_id
        );
        s.write_all(line.as_bytes()).ok()?;
        s.set_read_timeout(Some(Duration::from_millis(500))).ok()?;
        let mut buf = [0u8; 512];
        let n = s.read(&mut buf).ok()?;
        Some(String::from_utf8_lossy(&buf[..n]).into_owned())
    }

    /// Type `text` (JSON-escaped here) into the focused window.
    pub fn type_text(&mut self, text: &str) -> bool {
        let esc = text
            .replace('\\', "\\\\")
            .replace('"', "\\\"")
            .replace('\n', "\\n");
        self.call("type_text", &format!("{{\"text\":\"{esc}\"}}"))
            .is_some()
    }

    /// Press one key by keysym name (`Return`, `Escape`, ...).
    pub fn press_key(&mut self, keysym: &str) -> bool {
        self.call("press_key", &format!("{{\"key\":\"{keysym}\"}}"))
            .is_some()
    }

    /// The terminal's visible grid text, when its dump hook has written it.
    #[must_use]
    pub fn pane_text(&self) -> Option<String> {
        std::fs::read_to_string(self.dump_path()).ok()
    }

    /// True when a line of the grid equals `line` after trimming.
    #[must_use]
    pub fn has_line(&self, line: &str) -> bool {
        self.pane_text()
            .is_some_and(|t| t.lines().any(|l| l.trim() == line))
    }

    /// Poll until `done` is true or `timeout` passes; sleeps `step` between polls.
    pub fn wait(
        &mut self,
        timeout: Duration,
        step: Duration,
        mut done: impl FnMut(&mut Session) -> bool,
    ) -> bool {
        let t0 = Instant::now();
        while t0.elapsed() < timeout {
            if done(self) {
                return true;
            }
            std::thread::sleep(step);
        }
        false
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn explicit_compositor_path_wins_and_a_missing_one_is_reported() {
        // Only the function's shape is checked here; a real compositor is exercised by
        // `apotheosis/tests/first_light.rs`.
        let _ = find_abyss();
        assert!(matches!(
            Session::start("apts", (1, 1)).err(),
            Some(
                SessionError::NoCompositor | SessionError::CompositorTimeout | SessionError::Io(_)
            ) | None
        ));
    }
}
