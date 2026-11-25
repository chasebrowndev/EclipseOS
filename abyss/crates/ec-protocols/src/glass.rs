// SPDX-License-Identifier: Apache-2.0
//! Generated bindings for `eclipse_glass_v1` (COMP-02 §9): a client asks the
//! compositor to draw glass panes under parts of its own surface.

#[cfg(feature = "client")]
pub mod client {
    #![allow(dead_code, non_camel_case_types, unused_unsafe, unused_variables)]
    #![allow(non_upper_case_globals, non_snake_case, unused_imports)]
    #![allow(missing_docs, clippy::all)]
    use wayland_client;
    use wayland_client::protocol::*;

    pub mod __interfaces {
        use wayland_client::protocol::__interfaces::*;
        wayland_scanner::generate_interfaces!("protocols/eclipse-glass-v1.xml");
    }
    use self::__interfaces::*;

    wayland_scanner::generate_client_code!("protocols/eclipse-glass-v1.xml");
}

#[cfg(feature = "server")]
pub mod server {
    #![allow(dead_code, non_camel_case_types, unused_unsafe, unused_variables)]
    #![allow(non_upper_case_globals, non_snake_case, unused_imports)]
    #![allow(missing_docs, clippy::all)]
    use wayland_server;
    use wayland_server::protocol::*;

    pub mod __interfaces {
        use wayland_server::protocol::__interfaces::*;
        wayland_scanner::generate_interfaces!("protocols/eclipse-glass-v1.xml");
    }
    use self::__interfaces::*;

    wayland_scanner::generate_server_code!("protocols/eclipse-glass-v1.xml");
}

/// One `set_joins` record (`eclipse_glass_v1` version 3, ADR 0130): three
/// native-endian 32-bit fields, `a`, `b` and `radius`, twelve bytes in all.
pub const JOIN_RECORD_BYTES: usize = 12;

/// A join between two panes of the pane list it is sent with. Marshalling
/// only: the compositor checks the indices against the list and the caps
/// (`bad_join`, `too_many_joins`), in `ec-abyss-render`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct JoinRecord {
    /// Index of a pane in the list, from 0.
    pub a: u32,
    /// Index of another pane in the same list.
    pub b: u32,
    /// Fillet radius where the two meet, logical pixels.
    pub radius: u32,
}

impl JoinRecord {
    /// The record's wire bytes.
    pub fn to_bytes(self) -> [u8; JOIN_RECORD_BYTES] {
        let mut out = [0u8; JOIN_RECORD_BYTES];
        out[0..4].copy_from_slice(&self.a.to_ne_bytes());
        out[4..8].copy_from_slice(&self.b.to_ne_bytes());
        out[8..12].copy_from_slice(&self.radius.to_ne_bytes());
        out
    }

    /// The record in `rec`, or `None` unless it is exactly one record long.
    pub fn from_bytes(rec: &[u8]) -> Option<Self> {
        let word = |i: usize| -> Option<u32> {
            Some(u32::from_ne_bytes(rec.get(i * 4..i * 4 + 4)?.try_into().ok()?))
        };
        if rec.len() != JOIN_RECORD_BYTES {
            return None;
        }
        Some(Self {
            a: word(0)?,
            b: word(1)?,
            radius: word(2)?,
        })
    }
}

/// Pack join records into the `joins` array of `set_joins`.
pub fn encode_joins(joins: &[JoinRecord]) -> Vec<u8> {
    joins.iter().flat_map(|j| j.to_bytes()).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn join_records_round_trip_in_twelve_bytes() {
        let joins = [
            JoinRecord {
                a: 0,
                b: 1,
                radius: 16,
            },
            JoinRecord {
                a: 7,
                b: 3,
                radius: 64,
            },
        ];
        let bytes = encode_joins(&joins);
        assert_eq!(bytes.len(), 2 * JOIN_RECORD_BYTES);
        let back: Vec<JoinRecord> = bytes
            .chunks_exact(JOIN_RECORD_BYTES)
            .map(|c| JoinRecord::from_bytes(c).unwrap())
            .collect();
        assert_eq!(back, joins);
        assert_eq!(&bytes[0..4], &0u32.to_ne_bytes());
        assert_eq!(&bytes[8..12], &16u32.to_ne_bytes());
    }

    #[test]
    fn a_record_is_exactly_twelve_bytes() {
        assert!(JoinRecord::from_bytes(&[0u8; 11]).is_none());
        assert!(JoinRecord::from_bytes(&[0u8; 13]).is_none());
        assert_eq!(JoinRecord::from_bytes(&[0u8; 12]), Some(JoinRecord::default()));
        assert!(encode_joins(&[]).is_empty());
    }
}
