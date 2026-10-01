// SPDX-License-Identifier: AGPL-3.0-only
//! The control-socket authorisation gate now lives in `ec-abyss-wire`
//! (COMP-13 §2); this keeps the `crate::ipc::gate::` paths working.

pub use ec_abyss_wire::gate::*;
