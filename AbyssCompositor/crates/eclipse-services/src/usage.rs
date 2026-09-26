// SPDX-License-Identifier: AGPL-3.0-only
//! System usage: CPU, memory, GPU and disk, sampled for the taskbar's
//! `system-usage` widget (ADR 0065).
//!
//! Read from `/proc`, `statvfs` and GPU sysfs on one thread at
//! [`UsageConfig::interval`]. A source the machine does not have is `None`
//! and the widget hides it; it is never reported as zero. Same shape as
//! [`crate::status`]: an `mpsc` feed the GUI drains with [`Handle::try_recv`].
//!
//! Skeleton: the thread only keeps the channels alive. The sampling body
//! lands with the usage node of the taskbar-widgets work.

use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver, Sender};
use std::time::Duration;

/// What to sample, and how often. Mirrors `bar.widgets.system-usage.*`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UsageConfig {
    pub interval: Duration,
    /// The filesystem whose fill level `disk` reports.
    pub disk_path: PathBuf,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Update {
    Sample(Sample),
}

/// One reading. Every field is a fraction `0.0..=1.0`; `None` means the
/// source is unavailable on this machine and the widget hides it.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Sample {
    pub cpu: f32,
    pub mem: Option<f32>,
    pub gpu: Option<f32>,
    pub disk: Option<f32>,
}

// Read by the service body, which lands with its own node; `expect` fails
// the build the moment it does, so this cannot outlive the skeleton.
#[expect(dead_code, reason = "skeleton: the service body reads these")]
#[derive(Debug)]
enum Command {
    Reconfigure(UsageConfig),
}

/// The GUI's end of the sampler.
pub struct Handle {
    updates: Receiver<Update>,
    commands: Sender<Command>,
}

impl Handle {
    /// The next sample, if one is waiting. Never blocks.
    pub fn try_recv(&self) -> Option<Update> {
        self.updates.try_recv().ok()
    }

    /// Change the interval or disk path, e.g. after a config reload.
    pub fn reconfigure(&self, cfg: UsageConfig) {
        let _ = self.commands.send(Command::Reconfigure(cfg));
    }
}

/// Start sampling. The thread lives until the [`Handle`] is dropped.
pub fn spawn(cfg: UsageConfig) -> Handle {
    let (updates_tx, updates) = mpsc::channel();
    let (commands, commands_rx) = mpsc::channel();
    let _ = std::thread::Builder::new()
        .name("eclipse-usage".into())
        .spawn(move || run(cfg, updates_tx, commands_rx));
    Handle { updates, commands }
}

fn run(_cfg: UsageConfig, _updates: Sender<Update>, commands: Receiver<Command>) {
    for _command in commands {}
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spawn_starts_quiet() {
        let cfg = UsageConfig {
            interval: Duration::from_secs(1),
            disk_path: PathBuf::from("/"),
        };
        let h = spawn(cfg.clone());
        assert_eq!(h.try_recv(), None);
        h.reconfigure(cfg);
        assert_eq!(h.try_recv(), None);
    }
}
