// SPDX-License-Identifier: AGPL-3.0-only

//! Reusable Fog widgets: virtual list, glass shader, springs and the
//! contrast floor (FOG §Performance model, §Visual design).

pub mod contrast;
pub mod glass;
pub mod spring;
pub mod virtual_list;

pub use glass::{glass, Backdrop, Blur, Glass};
pub use spring::{Params, Spring};
pub use virtual_list::{reveal_offset, scroll_into_view, virtual_list, visible_range, VirtualList};
