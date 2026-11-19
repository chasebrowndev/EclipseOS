// SPDX-License-Identifier: AGPL-3.0-only
//! The login greeter's visual layer (ADR 0126).
//!
//! Draws the operator's tile layout as glass panes on any output size, with
//! the Glances and the login form. It draws, and it decides nothing about
//! who may log in: the backend is handed in as an [`Auth`]. See `CLAUDE.md`
//! for what this crate may and may not touch.

pub mod auth;
pub mod backdrop;
pub mod clock;
pub mod glance;
pub mod glass;
pub mod motion;
pub mod parts;
pub mod secret;
pub mod tokens;

mod app;
mod view;

pub use app::{run, run_with, Options, SHIPPED_WALLPAPER};
pub use auth::{Auth, AuthState, FakeAuth, Power, Refusal};
pub use secret::Secret;
