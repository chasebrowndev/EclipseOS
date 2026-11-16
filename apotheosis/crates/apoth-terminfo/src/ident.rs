// SPDX-License-Identifier: Apache-2.0
//! Device identification replies (T-19 §4): DA1, DA2, DA3 and XTVERSION, as pure builders.

/// DA1. `sixel` is false before M25 (`CSI ? 62 ; 22 c`), true afterwards
/// (`CSI ? 62 ; 4 ; 22 c`).
pub fn da1(sixel: bool) -> Vec<u8> {
    if sixel {
        b"\x1b[?62;4;22c".to_vec()
    } else {
        b"\x1b[?62;22c".to_vec()
    }
}

/// DA2 `CSI > 1 ; Pv ; 0 c`, `Pv = major*10000 + minor*100 + patch`. `None` when minor
/// or patch is >= 100 (the release-time rule) or `Pv` would overflow.
pub fn da2(major: u32, minor: u32, patch: u32) -> Option<Vec<u8>> {
    if minor >= 100 || patch >= 100 {
        return None;
    }
    let pv = major.checked_mul(10000)? + minor * 100 + patch;
    Some(format!("\x1b[>1;{pv};0c").into_bytes())
}

/// Parse `major.minor.patch[-pre][+build]`; pre-release and build are ignored.
pub fn parse_semver(v: &str) -> Option<(u32, u32, u32)> {
    let core = v.split(['-', '+']).next()?;
    let mut it = core.split('.');
    let mut n = || it.next()?.parse::<u32>().ok();
    let r = (n()?, n()?, n()?);
    it.next().is_none().then_some(r)
}

/// [`da2`] from a SemVer string.
pub fn da2_semver(v: &str) -> Option<Vec<u8>> {
    let (a, b, c) = parse_semver(v)?;
    da2(a, b, c)
}

/// DA3: `DCS ! | 00000000 ST`.
pub fn da3() -> Vec<u8> {
    b"\x1bP!|00000000\x1b\\".to_vec()
}

/// XTVERSION: `DCS > | Apotheosis <semver> ST`. Control bytes in `semver` are dropped
/// so the string cannot terminate the DCS early.
pub fn xtversion(semver: &str) -> Vec<u8> {
    let mut r = b"\x1bP>|Apotheosis ".to_vec();
    r.extend(semver.bytes().filter(|b| (0x20..0x7f).contains(b)));
    r.extend_from_slice(b"\x1b\\");
    r
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn da1_goldens() {
        assert_eq!(da1(true), b"\x1b[?62;4;22c");
        assert_eq!(da1(false), b"\x1b[?62;22c");
    }

    #[test]
    fn da2_goldens() {
        assert_eq!(da2_semver("0.1.0").unwrap(), b"\x1b[>1;100;0c");
        assert_eq!(da2_semver("1.4.2").unwrap(), b"\x1b[>1;10402;0c");
        assert_eq!(da2_semver("12.34.56").unwrap(), b"\x1b[>1;123456;0c");
        assert_eq!(da2_semver("1.4.2-rc.1+b5").unwrap(), b"\x1b[>1;10402;0c");
    }

    #[test]
    fn da2_rejects_wide_components() {
        assert!(da2(1, 100, 0).is_none());
        assert!(da2(1, 0, 100).is_none());
        assert!(da2_semver("1.2").is_none());
        assert!(da2_semver("1.2.3.4").is_none());
    }

    /// The release-time test of T-19 §4: the shipped version must fit the encoding.
    #[test]
    fn shipped_version_encodes() {
        assert!(da2_semver(env!("CARGO_PKG_VERSION")).is_some());
    }

    #[test]
    fn da3_and_xtversion_goldens() {
        assert_eq!(da3(), b"\x1bP!|00000000\x1b\\");
        assert_eq!(xtversion("1.4.2"), b"\x1bP>|Apotheosis 1.4.2\x1b\\");
        assert_eq!(xtversion("1\x1b\\2"), b"\x1bP>|Apotheosis 1\\2\x1b\\");
    }
}
