// SPDX-License-Identifier: AGPL-3.0-only
//! Session / workspace / window / tab / pane model and the Domain trait. No Wayland, no GPU.
//!
//! Governing spec: T-13 (`docs/ECLIPSEOS_SPECS_APOTHEOSIS.md`).
//!
//! M22a scope: the pure model. `Session > Window > Tab > { tiled split tree | floating list }`
//! with `u64` handles ([`Session`], [`Window`], [`Tab`], [`Node`]); split, close, resize,
//! equalize, swap, move, rotate, zoom, stacks, presets and focus on [`Tab`]; named layouts
//! as KDL ([`parse_layouts`], [`write_layouts`], [`Session::apply_layout`],
//! [`Session::capture_layout`]); and the [`Domain`]/[`Pane`] trait signatures.
//!
//! Not here: PTYs, rendering, threads, broadcast (T-13 §5), session save and undo-close
//! (R2). No logging of content anywhere: ids, counts and sizes only.
#![forbid(unsafe_code)]

mod domain;
mod geom;
mod layout_file;
mod preset;
mod session;
mod tab;
mod tree;

pub use domain::{Domain, DomainError, Pane, PaneInfo, SpawnSpec};
pub use geom::{Dir, MIN_COLS, MIN_ROWS, Orient, Rect, Size};
pub use layout_file::{
    ErrKind, FloatSpec, LayoutError, LayoutSpec, MAX_DEPTH, MAX_INPUT_BYTES, MAX_NODES, MAX_PANES,
    MAX_STRING_BYTES, NodeSpec, PaneNode, TabSpec, parse_layouts, write_layouts,
};
pub use preset::{Preset, build as build_preset};
pub use session::{
    CloseReport, MuxError, PaneMeta, PaneSpec, Session, TabTarget, Window, WindowId,
    WorkspaceSwitch,
};
pub use tab::{Float, FocusMove, SplitSize, Tab, TabError, TabId};
pub use tree::{Node, PaneId, Placed, RATIO_ONE, Split, Stack, StackStyle, ratio_of, split_cells};
