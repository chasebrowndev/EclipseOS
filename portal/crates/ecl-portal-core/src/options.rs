// SPDX-License-Identifier: AGPL-3.0-only

//! `a{sv}` option parsing. Everything in an options dict is a claim from the
//! caller: wrong types are ignored, never trusted or panicked on.

use std::collections::HashMap;

use crate::request::validate_token;
use crate::val::Val;

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Options(pub HashMap<String, Val>);

impl Options {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn from_pairs(pairs: impl IntoIterator<Item = (String, Val)>) -> Self {
        Options(pairs.into_iter().collect())
    }

    pub fn get(&self, key: &str) -> Option<&Val> {
        self.0.get(key)
    }

    pub fn take(&mut self, key: &str) -> Option<Val> {
        self.0.remove(key)
    }

    pub fn bool(&self, key: &str) -> Option<bool> {
        self.get(key).and_then(Val::as_bool)
    }

    pub fn u32(&self, key: &str) -> Option<u32> {
        self.get(key).and_then(Val::as_u32)
    }

    pub fn str(&self, key: &str) -> Option<&str> {
        self.get(key).and_then(Val::as_str)
    }

    pub fn strs(&self, key: &str) -> Option<Vec<String>> {
        self.get(key).and_then(Val::as_strs)
    }

    /// A NUL-terminated `ay` path/string with the terminator(s) removed.
    pub fn bytestring(&self, key: &str) -> Option<Vec<u8>> {
        self.get(key).and_then(Val::as_bytes).map(strip_nul)
    }

    /// The caller's `handle_token`, if present and well-formed. A malformed
    /// token is an error the caller must see, not something to paper over.
    pub fn handle_token(&self) -> Result<Option<&str>, crate::request::TokenError> {
        match self.get("handle_token") {
            None => Ok(None),
            Some(v) => {
                let t = v.as_str().ok_or(crate::request::TokenError::Invalid)?;
                validate_token(t).map(Some)
            }
        }
    }

    /// `session_handle_token` for session-creating calls.
    pub fn session_token(&self) -> Result<Option<&str>, crate::request::TokenError> {
        match self.get("session_handle_token") {
            None => Ok(None),
            Some(v) => {
                let t = v.as_str().ok_or(crate::request::TokenError::Invalid)?;
                validate_token(t).map(Some)
            }
        }
    }
}

/// Wire `ay` byte strings carry a trailing NUL.
pub fn strip_nul(mut b: Vec<u8>) -> Vec<u8> {
    while b.last() == Some(&0) {
        b.pop();
    }
    b
}

#[cfg(test)]
mod tests {
    use super::*;

    fn opts() -> Options {
        Options::from_pairs([
            ("b".into(), Val::Bool(true)),
            ("s".into(), Val::Str("x".into())),
            ("p".into(), Val::Bytes(b"/h\0".to_vec())),
            ("handle_token".into(), Val::Str("tok_1".into())),
        ])
    }

    #[test]
    fn typed_getters_ignore_wrong_types() {
        let o = opts();
        assert_eq!(o.bool("b"), Some(true));
        assert_eq!(o.bool("s"), None);
        assert_eq!(o.str("s"), Some("x"));
        assert_eq!(o.str("missing"), None);
        assert_eq!(o.bytestring("p"), Some(b"/h".to_vec()));
    }

    #[test]
    fn token_validation() {
        assert_eq!(opts().handle_token(), Ok(Some("tok_1")));
        assert_eq!(Options::new().handle_token(), Ok(None));
        let bad = Options::from_pairs([("handle_token".into(), Val::Str("a/b".into()))]);
        assert!(bad.handle_token().is_err());
        let wrong = Options::from_pairs([("handle_token".into(), Val::Bool(true))]);
        assert!(wrong.handle_token().is_err());
    }

    #[test]
    fn nul_strip() {
        assert_eq!(strip_nul(b"/a\0\0".to_vec()), b"/a");
        assert_eq!(strip_nul(Vec::new()), b"");
    }
}
