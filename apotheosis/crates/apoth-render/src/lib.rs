// SPDX-License-Identifier: AGPL-3.0-only
//! Renderer core: snapshot -> instance lists, retained-grid damage planning, atlases,
//! decorations, cursor, colour policy and the presentation policy.
//!
//! Governing spec: T-07 (`docs/ECLIPSEOS_SPECS_APOTHEOSIS.md`).
//!
//! This crate is the only one that compiles wgpu (T-07 head; Vulkan and WGSL only). Layers,
//! all GPU free unless noted: [`scene`] builds per-row instance lists, [`damage`] turns a grid
//! `Damage` into ring/copy/redraw operations, [`atlas`] packs and evicts, [`surface`] holds
//! the present-mode, alpha, adapter and error policies, [`sched`] is the frame scheduler's
//! fake-clock state machines, and [`reference`] is a CPU renderer of the instance lists used
//! for goldens and the damage mirror test. GPU: [`gpu`] (instance, adapter, device),
//! [`renderer`] (the wgpu executor: grid, cursor, WGSL embedded from `src/shaders`) and
//! [`presenter`] (the wgpu-surface presenter and the offscreen presenter).
//!
//! The raw-window-handle `unsafe` lives in `apoth-wl`, not here: see [`presenter`].
#![forbid(unsafe_code)] // BRIEF rule 6; surface handles come from apoth-wl

pub mod atlas;
pub mod chrome;
pub mod colour;
pub mod cursor;
pub mod damage;
pub mod decor;
pub mod glyph_cache;
pub mod gpu;
pub mod presenter;
pub mod reference;
pub mod renderer;
pub mod scene;
pub mod sched;
pub mod snapshot;
pub mod surface;

pub use apoth_grid::{Damage, FrameSnapshot, SnapCell, SnapRow};
