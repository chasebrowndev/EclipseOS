// SPDX-License-Identifier: AGPL-3.0-only
//! The secret record (S-08 §2.1): everything about a secret except its value.
//!
//! Metadata and value are sealed separately in the store so that every
//! precondition can be decided from the metadata, and the value is only
//! decrypted once they have all passed.

use crate::bind::Binding;
use ec_policy_eval::cbor::{enc, MapBuilder, Reader};
use ec_policy_eval::Ulid;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Bearer,
    Basic,
    Cookie,
    Password,
    Totp,
    SshKey,
    Env,
}

impl Kind {
    pub fn as_str(self) -> &'static str {
        match self {
            Kind::Bearer => "bearer",
            Kind::Basic => "basic",
            Kind::Cookie => "cookie",
            Kind::Password => "password",
            Kind::Totp => "totp",
            Kind::SshKey => "ssh_key",
            Kind::Env => "env",
        }
    }

    pub fn parse(s: &str) -> Option<Kind> {
        [
            Kind::Bearer,
            Kind::Basic,
            Kind::Cookie,
            Kind::Password,
            Kind::Totp,
            Kind::SshKey,
            Kind::Env,
        ]
        .into_iter()
        .find(|k| k.as_str() == s)
    }
}

/// An injection mode (S-08 §3). A property of the record: the agent cannot
/// choose it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    ProxyHeader,
    FieldFill,
    Materialize,
}

impl Mode {
    pub fn as_str(self) -> &'static str {
        match self {
            Mode::ProxyHeader => "proxy_header",
            Mode::FieldFill => "field_fill",
            Mode::Materialize => "materialize",
        }
    }

    pub fn parse(s: &str) -> Option<Mode> {
        [Mode::ProxyHeader, Mode::FieldFill, Mode::Materialize]
            .into_iter()
            .find(|m| m.as_str() == s)
    }
}

/// What the owner supplies when adding a secret.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewSecret {
    pub name: String,
    pub kind: Kind,
    pub bound_to: Vec<Binding>,
    pub modes: Vec<Mode>,
    pub requires_prompt: bool,
    pub rotation_hint_days: Option<u32>,
}

/// A stored record's metadata.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Meta {
    pub name: String,
    pub id: Ulid,
    pub kind: Kind,
    pub bound_to: Vec<Binding>,
    pub modes: Vec<Mode>,
    pub requires_prompt: bool,
    pub rotation_hint_days: Option<u32>,
    pub created_unix: u64,
    pub rotation_counter: u64,
}

pub const MAX_NAME: usize = 128;
pub const MAX_BINDINGS: usize = 32;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RecordError {
    /// A name outside `[A-Za-z0-9._-]{1,128}`.
    Name,
    /// `bound_to` is mandatory (S-08 §2.1) and bounded.
    Binding,
    /// A non-TOTP secret needs at least one injection mode.
    Modes,
    /// Undecodable metadata.
    Decode,
}

pub fn name_ok(n: &str) -> bool {
    !n.is_empty()
        && n.len() <= MAX_NAME
        && n.bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))
}

impl NewSecret {
    pub fn validate(&self) -> Result<(), RecordError> {
        if !name_ok(&self.name) {
            return Err(RecordError::Name);
        }
        if self.bound_to.is_empty() || self.bound_to.len() > MAX_BINDINGS {
            return Err(RecordError::Binding);
        }
        if self.modes.is_empty() && self.kind != Kind::Totp {
            return Err(RecordError::Modes);
        }
        Ok(())
    }
}

impl Meta {
    pub fn has_mode(&self, m: Mode) -> bool {
        self.modes.contains(&m)
    }

    pub fn encode(&self) -> Vec<u8> {
        let mut m = MapBuilder::new();
        m.insert(
            "bound_to",
            enc(|w| {
                w.array(self.bound_to.len());
                for b in &self.bound_to {
                    w.text(&b.to_text());
                }
            }),
        );
        m.insert("created", enc(|w| w.u64(self.created_unix)));
        m.insert_opt("hint", self.rotation_hint_days.map(|d| enc(|w| w.u64(d as u64))));
        m.insert("id", enc(|w| w.bytes(&self.id.0)));
        m.insert("kind", enc(|w| w.text(self.kind.as_str())));
        m.insert(
            "modes",
            enc(|w| {
                w.array(self.modes.len());
                for x in &self.modes {
                    w.text(x.as_str());
                }
            }),
        );
        m.insert("name", enc(|w| w.text(&self.name)));
        m.insert("prompt", enc(|w| w.bool(self.requires_prompt)));
        m.insert("rc", enc(|w| w.u64(self.rotation_counter)));
        m.finish()
    }

    pub fn decode(buf: &[u8]) -> Result<Meta, RecordError> {
        let e = |_| RecordError::Decode;
        let mut r = Reader::new(buf);
        let n = r.map_begin().map_err(e)?;
        let (mut name, mut id, mut kind, mut created, mut rc) = (None, None, None, None, None);
        let (mut bound, mut modes, mut prompt, mut hint) = (None, None, None, None);
        for _ in 0..n {
            match r.key().map_err(e)? {
                "bound_to" => {
                    let k = r.array_len().map_err(e)?;
                    if k > MAX_BINDINGS as u64 {
                        return Err(RecordError::Decode);
                    }
                    let mut v = Vec::new();
                    for _ in 0..k {
                        v.push(Binding::parse(r.text().map_err(e)?).map_err(|_| RecordError::Decode)?);
                    }
                    bound = Some(v);
                }
                "created" => created = Some(r.u64().map_err(e)?),
                "hint" => hint = Some(u32::try_from(r.u64().map_err(e)?).map_err(|_| RecordError::Decode)?),
                "id" => id = Some(Ulid(r.byte_array::<16>().map_err(e)?)),
                "kind" => kind = Some(Kind::parse(r.text().map_err(e)?).ok_or(RecordError::Decode)?),
                "modes" => {
                    let k = r.array_len().map_err(e)?;
                    if k > 3 {
                        return Err(RecordError::Decode);
                    }
                    let mut v = Vec::new();
                    for _ in 0..k {
                        v.push(Mode::parse(r.text().map_err(e)?).ok_or(RecordError::Decode)?);
                    }
                    modes = Some(v);
                }
                "name" => name = Some(r.text().map_err(e)?.to_owned()),
                "prompt" => prompt = Some(r.bool().map_err(e)?),
                "rc" => rc = Some(r.u64().map_err(e)?),
                _ => return Err(RecordError::Decode),
            }
        }
        r.map_end().map_err(e)?;
        r.finish().map_err(e)?;
        let d = RecordError::Decode;
        let meta = Meta {
            name: name.ok_or(d)?,
            id: id.ok_or(d)?,
            kind: kind.ok_or(d)?,
            bound_to: bound.ok_or(d)?,
            modes: modes.ok_or(d)?,
            requires_prompt: prompt.ok_or(d)?,
            rotation_hint_days: hint,
            created_unix: created.ok_or(d)?,
            rotation_counter: rc.ok_or(d)?,
        };
        if !name_ok(&meta.name) || meta.bound_to.is_empty() {
            return Err(d);
        }
        Ok(meta)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    pub fn sample() -> Meta {
        Meta {
            name: "acme-invoices-api".into(),
            id: Ulid([3; 16]),
            kind: Kind::Bearer,
            bound_to: vec![Binding::parse("host:api.acme-invoices.com").unwrap()],
            modes: vec![Mode::ProxyHeader],
            requires_prompt: false,
            rotation_hint_days: Some(90),
            created_unix: 1_788_000_000,
            rotation_counter: 3,
        }
    }

    #[test]
    fn meta_round_trips_canonically() {
        let m = sample();
        let b = m.encode();
        assert_eq!(Meta::decode(&b), Ok(m.clone()));
        assert_eq!(Meta::decode(&b).unwrap().encode(), b);
    }

    #[test]
    fn unbound_and_modeless_secrets_are_refused() {
        let mut n = NewSecret {
            name: "x".into(),
            kind: Kind::Bearer,
            bound_to: vec![],
            modes: vec![Mode::ProxyHeader],
            requires_prompt: false,
            rotation_hint_days: None,
        };
        assert_eq!(n.validate(), Err(RecordError::Binding));
        n.bound_to = vec![Binding::parse("host:a.com").unwrap()];
        n.modes.clear();
        assert_eq!(n.validate(), Err(RecordError::Modes));
        n.kind = Kind::Totp;
        assert_eq!(n.validate(), Ok(()));
        n.name = "../etc".into();
        assert_eq!(n.validate(), Err(RecordError::Name));
    }
}
