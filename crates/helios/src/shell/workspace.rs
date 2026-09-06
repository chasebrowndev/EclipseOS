// SPDX-License-Identifier: AGPL-3.0-only
//! Workspaces (COMP-05 §4). Ten numbered workspaces; M2 keeps a single set
//! shared by the (single) output — per-output sets land with multi-output in M3.

use smithay::{
    desktop::Window,
    utils::{Logical, Rectangle},
};

use crate::config::LayoutKind;

use super::layout::Tree;

pub const COUNT: usize = 10;

#[derive(Debug)]
pub struct Floating {
    pub window: Window,
    pub rect: Rectangle<i32, Logical>,
}

#[derive(Debug, Default)]
pub struct Workspace {
    pub tiled: Tree,
    pub floating: Vec<Floating>,
    /// Set by `toggle-layout`; otherwise the config decides.
    pub layout: Option<LayoutKind>,
}

impl Workspace {
    /// Every window on this workspace, tiled first then floating (stacking order).
    pub fn windows(&self) -> Vec<Window> {
        let mut v = self.tiled.windows();
        v.extend(self.floating.iter().map(|f| f.window.clone()));
        v
    }

    pub fn remove(&mut self, w: &Window) -> bool {
        let was_floating = self.floating.iter().position(|f| &f.window == w);
        if let Some(i) = was_floating {
            self.floating.remove(i);
            return true;
        }
        self.tiled.remove(w)
    }
}

pub fn new_set() -> Vec<Workspace> {
    (0..COUNT).map(|_| Workspace::default()).collect()
}
