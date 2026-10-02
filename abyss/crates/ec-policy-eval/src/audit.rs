// SPDX-License-Identifier: AGPL-3.0-only
//! The audit record kinds (S-04 §1.1, A-05) and the one message a source
//! sends `policyd` to have a record written (COMP-12 §1).
//!
//! Both ends link this module: abyss encodes an [`Emission`], `policyd`
//! decodes it and appends a record. The emission carries only what a record
//! is *about*. `seq`, the timestamps and the chain links are the store's, so
//! a source cannot choose its own place in history.
//!
//! `{"v": 1, "kind": text, "principal": text, "body": any, "grant_id"?:
//! bstr16, "task_id"?: bstr16, "chain_id"?: bstr16, "req_id"?: uint,
//! "serial"?: uint}`, canonical CBOR (ADR 0044). An optional field is absent
//! when it does not apply, never zeroed (S-04 §1).

use crate::cbor::{self, enc, MapBuilder, Reader};
use crate::Ulid;

/// The only emission version this build speaks.
pub const VERSION: u64 = 1;

/// The largest emission either end accepts. Bodies carry hashes and counts,
/// never content (S-04 §2), so a bigger one is a bug, and on `policyd`'s side
/// a bound on what a peer can make it allocate.
pub const MAX_EMISSION: usize = 64 * 1024;

/// What a record is about (S-04 §1.1 as amended by A-05 and A2-07).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Kind {
    Request,
    Decision,
    Prompt,
    Result,
    Input,
    Focus,
    Capture,
    Perception,
    Channel,
    Grant,
    Revoke,
    Launch,
    Sandbox,
    Policy,
    Lifecycle,
    Task,
    Net,
    Secret,
    Anchor,
}

impl Kind {
    pub const ALL: [Kind; 19] = [
        Kind::Request,
        Kind::Decision,
        Kind::Prompt,
        Kind::Result,
        Kind::Input,
        Kind::Focus,
        Kind::Capture,
        Kind::Perception,
        Kind::Channel,
        Kind::Grant,
        Kind::Revoke,
        Kind::Launch,
        Kind::Sandbox,
        Kind::Policy,
        Kind::Lifecycle,
        Kind::Task,
        Kind::Net,
        Kind::Secret,
        Kind::Anchor,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Kind::Request => "request",
            Kind::Decision => "decision",
            Kind::Prompt => "prompt",
            Kind::Result => "result",
            Kind::Input => "input",
            Kind::Focus => "focus",
            Kind::Capture => "capture",
            Kind::Perception => "perception",
            Kind::Channel => "channel",
            Kind::Grant => "grant",
            Kind::Revoke => "revoke",
            Kind::Launch => "launch",
            Kind::Sandbox => "sandbox",
            Kind::Policy => "policy",
            Kind::Lifecycle => "lifecycle",
            Kind::Task => "task",
            Kind::Net => "net",
            Kind::Secret => "secret",
            Kind::Anchor => "anchor",
        }
    }

    pub fn parse(s: &str) -> Option<Kind> {
        Kind::ALL.into_iter().find(|k| k.as_str() == s)
    }

    /// Whether the compositor may emit this kind (COMP-12 §2). `grant`,
    /// `revoke`, `channel`, `sandbox` and `anchor` originate in `policyd` or
    /// `agentd`; `task` is `policyd`'s (A-04); `net` and `secret` are the
    /// proxy's and `brokerd`'s (S-09 §7, S-08 §7). A compositor emission of
    /// any of them is refused, not stored.
    pub fn from_compositor(self) -> bool {
        matches!(
            self,
            Kind::Request
                | Kind::Decision
                | Kind::Prompt
                | Kind::Result
                | Kind::Input
                | Kind::Focus
                | Kind::Capture
                | Kind::Perception
                | Kind::Launch
                | Kind::Lifecycle
                | Kind::Policy
        )
    }
}

/// `agent:<id>`, `human` or `system:<daemon>` (S-04 §1), with a non-empty
/// printable id.
pub fn principal_ok(p: &str) -> bool {
    let id = match p {
        "human" => return true,
        _ => p.strip_prefix("agent:").or_else(|| p.strip_prefix("system:")),
    };
    id.is_some_and(|id| !id.is_empty() && id.len() <= 128 && id.bytes().all(|b| b.is_ascii_graphic()))
}

/// One record as a source hands it to `policyd`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Emission {
    pub kind: Kind,
    pub principal: String,
    pub grant_id: Option<Ulid>,
    pub task_id: Option<Ulid>,
    /// The provenance chain the record was evaluated against (A-05; S-07
    /// §5). 16 bytes on the wire, as on `eclipse_scene_v1` (A-06.2).
    pub chain_id: Option<u128>,
    pub req_id: Option<u64>,
    /// The compositor's event serial (COMP-12 §5).
    pub serial: Option<u64>,
    /// The kind's body (S-04 §1.1), itself one canonical CBOR item.
    pub body: Vec<u8>,
}

/// Why bytes are not an [`Emission`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EmissionError {
    Cbor(cbor::Error),
    /// A version, field or kind this build does not know, a malformed
    /// principal, or a body that is not exactly one CBOR item.
    Shape,
}

impl From<cbor::Error> for EmissionError {
    fn from(e: cbor::Error) -> Self {
        EmissionError::Cbor(e)
    }
}

impl Emission {
    pub fn encode(&self) -> Vec<u8> {
        let mut m = MapBuilder::new();
        m.insert("body", self.body.clone());
        m.insert_opt(
            "chain_id",
            self.chain_id.map(|c| enc(|w| w.bytes(&c.to_be_bytes()))),
        );
        m.insert_opt("grant_id", self.grant_id.map(|g| enc(|w| w.bytes(&g.0))));
        m.insert("kind", enc(|w| w.text(self.kind.as_str())));
        m.insert("principal", enc(|w| w.text(&self.principal)));
        m.insert_opt("req_id", self.req_id.map(|r| enc(|w| w.u64(r))));
        m.insert_opt("serial", self.serial.map(|s| enc(|w| w.u64(s))));
        m.insert_opt("task_id", self.task_id.map(|t| enc(|w| w.bytes(&t.0))));
        m.insert("v", enc(|w| w.u64(VERSION)));
        m.finish()
    }

    pub fn decode(buf: &[u8]) -> Result<Emission, EmissionError> {
        let mut r = Reader::new(buf);
        let n = r.map_begin()?;
        let (mut kind, mut principal, mut body, mut v) = (None, None, None, None);
        let mut e = Emission {
            kind: Kind::Anchor,
            principal: String::new(),
            grant_id: None,
            task_id: None,
            chain_id: None,
            req_id: None,
            serial: None,
            body: Vec::new(),
        };
        for _ in 0..n {
            match r.key()? {
                "body" => {
                    let at = r.position();
                    r.skip()?;
                    body = Some(buf[at..r.position()].to_vec());
                }
                "chain_id" => e.chain_id = Some(u128::from_be_bytes(r.byte_array::<16>()?)),
                "grant_id" => e.grant_id = Some(Ulid(r.byte_array::<16>()?)),
                "kind" => kind = Some(Kind::parse(r.text()?).ok_or(EmissionError::Shape)?),
                "principal" => principal = Some(r.text()?.to_owned()),
                "req_id" => e.req_id = Some(r.u64()?),
                "serial" => e.serial = Some(r.u64()?),
                "task_id" => e.task_id = Some(Ulid(r.byte_array::<16>()?)),
                "v" => v = Some(r.u64()?),
                _ => return Err(EmissionError::Shape),
            }
        }
        r.map_end()?;
        r.finish()?;
        if v != Some(VERSION) {
            return Err(EmissionError::Shape);
        }
        e.kind = kind.ok_or(EmissionError::Shape)?;
        e.principal = principal.ok_or(EmissionError::Shape)?;
        e.body = body.ok_or(EmissionError::Shape)?;
        if !principal_ok(&e.principal) {
            return Err(EmissionError::Shape);
        }
        Ok(e)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Emission {
        Emission {
            kind: Kind::Focus,
            principal: "human".into(),
            grant_id: None,
            task_id: Some(Ulid([7; 16])),
            chain_id: Some(0x0102_0304_0506_0708_090a_0b0c_0d0e_0f10),
            req_id: Some(42),
            serial: Some(9),
            body: enc(|w| w.text("x")),
        }
    }

    #[test]
    fn an_emission_round_trips_canonically() {
        let e = sample();
        let bytes = e.encode();
        assert_eq!(Emission::decode(&bytes), Ok(e.clone()));
        assert_eq!(Emission::decode(&bytes).unwrap().encode(), bytes);
    }

    #[test]
    fn absent_fields_stay_absent() {
        let e = Emission {
            task_id: None,
            chain_id: None,
            req_id: None,
            serial: None,
            ..sample()
        };
        let d = Emission::decode(&e.encode()).unwrap();
        assert_eq!(
            (d.task_id, d.chain_id, d.req_id, d.serial),
            (None, None, None, None)
        );
    }

    #[test]
    fn kind_names_are_the_spec_table() {
        for k in Kind::ALL {
            assert_eq!(Kind::parse(k.as_str()), Some(k));
        }
        assert_eq!(Kind::parse("grant_issued"), None);
    }

    #[test]
    fn the_compositor_emits_exactly_comp12_section_2() {
        let theirs = [
            Kind::Grant,
            Kind::Revoke,
            Kind::Channel,
            Kind::Sandbox,
            Kind::Anchor,
            Kind::Task,
            Kind::Net,
            Kind::Secret,
        ];
        for k in Kind::ALL {
            assert_eq!(k.from_compositor(), !theirs.contains(&k), "{k:?}");
        }
    }

    #[test]
    fn principals_are_the_three_shapes() {
        for ok in ["human", "agent:leak", "system:policyd"] {
            assert!(principal_ok(ok), "{ok}");
        }
        for bad in ["", "agent:", "root", "agent:a b", "humans", "system:"] {
            assert!(!principal_ok(bad), "{bad}");
        }
    }

    #[test]
    fn a_wrong_version_or_unknown_field_is_refused() {
        let mut m = MapBuilder::new();
        m.insert("body", enc(|w| w.null()));
        m.insert("kind", enc(|w| w.text("focus")));
        m.insert("principal", enc(|w| w.text("human")));
        m.insert("v", enc(|w| w.u64(2)));
        assert_eq!(Emission::decode(&m.finish()), Err(EmissionError::Shape));

        let mut m = MapBuilder::new();
        m.insert("body", enc(|w| w.null()));
        m.insert("kind", enc(|w| w.text("focus")));
        m.insert("principal", enc(|w| w.text("human")));
        m.insert("seq", enc(|w| w.u64(0)));
        m.insert("v", enc(|w| w.u64(1)));
        assert_eq!(Emission::decode(&m.finish()), Err(EmissionError::Shape));
    }

    #[test]
    fn trailing_bytes_are_refused() {
        let mut bytes = sample().encode();
        bytes.push(0);
        assert!(Emission::decode(&bytes).is_err());
    }
}
