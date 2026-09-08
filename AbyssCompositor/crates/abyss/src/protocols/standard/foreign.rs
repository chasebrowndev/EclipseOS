// SPDX-License-Identifier: AGPL-3.0-only
//! `zxdg_exporter_v2` / `zxdg_importer_v2` (COMP-06 §1) — cross-client transient
//! parenting, used by file choosers and other portal-backed dialogs so they sit
//! over the window that asked for them.
//!
//! An exported handle names a surface but confers nothing on the holder beyond
//! "be a child of this": the importer can only set a parent relationship, which
//! the compositor already mediates. No gate.

use smithay::{
    delegate_xdg_foreign,
    wayland::xdg_foreign::{XdgForeignHandler, XdgForeignState},
};

use crate::state::AbyssState;

impl XdgForeignHandler for AbyssState {
    fn xdg_foreign_state(&mut self) -> &mut XdgForeignState {
        &mut self.xdg_foreign_state
    }
}

delegate_xdg_foreign!(AbyssState);
