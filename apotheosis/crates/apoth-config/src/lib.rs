// SPDX-License-Identifier: AGPL-3.0-only
//! Configuration: schema table, KDL load and validation, layers, lossless editing, watching.
//!
//! Governing spec: T-10 (`docs/ECLIPSEOS_SPECS_APOTHEOSIS.md`). The inotify watcher (M15)
//! and importers beyond the `version` node are out of this crate for now; [`DEBOUNCE_MS`]
//! and [`POLL_FALLBACK_MS`] are the numbers it will use.
#![forbid(unsafe_code)]

mod collections;
mod rows;

pub mod chord;
pub mod diag;
pub mod diff;
pub mod edit;
pub mod fs;
pub mod lang;
pub mod load;
pub mod meta;
pub mod model;
pub mod overrides;
pub mod schema;
pub mod types;
pub mod value;
pub mod version;

pub use collections::{MEDIA_EVENTS, RULE_EVENTS, is_rule_event};
pub use diag::{Diagnostic, Severity};
pub use diff::{ConfigDiff, diff};
pub use edit::{EditError, Editor};
pub use load::{LoadInput, LoadResult, load};
pub use model::Config;
pub use schema::TABLE;
pub use types::{Key, Reload, Value};

/// Coalesce file events for this long before reloading (CONFIG-16).
pub const DEBOUNCE_MS: u64 = 100;
/// Re-stat interval when inotify is unavailable.
pub const POLL_FALLBACK_MS: u64 = 2000;

#[cfg(test)]
mod tests {
    #[test]
    fn debounce_is_100_ms() {
        assert_eq!(super::DEBOUNCE_MS, 100);
    }
}
