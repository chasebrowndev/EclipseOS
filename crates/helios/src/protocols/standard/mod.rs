// SPDX-License-Identifier: AGPL-3.0-only
mod compositor;
#[cfg(feature = "drm")]
mod dmabuf;
mod seat;
mod selection;
mod shm;
mod xdg_shell;
