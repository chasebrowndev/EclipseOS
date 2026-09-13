// SPDX-License-Identifier: AGPL-3.0-only
//! Workspaces (COMP-05 §4). Ten numbered workspaces per output; the set is
//! owned by the output's entry in `outputs::Outputs`, so unplugging a monitor
//! takes its workspaces with it (COMP-03 §5).

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

/// A window the human sent away (COMP-05 §4 `minimized`). It keeps its place
/// in the workspace but leaves the layout entirely: not tiled, not floating,
/// not mapped into the space. `was_floating` is the rectangle it owned before,
/// so restoring puts it back where it was rather than dropping it into tiling.
#[derive(Debug)]
pub struct Minimized {
    pub window: Window,
    pub was_floating: Option<Rectangle<i32, Logical>>,
}

#[derive(Debug, Default)]
pub struct Workspace {
    pub tiled: Tree,
    pub floating: Vec<Floating>,
    /// Set by `toggle-layout`; otherwise the config decides.
    pub layout: Option<LayoutKind>,
    /// Windows adopted from a departed output, not yet placed in the layout.
    /// `shell::arrange` drains this.
    pub pending: Vec<Window>,
    /// Minimized windows, oldest first. Minimizing is per workspace, so a
    /// window sent away on workspace 3 comes back on workspace 3.
    pub minimized: Vec<Minimized>,
}

impl Workspace {
    /// Every window on this workspace, tiled first then floating (stacking order).
    pub fn windows(&self) -> Vec<Window> {
        let mut v = self.tiled.windows();
        v.extend(self.floating.iter().map(|f| f.window.clone()));
        v
    }

    /// Every window this workspace owns, minimized ones included. `windows`
    /// answers "what is on screen"; this answers "what is here".
    pub fn all_windows(&self) -> Vec<Window> {
        let mut v = self.windows();
        v.extend(self.minimized.iter().map(|m| m.window.clone()));
        v
    }

    pub fn is_minimized(&self, w: &Window) -> bool {
        self.minimized.iter().any(|m| &m.window == w)
    }

    pub fn remove(&mut self, w: &Window) -> bool {
        if let Some(i) = self.pending.iter().position(|p| p == w) {
            self.pending.remove(i);
            return true;
        }
        if let Some(i) = self.minimized.iter().position(|m| &m.window == w) {
            self.minimized.remove(i);
            return true;
        }
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
