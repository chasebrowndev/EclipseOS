// SPDX-License-Identifier: AGPL-3.0-only
//! Toplevel generations (COMP-08 §3, §10 step 8). Not TCB.
//!
//! A toplevel's generation is a counter an agent reads from the scene and
//! sends back as `expected_generation`: if what the agent saw has changed
//! since, the act is `stale_generation` and nothing runs.
//!
//! Until the semantic tree exists (COMP-09) "what the agent saw" is the set
//! of facts the scene reports: app id, title, geometry, workspace, output
//! and the maximized, fullscreen, floating and minimized bits. Human focus is
//! left out on purpose: it moves constantly and says nothing about what is
//! on the window. A change in those facts bumps the generation; the bump is
//! noticed lazily, by whoever asks next (a scene event, a result, an
//! `expected_generation` check, a `wait_for`), which is enough because every
//! reader sees the facts as they are when it asks.
//!
//! Generations come from one counter that only goes up and starts at 1, so a
//! value is never reused for a different state of the same or another
//! window, even after an entry is dropped. 0 is "unchecked" on the wire.

use std::hash::{Hash, Hasher};
use std::time::Instant;

use smithay::desktop::Window;

use crate::state::AbyssState;

/// Windows tracked at once. An entry that is evicted comes back with a fresh
/// (larger) number, which can only make a stale agent more stale.
const MAX_TRACKED: usize = 4096;

/// `eclipse_scene_v1.state.focused`: not part of what a window shows.
const FOCUSED_BIT: u32 = 4;

#[derive(Debug)]
struct Entry {
    handle: u64,
    fingerprint: u64,
    generation: u32,
    /// When the fingerprint last changed (`wait_for idle`).
    since: Instant,
}

#[derive(Debug, Default)]
pub struct Generations {
    last: u32,
    entries: Vec<Entry>,
}

impl Generations {
    /// The generation of window `handle` whose facts hash to `fingerprint`
    /// now, and when it last changed.
    pub fn observe(&mut self, handle: u64, fingerprint: u64, now: Instant) -> (u32, Instant) {
        if let Some(e) = self.entries.iter_mut().find(|e| e.handle == handle) {
            if e.fingerprint != fingerprint {
                self.last = self.last.wrapping_add(1).max(1);
                e.fingerprint = fingerprint;
                e.generation = self.last;
                e.since = now;
            }
            return (e.generation, e.since);
        }
        if self.entries.len() >= MAX_TRACKED {
            self.entries.remove(0);
        }
        self.last = self.last.wrapping_add(1).max(1);
        self.entries.push(Entry {
            handle,
            fingerprint,
            generation: self.last,
            since: now,
        });
        (self.last, now)
    }

    /// The window is gone.
    pub fn forget(&mut self, handle: u64) {
        self.entries.retain(|e| e.handle != handle);
    }
}

/// What a change in the scene's facts is, as one number.
pub(super) fn fingerprint(f: &super::Facts) -> u64 {
    let mut h = std::collections::hash_map::DefaultHasher::new();
    f.app_id.hash(&mut h);
    f.title.hash(&mut h);
    (f.x, f.y, f.w, f.h).hash(&mut h);
    (f.workspace, f.output).hash(&mut h);
    (f.states & !FOCUSED_BIT).hash(&mut h);
    h.finish()
}

/// `window`'s generation and when it last changed, bumping it first if its
/// facts have changed since anyone last looked.
pub fn observe(state: &mut AbyssState, window: &Window) -> (u32, Instant) {
    let handle = match state.ipc.existing_handle(window) {
        Some(h) => h,
        None => state.ipc.handle_for(window),
    };
    let f = super::facts(state, u32::try_from(handle).unwrap_or(0), window);
    let fp = fingerprint(&f);
    state.agents.generations.observe(handle, fp, Instant::now())
}

/// `window`'s current generation.
pub fn of(state: &mut AbyssState, window: &Window) -> u32 {
    observe(state, window).0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_change_bumps_and_a_return_to_the_old_facts_is_not_the_old_number() {
        let t = Instant::now();
        let mut g = Generations::default();
        let (a, _) = g.observe(1, 100, t);
        assert_eq!(g.observe(1, 100, t).0, a, "unchanged facts, unchanged generation");
        let (b, _) = g.observe(1, 101, t);
        assert!(b > a);
        let (c, _) = g.observe(1, 100, t);
        assert!(c > b, "never reused, even for the same facts");
        let (other, _) = g.observe(2, 100, t);
        assert!(other > c, "one counter across windows");
    }

    #[test]
    fn eviction_never_reissues_a_number() {
        let t = Instant::now();
        let mut g = Generations::default();
        let (first, _) = g.observe(0, 1, t);
        for h in 1..=MAX_TRACKED as u64 {
            g.observe(h, 1, t);
        }
        assert!(g.observe(0, 1, t).0 > first);
    }
}
