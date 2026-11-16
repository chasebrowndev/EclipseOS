// SPDX-License-Identifier: AGPL-3.0-only
//! Hint mode (T-14 §7, HINTS-01..08): matchers over a viewport snapshot and prefix-free
//! labels.
//!
//! Built-in matchers: URL, path with optional `:line[:col]`, git hash, IPv4, UUID, plus
//! OSC 8 links (resolved from the grid by the caller). Each match's trailing punctuation
//! and unbalanced closing brackets are trimmed and the trimmed span re-validated. Overlaps
//! go to the earlier, then the longer, then the higher-priority match.
//!
//! Pane text is user content: nothing here logs it.

use regex_automata::hybrid::regex::{Cache, Regex};
use regex_automata::util::syntax;
use regex_automata::{Input, nfa::thompson};

/// What a hint matched.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) enum Kind {
    /// An OSC 8 hyperlink (its URI, not its visible text).
    Link,
    Url,
    Path,
    Hash,
    Ip,
    Uuid,
}

impl Kind {
    /// Priority when two matches start together and are equally long (lower wins).
    fn priority(self) -> u8 {
        self as u8
    }

    /// The default action: open for URLs and links, copy for the rest.
    pub(crate) fn opens(self) -> bool {
        matches!(self, Kind::Link | Kind::Url)
    }

    /// Kebab name to kind (`hints.start` matcher set).
    pub(crate) fn parse(s: &str) -> Option<Kind> {
        Some(match s {
            "link" => Kind::Link,
            "url" => Kind::Url,
            "path" => Kind::Path,
            "hash" => Kind::Hash,
            "ip" => Kind::Ip,
            "uuid" => Kind::Uuid,
            _ => return None,
        })
    }
}

const PATTERNS: &[(Kind, &str)] = &[
    (
        Kind::Url,
        r#"(?:(?:https?|ftp|file|ssh|git|sftp)://|mailto:)[^\s<>"'`\x00-\x1f]+"#,
    ),
    (
        Kind::Uuid,
        r"(?-u:\b)[0-9a-fA-F]{8}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{12}(?-u:\b)",
    ),
    (
        Kind::Path,
        r"(?:(?:~|\.\.?)?/[\w.@%+~-]+(?:/[\w.@%+~-]*)*|[\w.@%+-]+(?:/[\w.@%+~-]+)+|[\w@%+-][\w.@%+-]*\.[A-Za-z]\w*:\d+)(?::\d+(?::\d+)?)?",
    ),
    (Kind::Hash, r"(?-u:\b)[0-9a-f]{7,40}(?-u:\b)"),
    (Kind::Ip, r"(?-u:\b)(?:\d{1,3}\.){3}\d{1,3}(?-u:\b)"),
];

/// A match in one line of text (byte range).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Found {
    pub(crate) start: usize,
    pub(crate) end: usize,
    pub(crate) kind: Kind,
}

/// The compiled built-in matchers.
pub(crate) struct Matchers {
    set: Vec<(Kind, Regex, Cache)>,
}

impl std::fmt::Debug for Matchers {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Matchers").finish_non_exhaustive()
    }
}

impl Matchers {
    /// Compile the built-ins, limited to `only` when it is non-empty.
    pub(crate) fn new(only: &[Kind]) -> Matchers {
        let set = PATTERNS
            .iter()
            .filter(|(k, _)| only.is_empty() || only.contains(k))
            .filter_map(|(k, p)| {
                let re = Regex::builder()
                    .syntax(syntax::Config::new().unicode(true))
                    .thompson(thompson::Config::new().nfa_size_limit(Some(1 << 20)))
                    .build(p)
                    .ok()?;
                let c = re.create_cache();
                Some((*k, re, c))
            })
            .collect();
        Matchers { set }
    }

    /// Every match in `text`, trimmed, validated and with overlaps resolved, ascending.
    pub(crate) fn find(&mut self, text: &str) -> Vec<Found> {
        let mut all = Vec::new();
        for (kind, re, cache) in &mut self.set {
            let mut at = 0;
            while at < text.len() {
                let Ok(Some(m)) = re.try_search(cache, &Input::new(text).range(at..)) else {
                    break;
                };
                let (s, e) = (m.start(), m.end());
                at = if e > s {
                    e
                } else {
                    s + text[s..].chars().next().map_or(1, char::len_utf8)
                };
                let e = trim(text, s, e);
                if e > s && valid(*kind, &text[s..e]) {
                    all.push(Found {
                        start: s,
                        end: e,
                        kind: *kind,
                    });
                }
            }
        }
        resolve(all)
    }
}

/// Trim trailing punctuation and unbalanced closing brackets from `text[s..e]`.
pub(crate) fn trim(text: &str, s: usize, mut e: usize) -> usize {
    loop {
        let t = &text[s..e];
        let Some(last) = t.chars().next_back() else {
            return e;
        };
        let drop = match last {
            '.' | ',' | ';' | ':' | '!' | '?' | '\'' | '"' | '`' => true,
            ')' | ']' | '}' | '>' => {
                let open = match last {
                    ')' => '(',
                    ']' => '[',
                    '}' => '{',
                    _ => '<',
                };
                let opens = t.chars().filter(|&c| c == open).count();
                let closes = t.chars().filter(|&c| c == last).count();
                closes > opens
            }
            _ => false,
        };
        if !drop {
            return e;
        }
        e -= last.len_utf8();
    }
}

/// Re-validate a trimmed span.
pub(crate) fn valid(kind: Kind, s: &str) -> bool {
    match kind {
        Kind::Link => !s.is_empty(),
        Kind::Url => match s.find("://") {
            Some(i) => s.len() > i + 3,
            None => s.len() > "mailto:".len() && s.contains('@'),
        },
        Kind::Path => {
            // A bare `/` or `.` or a `word:line` with no extension is not a path hint.
            let core = s.split(':').next().unwrap_or("");
            core.len() > 1 && core.chars().any(|c| c.is_alphanumeric())
        }
        Kind::Hash => {
            s.chars().any(|c| c.is_ascii_digit()) && s.chars().any(|c| c.is_ascii_alphabetic())
        }
        Kind::Ip => s
            .split('.')
            .all(|o| o.parse::<u16>().is_ok_and(|n| n <= 255)),
        Kind::Uuid => s.len() == 36,
    }
}

/// Resolve overlaps: earlier start wins, then longer, then priority.
pub(crate) fn resolve(mut all: Vec<Found>) -> Vec<Found> {
    all.sort_by(|a, b| {
        a.start
            .cmp(&b.start)
            .then(b.end.cmp(&a.end))
            .then(a.kind.priority().cmp(&b.kind.priority()))
    });
    let mut out: Vec<Found> = Vec::with_capacity(all.len());
    for f in all {
        if out.last().is_some_and(|l| f.start < l.end) {
            continue;
        }
        out.push(f);
    }
    out
}

/// `n` prefix-free labels over `alphabet`, of minimal total length: the label tree is
/// grown by expanding the shortest leaves until there are `n` of them. Labels nearest the
/// front of the list are the shortest. An alphabet of fewer than two distinct characters
/// gives numbered labels over `0-9` instead.
pub(crate) fn labels(alphabet: &str, n: usize) -> Vec<String> {
    let mut chars: Vec<char> = Vec::new();
    for c in alphabet.chars() {
        if !c.is_whitespace() && !chars.contains(&c) {
            chars.push(c);
        }
    }
    if chars.len() < 2 {
        chars = ('0'..='9').collect();
    }
    let k = chars.len();
    if n == 0 {
        return Vec::new();
    }
    // Leaves in order; expanding the first leaf replaces it with k children at the end,
    // which keeps the queue sorted by length.
    let mut leaves: std::collections::VecDeque<String> =
        chars.iter().map(|c| c.to_string()).collect();
    while leaves.len() < n {
        let need = n - leaves.len();
        let Some(head) = leaves.pop_front() else {
            break;
        };
        // Expanding a leaf turns 1 label into k (net k-1); use only as many children as
        // needed so the last level stays short.
        let take = (need + 1).min(k);
        for c in chars.iter().take(take) {
            leaves.push_back(format!("{head}{c}"));
        }
    }
    let mut v: Vec<String> = leaves.into_iter().take(n).collect();
    v.sort_by_key(String::len);
    v
}

/// The label a typed prefix selects: `Some(i)` on an exact match, `None` while it is still
/// a prefix of some label or matches nothing.
pub(crate) fn pick(labels: &[String], typed: &str) -> Pick {
    if let Some(i) = labels.iter().position(|l| l == typed) {
        return Pick::Hit(i);
    }
    if labels.iter().any(|l| l.starts_with(typed)) {
        Pick::Prefix
    } else {
        Pick::None
    }
}

/// Result of [`pick`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Pick {
    Hit(usize),
    Prefix,
    None,
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    fn kinds(text: &str) -> Vec<(Kind, &str)> {
        let mut m = Matchers::new(&[]);
        m.find(text)
            .into_iter()
            .map(|f| (f.kind, &text[f.start..f.end]))
            .collect()
    }

    #[test]
    fn builtin_matchers() {
        assert_eq!(
            kinds("see https://example.com/a(b)c. and (https://x.org/y) now"),
            vec![
                (Kind::Url, "https://example.com/a(b)c"),
                (Kind::Url, "https://x.org/y")
            ]
        );
        assert_eq!(
            kinds("error at src/main.rs:12:5: oops"),
            vec![(Kind::Path, "src/main.rs:12:5")]
        );
        assert_eq!(
            kinds("open ~/notes.txt,"),
            vec![(Kind::Path, "~/notes.txt")]
        );
        assert_eq!(kinds("lib.rs:40 here"), vec![(Kind::Path, "lib.rs:40")]);
        assert_eq!(
            kinds("commit 1a2b3c4d merged"),
            vec![(Kind::Hash, "1a2b3c4d")]
        );
        assert_eq!(kinds("deadbeefcafe"), vec![], "a hash needs a digit");
        assert_eq!(kinds("1234567"), vec![], "and a letter");
        assert_eq!(kinds("ping 10.0.0.255."), vec![(Kind::Ip, "10.0.0.255")]);
        assert_eq!(kinds("bad 10.0.0.256"), vec![]);
        assert_eq!(
            kinds("id 123e4567-e89b-12d3-a456-426614174000 ok"),
            vec![(Kind::Uuid, "123e4567-e89b-12d3-a456-426614174000")]
        );
        assert_eq!(
            kinds("mail mailto:a@b.c."),
            vec![(Kind::Url, "mailto:a@b.c")]
        );
    }

    #[test]
    fn trimming_revalidates() {
        // Trimming `https://` leaves nothing past the scheme: no match.
        assert_eq!(kinds("x https://. y"), vec![]);
        assert_eq!(kinds("'(/tmp/a)'"), vec![(Kind::Path, "/tmp/a")]);
    }

    #[test]
    fn matcher_subset() {
        let mut m = Matchers::new(&[Kind::Ip]);
        let f = m.find("https://1.2.3.4/x 1a2b3c4d");
        assert_eq!(f.len(), 1);
        assert_eq!(f[0].kind, Kind::Ip);
    }

    #[test]
    fn labels_small_cases() {
        assert_eq!(labels("asdf", 3), vec!["a", "s", "d"]);
        assert_eq!(labels("asdf", 4), vec!["a", "s", "d", "f"]);
        assert_eq!(labels("asdf", 5), vec!["s", "d", "f", "aa", "as"]);
        assert_eq!(labels("", 2), vec!["0", "1"]);
        assert_eq!(pick(&labels("ab", 3), "a"), Pick::Prefix);
    }

    fn prefix_free(v: &[String]) -> bool {
        v.iter().enumerate().all(|(i, a)| {
            v.iter()
                .enumerate()
                .all(|(j, b)| i == j || !b.starts_with(a.as_str()))
        })
    }

    /// The minimal total length of `n` prefix-free words over `k` letters (Kraft: a
    /// complete k-ary tree with n leaves, shallowest possible).
    fn optimal_total(k: usize, n: usize) -> usize {
        // Greedy: fill depth by depth.
        let mut best = usize::MAX;
        // Leaves at depth d and d+1 only, for the smallest d with k^(d+1) >= n.
        let mut d = 1;
        while k.pow(d as u32) < n {
            d += 1;
        }
        if d == 1 {
            return n;
        }
        // All at depth d works; try moving some to depth d-1.
        let full = k.pow(d as u32 - 1);
        for short in 0..=full {
            // `short` leaves at depth d-1 use `short` of the `full` internal slots; the
            // rest expand to at most k children each.
            let room = (full - short) * k;
            if short + room >= n && short <= n {
                best = best.min(short * (d - 1) + (n - short) * d);
            }
        }
        best
    }

    proptest! {
        #[test]
        fn labels_are_prefix_free_unique_and_minimal(k in 2usize..8, n in 1usize..400) {
            let alpha: String = "asdfghjkl".chars().take(k).collect();
            let v = labels(&alpha, n);
            prop_assert_eq!(v.len(), n);
            prop_assert!(prefix_free(&v));
            let total: usize = v.iter().map(String::len).sum();
            prop_assert_eq!(total, optimal_total(k, n));
            for (i, l) in v.iter().enumerate() {
                prop_assert_eq!(pick(&v, l), Pick::Hit(i));
            }
        }
    }
}
