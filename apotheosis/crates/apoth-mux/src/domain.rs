// SPDX-License-Identifier: AGPL-3.0-only
//! The Domain and Pane traits (T-13 §2, MUX-02). Signatures only: R1 ships `LocalDomain`
//! (on `apoth-pty`, a later milestone); `UnixDomain`, `SshDomain` and `ExecDomain` are R2.
//!
//! The model ([`crate::Session`]) is generic over a per-pane payload; a domain's
//! [`Domain::Pane`] is what a caller stores there. Everything is synchronous and
//! non-blocking by contract: a domain never blocks the main thread, it queues.

use crate::geom::Size;
use crate::session::PaneSpec;
use crate::tree::PaneId;

/// Domain failures. Carries no content (no argv, no paths).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DomainError {
    /// The domain does not know that pane.
    NoSuchPane,
    /// The pane's process or connection is gone.
    Gone,
    /// The domain cannot do this (for example `attach` on a local domain).
    Unsupported,
    /// The spawn or transport failed; the reason is logged by the domain, not carried.
    Failed,
}

impl std::fmt::Display for DomainError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            DomainError::NoSuchPane => "no such pane in domain",
            DomainError::Gone => "pane is gone",
            DomainError::Unsupported => "unsupported by this domain",
            DomainError::Failed => "domain operation failed",
        })
    }
}

impl std::error::Error for DomainError {}

/// What to start.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SpawnSpec {
    /// cwd, argv, profile.
    pub spec: PaneSpec,
    /// Initial size in cells.
    pub size: Size,
}

/// Facts about a pane a domain hosts.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PaneInfo {
    /// The pane id.
    pub id: PaneId,
    /// Current size.
    pub size: Size,
    /// False once the process exited.
    pub alive: bool,
    /// Exit status when it has exited.
    pub exit_status: Option<i32>,
    /// Last known working directory.
    pub cwd: Option<String>,
}

/// A pane as a domain hosts it: the write side plus a handle for the feed side.
pub trait Pane {
    /// The feed-side handle: how `apoth-term` receives this pane's output (a channel
    /// receiver locally, a stream remotely). Opaque to the model.
    type Feed;

    /// The pane id.
    fn id(&self) -> PaneId;
    /// The feed-side handle. Callable once per attachment.
    fn feed_handle(&mut self) -> Option<Self::Feed>;
    /// Queue bytes for the pane's input. Never logged.
    fn write(&mut self, bytes: &[u8]) -> Result<(), DomainError>;
    /// Tell the pane its new size.
    fn resize(&mut self, size: Size) -> Result<(), DomainError>;
    /// Current facts.
    fn info(&self) -> PaneInfo;
}

/// A place panes live.
pub trait Domain {
    /// The pane type this domain hands out.
    type Pane: Pane;

    /// Domain name (`local`, a remote's name).
    fn name(&self) -> &str;
    /// Start a pane.
    fn spawn(&mut self, spec: &SpawnSpec) -> Result<Self::Pane, DomainError>;
    /// Start a pane that inherits cwd (and, remotely, the connection) from `parent`.
    fn split(&mut self, parent: PaneId, spec: &SpawnSpec) -> Result<Self::Pane, DomainError>;
    /// Attach to an existing pane (reconnect after a detach).
    fn attach(&mut self, id: PaneId) -> Result<Self::Pane, DomainError>;
    /// Detach without killing the process.
    fn detach(&mut self, id: PaneId) -> Result<(), DomainError>;
    /// All panes the domain hosts.
    fn list(&self) -> Vec<PaneInfo>;
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    struct MemPane {
        info: PaneInfo,
        written: Vec<u8>,
        feed: Option<u32>,
    }

    impl Pane for MemPane {
        type Feed = u32;
        fn id(&self) -> PaneId {
            self.info.id
        }
        fn feed_handle(&mut self) -> Option<u32> {
            self.feed.take()
        }
        fn write(&mut self, b: &[u8]) -> Result<(), DomainError> {
            if !self.info.alive {
                return Err(DomainError::Gone);
            }
            self.written.extend_from_slice(b);
            Ok(())
        }
        fn resize(&mut self, size: Size) -> Result<(), DomainError> {
            self.info.size = size;
            Ok(())
        }
        fn info(&self) -> PaneInfo {
            self.info.clone()
        }
    }

    #[derive(Default)]
    struct MemDomain {
        next: u64,
        infos: BTreeMap<PaneId, PaneInfo>,
    }

    impl MemDomain {
        fn make(&mut self, spec: &SpawnSpec) -> MemPane {
            self.next += 1;
            let info = PaneInfo {
                id: self.next,
                size: spec.size,
                alive: true,
                exit_status: None,
                cwd: spec.spec.cwd.clone(),
            };
            self.infos.insert(info.id, info.clone());
            MemPane {
                info,
                written: vec![],
                feed: Some(self.next as u32),
            }
        }
    }

    impl Domain for MemDomain {
        type Pane = MemPane;
        fn name(&self) -> &str {
            "mem"
        }
        fn spawn(&mut self, s: &SpawnSpec) -> Result<MemPane, DomainError> {
            Ok(self.make(s))
        }
        fn split(&mut self, parent: PaneId, s: &SpawnSpec) -> Result<MemPane, DomainError> {
            let cwd = self
                .infos
                .get(&parent)
                .ok_or(DomainError::NoSuchPane)?
                .cwd
                .clone();
            let mut s = s.clone();
            s.spec.cwd = s.spec.cwd.or(cwd);
            Ok(self.make(&s))
        }
        fn attach(&mut self, _: PaneId) -> Result<MemPane, DomainError> {
            Err(DomainError::Unsupported)
        }
        fn detach(&mut self, id: PaneId) -> Result<(), DomainError> {
            self.infos
                .remove(&id)
                .map(|_| ())
                .ok_or(DomainError::NoSuchPane)
        }
        fn list(&self) -> Vec<PaneInfo> {
            self.infos.values().cloned().collect()
        }
    }

    #[test]
    fn domain_contract_is_usable() {
        let mut d = MemDomain::default();
        let size = Size { cols: 80, rows: 24 };
        let spec = PaneSpec {
            cwd: Some("/x".into()),
            ..PaneSpec::default()
        };
        let mut p = d.spawn(&SpawnSpec { spec, size }).unwrap();
        assert_eq!(p.feed_handle(), Some(1));
        assert_eq!(p.feed_handle(), None);
        p.write(b"hi").unwrap();
        p.resize(Size { cols: 10, rows: 5 }).unwrap();
        assert_eq!(p.info().size.cols, 10);
        let q = d
            .split(
                p.id(),
                &SpawnSpec {
                    spec: PaneSpec::default(),
                    size,
                },
            )
            .unwrap();
        assert_eq!(q.info().cwd.as_deref(), Some("/x"));
        assert_eq!(d.list().len(), 2);
        assert_eq!(d.attach(1).err(), Some(DomainError::Unsupported));
        d.detach(1).unwrap();
        assert_eq!(d.detach(1), Err(DomainError::NoSuchPane));
        assert_eq!(
            d.split(
                1,
                &SpawnSpec {
                    spec: PaneSpec::default(),
                    size
                }
            )
            .err(),
            Some(DomainError::NoSuchPane)
        );
        assert_eq!(d.name(), "mem");
    }
}
