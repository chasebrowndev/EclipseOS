// SPDX-License-Identifier: AGPL-3.0-only
//! Schema versioning (T-10 §6): the `version` node and the rename compatibility table.
//! Migrators beyond the version node are out of scope for M4.

/// The schema version this build writes and understands.
pub const CURRENT_VERSION: u32 = 1;

/// Old key path -> new key path. A use of the old path loads as the new one and carries a
/// deprecation warning.
pub const RENAMED: &[(&str, &str)] = &[
    // T-10/T-06: one key for the behaviour, `render.min-contrast` removed.
    ("render.min-contrast", "colors.minimum-contrast"),
    ("scrollback.limit-bytes", "scrollback.bytes"),
    ("scrollback.limit-lines", "scrollback.lines"),
];

/// The new path for a renamed key.
pub fn renamed(path: &str) -> Option<&'static str> {
    RENAMED
        .iter()
        .find(|(old, _)| *old == path)
        .map(|(_, new)| *new)
}

/// Validate the argument of a `version` node.
pub fn check_version(value: Option<i128>) -> Result<u32, String> {
    match value {
        Some(v) if v >= 1 && v <= CURRENT_VERSION as i128 => Ok(v as u32),
        Some(v) if v > CURRENT_VERSION as i128 => Err(format!(
            "config version {v} is newer than this build understands ({CURRENT_VERSION})"
        )),
        _ => Err("`version` takes a whole number, e.g. `version 1`".to_string()),
    }
}
