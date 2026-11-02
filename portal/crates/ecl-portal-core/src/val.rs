// SPDX-License-Identifier: AGPL-3.0-only

//! A bus-free stand-in for the D-Bus variants the portals exchange.

/// One decoded (or to-be-encoded) variant value. Anything the daemon cannot
/// represent decodes to [`Val::Other`], which getters treat as absent.
#[derive(Debug, Clone, PartialEq)]
pub enum Val {
    Bool(bool),
    U8(u8),
    I32(i32),
    U32(u32),
    I64(i64),
    U64(u64),
    F64(f64),
    Str(String),
    /// Object path.
    Path(String),
    /// `ay`.
    Bytes(Vec<u8>),
    /// `as`.
    Strs(Vec<String>),
    /// A homogeneous array of anything (input side).
    List(Vec<Val>),
    /// A D-Bus struct.
    Struct(Vec<Val>),
    /// `(ddd)` colour or similar triple.
    Rgb(f64, f64, f64),
    /// `a{sv}`.
    Dict(Vec<(String, Val)>),
    Other,
}

impl Val {
    pub fn as_bool(&self) -> Option<bool> {
        match self {
            Val::Bool(b) => Some(*b),
            _ => None,
        }
    }

    pub fn as_u32(&self) -> Option<u32> {
        match self {
            Val::U32(v) => Some(*v),
            Val::I32(v) => u32::try_from(*v).ok(),
            Val::U8(v) => Some(u32::from(*v)),
            _ => None,
        }
    }

    pub fn as_str(&self) -> Option<&str> {
        match self {
            Val::Str(s) | Val::Path(s) => Some(s),
            _ => None,
        }
    }

    /// `ay` as bytes (also accepts a [`Val::List`] of [`Val::U8`]).
    pub fn as_bytes(&self) -> Option<Vec<u8>> {
        match self {
            Val::Bytes(b) => Some(b.clone()),
            Val::List(l) => l
                .iter()
                .map(|v| match v {
                    Val::U8(b) => Some(*b),
                    _ => None,
                })
                .collect(),
            _ => None,
        }
    }

    pub fn as_strs(&self) -> Option<Vec<String>> {
        match self {
            Val::Strs(s) => Some(s.clone()),
            Val::List(l) => l.iter().map(|v| v.as_str().map(str::to_owned)).collect(),
            _ => None,
        }
    }

    pub fn as_list(&self) -> Option<&[Val]> {
        match self {
            Val::List(l) => Some(l),
            _ => None,
        }
    }

    pub fn as_struct(&self) -> Option<&[Val]> {
        match self {
            Val::Struct(l) => Some(l),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn getters() {
        assert_eq!(Val::Bool(true).as_bool(), Some(true));
        assert_eq!(Val::Str("a".into()).as_bool(), None);
        assert_eq!(Val::I32(-1).as_u32(), None);
        assert_eq!(Val::I32(3).as_u32(), Some(3));
        assert_eq!(
            Val::List(vec![Val::U8(1), Val::U8(2)]).as_bytes(),
            Some(vec![1, 2])
        );
        assert_eq!(
            Val::List(vec![Val::U8(1), Val::Bool(true)]).as_bytes(),
            None
        );
        assert_eq!(
            Val::List(vec![Val::Str("x".into())]).as_strs(),
            Some(vec!["x".to_string()])
        );
        assert_eq!(Val::Other.as_str(), None);
    }
}
