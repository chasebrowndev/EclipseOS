// SPDX-License-Identifier: AGPL-3.0-only
//! Presentation rows paired with [`crate::schema::TABLE`] (T-10 §1).

pub use crate::rows::META;
pub use crate::types::{Meta, Tier};

/// The presentation row of a key.
pub fn meta(path: &str) -> Option<&'static Meta> {
    crate::schema::index_of(path).map(|i| &META[i])
}
