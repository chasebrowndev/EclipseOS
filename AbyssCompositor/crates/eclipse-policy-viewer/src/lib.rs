// SPDX-License-Identifier: AGPL-3.0-only
//! `eclipse-policy-viewer` — a read-only window onto `policy.kdl`.
//!
//! The security surface is shown, never edited. Editing it is the job of the
//! compositor-drawn policy editor (milestone 15): a layer-shell client must not
//! be able to widen an allowlist, so there is no edit path here to add later.

pub mod app;
pub mod read;
pub mod view;
