// SPDX-License-Identifier: AGPL-3.0-only
//! Wayland client: windows, seats, scale, clipboard, IME, optional protocols by capability.
//!
//! Governing spec: T-09 (`docs/ECLIPSEOS_SPECS_APOTHEOSIS.md`).
//!
//! A thin first-party layer: our own `Dispatch` impls on one state struct over
//! `wayland-client` on the system libwayland backend. No winit, no xkbcommon, no
//! rendering. [`connect`] returns a calloop [`WlSource`] and a [`WlHandle`]; GPU surfaces
//! use [`WlSurfaceHandle`] (raw-window-handle 0.6).
#![deny(unsafe_code)] // unsafe is confined to FFI modules, each with `#[allow(unsafe_code)]` and a SAFETY comment

pub mod caps;
pub mod clock;
pub mod cursor;
pub mod event;
pub mod extras;
pub mod gesture;
pub mod mime;
pub mod pace;
pub mod rawhandle;
pub mod repeat;
pub mod scale;
pub mod seat;
pub mod source;
pub mod state;
pub mod transfer;
pub mod window;

mod dispatch;
mod shmbuf;

pub use caps::{BindState, BlurPath, CapabilityTable};
pub use clock::Stamp;
pub use event::{
    ConfigureEvent, DndEvent, FatalError, ImeEvent, InputEvent, InputKind, Keymap, KeymapFormat,
    OutputEvent, OutputId, OutputInfo, PresentedEvent, ScrollFrame, SeatId, Secret, SelectionEvent,
    SelectionKind, WindowId, WlEvent,
};
pub use extras::ContentType;
pub use mime::DropContent;
pub use pace::{PaceEvent, Pacer};
pub use rawhandle::WlSurfaceHandle;
pub use seat::{ClipboardError, CursorError};
pub use source::{WlHandle, WlSource, connect};
pub use state::{BlurRequest, WindowSpec, WlConfig, WlError, WlState};
pub use transfer::{Transfer, TransferDone, TransferLimits, TransferStatus};
