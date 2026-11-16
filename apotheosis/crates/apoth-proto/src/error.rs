// SPDX-License-Identifier: Apache-2.0
//! Error codes (T-12 §1, §5, §6). The spec fixes no spellings; these are proposals.

use std::fmt;

use serde::{Deserialize, Deserializer, Serialize, Serializer};

use crate::limits::MAX_ERROR_MESSAGE_BYTES;

/// CLI exit code: success.
pub const EXIT_OK: u8 = 0;
/// CLI exit code: error.
pub const EXIT_ERROR: u8 = 1;
/// CLI exit code: usage.
pub const EXIT_USAGE: u8 = 2;
/// CLI exit code: denied.
pub const EXIT_DENIED: u8 = 3;
/// CLI exit code: no instance.
pub const EXIT_NO_INSTANCE: u8 = 4;

/// The closed error-code set.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ErrorCode {
    /// Method name not in the table.
    UnknownMethod,
    /// Malformed JSON, unknown or missing field, wrong type, bad value.
    BadRequest,
    /// `action.run` named something that is not a registry Action (consent answers too).
    UnknownAction,
    /// Capability or policy refused it.
    Denied,
    /// A line over the 1 MiB limit; the connection is closed after this reply.
    LineTooLong,
    /// No running instance (client side).
    NoInstance,
    /// The target expression is invalid.
    BadTarget,
    /// The target matched nothing.
    NoTarget,
    /// Protocol version outside the supported range.
    UnsupportedVersion,
    /// A request before `hello`, or a second `hello`.
    NotReady,
    /// Reserved (R2) or unavailable on this build.
    Unsupported,
    /// Server fault.
    Internal,
}

impl ErrorCode {
    /// Every code.
    pub const ALL: [ErrorCode; 12] = [
        ErrorCode::UnknownMethod,
        ErrorCode::BadRequest,
        ErrorCode::UnknownAction,
        ErrorCode::Denied,
        ErrorCode::LineTooLong,
        ErrorCode::NoInstance,
        ErrorCode::BadTarget,
        ErrorCode::NoTarget,
        ErrorCode::UnsupportedVersion,
        ErrorCode::NotReady,
        ErrorCode::Unsupported,
        ErrorCode::Internal,
    ];

    /// The wire spelling.
    pub const fn as_str(self) -> &'static str {
        match self {
            ErrorCode::UnknownMethod => "unknown-method",
            ErrorCode::BadRequest => "bad-request",
            ErrorCode::UnknownAction => "unknown-action",
            ErrorCode::Denied => "denied",
            ErrorCode::LineTooLong => "line-too-long",
            ErrorCode::NoInstance => "no-instance",
            ErrorCode::BadTarget => "bad-target",
            ErrorCode::NoTarget => "no-target",
            ErrorCode::UnsupportedVersion => "unsupported-version",
            ErrorCode::NotReady => "not-ready",
            ErrorCode::Unsupported => "unsupported",
            ErrorCode::Internal => "internal",
        }
    }

    /// Parse a wire spelling.
    pub fn parse(s: &str) -> Option<ErrorCode> {
        ErrorCode::ALL.iter().copied().find(|c| c.as_str() == s)
    }

    /// The CLI exit code a client reports for this error (T-12 §5).
    pub const fn exit_code(self) -> u8 {
        match self {
            ErrorCode::Denied => EXIT_DENIED,
            ErrorCode::NoInstance => EXIT_NO_INSTANCE,
            _ => EXIT_ERROR,
        }
    }
}

impl fmt::Display for ErrorCode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl Serialize for ErrorCode {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for ErrorCode {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let s = String::deserialize(d)?;
        ErrorCode::parse(&s).ok_or_else(|| serde::de::Error::custom("unknown error code"))
    }
}

/// The `{"code","message"}` object. Messages never carry content, only names and sizes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WireError {
    /// Machine-readable code.
    pub code: ErrorCode,
    /// Human-readable text, at most [`MAX_ERROR_MESSAGE_BYTES`].
    pub message: String,
}

impl WireError {
    /// A new error; the message is cut to the budget at a char boundary.
    pub fn new(code: ErrorCode, message: impl Into<String>) -> Self {
        let mut message = message.into();
        if message.len() > MAX_ERROR_MESSAGE_BYTES {
            let mut end = MAX_ERROR_MESSAGE_BYTES;
            while !message.is_char_boundary(end) {
                end -= 1;
            }
            message.truncate(end);
        }
        Self { code, message }
    }

    /// `bad-request`.
    pub fn bad_request(message: impl Into<String>) -> Self {
        Self::new(ErrorCode::BadRequest, message)
    }

    /// `line-too-long`.
    pub fn oversize(len: usize) -> Self {
        Self::new(
            ErrorCode::LineTooLong,
            format!(
                "line exceeds {} bytes (saw at least {len})",
                crate::limits::MAX_LINE_BYTES
            ),
        )
    }
}

impl fmt::Display for WireError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.code, self.message)
    }
}

impl std::error::Error for WireError {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn codes_roundtrip_and_are_distinct() {
        for c in ErrorCode::ALL {
            assert_eq!(ErrorCode::parse(c.as_str()), Some(c));
        }
        let mut names: Vec<_> = ErrorCode::ALL.iter().map(|c| c.as_str()).collect();
        names.sort_unstable();
        names.dedup();
        assert_eq!(names.len(), ErrorCode::ALL.len());
    }

    #[test]
    fn exit_codes() {
        assert_eq!(ErrorCode::Denied.exit_code(), 3);
        assert_eq!(ErrorCode::NoInstance.exit_code(), 4);
        assert_eq!(ErrorCode::BadRequest.exit_code(), 1);
    }

    #[test]
    fn message_is_bounded_on_char_boundary() {
        let e = WireError::new(ErrorCode::Internal, "é".repeat(1000));
        assert!(e.message.len() <= MAX_ERROR_MESSAGE_BYTES);
        assert!(e.message.chars().all(|c| c == 'é'));
    }

    #[test]
    fn unknown_code_rejected() {
        assert!(serde_json::from_str::<WireError>(r#"{"code":"nope","message":""}"#).is_err());
        assert!(
            serde_json::from_str::<WireError>(r#"{"code":"denied","message":"","x":1}"#).is_err()
        );
    }
}
