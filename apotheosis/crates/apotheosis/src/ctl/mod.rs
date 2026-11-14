// SPDX-License-Identifier: AGPL-3.0-only
//! The control socket's state-free parts (T-12): where it lives, who the peer is, what the
//! peer may do, connection buffers and targets. The method handlers live with the main loop
//! (`app/serve.rs`).

pub(crate) mod caps;
pub(crate) mod conn;
pub(crate) mod launch;
pub(crate) mod osc7;
pub(crate) mod path;
pub(crate) mod peer;
pub(crate) mod target;
pub(crate) mod text;
