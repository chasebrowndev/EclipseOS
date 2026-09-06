// SPDX-License-Identifier: AGPL-3.0-only
mod compositor;
#[cfg(feature = "drm")]
mod dmabuf;
mod layer_shell;
mod seat;
mod selection;
mod shm;
mod xdg_shell;
