// SPDX-License-Identifier: AGPL-3.0-only
//! The fuzzy primitives shared by settings search and the launchers: edit
//! distance, the typo allowance for a word length, and an abbreviation
//! (subsequence) match. Pure functions over bytes; what counts as a match, and
//! how it ranks, is each caller's business.

/// Longest word the typo tier compares, so the edit distance runs on the
/// stack.
pub const MAX_WORD: usize = 32;

/// Greedy subsequence match, counting letters that land on word starts.
/// With `prefer_starts` each letter takes the next word-start occurrence when
/// there is one, which finds the better reading but can miss a match the
/// plain greedy pass finds.
pub fn subsequence(text: &[u8], t: &[u8], prefer_starts: bool) -> Option<usize> {
    let at_start = |i: usize| i == 0 || text[i - 1] == b' ';
    let mut pos = (0..text.len()).find(|&i| text[i] == t[0] && at_start(i))?;
    let mut starts = 0;
    for &c in t {
        let hit = if prefer_starts {
            (pos..text.len()).find(|&i| text[i] == c && at_start(i))
        } else {
            None
        };
        let at = hit.or_else(|| (pos..text.len()).find(|&i| text[i] == c))?;
        starts += usize::from(at_start(at));
        pos = at + 1;
    }
    Some(starts)
}

/// Edits a word of this length may carry and still match: none under four
/// letters, one from four, two from seven.
pub fn typo_budget(term: &str) -> usize {
    match term.len() {
        0..=3 => 0,
        4..=6 => 1,
        _ => 2,
    }
}

/// Optimal-string-alignment (restricted Damerau–Levenshtein) distance:
/// insertions, deletions, substitutions and adjacent transpositions.
pub fn osa(a: &[u8], b: &[u8]) -> usize {
    let (n, m) = (a.len(), b.len());
    if n > MAX_WORD || m > MAX_WORD {
        return usize::MAX;
    }
    let mut prev2 = [0usize; MAX_WORD + 1];
    let mut prev = [0usize; MAX_WORD + 1];
    let mut cur = [0usize; MAX_WORD + 1];
    for (j, p) in prev.iter_mut().enumerate().take(m + 1) {
        *p = j;
    }
    for i in 1..=n {
        cur[0] = i;
        for j in 1..=m {
            let cost = usize::from(a[i - 1] != b[j - 1]);
            let mut v = (prev[j] + 1).min(cur[j - 1] + 1).min(prev[j - 1] + cost);
            if i > 1 && j > 1 && a[i - 1] == b[j - 2] && a[i - 2] == b[j - 1] {
                v = v.min(prev2[j - 2] + 1);
            }
            cur[j] = v;
        }
        prev2 = prev;
        prev = cur;
    }
    prev[m]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn osa_counts_a_transposition_as_one() {
        assert_eq!(osa(b"firefox", b"fierfox"), 1);
        assert_eq!(osa(b"kitten", b"sitting"), 3);
        assert_eq!(osa(b"", b"abc"), 3);
        assert_eq!(osa(b"same", b"same"), 0);
        assert_eq!(osa(&[b'a'; MAX_WORD + 1], b"a"), usize::MAX);
    }

    #[test]
    fn a_typo_allowance_grows_with_the_word() {
        assert_eq!(typo_budget("abc"), 0);
        assert_eq!(typo_budget("abcd"), 1);
        assert_eq!(typo_budget("abcdefg"), 2);
    }

    #[test]
    fn a_subsequence_starts_at_a_word_start() {
        assert_eq!(subsequence(b"visual studio code", b"vsc", true), Some(3));
        assert_eq!(subsequence(b"visual studio code", b"xyz", true), None);
        // The first letter must open a word.
        assert_eq!(subsequence(b"firefox", b"ire", false), None);
    }
}
