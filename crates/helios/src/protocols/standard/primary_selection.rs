// SPDX-License-Identifier: AGPL-3.0-only
//! `zwp_primary_selection_v1` — middle-click paste (COMP-06 §1).
//!
//! Provenance tracking is deliberately clipboard-only (COMP-06 §4): the
//! primary selection is transient mouse state, not a stored clipboard.

use smithay::{
    delegate_primary_selection,
    wayland::selection::primary_selection::{PrimarySelectionHandler, PrimarySelectionState},
};

use crate::state::HeliosState;

impl PrimarySelectionHandler for HeliosState {
    fn primary_selection_state(&self) -> &PrimarySelectionState {
        &self.primary_selection_state
    }
}

delegate_primary_selection!(HeliosState);
