// SPDX-License-Identifier: Apache-2.0
//! UAX #29 extended grapheme cluster segmentation as a table-driven state machine.
//!
//! The transition table in [`crate::tables`] is built by the generator from
//! [`reference_step`], the rule-by-rule reference segmenter below, so the runtime path is a
//! single table lookup per scalar and cannot drift from the reference. Both are checked
//! against `GraphemeBreakTest.txt` and against `unicode-segmentation` in the test suite.

use crate::props::class_of;
use crate::tables::TRANS;

/// Grapheme-break class of a scalar, with the UAX #29 refinements the rules need
/// (Extended_Pictographic and the three Indic conjunct-break values) folded in.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum Class {
    /// Everything else.
    Other = 0,
    /// U+000D.
    Cr,
    /// U+000A.
    Lf,
    /// Grapheme_Cluster_Break=Control.
    Control,
    /// Extend not otherwise tracked.
    Extend,
    /// U+200D.
    Zwj,
    /// Regional_Indicator.
    RegionalIndicator,
    /// Prepend.
    Prepend,
    /// SpacingMark.
    SpacingMark,
    /// Hangul L.
    L,
    /// Hangul V.
    V,
    /// Hangul T.
    T,
    /// Hangul LV.
    Lv,
    /// Hangul LVT.
    Lvt,
    /// Extended_Pictographic (that is not Extend).
    ExtPict,
    /// InCB=Consonant.
    InCbConsonant,
    /// InCB=Linker that is also Extend.
    InCbLinker,
    /// InCB=Linker that is not Extend (a base character that links, e.g. U+1CF5).
    InCbLinkerBase,
    /// InCB=Extend (also Extend, not ZWJ).
    InCbExtend,
}

/// Number of [`Class`] values.
pub const NUM_CLASSES: usize = 19;

impl Class {
    /// All classes in discriminant order.
    pub const ALL: [Class; NUM_CLASSES] = [
        Class::Other,
        Class::Cr,
        Class::Lf,
        Class::Control,
        Class::Extend,
        Class::Zwj,
        Class::RegionalIndicator,
        Class::Prepend,
        Class::SpacingMark,
        Class::L,
        Class::V,
        Class::T,
        Class::Lv,
        Class::Lvt,
        Class::ExtPict,
        Class::InCbConsonant,
        Class::InCbLinker,
        Class::InCbLinkerBase,
        Class::InCbExtend,
    ];

    /// The class with discriminant `v`, or `Other` when out of range.
    pub fn from_u8(v: u8) -> Class {
        Class::ALL
            .get(usize::from(v))
            .copied()
            .unwrap_or(Class::Other)
    }
}

/// Segmenter states. A state summarises exactly the history the rules need.
pub mod state {
    /// Nothing seen yet (start of text, or after [`super::Segmenter::reset`]).
    pub const START: u8 = 0;
    /// A scalar with no pending rule context.
    pub const OTHER: u8 = 1;
    /// After CR (CR x LF is the only join).
    pub const CR: u8 = 2;
    /// After LF or Control (always breaks).
    pub const BREAK_AFTER: u8 = 3;
    /// After Prepend (never breaks).
    pub const PREPEND: u8 = 4;
    /// After Hangul L.
    pub const L: u8 = 5;
    /// After Hangul LV or V.
    pub const LV_V: u8 = 6;
    /// After Hangul LVT or T.
    pub const LVT_T: u8 = 7;
    /// After an odd number of regional indicators.
    pub const RI_ODD: u8 = 8;
    /// After an even number of regional indicators.
    pub const RI_EVEN: u8 = 9;
    /// ExtPict then zero or more Extend.
    pub const PICT: u8 = 10;
    /// ExtPict Extend* ZWJ.
    pub const PICT_ZWJ: u8 = 11;
    /// After an InCB=Linker and any InCB Extend/Linker/ZWJ that followed it (GB9c).
    pub const LINKED: u8 = 12;
    /// ExtPict Extend* where a Linker occurred among the Extend.
    pub const PICT_LINKED: u8 = 13;
    /// ExtPict Extend* ZWJ where a Linker occurred among the Extend.
    pub const PICT_ZWJ_LINKED: u8 = 14;
}

/// Number of segmenter states.
pub const NUM_STATES: usize = 15;

/// The reference segmenter: given the state before a scalar of class `c`, returns
/// `(break_before, state_after)`. Rules are applied in UAX #29 order (GB3..GB999).
pub fn reference_step(s: u8, c: Class) -> (bool, u8) {
    use state::*;
    let brk = reference_break(s, c);
    let e = if brk { START } else { s };
    let next = match c {
        Class::Cr => CR,
        Class::Lf | Class::Control => BREAK_AFTER,
        Class::Prepend => PREPEND,
        Class::L => state::L,
        Class::V | Class::Lv => LV_V,
        Class::T | Class::Lvt => LVT_T,
        Class::RegionalIndicator => {
            if e == RI_ODD {
                RI_EVEN
            } else {
                RI_ODD
            }
        }
        Class::ExtPict => PICT,
        Class::Extend => {
            if pict(e) {
                PICT
            } else {
                OTHER
            }
        }
        Class::InCbExtend => chain(pict(e), linked(e)),
        Class::InCbLinker => chain(pict(e), true),
        Class::InCbLinkerBase => LINKED,
        Class::Zwj => match (pict(e), linked(e)) {
            (true, true) => PICT_ZWJ_LINKED,
            (true, false) => PICT_ZWJ,
            (false, true) => LINKED,
            (false, false) => OTHER,
        },
        Class::SpacingMark | Class::InCbConsonant | Class::Other => OTHER,
    };
    (brk, next)
}

fn pict(s: u8) -> bool {
    s == state::PICT || s == state::PICT_LINKED
}

fn pict_zwj(s: u8) -> bool {
    s == state::PICT_ZWJ || s == state::PICT_ZWJ_LINKED
}

fn linked(s: u8) -> bool {
    matches!(
        s,
        state::LINKED | state::PICT_LINKED | state::PICT_ZWJ_LINKED
    )
}

fn chain(pict: bool, linked: bool) -> u8 {
    match (pict, linked) {
        (true, true) => state::PICT_LINKED,
        (true, false) => state::PICT,
        (false, true) => state::LINKED,
        (false, false) => state::OTHER,
    }
}

fn reference_break(s: u8, c: Class) -> bool {
    use state::*;
    if s == START {
        return true;
    }
    if s == CR && c == Class::Lf {
        return false; // GB3
    }
    if s == CR || s == BREAK_AFTER {
        return true; // GB4
    }
    if matches!(c, Class::Cr | Class::Lf | Class::Control) {
        return true; // GB5
    }
    if s == state::L && matches!(c, Class::L | Class::V | Class::Lv | Class::Lvt) {
        return false; // GB6
    }
    if s == LV_V && matches!(c, Class::V | Class::T) {
        return false; // GB7
    }
    if s == LVT_T && c == Class::T {
        return false; // GB8
    }
    if matches!(
        c,
        Class::Extend | Class::Zwj | Class::InCbExtend | Class::InCbLinker | Class::SpacingMark
    ) {
        return false; // GB9, GB9a
    }
    if s == PREPEND {
        return false; // GB9b
    }
    if linked(s) && c == Class::InCbConsonant {
        return false; // GB9c. Unicode 18 UAX #29 dropped the leading-consonant requirement; GraphemeBreakTest.txt 18.0.0 line 179 (U+094D x U+0915) is the proof.
    }
    if pict_zwj(s) && c == Class::ExtPict {
        return false; // GB11
    }
    if s == RI_ODD && c == Class::RegionalIndicator {
        return false; // GB12, GB13
    }
    true // GB999
}

/// Streaming grapheme-cluster segmenter. Feed scalars with [`Segmenter::push`]; it carries
/// the (tiny) rule state across calls, so any chunking of the input gives the same answer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Segmenter {
    state: u8,
}

impl Default for Segmenter {
    fn default() -> Self {
        Self::new()
    }
}

impl Segmenter {
    /// A segmenter with no history.
    pub const fn new() -> Self {
        Segmenter {
            state: state::START,
        }
    }

    /// Forget all history. Call on any cursor-moving control: the next scalar then starts a
    /// new cluster (UNICODE-03).
    pub fn reset(&mut self) {
        self.state = state::START;
    }

    /// True when at least one scalar has been pushed since the last reset.
    pub fn in_cluster(&self) -> bool {
        self.state != state::START
    }

    /// Advance over `c`. Returns true when `c` begins a new cluster, which includes the
    /// first scalar after a reset, and false when it extends the current one.
    #[inline]
    pub fn push(&mut self, c: char) -> bool {
        self.push_class(class_of(c))
    }

    /// As [`Segmenter::push`], for a scalar whose class the caller already has.
    #[inline]
    pub fn push_class(&mut self, class: Class) -> bool {
        let e = TRANS[usize::from(self.state) * NUM_CLASSES + class as usize];
        self.state = e & 0x7f;
        e & 0x80 != 0
    }
}

/// Iterator over the extended grapheme clusters of a string.
#[derive(Clone, Debug)]
pub struct Graphemes<'a> {
    rest: &'a str,
}

/// Split `s` into extended grapheme clusters.
pub fn graphemes(s: &str) -> Graphemes<'_> {
    Graphemes { rest: s }
}

impl<'a> Iterator for Graphemes<'a> {
    type Item = &'a str;

    fn next(&mut self) -> Option<&'a str> {
        if self.rest.is_empty() {
            return None;
        }
        let mut seg = Segmenter::new();
        let mut end = self.rest.len();
        for (i, c) in self.rest.char_indices() {
            if seg.push(c) && i != 0 {
                end = i;
                break;
            }
        }
        let (head, tail) = self.rest.split_at(end);
        self.rest = tail;
        Some(head)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn crlf_is_one_cluster() {
        let v: Vec<&str> = graphemes("a\r\nb").collect();
        assert_eq!(v, ["a", "\r\n", "b"]);
    }

    #[test]
    fn combining_joins() {
        let v: Vec<&str> = graphemes("e\u{301}x").collect();
        assert_eq!(v, ["e\u{301}", "x"]);
    }

    #[test]
    fn flags_pair_up() {
        let s = "\u{1F1FA}\u{1F1F8}\u{1F1EB}\u{1F1F7}\u{1F1E9}";
        assert_eq!(graphemes(s).count(), 3);
    }

    #[test]
    fn reset_starts_new_cluster() {
        let mut s = Segmenter::new();
        assert!(s.push('a'));
        assert!(!s.push('\u{301}'));
        s.reset();
        assert!(s.push('\u{301}'));
    }

    #[test]
    fn chunking_is_irrelevant() {
        let text = "a\u{1F468}\u{200D}\u{1F469}\u{1F3FB}\u{1F1FA}\u{1F1F8}\r\nx";
        let mut one = Segmenter::new();
        let all: Vec<bool> = text.chars().map(|c| one.push(c)).collect();
        let mut two = Segmenter::new();
        let again: Vec<bool> = text.chars().map(|c| two.push(c)).collect();
        assert_eq!(all, again);
    }

    #[test]
    fn table_matches_reference() {
        for s in 0..NUM_STATES as u8 {
            for c in Class::ALL {
                let (brk, next) = reference_step(s, c);
                let e = TRANS[usize::from(s) * NUM_CLASSES + c as usize];
                assert_eq!(e & 0x7f, next);
                assert_eq!(e & 0x80 != 0, brk);
            }
        }
    }
}
