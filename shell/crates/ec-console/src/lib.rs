// SPDX-License-Identifier: AGPL-3.0-only
//! `ec-console` — the agent console pane (A-08 §8, D-05).
//!
//! It shows tasks and conversations and carries no authority (A-08 §1): it
//! cannot create, resume or approve anything. A task starts only when the human
//! commits the compositor's card, which this window leaves a hole for and never
//! draws.

pub mod accounts;
pub mod app;
#[cfg(debug_assertions)]
pub mod fixture;
pub mod model;
pub mod net;
pub mod shield;
pub mod view;

/// The window's app id. The shipped policy hides this app id from every agent
/// scene and capture (A-08 §10), so it is load-bearing and must not change.
pub const APP_ID: &str = "eclipse-console";
