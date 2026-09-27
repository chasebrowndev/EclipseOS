// SPDX-License-Identifier: AGPL-3.0-only

//! Natural sort, computed in `fogd` (FOG §Performance model, technique 7).

use std::cmp::Ordering;

use fog_proto::{Entry, Kind};

/// Display order of `entries` as an index permutation: directories first,
/// then natural name order.
pub fn order(entries: &[Entry]) -> Vec<u32> {
    let mut idx: Vec<u32> = (0..entries.len() as u32).collect();
    idx.sort_unstable_by(|&a, &b| compare(&entries[a as usize], &entries[b as usize]));
    idx
}

/// Directories first, then [`natural`] by name.
pub fn compare(a: &Entry, b: &Entry) -> Ordering {
    (b.kind == Kind::Dir)
        .cmp(&(a.kind == Kind::Dir))
        .then_with(|| natural(&a.name, &b.name))
}

/// Natural byte-string order: digit runs compare by numeric value (leading
/// zeros ignored), everything else ASCII case-insensitively. Ties fall back
/// to the raw bytes, so this is a total order.
pub fn natural(a: &[u8], b: &[u8]) -> Ordering {
    let (mut i, mut j) = (0, 0);
    while i < a.len() && j < b.len() {
        if a[i].is_ascii_digit() && b[j].is_ascii_digit() {
            let (ra, ni) = digit_run(a, i);
            let (rb, nj) = digit_run(b, j);
            let ord = ra.len().cmp(&rb.len()).then_with(|| ra.cmp(rb));
            if ord != Ordering::Equal {
                return ord;
            }
            i = ni;
            j = nj;
        } else {
            let ord = a[i].to_ascii_lowercase().cmp(&b[j].to_ascii_lowercase());
            if ord != Ordering::Equal {
                return ord;
            }
            i += 1;
            j += 1;
        }
    }
    (a.len() - i).cmp(&(b.len() - j)).then_with(|| a.cmp(b))
}

/// The digit run starting at `start` with leading zeros stripped, and the
/// index just past the run.
fn digit_run(s: &[u8], start: usize) -> (&[u8], usize) {
    let end = s[start..]
        .iter()
        .position(|c| !c.is_ascii_digit())
        .map_or(s.len(), |n| start + n);
    let run = &s[start..end];
    let nz = run.iter().position(|&c| c != b'0').unwrap_or(run.len());
    (&run[nz..], end)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sorted(names: &[&str]) -> Vec<String> {
        let mut v: Vec<&str> = names.to_vec();
        v.sort_by(|a, b| natural(a.as_bytes(), b.as_bytes()));
        v.into_iter().map(String::from).collect()
    }

    #[test]
    fn natural_cases() {
        assert_eq!(
            sorted(&["file10", "file2", "file1", "File3"]),
            ["file1", "file2", "File3", "file10"]
        );
        assert_eq!(natural(b"a01", b"a1"), Ordering::Less); // raw tiebreak
        assert_eq!(natural(b"a1", b"a01"), Ordering::Greater);
        assert_eq!(natural(b"a007b", b"a7c"), Ordering::Less);
        assert_eq!(natural(b"ABC", b"abc"), Ordering::Less); // raw tiebreak
        assert_eq!(natural(b"abc", b"abd"), Ordering::Less);
        assert_eq!(natural(b"a", b"a0"), Ordering::Less);
        assert_eq!(natural(b"x9", b"x10"), Ordering::Less);
        assert_eq!(
            natural(b"v99999999999999999999999", b"v100000000000000000000000"),
            Ordering::Less
        );
        assert_eq!(natural(b"same", b"same"), Ordering::Equal);
        assert_eq!(natural(b"\xff", b"a"), Ordering::Greater);
    }

    #[test]
    fn dirs_first_permutation() {
        let e = |n: &str, k| Entry {
            name: n.as_bytes().to_vec(),
            kind: k,
        };
        let v = vec![
            e("b10", Kind::File),
            e("zdir", Kind::Dir),
            e("b2", Kind::File),
            e("adir", Kind::Dir),
            e("link", Kind::Symlink),
        ];
        let names: Vec<&[u8]> = order(&v)
            .into_iter()
            .map(|i| v[i as usize].name.as_slice())
            .collect();
        assert_eq!(names, [&b"adir"[..], b"zdir", b"b2", b"b10", b"link"]);
    }
}
