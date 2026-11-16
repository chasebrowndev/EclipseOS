// SPDX-License-Identifier: Apache-2.0
//! Capability names on the wire: the T-12 §4 class names, reused from `apoth-action`.

use serde::{Deserialize, Deserializer, Serialize, Serializer};

pub use apoth_action::Class;

/// A capability class as it appears in `want` / `granted`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Cap(pub Class);

impl Serialize for Cap {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(self.0.name())
    }
}

impl<'de> Deserialize<'de> for Cap {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let s = String::deserialize(d)?;
        Class::parse(&s)
            .map(Cap)
            .ok_or_else(|| serde::de::Error::custom("unknown capability class"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_class_roundtrips() {
        for c in Class::ALL {
            let j = serde_json::to_string(&Cap(c)).unwrap();
            assert_eq!(j, format!("\"{}\"", c.name()));
            assert_eq!(serde_json::from_str::<Cap>(&j).unwrap(), Cap(c));
        }
        assert!(serde_json::from_str::<Cap>("\"root\"").is_err());
    }
}
