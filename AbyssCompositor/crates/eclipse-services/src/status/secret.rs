// SPDX-License-Identifier: AGPL-3.0-only
//! A `password`-role value in transit: a wifi passphrase, a Bluetooth PIN.
//!
//! It is never logged, never stored and never shown by `Debug`, and its bytes
//! are overwritten when it is dropped. That last part is best-effort by
//! nature: the copy zbus serialises into a message buffer is zbus's, and we
//! cannot reach it. What we can promise is that no copy *we* hold outlives
//! the call that needed it.

use std::fmt;

pub struct Secret(String);

impl Secret {
    pub fn new(value: String) -> Self {
        Self(value)
    }

    /// The value, for the one call that hands it to the daemon. Crate-private
    /// so a view cannot read a secret back out of a handle it was given.
    pub(crate) fn expose(&self) -> &str {
        &self.0
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

impl From<String> for Secret {
    fn from(value: String) -> Self {
        Self::new(value)
    }
}

impl fmt::Debug for Secret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Secret(<redacted>)")
    }
}

impl Drop for Secret {
    fn drop(&mut self) {
        wipe(std::mem::take(&mut self.0));
    }
}

/// Overwrite every byte the string's buffer owns, spare capacity included.
/// `black_box` keeps the optimiser from proving the writes dead — the plain
/// overwrite the brief allows, since `zeroize` is not a dependency here.
pub(crate) fn wipe(value: String) {
    let mut bytes = value.into_bytes();
    let capacity = bytes.capacity();
    bytes.clear();
    bytes.resize(capacity, 0);
    std::hint::black_box(&mut bytes);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn debug_never_shows_the_value() {
        let secret = Secret::new("hunter22".to_owned());
        let shown = format!("{secret:?}");
        assert!(!shown.contains("hunter22"), "{shown}");
        assert_eq!(secret.expose(), "hunter22");
    }
}
