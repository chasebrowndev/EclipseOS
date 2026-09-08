// SPDX-License-Identifier: AGPL-3.0-only
//! `wp_presentation` v2 (COMP-06 §1). Required by the standard-protocol table.
//!
//! The global is created in [`crate::state::AbyssState::new`] against
//! `CLOCK_MONOTONIC`; feedback is delivered by the backends — from the real
//! page-flip event on DRM, best-effort from the frame clock under winit.
//! There is no handler trait: `PresentationState` is entirely passive.

use crate::state::AbyssState;

smithay::delegate_presentation!(AbyssState);
