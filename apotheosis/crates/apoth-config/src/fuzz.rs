// SPDX-License-Identifier: AGPL-3.0-only
//! Stable entry points for the `config_parse`, `match_lang` and `template` fuzz targets
//! (T-18 §11). The config file, match expressions and templates are user-authored but can
//! arrive from anywhere (a downloaded theme, an imported config, a profile someone shared),
//! so each must survive arbitrary input. They panic only when an invariant breaks.

use std::path::Path;

use crate::fs::MemFs;
use crate::load::{LoadInput, load};

/// Arbitrary bytes (lossily UTF-8) as the user's main config file, with an include of a
/// second file holding the same bytes: loading never panics and is deterministic.
pub fn fuzz_config(data: &[u8]) {
    let text = String::from_utf8_lossy(data);
    let fs = MemFs::new()
        .with("/c/apotheosis.kdl", &text)
        .with("/c/inc.kdl", &text);
    let input = LoadInput {
        user: Some(Path::new("/c/apotheosis.kdl").to_path_buf()),
        ..LoadInput::default()
    };
    let a = load(&fs, &input, None);
    let b = load(&fs, &input, Some(&a.config));
    assert!(a.config == b.config, "loading is not deterministic");
    assert_eq!(a.diagnostics.len(), b.diagnostics.len());
}

/// Arbitrary text through the match-expression validator.
pub fn fuzz_match(data: &[u8]) {
    let _ = crate::lang::validate_match(&String::from_utf8_lossy(data));
}

/// Arbitrary text through the template validator.
pub fn fuzz_template(data: &[u8]) {
    let _ = crate::lang::validate_template(&String::from_utf8_lossy(data));
}

#[cfg(test)]
mod tests {
    use super::*;

    fn corpus(name: &str, f: fn(&[u8])) -> usize {
        let dir = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("corpus")
            .join(name);
        let mut n = 0;
        for entry in std::fs::read_dir(dir).unwrap() {
            f(&std::fs::read(entry.unwrap().path()).unwrap());
            n += 1;
        }
        n
    }

    #[test]
    fn config_seeds_load() {
        assert!(corpus("config_parse", fuzz_config) >= 5);
    }

    #[test]
    fn match_seeds_validate() {
        assert!(corpus("match_lang", fuzz_match) >= 5);
    }

    #[test]
    fn template_seeds_validate() {
        assert!(corpus("template", fuzz_template) >= 5);
    }

    #[test]
    fn junk_never_panics() {
        for d in [
            &b""[..],
            b"\xff\xfe",
            b"{",
            b"}",
            b"(((((((((((((((((((((((((((((((",
            b"include \"/c/inc.kdl\"",
            b"/-",
            b"r#\"",
            b"${${${${",
            b"a == \"",
        ] {
            fuzz_config(d);
            fuzz_match(d);
            fuzz_template(d);
        }
    }

    #[test]
    fn deep_nesting_is_refused_not_overflowed() {
        let deep = format!("{}title:x{}", "(".repeat(200_000), ")".repeat(200_000));
        assert!(crate::lang::validate_match(&deep).is_err());
        let nots = format!("{}title:x", "not ".repeat(200_000));
        assert!(crate::lang::validate_match(&nots).is_err());
        let ok = format!("{}title:x{}", "(".repeat(30), ")".repeat(30));
        assert!(crate::lang::validate_match(&ok).is_ok());
        let kdl = format!("{}{}", "a {".repeat(20_000), "}".repeat(20_000));
        fuzz_config(kdl.as_bytes());
    }
}
