// SPDX-License-Identifier: Apache-2.0
//! The xterm-apotheosis terminfo entry as a Rust table, its term(5) compiler and the XTGETTCAP table.
//!
//! Governing spec: T-19 (`docs/ECLIPSEOS_SPECS_APOTHEOSIS.md`). One table
//! (`table.rs`) feeds three outputs that must agree:
//! - [`compile`]: the compiled `term(5)` binary, including the extended section;
//! - [`xtgettcap`]: the XTGETTCAP responder (sorted static table, binary search);
//! - [`source`]: the `tic`-compatible `.ti` text, in `infocmp -x -1` order.
//!
//! Also: device identification replies ([`da1`], [`da2`], [`da3`], [`xtversion`]) and the
//! terminfo probe / child environment ([`probe`], [`child_env`]).
//!
//! Owed (not delivered): T-19 §3's test that feeds each capability string through
//! `apoth-term` and asserts its effect. `apoth-term` has no feed API yet; this is an M13
//! follow-up for the slice that lands it, and `table.rs`'s `t03` column is its input.
//!
//! No log lines are emitted here; XTGETTCAP payloads are never logged (BRIEF rule 5).
#![forbid(unsafe_code)]

mod compile;
mod ident;
mod install;
mod probe;
mod source;
mod std_names;
mod table;
mod tcap;

pub use compile::{CompileError, NumFormat, compile};
pub use ident::{da1, da2, da2_semver, da3, parse_semver, xtversion};
pub use install::{tree, write_tree};
pub use probe::{FALLBACK_TERM, SYSTEM_DIRS, TermChoice, child_env, probe, probe_system};
pub use source::source;
pub use std_names::{BOOL_NAMES, NUM_NAMES, STR_NAMES};
pub use tcap::{TcapAnswer, lookup, xtgettcap, xtgettcap_flat};

/// A capability value.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Value {
    /// Boolean, present (we never write a false boolean).
    Bool,
    /// Numeric.
    Num(u32),
    /// String (ASCII, no NUL; bytes are the raw control sequence).
    Str(&'static str),
}

/// One capability and the T-03 sequence that implements it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Cap {
    /// Terminfo name, e.g. `Smulx`.
    pub name: &'static str,
    /// Value.
    pub value: Value,
    /// The T-03 sequence (or input rule) that implements it, in prose.
    pub t03: &'static str,
}

impl Cap {
    /// Boolean capability.
    pub const fn b(name: &'static str, t03: &'static str) -> Self {
        Self {
            name,
            value: Value::Bool,
            t03,
        }
    }
    /// Numeric capability.
    pub const fn n(name: &'static str, v: u32, t03: &'static str) -> Self {
        Self {
            name,
            value: Value::Num(v),
            t03,
        }
    }
    /// String capability.
    pub const fn s(name: &'static str, v: &'static str, t03: &'static str) -> Self {
        Self {
            name,
            value: Value::Str(v),
            t03,
        }
    }
}

/// The two shipped entries (T-19 §1).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Variant {
    /// `xterm-apotheosis`: 256 colours, `setaf`/`setab` in the `38;5` form.
    Indexed,
    /// `xterm-apotheosis-direct`: direct colour, colon `38:2` form.
    Direct,
}

impl Variant {
    /// Both variants, in install order.
    pub const ALL: [Variant; 2] = [Variant::Indexed, Variant::Direct];

    /// The `TERM` name.
    pub const fn term_name(self) -> &'static str {
        match self {
            Variant::Indexed => "xterm-apotheosis",
            Variant::Direct => "xterm-apotheosis-direct",
        }
    }

    /// The description field of the names line (no commas: `,` ends a terminfo field).
    pub const fn description(self) -> &'static str {
        match self {
            Variant::Indexed => "Apotheosis terminal with 256 colors",
            Variant::Direct => "Apotheosis terminal with direct-color indexing",
        }
    }
}

/// A terminfo entry: the names line and its capabilities, sorted by name (byte order).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Entry {
    /// The full names field, `name|alias|description`, without the trailing comma.
    pub names: String,
    /// Capabilities, sorted by name, names unique.
    pub caps: Vec<Cap>,
}

impl Entry {
    /// Look a capability up by terminfo name.
    pub fn get(&self, name: &str) -> Option<&Cap> {
        self.caps
            .binary_search_by(|c| c.name.as_bytes().cmp(name.as_bytes()))
            .ok()
            .map(|i| &self.caps[i])
    }
}

/// Build the entry for a variant from the single table.
pub fn entry(variant: Variant) -> Entry {
    let mut caps = table::INDEXED.to_vec();
    if variant == Variant::Direct {
        for d in table::DIRECT_DELTA {
            match caps.binary_search_by(|c| c.name.as_bytes().cmp(d.name.as_bytes())) {
                Ok(i) => caps[i] = *d,
                Err(i) => caps.insert(i, *d),
            }
        }
    }
    Entry {
        names: format!("{}|{}", variant.term_name(), variant.description()),
        caps,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tables_sorted_unique_ascii() {
        for t in [table::INDEXED, table::DIRECT_DELTA] {
            for w in t.windows(2) {
                assert!(
                    w[0].name.as_bytes() < w[1].name.as_bytes(),
                    "{} !< {}",
                    w[0].name,
                    w[1].name
                );
            }
            for c in t {
                assert!(!c.t03.is_empty(), "{} has no T-03 sequence", c.name);
                if let Value::Str(s) = c.value {
                    assert!(s.is_ascii() && !s.contains('\0'), "{}", c.name);
                }
            }
        }
        for v in Variant::ALL {
            let e = entry(v);
            for w in e.caps.windows(2) {
                assert!(w[0].name.as_bytes() < w[1].name.as_bytes());
            }
        }
    }

    #[test]
    fn std_name_tables_have_expected_anchors() {
        assert_eq!(BOOL_NAMES[1], "am");
        assert_eq!(NUM_NAMES[13], "colors");
        assert_eq!(STR_NAMES[1], "bel");
        assert_eq!(STR_NAMES[10], "cup");
        assert_eq!(STR_NAMES[359], "setaf");
        assert_eq!(STR_NAMES[360], "setab");
    }

    #[test]
    fn spec_capabilities_present() {
        let e = entry(Variant::Indexed);
        for n in [
            "Tc", "Su", "Smulx", "Setulc", "Smol", "Ss", "Se", "Cs", "Cr", "Ms", "Sync", "XT",
            "BD", "BE", "PS", "PE", "fe", "fd", "XF", "hs", "tsl", "fsl", "fullkbd", "rep", "E3",
            "XM", "kxIN",
        ] {
            assert!(e.get(n).is_some(), "{n}");
        }
        assert!(e.get("RGB").is_none());
        let d = entry(Variant::Direct);
        assert_eq!(d.get("RGB").map(|c| c.value), Some(Value::Bool));
        assert_eq!(
            d.get("colors").map(|c| c.value),
            Some(Value::Num(0x100_0000))
        );
        assert_eq!(e.get("colors").map(|c| c.value), Some(Value::Num(256)));
        let Some(Cap {
            value: Value::Str(af),
            ..
        }) = e.get("setaf")
        else {
            panic!()
        };
        assert!(af.contains("38;5;"));
        let Some(Cap {
            value: Value::Str(af),
            ..
        }) = d.get("setaf")
        else {
            panic!()
        };
        assert!(af.contains("38:2::"));
    }
}
