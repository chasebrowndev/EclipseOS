// SPDX-License-Identifier: AGPL-3.0-only
//! Wayland protocol handlers. `standard/` is the public surface (COMP-06);
//! `agent/` is the privileged `eclipse_agent_v1` (COMP-08); `semantic/` is
//! `eclipse_semantic_v1` (COMP-09).

pub mod agent;
pub mod semantic;
pub mod standard;
