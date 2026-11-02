// SPDX-License-Identifier: AGPL-3.0-only

//! Daemon-wide state shared by every interface object. Locks are plain
//! `std::sync::Mutex` and are never held across an `.await`.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{SystemTime, UNIX_EPOCH};

use ecl_portal_core::identity::DesktopIndex;
use ecl_portal_core::request::LiveTable;
use ecl_portal_core::screencast::ScreenCastBackend;
use ecl_portal_core::settings::Appearance;
use ecl_portal_store::Store;
use tokio::sync::oneshot;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionKind {
    ScreenCast,
    InhibitMonitor,
}

pub struct Shared {
    pub store: Store,
    pub appearance: Mutex<Appearance>,
    pub requests: Mutex<LiveTable<oneshot::Sender<()>>>,
    pub sessions: Mutex<LiveTable<SessionKind>>,
    pub index: Mutex<DesktopIndex>,
    pub backend: Arc<dyn ScreenCastBackend>,
    /// `(sender, portal notification id)` -> Notifications server id.
    pub notifications: Mutex<HashMap<(String, String), u32>>,
    pub config_dir: PathBuf,
    counter: AtomicU64,
}

pub fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    // A poisoned lock only means another task panicked; the data is plain.
    m.lock().unwrap_or_else(|e| e.into_inner())
}

impl Shared {
    pub fn new(
        store: Store,
        backend: Arc<dyn ScreenCastBackend>,
        index: DesktopIndex,
        config_dir: PathBuf,
        appearance: Appearance,
    ) -> Self {
        Shared {
            store,
            appearance: Mutex::new(appearance),
            requests: Mutex::new(LiveTable::new()),
            sessions: Mutex::new(LiveTable::new()),
            index: Mutex::new(index),
            backend,
            notifications: Mutex::new(HashMap::new()),
            config_dir,
            counter: AtomicU64::new(1),
        }
    }

    pub fn next_id(&self) -> u64 {
        self.counter.fetch_add(1, Ordering::Relaxed)
    }

    pub fn now(&self) -> u64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0)
    }

    /// Fire a request's cancel (the `Close` method, or its owner vanishing).
    pub fn cancel_request(&self, path: &str) {
        if let Some(tx) = lock(&self.requests).remove(path) {
            let _ = tx.send(());
        }
    }
}
