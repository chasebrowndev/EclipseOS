// SPDX-License-Identifier: AGPL-3.0-only
//! Backends (COMP-01 §3): `winit` for nested development, `drm` for
//! production (M2), `headless` for CI (M9).

#[cfg(feature = "drm")]
pub mod drm;
#[cfg(feature = "winit")]
pub mod winit;
