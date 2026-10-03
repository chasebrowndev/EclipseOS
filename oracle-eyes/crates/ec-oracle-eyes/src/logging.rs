// SPDX-License-Identifier: AGPL-3.0-only

//! Logging setup. Normal mode is `info` on stderr (journal), `RUST_LOG`
//! honoured. Debug mode adds a `trace` copy in
//! `$XDG_STATE_HOME/oracle-eyes/debug.log`, owner-only and truncated each
//! run. Screen-derived text reaches either sink only through
//! [`crate::logsafe::log_safe`].

use std::fs::{File, OpenOptions};
use std::io::Write;
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use tracing_subscriber::fmt::MakeWriter;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;
use tracing_subscriber::{EnvFilter, Layer};

/// Held for the life of `main`; flushes the debug log to disk on drop.
pub struct Guard {
    file: Option<Arc<Mutex<File>>>,
}

impl Drop for Guard {
    fn drop(&mut self) {
        if let Some(f) = &self.file {
            let mut f = f.lock().unwrap_or_else(|p| p.into_inner());
            let _ = f.flush();
            let _ = f.sync_all();
        }
    }
}

#[derive(Clone)]
struct FileWriter(Arc<Mutex<File>>);

struct Locked<'a>(std::sync::MutexGuard<'a, File>);

impl Write for Locked<'_> {
    fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
        self.0.write(b)
    }
    fn flush(&mut self) -> std::io::Result<()> {
        self.0.flush()
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

    let mut file = None;
    let mut note = None;
    if debug {
        match debug_log_path() {
            None => note = Some("debug: no HOME or XDG_STATE_HOME, debug.log disabled".to_string()),
            Some(path) => match open_log(&path) {
                Ok(f) => file = Some((Arc::new(Mutex::new(f)), path)),
                Err(e) => {
                    note = Some(format!(
                        "debug: {}: {e}, debug.log disabled",
                        path.display()
                    ))
                }
            },
        }
    }
    let file_layer = file.as_ref().map(|(f, _)| {
        tracing_subscriber::fmt::layer()
            .with_writer(FileWriter(Arc::clone(f)))
            .with_ansi(false)
            .with_filter(EnvFilter::new("trace"))
    });
    tracing_subscriber::registry()
        .with(stderr)
        .with(file_layer)
        .init();

    if debug {
        tracing::info!("debug mode on");
    }
    if let Some((_, path)) = &file {
        tracing::info!(path = %path.display(), "debug log (trace level, truncated each run)");
    }
    if let Some(n) = note {
        tracing::warn!("{n}");
    }
    Guard {
        file: file.map(|(f, _)| f),
    }
}
