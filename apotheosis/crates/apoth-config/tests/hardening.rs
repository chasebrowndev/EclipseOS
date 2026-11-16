// SPDX-License-Identifier: AGPL-3.0-only
//! Hostile and awkward input: the limits are part of the feature (T-10 §4, T-18 §5).
use apoth_config::fs::MemFs;
use apoth_config::lang::{validate_regex, validate_template};
use apoth_config::load::{LoadInput, load};
use std::path::PathBuf;

fn user(text: &str) -> apoth_config::LoadResult {
    let fs = MemFs::new().with("/home/u/apotheosis.kdl", text);
    let input = LoadInput {
        user: Some(PathBuf::from("/home/u/apotheosis.kdl")),
        ..Default::default()
    };
    load(&fs, &input, None)
}

#[test]
fn regex_is_compiled_by_the_runtime_engine() {
    for bad in [
        "*", "+", "a{2,1}", "(?z)", "(", "a)", "[a", "\\", "(a)\\1", "",
    ] {
        assert!(validate_regex(bad).is_err(), "{bad:?} should be refused");
    }
    for ok in [
        "[]]",
        "a+b*",
        "^\\d{1,3}(\\.\\d{1,3}){3}$",
        "(?i)foo|bar",
        "[a-z]+",
    ] {
        assert!(validate_regex(ok).is_ok(), "{ok:?} should be accepted");
    }
    assert!(validate_regex(&"a".repeat(5000)).is_err());
    // Nesting is bounded.
    assert!(validate_regex(&format!("{}a{}", "(".repeat(100), ")".repeat(100))).is_err());
}

#[test]
fn template_quote_scan_honours_escapes() {
    assert!(validate_template(r#"{if x == "a\"b"}yes{end}"#).is_ok());
    assert!(validate_template(r#"{if x == "a\"}yes{end}"#).is_err());
    assert!(validate_template("{if x}y{else}z{end}").is_ok());
    assert!(validate_template("{if x}y").is_err());
}

#[test]
fn profile_cycle_blames_only_members() {
    let r = user(
        "profile \"a\" { inherits \"b\" }\nprofile \"b\" { inherits \"c\" }\nprofile \"c\" { inherits \"b\" }\n",
    );
    let cycle: Vec<_> = r
        .diagnostics
        .iter()
        .filter(|d| d.message.contains("cycle"))
        .collect();
    assert!(!cycle.is_empty(), "{}", r.render());
    for d in &cycle {
        assert!(
            !d.message.starts_with("profile `a`"),
            "a is not on the cycle: {}",
            d.message
        );
        assert!(d.message.contains("->"), "path is reported: {}", d.message);
    }
    let blamed_b_or_c = cycle
        .iter()
        .any(|d| d.message.starts_with("profile `b`") || d.message.starts_with("profile `c`"));
    assert!(blamed_b_or_c, "{}", r.render());
    let selfish = user("profile \"a\" { inherits \"a\" }\n");
    assert!(selfish.render().contains("cycle"), "{}", selfish.render());
}

#[test]
fn deep_nesting_is_refused_not_overflowed() {
    let deep = format!("{}{}", "a {".repeat(5000), "}".repeat(5000));
    let r = user(&deep);
    assert!(r.rejected && !r.is_valid());
    // Within the file-level limit but beyond the action-nesting limit: a diagnostic, no panic.
    let n = 60;
    let nested = format!(
        "binds {{\n bind \"ctrl+a\" {{\n{}{}}}\n}}\n",
        "pane.close {\n".repeat(n),
        "}\n".repeat(n)
    );
    let r = user(&nested);
    assert!(r.render().contains("nest"), "{}", r.render());
    assert!(r.config.binds.is_empty());
}

#[test]
fn oversize_integer_is_an_error_not_a_wrapped_value() {
    let r = user("binds {\n bind \"ctrl+a\" {\n  pane.focus 18446744073709551617\n }\n}\n");
    assert!(!r.is_valid(), "{}", r.render());
    assert!(r.config.binds.is_empty());
}

#[test]
fn boolean_child_refuses_properties() {
    let src = |child: &str| {
        format!("rule \"r\" {{\n on \"bell\"\n {child}\n do {{\n  notify \"x\"\n }}\n}}\n")
    };
    let r = user(&src("allow-spawn enabled=#false"));
    assert!(!r.is_valid(), "{}", r.render());
    assert!(r.config.rules.is_empty());
    let r = user(&src("allow-spawn #false"));
    assert!(r.is_valid(), "{}", r.render());
}

#[test]
fn only_the_four_media_events_exist() {
    use apoth_config::is_rule_event;
    for ok in [
        "media-started",
        "media-ended",
        "media-error",
        "media-position",
    ] {
        assert!(is_rule_event(ok), "{ok}");
    }
    assert!(!is_rule_event("media-foo"));
    assert!(!is_rule_event("media-"));
    assert!(is_rule_event("bell"));
}

#[test]
fn unbind_inside_key_table_and_profile_is_kept() {
    let r = user(
        "key-table \"t\" {\n bind \"a\" { pane.close }\n unbind \"b\"\n}\nprofile \"p\" {\n binds {\n  unbind \"ctrl+x\"\n }\n}\n",
    );
    assert!(r.is_valid(), "{}", r.render());
    let t = &r.config.key_tables[0].value;
    assert_eq!(t.binds.len(), 1);
    assert_eq!(t.unbinds.len(), 1);
    assert_eq!(r.config.profiles[0].value.unbinds.len(), 1);
}

#[test]
fn bind_then_unbind_in_one_layer_leaves_nothing() {
    let r = user("binds {\n bind \"ctrl+a\" { pane.close }\n unbind \"ctrl+a\"\n}\n");
    assert!(r.is_valid(), "{}", r.render());
    assert!(
        r.config
            .binds
            .iter()
            .all(|b| b.value.chords.len() != 1 || b.value.chords[0].format() != "ctrl+a")
    );
}

#[test]
fn diagnostics_are_capped_and_line_numbers_are_exact() {
    let mut text = String::new();
    for i in 0..5000 {
        text.push_str(&format!("bogus{i} 1\n"));
    }
    let r = user(&text);
    assert!(r.diagnostics.len() <= apoth_config::diag::MAX_DIAGNOSTICS + 1);
    assert_eq!(r.diagnostics[0].line, 1);
    assert_eq!(r.diagnostics[10].line, 11);
}

#[test]
fn line_index_matches_a_naive_scan() {
    use apoth_config::diag::{LineIndex, line_col};
    let text = "ab\ncé\n\nxyz";
    let idx = LineIndex::new(text);
    for off in 0..=text.len() + 3 {
        let mut o = off.min(text.len());
        while !text.is_char_boundary(o) {
            o -= 1;
        }
        let (mut l, mut c) = (1, 1);
        for (i, ch) in text.char_indices() {
            if i >= o {
                break;
            }
            if ch == '\n' {
                l += 1;
                c = 1;
            } else {
                c += 1;
            }
        }
        assert_eq!(idx.line_col(text, off), (l, c), "offset {off}");
        assert_eq!(line_col(text, off), (l, c));
    }
}

#[test]
fn many_invalid_macros_resolve_quickly() {
    // A chain of macros each running the next, the last running an unknown one: every round
    // peels one, so this must stay cheap (the old code was cubic in the macro count).
    let mut text = String::new();
    let n = 600;
    for i in 0..n {
        text.push_str(&format!(
            "macro \"m{i}\" {{\n macro.run \"m{}\"\n}}\n",
            i + 1
        ));
    }
    let t = std::time::Instant::now();
    let r = user(&text);
    assert!(t.elapsed().as_secs() < 20);
    assert!(r.config.macros.is_empty());
    // A true cycle is reported once per member.
    let r = user("macro \"a\" { macro.run \"b\" }\nmacro \"b\" { macro.run \"a\" }\n");
    assert_eq!(
        r.diagnostics
            .iter()
            .filter(|d| d.message.contains("recurses"))
            .count(),
        2,
        "{}",
        r.render()
    );
}
