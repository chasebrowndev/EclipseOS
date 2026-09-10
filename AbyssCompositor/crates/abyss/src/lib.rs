// SPDX-License-Identifier: AGPL-3.0-only
//! abyss — the EclipseOS compositor, as a library.
//!
//! The binary in `main.rs` is a thin argument-parsing shim over this. The
//! split exists so the `wlcs` conformance harness (COMP-15 §1) can link the
//! compositor and drive it in-process; nothing else about the layout changes.

pub mod backend;
pub mod config;
pub mod input;
pub mod ipc;
pub mod outputs;
pub mod protocols;
pub mod render;
pub mod session;
pub mod shell;
pub mod state;
pub mod xwayland;
