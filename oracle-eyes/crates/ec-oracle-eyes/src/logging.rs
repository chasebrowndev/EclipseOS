// SPDX-License-Identifier: AGPL-3.0-only

//! Logging setup. Normal mode is `info` on stderr (journal), `RUST_LOG`
//! honoured. Debug mode adds a `trace` copy in
//! `$XDG_STATE_HOME/oracle-eyes/debug.log`, owner-only, truncated the first
//! time it is opened in a run. Debug can be switched on and off while the
//! daemon runs (Settings writes `oracle-eyes.debug`), so the file sink is a
//! switch rather than a layer installed once. Screen-derived text reaches
//! either sink only through [`crate::logsafe::log_safe`].

use std::fs::{File, OpenOptions};
use std::io::Write;
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use tracing_subscriber::filter::filter_fn;
use tracing_subscriber::fmt::MakeWriter;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;
use tracing_subscriber::{EnvFilter, Layer};

type Sink = Arc<Mutex<Option<File>>>;

/// Held for the life of `main`: flips debug mode, and flushes the debug log
/// to disk on drop.
pub struct Guard {
    file: Sink,
    on: Arc<AtomicBool>,
}

impl Drop for Guard {
    fn drop(&mut self) {
        let mut f = self.file.lock().unwrap_or_else(|p| p.into_inner());
        if let Some(f) = f.as_mut() {
            let _ = f.flush();
            let _ = f.sync_all();
        }
    }
}

impl Guard {
    /// Debug on or off, now. The file is opened (and truncated) the first
    /// time debug comes on and kept for the rest of the run, so toggling it
    /// off and on again appends rather than losing what was logged.
    pub fn set_debug(&self, debug: bool) {
        if self.on.load(Ordering::Relaxed) == debug {
            return;
        }
        if !debug {
            tracing::info!("debug mode off");
            self.on.store(false, Ordering::Relaxed);
            let mut f = self.file.lock().unwrap_or_else(|p| p.into_inner());
            if let Some(f) = f.as_mut() {
                let _ = f.flush();
            }
            return;
        }
        let (mut note, mut warn) = (None, None);
        {
            let mut f = self.file.lock().unwrap_or_else(|p| p.into_inner());
            if f.is_none() {
                match debug_log_path() {
                    None => {
                        warn =
                            Some("debug: no HOME or XDG_STATE_HOME, debug.log disabled".to_string())
                    }
                    Some(path) => match open_log(&path) {
                        Ok(file) => {
                            *f = Some(file);
                            note = Some(format!(
                                "debug log at {} (trace level, truncated each run)",
                                path.display()
                            ));
                        }
                        Err(e) => {
                            warn = Some(format!(
                                "debug: {}: {e}, debug.log disabled",
                                path.display()
                            ))
                        }
                    },
                }
            }
        }
        self.on.store(true, Ordering::Relaxed);
        tracing::info!("debug mode on");
        if let Some(n) = note {
            tracing::info!("{n}");
        }
        if let Some(w) = warn {
            tracing::warn!("{w}");
        }
    }
}

/// Writes to the debug file while there is one, and nowhere otherwise.
#[derive(Clone)]
struct FileWriter(Sink);

struct Locked<'a>(std::sync::MutexGuard<'a, Option<File>>);

impl Write for Locked<'_> {
    fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
        match self.0.as_mut() {
            Some(f) => f.write(b),
            None => Ok(b.len()),
        }
    }
    fn flush(&mut self) -> std::io::Result<()> {
        match self.0.as_mut() {
            Some(f) => f.flush(),
            None => Ok(()),
        }
    }
}

impl<'a> MakeWriter<'a> for FileWriter {
    type Writer = Locked<'a>;
    fn make_writer(&'a self) -> Locked<'a> {
        Locked(self.0.lock().unwrap_or_else(|p| p.into_inner()))
    }
}

/// `$XDG_STATE_HOME/oracle-eyes/debug.log`, falling back to
/// `~/.local/state`.
pub fn debug_log_path() -> Option<PathBuf> {
    let base = std::env::var_os("XDG_STATE_HOME")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .or_else(|| {
            std::env::var_os("HOME")
                .filter(|v| !v.is_empty())
                .map(|h| PathBuf::from(h).join(".local/state"))
        })?;
    Some(base.join("oracle-eyes").join("debug.log"))
}

fn open_log(path: &PathBuf) -> std::io::Result<File> {
    if let Some(dir) = path.parent() {
        std::fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(dir)?;
    }
    let f = OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(path)?;
    // `mode` only applies on creation; a pre-existing file keeps its own.
    f.set_permissions(std::fs::Permissions::from_mode(0o600))?;
    Ok(f)
}

pub fn init(debug: bool) -> Guard {
    let stderr_filter =
        EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
    let stderr = tracing_subscriber::fmt::layer()
        .with_writer(std::io::stderr)
        .with_target(false)
        .with_ansi(std::io::IsTerminal::is_terminal(&std::io::stderr()))
        .with_filter(stderr_filter);

    // Always installed, gated by `on`: a layer cannot be added to the
    // global subscriber after `init`, and a filter that answers per event
    // (rather than per callsite) is what lets the switch take effect at once.
    let file: Sink = Arc::new(Mutex::new(None));
    let on = Arc::new(AtomicBool::new(false));
    let gate = Arc::clone(&on);
    let file_layer = tracing_subscriber::fmt::layer()
        .with_writer(FileWriter(Arc::clone(&file)))
        .with_ansi(false)
        .with_filter(filter_fn(move |_| gate.load(Ordering::Relaxed)));
    tracing_subscriber::registry()
        .with(stderr)
        .with(file_layer)
        .init();

    let guard = Guard { file, on };
    guard.set_debug(debug);
    guard
}
