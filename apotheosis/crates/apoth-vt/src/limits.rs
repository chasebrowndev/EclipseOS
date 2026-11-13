// SPDX-License-Identifier: Apache-2.0
//! Every compile-time limit of the parser, in one place (T-01 §4).

/// Maximum parameters kept per CSI/DCS; further ones set `Params::overflow`.
pub const MAX_PARAMS: usize = 64;
/// Maximum intermediate bytes per sequence; more sends CSI/DCS to its ignore state.
pub const MAX_INTERMEDIATES: usize = 4;
/// Size of the string head buffer (OSC command + selector, DCS header, APC first byte).
pub const HEAD_LEN: usize = 32;
/// Longest partial UTF-8 code point carried across calls.
pub const UTF8_CARRY: usize = 3;
