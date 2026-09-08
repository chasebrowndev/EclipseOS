// SPDX-License-Identifier: AGPL-3.0-only
//! Three small surface-side globals from the COMP-06 §1 table, grouped because
//! each is a single passive global with no handler trait and no compositor
//! policy attached to it.
//!
//! - `wp_single_pixel_buffer_v1` — a solid-colour `wl_buffer` without a shm
//!   pool. Toolkits use it for backdrops; smithay resolves it in the renderer.
//! - `wp_content_type_v1` — the client's hint (`photo`/`video`/`game`) about
//!   what a surface is showing. Recorded on the surface by smithay; COMP-02 may
//!   later read it for VRR and scanout heuristics. Nothing reads it yet.
//! - `wp_alpha_modifier_v1` — a per-surface alpha multiplier applied at
//!   composite time.
//!
//! None of the three grants a client anything it could not already do with more
//! buffers and more CPU, so there is no gate on any of them.

use crate::state::AbyssState;

smithay::delegate_single_pixel_buffer!(AbyssState);
smithay::delegate_content_type!(AbyssState);
smithay::delegate_alpha_modifier!(AbyssState);
