// SPDX-License-Identifier: AGPL-3.0-only
//! Socket targets (T-12 §3) in the T-11 §7 match language. Only the single-pane terms are
//! implemented: `self`, `id:<n>`, `state:focused`. Anything else is `bad-target` (the full
//! grammar is a reported gap; content-field terms such as `title:` would also need the
//! `read-content` check before they could match).

/// A resolved target term.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Target {
    /// The pane the client runs in.
    SelfPane,
    /// A pane by id.
    Id(u64),
    /// The focused pane of the window.
    Focused,
}

/// Parse a target expression.
pub(crate) fn parse(s: &str) -> Result<Target, &'static str> {
    let s = s.trim();
    match s {
        "self" => return Ok(Target::SelfPane),
        "state:focused" => return Ok(Target::Focused),
        _ => {}
    }
    if let Some(n) = s.strip_prefix("id:") {
        return n
            .parse()
            .map(Target::Id)
            .map_err(|_| "id: takes a pane number");
    }
    Err("only self, id:<n> and state:focused are supported as targets")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn terms() {
        assert_eq!(parse("self"), Ok(Target::SelfPane));
        assert_eq!(parse(" id:42 "), Ok(Target::Id(42)));
        assert_eq!(parse("state:focused"), Ok(Target::Focused));
        assert!(parse("id:x").is_err());
        assert!(parse("title:vim and not id:1").is_err());
    }
}
