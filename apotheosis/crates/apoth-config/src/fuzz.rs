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

/// Field values the evaluators see: hostile text in every slot.
struct Hostile<'a>(&'a str);

impl crate::expr::Fields for Hostile<'_> {
    fn text(&self, _field: &str) -> Option<std::borrow::Cow<'_, str>> {
        Some(std::borrow::Cow::Borrowed(self.0))
    }
    fn state(&self, w: &str) -> bool {
        self.0.contains(w)
    }
    fn var(&self, _n: &str) -> Option<std::borrow::Cow<'_, str>> {
        Some(std::borrow::Cow::Borrowed(self.0))
    }
}

/// Arbitrary text as a match expression: parsing never panics, a parsed expression
/// evaluates (against its own text as every field) without panicking.
pub fn fuzz_match(data: &[u8]) {
    let s = String::from_utf8_lossy(data);
    let ok = crate::lang::validate_match(&s).is_ok();
    if let Ok(e) = crate::expr::Expr::parse(&s) {
        assert!(ok, "validator and parser disagree");
        let _ = e.eval(&Hostile(&s));
    }
}

/// Arbitrary text as a template: parsing never panics, rendering stays under the output
/// cap and carries no control character.
pub fn fuzz_template(data: &[u8]) {
    let s = String::from_utf8_lossy(data);
    if let Ok(t) = crate::template::Template::parse(&s) {
        let out = t.render(&Hostile(&s));
        assert!(out.len() <= crate::template::MAX_OUTPUT_BYTES);
        // Literal text is the user's own; substituted text is filtered. A template with
        // no literal control characters must render none.
        if !s.chars().any(char::is_control) {
            assert!(
                !out.chars().any(char::is_control),
                "a control reached the output"
            );
        }
    }
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
