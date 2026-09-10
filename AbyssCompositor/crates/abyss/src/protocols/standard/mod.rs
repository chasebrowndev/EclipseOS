// SPDX-License-Identifier: AGPL-3.0-only
mod activation;
mod compositor;
pub mod data_control;
pub mod data_device;
mod decoration;
pub mod dmabuf;
// Explicit sync rides on the DRM state; without that backend there is no
// syncobj eventfd to hand out, so the global is not advertised at all.
#[cfg(feature = "drm")]
pub mod drm_syncobj;
pub mod foreign;
pub mod foreign_toplevel;
pub mod fractional_scale;
pub mod gamma_control;
mod idle_inhibit;
mod idle_notify;
pub mod image_copy_capture;
pub mod input_method;
mod layer_shell;
pub mod output_management;
pub mod output_power;
pub(crate) mod pointer_constraints;
mod pointer_extra;
mod presentation;
mod primary_selection;
pub mod screencopy;
mod seat;
mod security_context;
pub mod session_lock;
mod shm;
mod surface_extra;
mod tablet;
mod text_input;
pub mod virtual_pointer;
mod xdg_shell;
