// SPDX-License-Identifier: AGPL-3.0-only
//! apoth-unicode segmenter: clusters concatenate to the input, `Segmenter::push` agrees with
//! `graphemes`, and a reset at any scalar equals segmenting the two halves separately.
#![no_main]
use apoth_unicode::{Segmenter, graphemes};
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let Some((&cut, rest)) = data.split_first() else {
        return;
    };
    let s = String::from_utf8_lossy(rest);
    let clusters: Vec<&str> = graphemes(&s).collect();
    assert_eq!(clusters.concat(), *s);
    assert!(clusters.iter().all(|c| !c.is_empty()));

    let mut seg = Segmenter::new();
    let starts = s.chars().filter(|&c| seg.push(c)).count();
    assert_eq!(starts, clusters.len());

    // Reset at a scalar boundary == two independent segmentations.
    let n = s.chars().count();
    let k = if n == 0 { 0 } else { cut as usize % (n + 1) };
    let byte = s.char_indices().nth(k).map_or(s.len(), |(i, _)| i);
    let (l, r) = s.split_at(byte);
    let mut seg = Segmenter::new();
    let mut with_reset = 0;
    for (i, c) in s.chars().enumerate() {
        if i == k {
            seg.reset();
        }
        with_reset += usize::from(seg.push(c));
    }
    assert_eq!(with_reset, graphemes(l).count() + graphemes(r).count());
});
