// SPDX-License-Identifier: AGPL-3.0-only

//! Reusable Fog widgets: virtual list, glass shader, springs, the contrast
//! floor, elided text and drag and drop (FOG §Performance model, §Visual design).

pub mod contrast;
pub mod dnd;
pub mod elide;
pub mod glass;
pub mod spring;
pub mod virtual_list;

pub use dnd::{draggable, Draggable};
pub use elide::{elide, Elide};
pub use glass::{glass, Backdrop, Blur, Glass};
pub use spring::{Params, Spring};
pub use virtual_list::{reveal_offset, scroll_into_view, virtual_list, visible_range, VirtualList};
