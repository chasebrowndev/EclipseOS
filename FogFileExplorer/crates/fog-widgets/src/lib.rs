// SPDX-License-Identifier: AGPL-3.0-only

//! Reusable Fog widgets: virtual list, glass shader (FOG §Performance model).

pub mod virtual_list;

pub use virtual_list::{reveal_offset, scroll_into_view, virtual_list, visible_range, VirtualList};
