// SPDX-License-Identifier: AGPL-3.0-only
mod compositor;
pub mod data_control;
pub mod data_device;
pub mod dmabuf;
#[cfg(feature = "drm")]
pub mod drm_syncobj;
pub mod fractional_scale;
mod idle_inhibit;
mod idle_notify;
pub mod input_method;
mod layer_shell;
pub mod output_power;
mod presentation;
mod primary_selection;
mod seat;
pub mod session_lock;
mod shm;
mod text_input;
mod xdg_shell;
