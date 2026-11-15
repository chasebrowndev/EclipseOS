// SPDX-License-Identifier: Apache-2.0
//! The pure-Rust `term(5)` writer, legacy (16-bit numbers) and extended-number (32-bit)
//! layouts, each with the user-defined capability section ncurses reads with `tic -x`.
//!
//! Layout (all integers little-endian): header of six 16-bit words (magic, names size,
//! bool count, number count, string-offset count, string-table size); names (NUL ended);
//! booleans; a pad byte if the position is odd; numbers; string offsets; string table.
//! The extended section follows, preceded by a pad byte if the position is odd: five
//! 16-bit words (bool, number, string counts, total offset count, table size), booleans,
//! pad if odd, numbers, one offset per string value then one per capability name, then
//! the value strings followed by the name strings (name offsets are relative to the
//! start of the names).

use crate::std_names::{BOOL_NAMES, NUM_NAMES, STR_NAMES};
use crate::{Cap, Entry, Value};

/// Number width of the compiled file.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NumFormat {
    /// Magic `0x011A`, 16-bit numbers (at most 32767).
    Legacy16,
    /// Magic `0x021E`, 32-bit numbers.
    Extended32,
}

/// Why an entry cannot be compiled.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CompileError {
    /// A number does not fit the chosen width.
    NumberTooLarge {
        /// Capability name.
        name: &'static str,
        /// Its value.
        value: u32,
    },
    /// A string value contains NUL or a non-ASCII byte.
    BadString(&'static str),
    /// A section exceeds what the 16-bit header words can describe.
    TooLarge,
}

impl std::fmt::Display for CompileError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CompileError::NumberTooLarge { name, value } => {
                write!(f, "number {name}#{value} does not fit the chosen format")
            }
            CompileError::BadString(n) => write!(f, "capability {n} has a NUL or non-ASCII byte"),
            CompileError::TooLarge => f.write_str("entry too large for the term(5) header"),
        }
    }
}

impl std::error::Error for CompileError {}

const ABSENT16: u16 = 0xFFFF;
const MAX16: u32 = 0x7FFF;
const MAX32: u32 = 0x7FFF_FFFF;

fn push16(out: &mut Vec<u8>, v: usize) -> Result<(), CompileError> {
    let v = u16::try_from(v).map_err(|_| CompileError::TooLarge)?;
    out.extend_from_slice(&v.to_le_bytes());
    Ok(())
}

fn push_num(out: &mut Vec<u8>, v: Option<u32>, fmt: NumFormat) {
    match fmt {
        NumFormat::Legacy16 => {
            out.extend_from_slice(&v.map_or(ABSENT16, |v| v as u16).to_le_bytes());
        }
        NumFormat::Extended32 => out.extend_from_slice(&v.unwrap_or(u32::MAX).to_le_bytes()),
    }
}

fn pad_even(out: &mut Vec<u8>) {
    if out.len() % 2 == 1 {
        out.push(0);
    }
}

fn index_of(table: &[&str], name: &str) -> Option<usize> {
    table.iter().position(|n| *n == name)
}

/// Compile an entry. `fmt = None` picks `Extended32` when any number exceeds 32767 and
/// `Legacy16` otherwise (what `tic` does); `Some(f)` forces a layout.
pub fn compile(entry: &Entry, fmt: Option<NumFormat>) -> Result<Vec<u8>, CompileError> {
    let any_big = entry
        .caps
        .iter()
        .any(|c| matches!(c.value, Value::Num(n) if n > MAX16));
    let fmt = fmt.unwrap_or(if any_big {
        NumFormat::Extended32
    } else {
        NumFormat::Legacy16
    });
    let max = match fmt {
        NumFormat::Legacy16 => MAX16,
        NumFormat::Extended32 => MAX32,
    };

    let mut std_b: Vec<(usize, &Cap)> = Vec::new();
    let mut std_n: Vec<(usize, &Cap)> = Vec::new();
    let mut std_s: Vec<(usize, &Cap)> = Vec::new();
    let (mut ext_b, mut ext_n, mut ext_s): (Vec<&Cap>, Vec<&Cap>, Vec<&Cap>) =
        (vec![], vec![], vec![]);
    for cap in &entry.caps {
        match cap.value {
            Value::Bool => match index_of(&BOOL_NAMES, cap.name) {
                Some(i) => std_b.push((i, cap)),
                None => ext_b.push(cap),
            },
            Value::Num(v) => {
                if v > max {
                    return Err(CompileError::NumberTooLarge {
                        name: cap.name,
                        value: v,
                    });
                }
                match index_of(&NUM_NAMES, cap.name) {
                    Some(i) => std_n.push((i, cap)),
                    None => ext_n.push(cap),
                }
            }
            Value::Str(s) => {
                if !s.is_ascii() || s.contains('\0') {
                    return Err(CompileError::BadString(cap.name));
                }
                match index_of(&STR_NAMES, cap.name) {
                    Some(i) => std_s.push((i, cap)),
                    None => ext_s.push(cap),
                }
            }
        }
    }
    for v in [&mut ext_b, &mut ext_n, &mut ext_s] {
        v.sort_by(|a, b| a.name.as_bytes().cmp(b.name.as_bytes()));
    }

    let nb = std_b.iter().map(|(i, _)| i + 1).max().unwrap_or(0);
    let nn = std_n.iter().map(|(i, _)| i + 1).max().unwrap_or(0);
    let ns = std_s.iter().map(|(i, _)| i + 1).max().unwrap_or(0);
    let mut bools = vec![0u8; nb];
    for (i, _) in &std_b {
        bools[*i] = 1;
    }
    let mut nums: Vec<Option<u32>> = vec![None; nn];
    for (i, c) in &std_n {
        if let Value::Num(v) = c.value {
            nums[*i] = Some(v);
        }
    }
    let mut strs: Vec<Option<&str>> = vec![None; ns];
    for (i, c) in &std_s {
        if let Value::Str(v) = c.value {
            strs[*i] = Some(v);
        }
    }

    let mut table = Vec::new();
    let mut offsets: Vec<u16> = Vec::with_capacity(ns);
    for s in &strs {
        match s {
            None => offsets.push(ABSENT16),
            Some(s) => {
                let off = u16::try_from(table.len()).map_err(|_| CompileError::TooLarge)?;
                if off >= 0xFFFE {
                    return Err(CompileError::TooLarge);
                }
                offsets.push(off);
                table.extend_from_slice(s.as_bytes());
                table.push(0);
            }
        }
    }

    let magic: u16 = match fmt {
        NumFormat::Legacy16 => 0x011A,
        NumFormat::Extended32 => 0x021E,
    };
    let mut out = Vec::new();
    out.extend_from_slice(&magic.to_le_bytes());
    push16(&mut out, entry.names.len() + 1)?;
    push16(&mut out, nb)?;
    push16(&mut out, nn)?;
    push16(&mut out, ns)?;
    push16(&mut out, table.len())?;
    out.extend_from_slice(entry.names.as_bytes());
    out.push(0);
    out.extend_from_slice(&bools);
    pad_even(&mut out);
    for n in &nums {
        push_num(&mut out, *n, fmt);
    }
    for o in &offsets {
        out.extend_from_slice(&o.to_le_bytes());
    }
    out.extend_from_slice(&table);

    if ext_b.is_empty() && ext_n.is_empty() && ext_s.is_empty() {
        return Ok(out);
    }

    pad_even(&mut out);
    let mut vals = Vec::new();
    let mut val_offs: Vec<usize> = Vec::new();
    for c in &ext_s {
        if let Value::Str(s) = c.value {
            val_offs.push(vals.len());
            vals.extend_from_slice(s.as_bytes());
            vals.push(0);
        }
    }
    let mut names = Vec::new();
    let mut name_offs: Vec<usize> = Vec::new();
    for c in ext_b.iter().chain(&ext_n).chain(&ext_s) {
        name_offs.push(names.len());
        names.extend_from_slice(c.name.as_bytes());
        names.push(0);
    }
    push16(&mut out, ext_b.len())?;
    push16(&mut out, ext_n.len())?;
    push16(&mut out, ext_s.len())?;
    push16(&mut out, val_offs.len() + name_offs.len())?;
    push16(&mut out, vals.len() + names.len())?;
    out.extend(std::iter::repeat_n(1u8, ext_b.len()));
    pad_even(&mut out);
    for c in &ext_n {
        if let Value::Num(v) = c.value {
            push_num(&mut out, Some(v), fmt);
        }
    }
    for o in val_offs.iter().chain(&name_offs) {
        push16(&mut out, *o)?;
    }
    out.extend_from_slice(&vals);
    out.extend_from_slice(&names);
    Ok(out)
}
