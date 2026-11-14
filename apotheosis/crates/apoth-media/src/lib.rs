// SPDX-License-Identifier: AGPL-3.0-only
//! Media placements: protocol, the sandboxed decoder helper, A/V sync.
//!
//! Governing spec: T-16 (`docs/ECLIPSEOS_SPECS_APOTHEOSIS.md`).
#![deny(unsafe_code)] // unsafe is confined to FFI modules, each with `#[allow(unsafe_code)]` and a SAFETY comment

pub mod audio_dec;
pub mod av1;
pub mod chan;
pub mod client;
pub mod demux;
pub mod fuzz;
pub mod helper;
pub mod image;
pub mod player;
pub mod sandbox;
pub mod sink;
pub mod sync;
pub mod wire;
pub mod yuv;
