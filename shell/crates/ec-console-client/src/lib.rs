// SPDX-License-Identifier: AGPL-3.0-only
//! Everything the console frontend needs from outside its own process, and no
//! UI (A-08 §7, COMP-19).
//!
//! * [`console`]: agentd's `console.sock`, typed requests and events.
//! * [`protected`]: `eclipse_protected_surface_v1` glue that attaches to the
//!   display and surface winit/iced already own.

pub mod console;
pub mod protected;
