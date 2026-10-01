// SPDX-License-Identifier: AGPL-3.0-only

//! Listing flow: cold two-phase listing, cached revalidation, and the one
//! place a new generation is published (FOG §Performance model,
//! techniques 1–3).
//!
//! Every read-modify-publish of a directory's listing runs under
//! [`Hub::lock`](crate::watch::Hub::lock) for that path, so generations are
//! strictly sequential and reach each subscriber in order.

use std::collections::HashMap;
use std::io;
use std::path::Path;
use std::sync::atomic::Ordering;
use std::sync::Arc;

use fog_proto::{apply_diff, Entry, Kind, Reply, Sort, SortKey};
use rustix::io::Errno;

use crate::cache::{diff, Listing};
use crate::watch::{Dirty, Peer};
use crate::{abs, error, meta, snapshot, sort, Daemon, BATCH};

/// The requesting client, which gets its replies in-line (blocking) rather
/// than through the non-blocking subscriber push.
pub(crate) struct Direct<'a> {
    pub peer: Option<u64>,
    pub out: &'a mut dyn FnMut(Reply),
}

impl Daemon {
    /// `ListDir` and `Subscribe`: cached snapshot or cold listing, then
    /// phase-2 metadata. With `sub`, the peer is registered (under the
    /// directory's lock, so it misses no generation) and the listing pinned.
    pub(crate) fn open_dir(&self, raw: Vec<u8>, sub: Option<&Peer>, out: &mut dyn FnMut(Reply)) {
        let path = match abs(&raw) {
            Ok(p) => p,
            Err(e) => return out(error(&raw, &e)),
        };
        let mut d = Direct {
            peer: sub.map(|p| p.id),
            out,
        };
        {
            let _g = self.hub.lock(&raw);
            let cached = self.cache().get(&raw);
            match cached {
                Some(old) => {
                    (d.out)(snapshot(&raw, &old, true));
                    (d.out)(self.sorted(&raw, old.dir));
                    // One statx catches what changed while unwatched.
                    match meta::dir_mtime(path) {
                        Err(e) => return self.fail(&raw, &e, Some(&mut d)),
                        Ok(mt) if Some(mt) != old.mtime_ns => {
                            if !self.refresh(&raw, path, &old, &Dirty::rescan(), Some(&mut d)) {
                                return;
                            }
                        }
                        Ok(_) => {}
                    }
                }
                None => {
                    if !self.cold(&raw, path, d.out) {
                        return;
                    }
                }
            }
            if let Some(p) = sub {
                let mut c = self.cache();
                if let Some(dir) = c.peek(&raw).map(|l| l.dir) {
                    c.pin(&raw);
                    drop(c);
                    self.hub.subscribe(p, &raw, dir);
                    self.hub.watch(&raw, path);
                }
            }
        }
        self.phase2(&raw, path, &mut d);
    }

    /// `SetSort`: remember the folder's sort, publish the new order as the
    /// next generation (a diff with no entry changes) to every subscriber,
    /// then send them and the requester `Sorted`. `peer` is the requester,
    /// skipped by the push because it gets both in-line.
    pub(crate) fn set_sort(
        &self,
        dir: u64,
        sort: Sort,
        peer: Option<u64>,
        out: &mut dyn FnMut(Reply),
    ) {
        let Some(raw) = self.cache().path_of(dir) else {
            return out(error(&[], &Errno::NOENT.into()));
        };
        let _g = self.hub.lock(&raw);
        let cur = match self.cache().peek(&raw).cloned() {
            Some(c) if c.dir == dir => c,
            _ => return out(error(&[], &Errno::NOENT.into())),
        };
        self.remember_sort(&raw, sort);
        let order = sort::order_by(&cur.entries, &sort);
        if order != cur.order {
            let generation = cur.generation + 1;
            let reply = Reply::DirDiff {
                dir,
                generation,
                removed: Vec::new(),
                added: Vec::new(),
                changed: Vec::new(),
                order: order.clone(),
                complete: true,
            };
            self.store(
                &raw,
                Arc::new(Listing {
                    generation,
                    order,
                    ..Listing::clone(&cur)
                }),
            );
            self.hub.broadcast(&raw, &reply, peer);
            out(reply);
        }
        let sorted = Reply::Sorted { dir, sort };
        self.hub.broadcast(&raw, &sorted, peer);
        out(sorted);
    }

    fn sorted(&self, raw: &[u8], dir: u64) -> Reply {
        Reply::Sorted {
            dir,
            sort: self.sort_of(raw),
        }
    }

    /// `Unsubscribe`: the listing stays cached (and watched) but may be
    /// evicted again.
    pub(crate) fn unsubscribe(&self, peer: u64, dir: u64) {
        if let Some(raw) = self.hub.unsubscribe(peer, dir) {
            self.cache().unpin(&raw);
        }
    }

    /// A client went away: drop its subscriptions.
    pub(crate) fn drop_peer(&self, peer: u64) {
        for raw in self.hub.drop_peer(peer) {
            self.cache().unpin(&raw);
        }
    }

    /// Uncached: stream the first batch as a partial snapshot, complete with
    /// one diff, then cache and watch. False if listing failed (the error
    /// was sent).
    fn cold(&self, raw: &[u8], path: &Path, out: &mut dyn FnMut(Reply)) -> bool {
        let dir = self.next_dir.fetch_add(1, Ordering::Relaxed);
        // Before the scan: a change during it moves the mtime past this.
        let mtime_ns = meta::dir_mtime(path).ok();
        let mut first: Option<Vec<Entry>> = None;
        let mut rest = Vec::new();
        let by = self.sort_of(raw);
        let res = self.backend.list(path, &mut |b| {
            if first.is_none() {
                out(Reply::DirSnapshot {
                    path: raw.to_vec(),
                    dir,
                    generation: 0,
                    entries: b.clone(),
                    order: sort::order_by(&b, &by),
                    complete: false,
                });
                out(Reply::Sorted { dir, sort: by });
                first = Some(b);
            } else {
                rest.extend(b);
            }
        });
        let tail = match res {
            Ok(t) => t,
            Err(e) => {
                out(error(raw, &e));
                return false;
            }
        };
        let listing = match first {
            None => {
                let l = Listing {
                    dir,
                    generation: 0,
                    order: sort::order_by(&tail, &by),
                    entries: tail,
                    mtime_ns,
                };
                out(snapshot(raw, &l, true));
                out(Reply::Sorted { dir, sort: by });
                l
            }
            Some(mut entries) => {
                rest.extend(tail);
                entries.extend_from_slice(&rest);
                let order = sort::order_by(&entries, &by);
                out(Reply::DirDiff {
                    dir,
                    generation: 1,
                    removed: Vec::new(),
                    added: rest,
                    changed: Vec::new(),
                    order: order.clone(),
                    complete: true,
                });
                Listing {
                    dir,
                    generation: 1,
                    entries,
                    order,
                    mtime_ns,
                }
            }
        };
        self.store(raw, Arc::new(listing));
        self.hub.watch(raw, path);
        true
    }

    /// Phase 2: `statx` every entry still lacking metadata, in display
    /// order, first [`meta::FIRST`] (the visible rows) and then
    /// [`BATCH`]-sized batches, each published as a `DirDiff{changed}`.
    fn phase2(&self, raw: &[u8], path: &Path, d: &mut Direct<'_>) {
        let Some(cur) = self.cache().peek(raw).cloned() else {
            return;
        };
        let todo: Vec<Entry> = meta::todo(&cur.entries, &cur.order)
            .into_iter()
            .map(|i| cur.entries[i].clone())
            .collect();
        if todo.is_empty() {
            return;
        }
        let Ok(fd) = meta::open_dir(path) else {
            return;
        };
        let mut seen = cur.generation;
        let mut start = 0;
        let mut size = meta::FIRST;
        while start < todo.len() {
            let chunk = &todo[start..todo.len().min(start + size)];
            start += size;
            size = BATCH;
            let mut resort = false;
            let mut filled: Vec<Entry> = chunk
                .iter()
                .filter_map(|e| {
                    let f = meta::fill(&fd, e)?;
                    resort |= f.kind != e.kind;
                    Some(f)
                })
                .collect();
            if filled.is_empty() {
                continue;
            }
            let _g = self.hub.lock(raw);
            let Some(cur) = self.cache().peek(raw).cloned() else {
                return;
            };
            if cur.generation != seen {
                // Someone else published meanwhile: keep only entries that
                // are still there and still owed metadata.
                let now: HashMap<&[u8], &Entry> =
                    cur.entries.iter().map(|e| (e.name.as_slice(), e)).collect();
                filled.retain(|f| {
                    now.get(f.name.as_slice()).is_some_and(|o| {
                        meta::missing(o) && (o.kind == f.kind || o.kind == Kind::Unknown)
                    })
                });
            }
            match self.publish(
                raw,
                &cur,
                Vec::new(),
                Vec::new(),
                filled,
                resort,
                cur.mtime_ns,
                Some(&mut *d),
            ) {
                Some(g) => seen = g,
                None => seen = cur.generation,
            }
        }
    }

    /// The watcher saw `dirty` in `raw`.
    pub(crate) fn on_change(&self, raw: &[u8], dirty: &Dirty) {
        let Ok(path) = abs(raw) else { return };
        let _g = self.hub.lock(raw);
        let cur = self.cache().peek(raw).cloned();
        match cur {
            Some(cur) => {
                self.refresh(raw, path, &cur, dirty, None);
            }
            // Evicted behind the watcher's back (or removed): stop watching.
            None => self.hub.unwatch(raw),
        }
    }

    /// An unwatched directory's periodic check: retry the watch, and rescan
    /// if its mtime moved.
    pub(crate) fn recheck(&self, raw: &[u8]) {
        let Ok(path) = abs(raw) else { return };
        let _g = self.hub.lock(raw);
        let Some(cur) = self.cache().peek(raw).cloned() else {
            return self.hub.unwatch(raw);
        };
        self.hub.watch(raw, path);
        match meta::dir_mtime(path) {
            Ok(mt) if Some(mt) == cur.mtime_ns => {}
            Ok(_) => {
                self.refresh(raw, path, &cur, &Dirty::rescan(), None);
            }
            Err(e) => self.fail(raw, &e, None),
        }
    }

    /// Bring `cur` up to date with the directory and publish the difference.
    /// Call under the directory's lock. False if the directory failed (the
    /// error was sent and the listing dropped).
    fn refresh(
        &self,
        raw: &[u8],
        path: &Path,
        cur: &Arc<Listing>,
        dirty: &Dirty,
        mut direct: Option<&mut Direct<'_>>,
    ) -> bool {
        let old: HashMap<&[u8], &Entry> =
            cur.entries.iter().map(|e| (e.name.as_slice(), e)).collect();
        let (mut new, mtime) = if dirty.rescan {
            let mt = meta::dir_mtime(path).ok();
            let mut v = match self.list_all(path) {
                Ok(v) => v,
                Err(e) => {
                    self.fail(raw, &e, direct.as_deref_mut());
                    return false;
                }
            };
            // Unchanged entries keep their metadata.
            for e in &mut v {
                if let Some(o) = old.get(e.name.as_slice()) {
                    if o.kind == e.kind && !dirty.full && !dirty.names.contains(&e.name) {
                        e.clone_from(o);
                    }
                }
            }
            self.hub.watch(raw, path);
            (v, mt)
        } else {
            let mut v = cur.entries.clone();
            for e in &mut v {
                if dirty.names.contains(&e.name) {
                    *e = Entry::new(std::mem::take(&mut e.name), e.kind);
                }
            }
            (v, cur.mtime_ns)
        };
        // Stat what the events named and what is new; phase 2 still owns
        // entries that never had metadata.
        if let Ok(fd) = meta::open_dir(path) {
            for e in &mut new {
                let wanted = dirty.full
                    || dirty.names.contains(&e.name)
                    || !old.contains_key(e.name.as_slice());
                if wanted && meta::missing(e) {
                    if let Some(f) = meta::fill(&fd, e) {
                        *e = f;
                    }
                }
            }
        }
        let (removed, added, changed) = diff(&cur.entries, &new);
        self.publish(raw, cur, removed, added, changed, false, mtime, direct);
        true
    }

    /// Publish the next generation of `cur`: cache it and send the diff to
    /// `direct` (blocking) and every other subscriber. Call under the
    /// directory's lock. Returns the new generation, or `None` if nothing
    /// changed.
    #[allow(clippy::too_many_arguments)]
    fn publish(
        &self,
        raw: &[u8],
        cur: &Arc<Listing>,
        removed: Vec<Vec<u8>>,
        added: Vec<Entry>,
        changed: Vec<Entry>,
        resort: bool,
        mtime_ns: Option<i128>,
        direct: Option<&mut Direct<'_>>,
    ) -> Option<u64> {
        if removed.is_empty() && added.is_empty() && changed.is_empty() {
            if mtime_ns != cur.mtime_ns {
                let l = Listing {
                    mtime_ns,
                    ..Listing::clone(cur)
                };
                self.store(raw, Arc::new(l));
            }
            return None;
        }
        let mut entries = cur.entries.clone();
        apply_diff(&mut entries, &removed, &added, &changed);
        let by = self.sort_of(raw);
        // Under any key but the name, new metadata can move a row.
        let keyed = by.key != SortKey::Name && !changed.is_empty();
        let order = if resort || keyed || !removed.is_empty() || !added.is_empty() {
            sort::order_by(&entries, &by)
        } else {
            cur.order.clone()
        };
        let generation = cur.generation + 1;
        let reply = Reply::DirDiff {
            dir: cur.dir,
            generation,
            removed,
            added,
            changed,
            order: order.clone(),
            complete: true,
        };
        self.store(
            raw,
            Arc::new(Listing {
                dir: cur.dir,
                generation,
                entries,
                order,
                mtime_ns,
            }),
        );
        let skip = direct.as_ref().and_then(|d| d.peer);
        self.hub.broadcast(raw, &reply, skip);
        if let Some(d) = direct {
            (d.out)(reply);
        }
        Some(generation)
    }

    /// The directory can no longer be listed: drop it everywhere and tell
    /// the requester and every subscriber.
    fn fail(&self, raw: &[u8], e: &io::Error, direct: Option<&mut Direct<'_>>) {
        {
            let mut c = self.cache();
            c.remove(raw);
            c.unpin(raw);
        }
        self.hub.unwatch(raw);
        let reply = error(raw, e);
        let skip = direct.as_ref().and_then(|d| d.peer);
        self.hub.broadcast(raw, &reply, skip);
        self.hub.take_subs(raw);
        if let Some(d) = direct {
            (d.out)(reply);
        }
    }

    /// Cache `listing`; listings it evicts lose their watch.
    fn store(&self, raw: &[u8], listing: Arc<Listing>) {
        let evicted = self.cache().insert(raw.to_vec(), listing);
        for p in evicted {
            self.hub.unwatch(&p);
        }
    }

    fn list_all(&self, path: &Path) -> io::Result<Vec<Entry>> {
        let mut all = Vec::new();
        let tail = self.backend.list(path, &mut |b| all.extend(b))?;
        all.extend(tail);
        Ok(all)
    }
}
