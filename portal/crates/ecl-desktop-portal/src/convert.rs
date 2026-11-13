// SPDX-License-Identifier: AGPL-3.0-only

//! The only place that knows how `Val` maps to and from D-Bus variants.

use std::collections::HashMap;

use ecl_portal_core::options::Options;
use ecl_portal_core::request::Results;
use ecl_portal_core::val::Val;
use zbus::{OwnedObjectPath, OwnedVariant, Structure, Variant};

/// The `a{sv}` as it arrives on the wire.
pub type WireDict = HashMap<String, OwnedVariant>;

/// Decode a variant. Unknown shapes become [`Val::Other`], which every getter
/// treats as absent.
pub fn to_val(v: &Variant<'_>) -> Val {
    match v {
        Variant::Bool(b) => Val::Bool(*b),
        Variant::U8(x) => Val::U8(*x),
        Variant::I32(x) => Val::I32(*x),
        Variant::U32(x) => Val::U32(*x),
        Variant::I64(x) => Val::I64(*x),
        Variant::U64(x) => Val::U64(*x),
        Variant::F64(x) => Val::F64(*x),
        Variant::Str(s) => Val::Str(s.as_str().to_owned()),
        Variant::ObjectPath(p) => Val::Path(p.as_str().to_owned()),
        Variant::Variant(inner) => to_val(inner),
        Variant::Array(a) => Val::List(a.inner().iter().map(to_val).collect()),
        Variant::Structure(s) => Val::Struct(s.fields().iter().map(to_val).collect()),
        Variant::Dict(d) => Val::Dict(
            d.iter()
                .filter_map(|(k, v)| match to_val(k) {
                    Val::Str(k) => Some((k, to_val(v))),
                    _ => None,
                })
                .collect(),
        ),
        _ => Val::Other,
    }
}

pub fn to_options(d: &WireDict) -> Options {
    Options::from_pairs(d.iter().map(|(k, v)| (k.clone(), to_val(v))))
}

/// Encode a value; `None` if it has no wire form (e.g. [`Val::Other`], or a
/// mixed list).
pub fn from_val(v: &Val) -> Option<Variant<'static>> {
    Some(match v {
        Val::Bool(b) => Variant::from(*b),
        Val::U8(x) => Variant::from(*x),
        Val::I32(x) => Variant::from(*x),
        Val::U32(x) => Variant::from(*x),
        Val::I64(x) => Variant::from(*x),
        Val::U64(x) => Variant::from(*x),
        Val::F64(x) => Variant::from(*x),
        Val::Str(s) => Variant::from(s.clone()),
        Val::Path(p) => Variant::from(OwnedObjectPath::try_from(p.clone()).ok()?),
        Val::Bytes(b) => Variant::from(b.clone()),
        Val::Strs(s) => Variant::from(s.clone()),
        Val::Rgb(r, g, b) => Variant::from((*r, *g, *b)),
        // Homogeneous array of structs (ScreenCast `streams`: `a(ua{sv})`).
        Val::List(l) if !l.is_empty() && l.iter().all(|x| matches!(x, Val::Struct(_))) => {
            let items: Vec<Variant<'static>> = l.iter().map(from_val).collect::<Option<_>>()?;
            let sig = items[0].value_signature().clone();
            let mut a = zbus::Array::new(&sig);
            for i in items {
                a.append(i).ok()?;
            }
            Variant::Array(a)
        }
        Val::List(l) => {
            if l.iter().all(|x| matches!(x, Val::U8(_))) && !l.is_empty() {
                Variant::from(v.as_bytes()?)
            } else {
                Variant::from(v.as_strs()?)
            }
        }
        Val::Struct(fields) => {
            let mut b = Structure::builder();
            for f in fields {
                b = b.append_field(from_val(f)?);
            }
            Variant::Structure(b.build().ok()?)
        }
        Val::Dict(d) => Variant::from(dict(d)),
        Val::Other => return None,
    })
}

pub fn owned(v: &Val) -> Option<OwnedVariant> {
    OwnedVariant::try_from(from_val(v)?).ok()
}

fn dict(pairs: &[(String, Val)]) -> WireDict {
    pairs
        .iter()
        .filter_map(|(k, v)| Some((k.clone(), owned(v)?)))
        .collect()
}

/// `results` for `Request::Response`.
pub fn results_map(r: &Results) -> WireDict {
    dict(&r.0)
}

pub fn object_path(p: &str) -> Result<OwnedObjectPath, zbus::Error> {
    OwnedObjectPath::try_from(p.to_owned())
}

/// A `v` return wrapping another variant (legacy `Settings.Read`).
pub fn wrapped(v: &Val) -> Option<OwnedVariant> {
    OwnedVariant::try_from(Variant::Variant(Box::new(from_val(v)?))).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scalar_round_trips() {
        for v in [
            Val::Bool(true),
            Val::U32(7),
            Val::I32(-2),
            Val::F64(0.5),
            Val::Str("x".into()),
            Val::Strs(vec!["a".into(), "b".into()]),
            Val::Bytes(vec![1, 2, 3]),
        ] {
            let back = to_val(&from_val(&v).unwrap());
            match (&v, &back) {
                (Val::Strs(a), Val::List(l)) => {
                    assert_eq!(Some(a.clone()), Val::List(l.clone()).as_strs())
                }
                (Val::Bytes(a), Val::List(l)) => {
                    assert_eq!(Some(a.clone()), Val::List(l.clone()).as_bytes())
                }
                _ => assert_eq!(v, back),
            }
        }
    }

    #[test]
    fn accent_is_a_triple() {
        let back = to_val(&from_val(&Val::Rgb(1.0, 0.5, 0.0)).unwrap());
        assert_eq!(
            back,
            Val::Struct(vec![Val::F64(1.0), Val::F64(0.5), Val::F64(0.0)])
        );
    }

    #[test]
    fn nested_variant_unwraps() {
        let v = Variant::Variant(Box::new(Variant::from(3u32)));
        assert_eq!(to_val(&v), Val::U32(3));
    }

    #[test]
    fn dict_input() {
        let mut d = WireDict::new();
        d.insert(
            "handle_token".into(),
            OwnedVariant::try_from(Variant::from("t")).unwrap(),
        );
        d.insert("multiple".into(), OwnedVariant::from(true));
        let o = to_options(&d);
        assert_eq!(o.str("handle_token"), Some("t"));
        assert_eq!(o.bool("multiple"), Some(true));
    }

    #[test]
    fn unrepresentable_is_dropped() {
        let r = Results::new()
            .with("ok", Val::U32(1))
            .with("bad", Val::Other);
        let m = results_map(&r);
        assert_eq!(m.len(), 1);
        assert!(m.contains_key("ok"));
    }

    #[test]
    fn streams_encode_as_a_ua_sv() {
        let st = Val::Struct(vec![
            Val::U32(57),
            Val::Dict(vec![
                ("source_type".into(), Val::U32(1)),
                (
                    "size".into(),
                    Val::Struct(vec![Val::I32(1920), Val::I32(1080)]),
                ),
            ]),
        ]);
        let v = from_val(&Val::List(vec![st])).unwrap();
        assert_eq!(v.value_signature().to_string(), "a(ua{sv})");
    }
}
