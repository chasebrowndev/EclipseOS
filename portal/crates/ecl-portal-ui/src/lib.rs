// SPDX-License-Identifier: AGPL-3.0-only
//! `ecl-portal-ui`: the dialog panes `ecl-desktop-portal` launches, one per
//! request, over an inherited socketpair (`--fd N`).
//!
//! A pane reads one `PaneRequest`, shows it, writes one `PaneReply` and exits.
//! It is an ordinary `xdg_toplevel` client with no authority: what it returns
//! is a *choice*, and the compositor asks again before anything is captured
//! (ADR 0097).

pub mod app;
pub mod input;
pub mod metrics;
pub mod model;
pub mod tile;
pub mod view;
pub mod wire;

/// The Wayland app id, so window rules can match the dialogs.
pub const APP_ID: &str = "ecl-portal-ui";
