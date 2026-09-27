// SPDX-License-Identifier: AGPL-3.0-only
//! `eclipse-setup`: the graphical installer (D-07 §4, steps 0 to 6, 13 and 14).
//!
//! One full-screen toplevel, keyboard first. Step 0 is `eclipse-welcome`
//! embedded as a library; the rest are the screens under [`view`]. What the
//! wizard is lives in [`model`], a state machine with no I/O; what it does to
//! the machine goes through exactly three doors, each reviewable on its own:
//!
//! - [`config`]: the control socket's `set_config_value`, behind a closed
//!   allowlist of keys. There is no path from this program to `policy.kdl`.
//! - [`helper`]: `eclipse-setup-helper`, one JSON request on stdin, progress
//!   lines on stdout. The password travels only inside that request.
//! - [`net`]: `nmcli` with fixed argv, a passphrase on stdin and never on argv.
//!
//! `--fake-helper` swaps all three (and the reboot) for simulations, so the
//! whole flow runs without root and without touching the desktop it runs on.
//! It is never the default.

pub mod app;
pub mod choices;
pub mod config;
pub mod data;
pub mod helper;
pub mod metrics;
pub mod model;
pub mod net;
pub mod parts;
pub mod shot;
pub mod sys;
pub mod view;

#[cfg(test)]
mod exec;
