// SPDX-License-Identifier: AGPL-3.0-only

//! ScreenCast + Screenshot backend logic for `ecl-desktop-portal`.
//!
//! Untrusted session code: this crate holds no capture authority. Every frame
//! source is gated by abyss (ADR 0097); the [`grant`] client only *asks*, and
//! any failure means no capture (fail closed).

pub mod grant;
pub mod pacing;
pub mod pipeline;
#[cfg(feature = "pipewire")]
pub mod pw;
pub mod remote;
pub mod screenshot;
pub mod session;
pub mod source;
pub mod stream;
#[cfg(feature = "wayland")]
pub mod wlr;

/// 128-bit random opaque token (hex), for `restore_token`s and stream ids.
/// Reads `/dev/urandom`; an unreadable urandom is an error, never a weak token.
pub fn random_token() -> std::io::Result<String> {
    use std::io::Read;
    let mut b = [0u8; 16];
    std::fs::File::open("/dev/urandom")?.read_exact(&mut b)?;
    Ok(b.iter().map(|x| format!("{x:02x}")).collect())
}

#[cfg(test)]
mod tests {
    #[test]
    fn tokens_differ() {
        let a = super::random_token().unwrap();
        let b = super::random_token().unwrap();
        assert_eq!(a.len(), 32);
        assert_ne!(a, b);
    }
}
