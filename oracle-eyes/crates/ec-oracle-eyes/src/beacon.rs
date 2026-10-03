// SPDX-License-Identifier: AGPL-3.0-only

//! The status beacon (ADR 0055): one word per line on a socket the taskbar
//! reads, so its eclipse mark can open into an eye while automatic mode is
//! watching and pinpoint while the model is thinking.
//!
//! Output only. The server never reads from a client, and nothing that came
//! off the screen — no OCR text, no reply, no geometry — ever crosses it: the
//! whole vocabulary is [`Eye`]. A reader that loses the connection must treat
//! that as `off`, so a crashed daemon never leaves an eye staring.
//!
//! In debug mode (`--debug` / `OE_DEBUG=1`) every line carries a leading
//! `debug ` token (`debug watch`), which the taskbar shows as a red eye. The
//! vocabulary after it is unchanged and still carries nothing screen-derived.
//!
//! The eye is decorative. A beacon that cannot bind or write says so in the
//! journal once and the daemon carries on without it.

use std::io::Write;
use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Eye {
    /// Automatic mode off and nothing in flight: the plain eclipse.
    Off,
    /// Automatic mode on, between passes.
    Watch,
    /// A model call is in flight, whichever mode asked for it.
    Think,
}

impl Eye {
    pub fn word(self) -> &'static str {
        match self {
            Eye::Off => "off",
            Eye::Watch => "watch",
            Eye::Think => "think",
        }
    }
}

struct Shared {
    eye: Eye,
    debug: bool,
    clients: Vec<UnixStream>,
}

pub struct Beacon {
    shared: Option<Arc<Mutex<Shared>>>,
}

fn lock(m: &Mutex<Shared>) -> MutexGuard<'_, Shared> {
    m.lock().unwrap_or_else(|p| p.into_inner())
}

/// Write one state line; `false` means the client is gone or not reading,
/// and it is dropped rather than allowed to stall the daemon.
fn tell(c: &mut UnixStream, eye: Eye, debug: bool) -> bool {
    c.write_all(wire(eye, debug).as_bytes()).is_ok()
}

/// One wire line, newline included.
fn wire(eye: Eye, debug: bool) -> String {
    if debug {
        format!("debug {}\n", eye.word())
    } else {
        format!("{}\n", eye.word())
    }
}

impl Beacon {
    /// `$XDG_RUNTIME_DIR/oracle-eyes/eye.sock`, or a disabled beacon when
    /// there is no runtime dir or the bind fails.
    pub fn bind(debug: bool) -> Beacon {
        let Some(dir) = std::env::var_os("XDG_RUNTIME_DIR") else {
            tracing::warn!("beacon: XDG_RUNTIME_DIR unset, taskbar eye disabled");
            return Beacon { shared: None };
        };
        let path = PathBuf::from(dir).join("oracle-eyes").join("eye.sock");
        match Beacon::at(&path, debug) {
            Ok(b) => {
                let prefixed = debug;
                tracing::debug!(path = %path.display(), prefixed = prefixed, "beacon: listening");
                b
            }
            Err(e) => {
                tracing::warn!("beacon: {}: {e}, taskbar eye disabled", path.display());
                Beacon { shared: None }
            }
        }
    }

    /// A beacon that tells nobody, for tests: binding the real path would
    /// unlink the socket of a daemon running in the same session.
    #[cfg(test)]
    pub fn disabled() -> Beacon {
        Beacon { shared: None }
    }

    pub fn at(path: &Path, debug: bool) -> std::io::Result<Beacon> {
        if let Some(dir) = path.parent() {
            std::fs::DirBuilder::new()
                .recursive(true)
                .mode(0o700)
                .create(dir)?;
        }
        // A socket left by a previous run refuses the bind; it is ours.
        match std::fs::remove_file(path) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => return Err(e),
            _ => {}
        }
        let listener = UnixListener::bind(path)?;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
        let shared = Arc::new(Mutex::new(Shared {
            eye: Eye::Off,
            debug,
            clients: Vec::new(),
        }));
        let accept = Arc::clone(&shared);
        std::thread::spawn(move || {
            for conn in listener.incoming() {
                let Ok(mut c) = conn else { continue };
                if c.set_nonblocking(true).is_err() {
                    continue;
                }
                let mut s = lock(&accept);
                // A late joiner is told where things stand, not left
                // waiting for the next transition.
                if tell(&mut c, s.eye, s.debug) {
                    s.clients.push(c);
                    tracing::debug!(
                        clients = s.clients.len(),
                        state = s.eye.word(),
                        "beacon: subscriber joined"
                    );
                }
            }
        });
        Ok(Beacon {
            shared: Some(shared),
        })
    }

    pub fn set(&mut self, eye: Eye) {
        let Some(shared) = &self.shared else { return };
        let mut s = lock(shared);
        if s.eye == eye {
            return;
        }
        tracing::debug!(
            from = s.eye.word(),
            to = eye.word(),
            clients = s.clients.len(),
            "beacon: transition"
        );
        s.eye = eye;
        let debug = s.debug;
        s.clients.retain_mut(|c| tell(c, eye, debug));
    }

    /// Debug toggled live (Settings): every client is re-told the current
    /// state with or without the prefix, so the taskbar's eye turns red or
    /// back at once rather than at the next transition.
    pub fn set_debug(&mut self, debug: bool) {
        let Some(shared) = &self.shared else { return };
        let mut s = lock(shared);
        if s.debug == debug {
            return;
        }
        s.debug = debug;
        let eye = s.eye;
        s.clients.retain_mut(|c| tell(c, eye, debug));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{BufRead, BufReader};
    use std::time::Duration;

    fn sock(name: &str) -> PathBuf {
        std::env::temp_dir()
            .join(format!("oracle-eyes-beacon-{}-{name}", std::process::id()))
            .join("eye.sock")
    }

    fn join(path: &Path) -> BufReader<UnixStream> {
        let c = UnixStream::connect(path).unwrap();
        c.set_read_timeout(Some(Duration::from_secs(2))).unwrap();
        BufReader::new(c)
    }

    fn next(r: &mut BufReader<UnixStream>) -> String {
        let mut line = String::new();
        r.read_line(&mut line).unwrap();
        line.trim_end().to_string()
    }

    /// The accept thread registers a client after telling it the current
    /// state; reading that line is the sign it is registered.
    #[test]
    fn a_late_joiner_is_told_the_current_state() {
        let path = sock("late");
        let mut b = Beacon::at(&path, false).unwrap();
        b.set(Eye::Watch);
        let mut r = join(&path);
        assert_eq!(next(&mut r), "watch");
    }

    #[test]
    fn transitions_reach_a_client_and_repeats_are_not_sent() {
        let path = sock("repeat");
        let mut b = Beacon::at(&path, false).unwrap();
        let mut r = join(&path);
        assert_eq!(next(&mut r), "off");
        b.set(Eye::Think);
        b.set(Eye::Think);
        b.set(Eye::Off);
        assert_eq!(next(&mut r), "think");
        assert_eq!(next(&mut r), "off");
    }

    #[test]
    fn debug_lines_carry_the_prefix_including_the_idle_greeting() {
        let path = sock("debug");
        let mut b = Beacon::at(&path, true).unwrap();
        let mut r = join(&path);
        assert_eq!(next(&mut r), "debug off");
        b.set(Eye::Watch);
        b.set(Eye::Think);
        assert_eq!(next(&mut r), "debug watch");
        assert_eq!(next(&mut r), "debug think");
    }

    #[test]
    fn a_live_debug_toggle_re_tells_the_state_with_the_new_prefix() {
        let path = sock("toggle");
        let mut b = Beacon::at(&path, false).unwrap();
        let mut r = join(&path);
        assert_eq!(next(&mut r), "off");
        b.set(Eye::Watch);
        b.set_debug(true);
        b.set_debug(true);
        b.set_debug(false);
        assert_eq!(next(&mut r), "watch");
        assert_eq!(next(&mut r), "debug watch");
        assert_eq!(next(&mut r), "watch");
    }

    #[test]
    fn wire_format_is_unchanged_without_debug() {
        assert_eq!(wire(Eye::Think, false), "think\n");
        assert_eq!(wire(Eye::Think, true), "debug think\n");
    }

    #[test]
    fn the_socket_is_owner_only() {
        let path = sock("mode");
        let _b = Beacon::at(&path, false).unwrap();
        let mode = std::fs::metadata(&path).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600);
    }

    #[test]
    fn words_are_the_whole_vocabulary() {
        assert_eq!(
            [Eye::Off, Eye::Watch, Eye::Think].map(Eye::word),
            ["off", "watch", "think"]
        );
    }

    #[test]
    fn a_disabled_beacon_ignores_sets() {
        Beacon { shared: None }.set(Eye::Think);
    }
}
