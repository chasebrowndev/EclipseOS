// SPDX-License-Identifier: AGPL-3.0-only
//! The API key's container.

use zeroize::Zeroizing;

/// An Anthropic API key. `Debug` is redacted so a stray `{:?}` in a log line
/// or an error chain cannot leak it, and the bytes are zeroed on drop.
pub struct ApiKey(Zeroizing<String>);

impl ApiKey {
    /// `None` when the bytes cannot be an HTTP header value (not UTF-8, empty,
    /// or holding a control character), which also closes header injection
    /// through a malformed stored value.
    pub fn from_bytes(b: &[u8]) -> Option<ApiKey> {
        let s = std::str::from_utf8(b).ok()?.trim();
        if s.is_empty() || s.chars().any(|c| c.is_control() || c == ' ') {
            return None;
        }
        Some(ApiKey(Zeroizing::new(s.to_owned())))
    }

    /// The header value. Callers must not log it.
    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Debug for ApiKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("ApiKey(<redacted>)")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn debug_never_prints_the_key() {
        let k = ApiKey::from_bytes(b"sk-ant-SECRET-123").unwrap();
        assert_eq!(format!("{k:?}"), "ApiKey(<redacted>)");
        assert_eq!(format!("{k:#?}"), "ApiKey(<redacted>)");
        assert_eq!(k.expose(), "sk-ant-SECRET-123");
    }

    #[test]
    fn a_value_that_cannot_be_a_header_is_refused() {
        assert!(ApiKey::from_bytes(b"").is_none());
        assert!(ApiKey::from_bytes(b"a\r\nx-evil: 1").is_none());
        assert!(ApiKey::from_bytes(&[0xff, 0xfe]).is_none());
        assert!(ApiKey::from_bytes(b"two words").is_none());
        // A trailing newline from `ec-secret add` is tolerated.
        assert_eq!(ApiKey::from_bytes(b"sk-1\n").unwrap().expose(), "sk-1");
    }
}
