// SPDX-License-Identifier: AGPL-3.0-only
//! GPU objects that exist once per renderer (ADR 0084).
//!
//! With one GPU compositing per output, a compiled program or a texture made
//! on one renderer is no use on another: GL objects belong to their context.
//! Stores that kept such objects in a plain field (`Option<program>`) hold them
//! in a [`PerContext`] instead. The field accessed through `Deref` is the
//! *live* set for the renderer last [`bound`](PerContext::bind); binding a
//! different renderer parks the live set under its context and takes (or
//! creates) the other's. A frame draws with one renderer start to finish, so
//! the swap happens at most when the compositor moves on to another GPU's
//! output, and costs a few moves of the (small) set.

use std::collections::HashMap;
use std::ops::{Deref, DerefMut};

use smithay::backend::renderer::{gles::GlesTexture, ContextId};

/// `T` once per renderer context. See the module note.
#[derive(Debug)]
pub struct PerContext<T> {
    current: Option<ContextId<GlesTexture>>,
    live: T,
    parked: HashMap<ContextId<GlesTexture>, T>,
}

impl<T: Default> Default for PerContext<T> {
    fn default() -> Self {
        Self {
            current: None,
            live: T::default(),
            parked: HashMap::new(),
        }
    }
}

impl<T: Default> PerContext<T> {
    /// Make `context`'s set the live one. A no-op while it already is.
    pub fn bind(&mut self, context: &ContextId<GlesTexture>) {
        if self.current.as_ref() == Some(context) {
            return;
        }
        let parked = self.parked.remove(context);
        match self.current.replace(context.clone()) {
            Some(old) => {
                let next = parked.unwrap_or_default();
                let previous = std::mem::replace(&mut self.live, next);
                self.parked.insert(old, previous);
            }
            // Nothing live: either the very first bind, which adopts whatever
            // was set before any renderer was known as this renderer's set, or
            // a rebind after `retain_contexts` dropped the live set, which
            // restores the set parked for it.
            None => {
                if let Some(set) = parked {
                    self.live = set;
                }
            }
        }
    }

    /// Forget the sets of contexts not in `keep` (a GPU that went away), the
    /// live one included.
    pub fn retain_contexts(&mut self, keep: &[ContextId<GlesTexture>]) {
        self.parked.retain(|c, _| keep.contains(c));
        if self.current.as_ref().is_some_and(|c| !keep.contains(c)) {
            self.live = T::default();
            self.current = None;
        }
    }

    /// The context whose set is live, if any renderer has bound yet.
    pub fn current(&self) -> Option<&ContextId<GlesTexture>> {
        self.current.as_ref()
    }

    /// Every set, live and parked, for a change that concerns them all (an
    /// output going away, a catalog being replaced).
    pub fn each_mut(&mut self, mut f: impl FnMut(&mut T)) {
        f(&mut self.live);
        self.parked.values_mut().for_each(f);
    }
}

impl<T> Deref for PerContext<T> {
    type Target = T;
    fn deref(&self) -> &T {
        &self.live
    }
}

impl<T> DerefMut for PerContext<T> {
    fn deref_mut(&mut self) -> &mut T {
        &mut self.live
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn each_context_keeps_its_own_set() {
        let (a, b) = (ContextId::<GlesTexture>::new(), ContextId::<GlesTexture>::new());
        let mut p: PerContext<Vec<u8>> = PerContext::default();
        p.bind(&a);
        p.push(1);
        p.bind(&b);
        assert!(p.is_empty(), "a new context starts empty");
        p.push(2);
        p.bind(&a);
        assert_eq!(*p, [1]);
        p.bind(&b);
        assert_eq!(*p, [2]);
        // Binding the live context again changes nothing.
        p.bind(&b);
        assert_eq!(*p, [2]);
    }

    #[test]
    fn the_set_made_before_any_bind_goes_to_the_first_renderer() {
        let a = ContextId::<GlesTexture>::new();
        let mut p: PerContext<Vec<u8>> = PerContext::default();
        p.push(7);
        p.bind(&a);
        assert_eq!(*p, [7]);
    }

    #[test]
    fn each_mut_reaches_parked_sets() {
        let (a, b) = (ContextId::<GlesTexture>::new(), ContextId::<GlesTexture>::new());
        let mut p: PerContext<Vec<u8>> = PerContext::default();
        p.bind(&a);
        p.push(1);
        p.bind(&b);
        p.push(2);
        p.each_mut(|v| v.clear());
        p.bind(&a);
        assert!(p.is_empty());
    }

    #[test]
    fn a_parked_set_survives_the_live_one_being_dropped() {
        let (a, b) = (ContextId::<GlesTexture>::new(), ContextId::<GlesTexture>::new());
        let mut p: PerContext<Vec<u8>> = PerContext::default();
        p.bind(&a);
        p.push(1);
        p.bind(&b);
        p.push(2);
        // `b` (live) goes away; `a` stays parked and is restored on rebind.
        p.retain_contexts(std::slice::from_ref(&a));
        assert!(p.is_empty());
        p.bind(&a);
        assert_eq!(*p, [1]);
    }
}
