// SPDX-License-Identifier: AGPL-3.0-only
//! The worker pool (T-00 §4.2 threading model: "workers (2-3)"). Blocking or slow jobs (file
//! I/O such as the config load, later glyph raster misses and image decode) run here and
//! return their result by a channel the caller captured in the job. The pool owns no pane
//! state. The queue is bounded: a full queue refuses the job instead of growing.

use std::sync::mpsc::{self, SyncSender, TrySendError};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;

/// A unit of work.
pub(crate) type Job = Box<dyn FnOnce() + Send + 'static>;

/// Queue bound (jobs waiting, not running).
const QUEUE: usize = 64;

pub(crate) struct Pool {
    tx: Queue,
    threads: Vec<JoinHandle<()>>,
}

/// The queue's sender, shared with [`Spawner`]s; `None` once the pool is dropped (or when no
/// worker started), so a spawner never keeps the workers alive.
type Queue = Arc<Mutex<Option<SyncSender<Job>>>>;

/// A handle other threads submit through (the io threads' image decodes, T-15 §5).
#[derive(Clone)]
pub(crate) struct Spawner(Queue);

impl Spawner {
    /// Queue `job`. Returns it back when the queue is full or the pool is gone.
    pub(crate) fn submit(&self, job: Job) -> Result<(), Job> {
        try_send(&self.0, job)
    }
}

fn try_send(q: &Queue, job: Job) -> Result<(), Job> {
    let g = q.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    let Some(tx) = g.as_ref() else {
        return Err(job);
    };
    match tx.try_send(job) {
        Ok(()) => Ok(()),
        Err(TrySendError::Full(j) | TrySendError::Disconnected(j)) => Err(j),
    }
}

/// Worker count for `cpus` logical CPUs: 2 or 3 (T-00 §4.2).
pub(crate) fn worker_count(cpus: usize) -> usize {
    if cpus >= 6 { 3 } else { 2 }
}

impl Pool {
    /// A pool of `n` threads (at least one). A thread that cannot start is skipped; a pool
    /// with none refuses every job and the caller runs it inline.
    pub(crate) fn new(n: usize) -> Pool {
        let (tx, rx) = mpsc::sync_channel::<Job>(QUEUE);
        let rx = Arc::new(Mutex::new(rx));
        let mut threads = Vec::new();
        for i in 0..n.max(1) {
            let rx = Arc::clone(&rx);
            let spawned = std::thread::Builder::new()
                .name(format!("apoth-worker-{i}"))
                .spawn(move || {
                    loop {
                        let job = {
                            let g = rx.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
                            g.recv()
                        };
                        let Ok(job) = job else { break };
                        // A panicking job must not take the worker down.
                        let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(job));
                    }
                });
            match spawned {
                Ok(h) => threads.push(h),
                Err(e) => tracing::warn!(error = %e, "cannot start a worker thread"),
            }
        }
        let tx = (!threads.is_empty()).then_some(tx);
        Pool {
            tx: Arc::new(Mutex::new(tx)),
            threads,
        }
    }

    /// The default-sized pool for this machine.
    pub(crate) fn auto() -> Pool {
        let cpus = std::thread::available_parallelism().map_or(2, usize::from);
        Pool::new(worker_count(cpus))
    }

    pub(crate) fn threads(&self) -> usize {
        self.threads.len()
    }

    /// Queue `job`. Returns it back when the queue is full or no worker exists.
    pub(crate) fn submit(&self, job: Job) -> Result<(), Job> {
        try_send(&self.tx, job)
    }

    pub(crate) fn spawner(&self) -> Spawner {
        Spawner(Arc::clone(&self.tx))
    }
}

impl Drop for Pool {
    fn drop(&mut self) {
        self.tx
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take();
        for t in self.threads.drain(..) {
            let _ = t.join();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn counts_are_two_or_three() {
        assert_eq!(worker_count(1), 2);
        assert_eq!(worker_count(4), 2);
        assert_eq!(worker_count(8), 3);
    }

    #[test]
    fn jobs_return_by_channel() {
        let pool = Pool::new(2);
        assert_eq!(pool.threads(), 2);
        let (tx, rx) = mpsc::channel();
        for i in 0..10u32 {
            let tx = tx.clone();
            assert!(
                pool.submit(Box::new(move || {
                    let _ = tx.send(i * 2);
                }))
                .is_ok()
            );
        }
        let mut got: Vec<u32> = (0..10)
            .map(|_| rx.recv_timeout(Duration::from_secs(5)).unwrap())
            .collect();
        got.sort_unstable();
        assert_eq!(got, (0..10).map(|i| i * 2).collect::<Vec<_>>());
    }

    #[test]
    fn a_panicking_job_does_not_kill_the_worker() {
        let pool = Pool::new(1);
        assert!(pool.submit(Box::new(|| panic!("boom"))).is_ok());
        let (tx, rx) = mpsc::channel();
        assert!(
            pool.submit(Box::new(move || {
                let _ = tx.send(7);
            }))
            .is_ok()
        );
        assert_eq!(rx.recv_timeout(Duration::from_secs(5)).unwrap(), 7);
    }

    #[test]
    fn a_full_queue_hands_the_job_back() {
        let pool = Pool::new(1);
        let (gate_tx, gate_rx) = mpsc::channel::<()>();
        // Block the only worker, then fill the queue.
        assert!(
            pool.submit(Box::new(move || {
                let _ = gate_rx.recv();
            }))
            .is_ok()
        );
        std::thread::sleep(Duration::from_millis(50));
        let mut refused = false;
        for _ in 0..(QUEUE + 2) {
            if pool.submit(Box::new(|| {})).is_err() {
                refused = true;
                break;
            }
        }
        assert!(refused);
        let _ = gate_tx.send(());
    }

    #[test]
    fn a_spawner_does_not_outlive_the_pool() {
        let pool = Pool::new(1);
        let sp = pool.spawner();
        let (tx, rx) = mpsc::channel();
        assert!(
            sp.submit(Box::new(move || {
                let _ = tx.send(1);
            }))
            .is_ok()
        );
        assert_eq!(rx.recv_timeout(Duration::from_secs(5)).unwrap(), 1);
        drop(pool); // joins: the spawner holds no sender of its own
        assert!(sp.submit(Box::new(|| {})).is_err());
    }
}
