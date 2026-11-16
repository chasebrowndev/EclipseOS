// SPDX-License-Identifier: AGPL-3.0-only
//! Media placements: protocol, the sandboxed decoder helper, A/V sync.
//!
//! Governing spec: T-16 (`docs/ECLIPSEOS_SPECS_APOTHEOSIS.md`).
#![deny(unsafe_code)] // unsafe is confined to FFI modules, each with `#[allow(unsafe_code)]` and a SAFETY comment

// T-16 §3: the nasm exception covers x86-64 assembly only; arm64 asm is never enabled.
#[cfg(all(feature = "av1-asm", not(target_arch = "x86_64")))]
compile_error!("`av1-asm` is x86_64-only (T-16 §3, ADR 0118)");

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
