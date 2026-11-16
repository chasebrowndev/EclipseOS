// SPDX-License-Identifier: Apache-2.0
//! Events on the wire: `{"event":"…","seq":n,…}` (T-12 §1). The payload is the
//! `apoth-action` [`Event`](apoth_action::Event) flattened next to `event`, `seq`,
//! `subscription` and `cause`. Content-bearing string fields may be `null` (withheld).

use apoth_action::event::{COMMON_FIELDS, FieldType};
use apoth_action::{Cause, Event, EventKind, FieldValue};
use serde::ser::SerializeMap;
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use serde_json::{Map, Value};

use crate::error::WireError;

/// Keys the envelope reserves; no event field may use one (checked by test).
pub const RESERVED_KEYS: [&str; 4] = ["event", "seq", "subscription", "cause"];

/// Wire spelling of a cause.
pub const fn cause_name(c: Cause) -> &'static str {
    match c {
        Cause::User => "user",
        Cause::Config => "config",
        Cause::Sequence => "sequence",
        Cause::Socket => "socket",
        Cause::Child => "child",
    }
}

fn cause_parse(s: &str) -> Option<Cause> {
    [
        Cause::User,
        Cause::Config,
        Cause::Sequence,
        Cause::Socket,
        Cause::Child,
    ]
    .into_iter()
    .find(|c| cause_name(*c) == s)
}

/// One event line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WireEvent {
    /// Per-connection sequence number, strictly increasing (gap: scope is a proposal).
    pub seq: u64,
    /// The subscription it belongs to.
    pub subscription: Option<u64>,
    /// Event kind.
    pub kind: EventKind,
    /// What caused it.
    pub cause: Cause,
    /// Fields; `None` is a withheld content-bearing value, serialised as `null`.
    pub fields: Vec<(String, Option<FieldValue>)>,
}

impl WireEvent {
    /// Build from an internal event. With `redact_content`, every content-bearing field
    /// is withheld (agent-descendant peers without `read-content`; credential state).
    pub fn from_event(
        seq: u64,
        subscription: Option<u64>,
        ev: &Event,
        redact_content: bool,
    ) -> Self {
        let mut fields = Vec::new();
        for spec in ev.kind.fields().iter().chain(COMMON_FIELDS.iter()) {
            if let Some(v) = ev.get(spec.name) {
                let v = if redact_content && spec.content {
                    None
                } else {
                    Some(v.clone())
                };
                fields.push((spec.name.to_owned(), v));
            }
        }
        fields.sort_by(|a, b| a.0.cmp(&b.0));
        Self {
            seq,
            subscription,
            kind: ev.kind,
            cause: ev.cause,
            fields,
        }
    }

    /// The JSON object.
    pub fn to_value(&self) -> Value {
        let mut m = Map::new();
        m.insert("event".into(), Value::from(self.kind.as_str()));
        m.insert("seq".into(), Value::from(self.seq));
        if let Some(s) = self.subscription {
            m.insert("subscription".into(), Value::from(s));
        }
        m.insert("cause".into(), Value::from(cause_name(self.cause)));
        for (k, v) in &self.fields {
            let v = match v {
                None => Value::Null,
                Some(FieldValue::Str(s)) => Value::from(s.as_str()),
                Some(FieldValue::Int(n)) => Value::from(*n),
                Some(FieldValue::Bool(b)) => Value::from(*b),
            };
            m.insert(k.clone(), v);
        }
        Value::Object(m)
    }

    /// Parse and validate against the event vocabulary: unknown field names, wrong types,
    /// nulls on non-content fields and over-budget strings are errors.
    pub fn from_value(v: Value) -> Result<Self, WireError> {
        let Value::Object(mut m) = v else {
            return Err(WireError::bad_request("event is not an object"));
        };
        let kind = match m.remove("event") {
            Some(Value::String(s)) => {
                EventKind::parse(&s).ok_or_else(|| WireError::bad_request("unknown event name"))?
            }
            _ => return Err(WireError::bad_request("event name missing or not a string")),
        };
        let seq = m
            .remove("seq")
            .and_then(|v| v.as_u64())
            .ok_or_else(|| WireError::bad_request("seq missing or not an unsigned integer"))?;
        let subscription = match m.remove("subscription") {
            None => None,
            Some(v) => Some(
                v.as_u64()
                    .ok_or_else(|| WireError::bad_request("subscription is not an id"))?,
            ),
        };
        let cause = match m.remove("cause") {
            Some(Value::String(s)) => {
                cause_parse(&s).ok_or_else(|| WireError::bad_request("unknown cause"))?
            }
            _ => return Err(WireError::bad_request("cause missing or not a string")),
        };
        let mut fields = Vec::with_capacity(m.len());
        // `Map` iterates in key order, matching `from_event`'s sorted order.
        for (name, val) in m {
            let Some(spec) = kind.field(&name) else {
                return Err(WireError::bad_request("unknown event field"));
            };
            let fv = match (val, spec.ty) {
                (Value::Null, _) if spec.content => None,
                (Value::String(s), FieldType::Str) => {
                    if s.len() > apoth_action::event::MAX_FIELD_BYTES {
                        return Err(WireError::bad_request("event string field over budget"));
                    }
                    Some(FieldValue::Str(s))
                }
                (Value::Number(n), FieldType::Int | FieldType::Duration) => {
                    Some(FieldValue::Int(n.as_i64().ok_or_else(|| {
                        WireError::bad_request("event integer out of range")
                    })?))
                }
                (Value::Bool(b), FieldType::Bool) => Some(FieldValue::Bool(b)),
                _ => return Err(WireError::bad_request("event field has the wrong type")),
            };
            fields.push((spec.name.to_owned(), fv));
        }
        fields.sort_by(|a, b| a.0.cmp(&b.0));
        Ok(Self {
            seq,
            subscription,
            kind,
            cause,
            fields,
        })
    }

    /// Look a field up; the outer `Option` is presence, the inner is "withheld".
    pub fn get(&self, name: &str) -> Option<&Option<FieldValue>> {
        self.fields.iter().find(|(n, _)| n == name).map(|(_, v)| v)
    }
}

impl Serialize for WireEvent {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        match self.to_value() {
            Value::Object(m) => {
                let mut map = s.serialize_map(Some(m.len()))?;
                for (k, v) in &m {
                    map.serialize_entry(k, v)?;
                }
                map.end()
            }
            _ => Err(serde::ser::Error::custom("event is not an object")),
        }
    }
}

impl<'de> Deserialize<'de> for WireEvent {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let v = Value::deserialize(d)?;
        WireEvent::from_value(v).map_err(|e| serde::de::Error::custom(e.message))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_event_field_collides_with_envelope_keys() {
        for k in EventKind::ALL {
            for f in k.fields().iter().chain(COMMON_FIELDS.iter()) {
                assert!(
                    !RESERVED_KEYS.contains(&f.name),
                    "{} / {}",
                    k.as_str(),
                    f.name
                );
            }
        }
    }

    #[test]
    fn every_kind_roundtrips_without_fields() {
        for k in EventKind::ALL {
            let ev = Event::new(*k, Cause::Child);
            let w = WireEvent::from_event(7, Some(2), &ev, false);
            let back = WireEvent::from_value(w.to_value()).unwrap();
            assert_eq!(back, w);
        }
    }
}
