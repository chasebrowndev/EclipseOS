// SPDX-License-Identifier: Apache-2.0
//! Wire limits (T-12 §1, T-18 §5). Only [`MAX_LINE_BYTES`] is fixed by the spec; the
//! others are this crate's proposals and are listed as such in the M29a report.

/// Longest line, excluding the newline (T-12 §1).
pub const MAX_LINE_BYTES: usize = 1024 * 1024;
/// Longest error message put on the wire; longer text is cut at a char boundary.
pub const MAX_ERROR_MESSAGE_BYTES: usize = 512;
/// Longest target / match expression (same budget as an event string field).
pub const MAX_TARGET_BYTES: usize = 4096;
/// Longest short string (client name, title, app id, profile, layout, key names).
pub const MAX_SHORT_BYTES: usize = 1024;
/// Longest path.
pub const MAX_PATH_BYTES: usize = 4096;
/// Most capability names in `want`/`granted`.
pub const MAX_CAPS: usize = 16;
/// Most features in a hello reply.
pub const MAX_FEATURES: usize = 64;
/// Most key chords in one `input.send-keys`.
pub const MAX_KEYS: usize = 1024;
/// Most argv elements in a `launch`.
pub const MAX_ARGV: usize = 4096;
/// Most environment entries / overrides in a `launch`.
pub const MAX_ENV: usize = 4096;
/// Most `-o key=value` overrides in a `launch`.
pub const MAX_OVERRIDES: usize = 256;
/// Longest single argv element, env value or override value.
pub const MAX_ELEM_BYTES: usize = 64 * 1024;
/// Longest activation token.
pub const MAX_TOKEN_BYTES: usize = 1024;
/// Most event names in one `subscribe`.
pub const MAX_SUBSCRIBE_EVENTS: usize = 64;
