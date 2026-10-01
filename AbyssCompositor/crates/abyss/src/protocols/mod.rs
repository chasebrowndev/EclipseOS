// SPDX-License-Identifier: AGPL-3.0-only
//! Wayland protocol handlers. `standard/` is the public surface (COMP-06);
//! `agent/` is the privileged `eclipse_agent_v1` (COMP-08); `semantic/`
//! arrives in Phase 2.

pub mod agent;
pub mod standard;
