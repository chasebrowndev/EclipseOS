// SPDX-License-Identifier: AGPL-3.0-only

//! Natural sort, computed in `ec-fogd` (FOG §Performance model, technique 7).

use std::cmp::Ordering;

use ec_fog_proto::{Entry, Kind, Sort, SortKey};

/// Display order of `entries` as an index permutation: directories first,
/// then natural name order (the default [`Sort`]).
pub fn order(entries: &[Entry]) -> Vec<u32> {
    order_by(entries, &Sort::default())
}

/// Display order of `entries` under `sort`.
///
/// Size and mtime are phase-2 metadata: entries still without them sort
/// after those that have them, in either direction, so a folder filling in
/// never shuffles its known rows. Folders' own sizes mean nothing, so under
/// [`SortKey::Size`] two folders compare by name.
pub fn order_by(entries: &[Entry], sort: &Sort) -> Vec<u32> {
    let mut idx: Vec<u32> = (0..entries.len() as u32).collect();
    // The type label allocates; compute it once per entry, not per compare.
    let labels: Vec<_> = match sort.key {
        SortKey::Type => entries.iter().map(Entry::type_label).collect(),
        _ => Vec::new(),
    };
    let dir = |e: &Entry| e.kind == Kind::Dir;
    idx.sort_unstable_by(|&ia, &ib| {
        let (a, b) = (&entries[ia as usize], &entries[ib as usize]);
        let group = if sort.dirs_first {
            dir(b).cmp(&dir(a))
        } else {
            Ordering::Equal
        };
        // Known before unknown, whatever the direction.
        let known = |x: bool, y: bool| y.cmp(&x);
        let (unknown, key) = match sort.key {
            SortKey::Name => (Ordering::Equal, Ordering::Equal),
            SortKey::Size if dir(a) && dir(b) => (Ordering::Equal, Ordering::Equal),
            SortKey::Size => (
                known(a.size.is_some(), b.size.is_some()),
                a.size.cmp(&b.size),
            ),
            SortKey::Modified => (
                known(a.mtime_ns.is_some(), b.mtime_ns.is_some()),
                a.mtime_ns.cmp(&b.mtime_ns),
            ),
            SortKey::Type => (
                Ordering::Equal,
                labels[ia as usize].cmp(&labels[ib as usize]),
            ),
        };
        let name = natural(&a.name, &b.name);
        let keyed = key.then(name);
        group
            .then(unknown)
            .then(if sort.reverse { keyed.reverse() } else { keyed })
    });
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
        let e = |n: &str, k| Entry::new(n.as_bytes().to_vec(), k);
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

    #[test]
    fn keyed_orders() {
        let f = |n: &str, size: Option<u64>, mt: Option<i128>| Entry {
            size,
            mtime_ns: mt,
            mode: size.map(|_| 0o100644),
            ..Entry::new(n.as_bytes().to_vec(), Kind::File)
        };
        let d = |n: &str| Entry {
            size: Some(4096),
            ..Entry::new(n.as_bytes().to_vec(), Kind::Dir)
        };
        let v = vec![
            f("big.txt", Some(900), Some(3)),
            d("zdir"),
            f("small.rs", Some(10), Some(9)),
            f("pending", None, None),
            d("adir"),
            f("mid.rs", Some(50), Some(1)),
        ];
        let by = |key, reverse, dirs_first| -> Vec<String> {
            order_by(
                &v,
                &Sort {
                    key,
                    reverse,
                    dirs_first,
                },
            )
            .into_iter()
            .map(|i| v[i as usize].display().into_owned())
            .collect()
        };
        assert_eq!(
            by(SortKey::Size, false, true),
            ["adir", "zdir", "small.rs", "mid.rs", "big.txt", "pending"]
        );
        // Reversed: folders stay in front, unknown sizes stay last.
        assert_eq!(
            by(SortKey::Size, true, true),
            ["zdir", "adir", "big.txt", "mid.rs", "small.rs", "pending"]
        );
        // Newest first, folders mixed in; no mtime yet sorts last.
        assert_eq!(
            by(SortKey::Modified, true, false),
            ["small.rs", "big.txt", "mid.rs", "zdir", "pending", "adir"]
        );
        assert_eq!(
            by(SortKey::Type, false, true),
            ["adir", "zdir", "pending", "mid.rs", "small.rs", "big.txt"]
        );
        assert_eq!(
            by(SortKey::Name, true, true),
            ["zdir", "adir", "small.rs", "pending", "mid.rs", "big.txt"]
        );
    }
}
