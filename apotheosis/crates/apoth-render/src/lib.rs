// SPDX-License-Identifier: AGPL-3.0-only
//! Renderer core: snapshot -> instance lists, retained-grid damage planning, atlases,
//! decorations, cursor, colour policy and the presentation policy.
//!
//! Governing spec: T-07 (`docs/ECLIPSEOS_SPECS_APOTHEOSIS.md`).
//!
//! M10 is PARTIAL: scene, damage, atlas policy and surface policy only. There is no wgpu
//! executor and no lavapipe golden yet; the PNG goldens here come from the CPU `reference`
//! renderer and are not T-25 pixel goldens. The wgpu executor and lavapipe goldens are owed
//! before M11.
//!
//! This crate is the only one that will compile wgpu (T-07 head). The GPU executor (surface,
//! device, pipelines, texture upload) is not in this revision: the pinned wgpu 30.0.2 source
//! was not available to read (BRIEF rule 10). Everything the executor consumes is here, GPU
//! free and testable: [`scene`] builds per-row instance lists, [`damage`] turns a grid
//! `Damage` into ring/copy/redraw operations, [`atlas`] packs and evicts, [`surface`] holds
//! the present-mode, alpha, adapter and error policies, and [`reference`] is a CPU renderer
//! of the instance lists used for goldens and the damage mirror test.
#![forbid(unsafe_code)] // BRIEF rule 6; wgpu surface creation, if it needs unsafe, belongs in apoth-wl

pub mod atlas;
pub mod colour;
pub mod cursor;
pub mod damage;
pub mod decor;
pub mod glyph_cache;
pub mod reference;
pub mod scene;
pub mod surface;

pub use apoth_grid::{Damage, FrameSnapshot, SnapCell, SnapRow};
