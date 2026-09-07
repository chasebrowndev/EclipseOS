// SPDX-License-Identifier: AGPL-3.0-only
mod compositor;
pub mod data_control;
pub mod data_device;
pub mod dmabuf;
#[cfg(feature = "drm")]
pub mod drm_syncobj;
pub mod fractional_scale;
pub mod input_method;
mod layer_shell;
mod presentation;
mod primary_selection;
mod seat;
mod shm;
mod text_input;
mod xdg_shell;
