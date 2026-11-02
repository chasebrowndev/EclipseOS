// SPDX-License-Identifier: AGPL-3.0-only

//! Request and Session object model: tokens, object paths, response codes,
//! result builders and the live-object tables. Pure; the daemon exports the
//! actual D-Bus objects.

use std::collections::HashMap;

use crate::val::Val;

pub const REQUEST_PREFIX: &str = "/org/freedesktop/portal/desktop/request";
pub const SESSION_PREFIX: &str = "/org/freedesktop/portal/desktop/session";

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum TokenError {
    #[error("handle token must be non-empty and contain only [A-Za-z0-9_]")]
    Invalid,
    #[error("bus name is not a unique connection name")]
    BadSender,
}

/// A token becomes one object-path element: `[A-Za-z0-9_]+`, bounded.
pub fn validate_token(t: &str) -> Result<&str, TokenError> {
    if t.is_empty() || t.len() > 255 || !t.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_') {
        Err(TokenError::Invalid)
    } else {
        Ok(t)
    }
}

/// `:1.42` becomes `1_42` (leading colon dropped, dots to underscores).
pub fn sender_component(unique_name: &str) -> Result<String, TokenError> {
    let rest = unique_name.strip_prefix(':').ok_or(TokenError::BadSender)?;
    let c: String = rest.replace('.', "_");
    // Unique names may also contain '-'; map it so the element stays valid.
    let c: String = c.replace('-', "_");
    if c.is_empty() || !c.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_') {
        return Err(TokenError::BadSender);
    }
    Ok(c)
}

pub fn request_path(sender: &str, token: &str) -> Result<String, TokenError> {
    Ok(format!(
        "{REQUEST_PREFIX}/{}/{}",
        sender_component(sender)?,
        validate_token(token)?
    ))
}

pub fn session_path(sender: &str, token: &str) -> Result<String, TokenError> {
    Ok(format!(
        "{SESSION_PREFIX}/{}/{}",
        sender_component(sender)?,
        validate_token(token)?
    ))
}

/// A token for callers that gave none; `n` is a daemon-wide counter.
pub fn generated_token(n: u64) -> String {
    format!("ecl_{n}")
}

/// The `response` of `Request::Response`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Response {
    Success,
    /// The user cancelled the interaction.
    Cancelled,
    /// Ended in some other way (error, unsupported, denied by policy).
    Other,
}

impl Response {
    pub const fn code(self) -> u32 {
        match self {
            Response::Success => 0,
            Response::Cancelled => 1,
            Response::Other => 2,
        }
    }
}

/// `results` dictionary builder. Insertion order is kept; a later insert of
/// the same key replaces the earlier one.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Results(pub Vec<(String, Val)>);

impl Results {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with(mut self, key: &str, v: Val) -> Self {
        self.insert(key, v);
        self
    }

    pub fn insert(&mut self, key: &str, v: Val) {
        if let Some(slot) = self.0.iter_mut().find(|(k, _)| k == key) {
            slot.1 = v;
        } else {
            self.0.push((key.to_owned(), v));
        }
    }

    pub fn with_str(self, key: &str, s: impl Into<String>) -> Self {
        self.with(key, Val::Str(s.into()))
    }

    pub fn with_uris(self, key: &str, uris: Vec<String>) -> Self {
        self.with(key, Val::Strs(uris))
    }

    pub fn get(&self, key: &str) -> Option<&Val> {
        self.0.iter().find(|(k, _)| k == key).map(|(_, v)| v)
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

/// What a request ends with.
#[derive(Debug, Clone, PartialEq)]
pub struct Reply {
    pub response: Response,
    pub results: Results,
}

impl Reply {
    pub fn ok(results: Results) -> Self {
        Reply {
            response: Response::Success,
            results,
        }
    }

    pub fn cancelled() -> Self {
        Reply {
            response: Response::Cancelled,
            results: Results::new(),
        }
    }

    /// Failure/denial. Results are empty: nothing leaks on a refusal.
    pub fn other() -> Self {
        Reply {
            response: Response::Other,
            results: Results::new(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("object path already in use")]
pub struct Duplicate;

/// Live objects (Requests or Sessions) keyed by object path. `C` is whatever
/// the daemon needs to cancel one (a oneshot sender). Each entry remembers its
/// owning bus name so a vanished peer can take its objects with it.
#[derive(Debug)]
pub struct LiveTable<C> {
    map: HashMap<String, (String, C)>,
}

impl<C> Default for LiveTable<C> {
    fn default() -> Self {
        LiveTable {
            map: HashMap::new(),
        }
    }
}

impl<C> LiveTable<C> {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn insert(&mut self, path: String, owner: &str, c: C) -> Result<(), Duplicate> {
        if self.map.contains_key(&path) {
            return Err(Duplicate);
        }
        self.map.insert(path, (owner.to_owned(), c));
        Ok(())
    }

    pub fn contains(&self, path: &str) -> bool {
        self.map.contains_key(path)
    }

    pub fn remove(&mut self, path: &str) -> Option<C> {
        self.map.remove(path).map(|(_, c)| c)
    }

    pub fn get(&self, path: &str) -> Option<&C> {
        self.map.get(path).map(|(_, c)| c)
    }

    /// Owner of `path`, for checking that only the creator closes it.
    pub fn owner(&self, path: &str) -> Option<&str> {
        self.map.get(path).map(|(o, _)| o.as_str())
    }

    /// Remove and return everything owned by `owner` (peer exit).
    pub fn take_owner(&mut self, owner: &str) -> Vec<(String, C)> {
        let paths: Vec<String> = self
            .map
            .iter()
            .filter(|(_, (o, _))| o == owner)
            .map(|(p, _)| p.clone())
            .collect();
        paths
            .into_iter()
            .filter_map(|p| self.map.remove(&p).map(|(_, c)| (p, c)))
            .collect()
    }

    /// Distinct owners with at least one live object.
    pub fn owners(&self) -> Vec<String> {
        let mut v: Vec<String> = self.map.values().map(|(o, _)| o.clone()).collect();
        v.sort();
        v.dedup();
        v
    }

    pub fn len(&self) -> usize {
        self.map.len()
    }

    pub fn is_empty(&self) -> bool {
        self.map.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tokens() {
        assert!(validate_token("abc_123").is_ok());
        for bad in ["", "a b", "a/b", "a.b", "é", "a\0"] {
            assert_eq!(validate_token(bad), Err(TokenError::Invalid), "{bad:?}");
        }
        assert!(validate_token(&"a".repeat(256)).is_err());
    }

    #[test]
    fn paths() {
        assert_eq!(sender_component(":1.42").as_deref(), Ok("1_42"));
        assert_eq!(sender_component("org.x.Y"), Err(TokenError::BadSender));
        assert_eq!(sender_component(":"), Err(TokenError::BadSender));
        assert_eq!(
            request_path(":1.42", "t0").unwrap(),
            "/org/freedesktop/portal/desktop/request/1_42/t0"
        );
        assert_eq!(
            session_path(":1.7", "s").unwrap(),
            "/org/freedesktop/portal/desktop/session/1_7/s"
        );
        assert!(request_path(":1.7", "../x").is_err());
    }

    #[test]
    fn response_codes() {
        assert_eq!(Response::Success.code(), 0);
        assert_eq!(Response::Cancelled.code(), 1);
        assert_eq!(Response::Other.code(), 2);
        assert_eq!(generated_token(5), "ecl_5");
    }

    #[test]
    fn results_builder_replaces() {
        let r = Results::new()
            .with("a", Val::U32(1))
            .with_str("b", "x")
            .with("a", Val::U32(2));
        assert_eq!(r.0.len(), 2);
        assert_eq!(r.get("a"), Some(&Val::U32(2)));
        assert!(Reply::other().results.is_empty());
    }

    #[test]
    fn live_table() {
        let mut t: LiveTable<u8> = LiveTable::new();
        t.insert("/a".into(), ":1.1", 1).unwrap();
        t.insert("/b".into(), ":1.1", 2).unwrap();
        t.insert("/c".into(), ":1.2", 3).unwrap();
        assert_eq!(t.insert("/a".into(), ":1.9", 9), Err(Duplicate));
        assert_eq!(t.owner("/c"), Some(":1.2"));
        assert_eq!(t.owners(), vec![":1.1", ":1.2"]);
        let mut gone: Vec<_> = t.take_owner(":1.1").into_iter().map(|(_, c)| c).collect();
        gone.sort();
        assert_eq!(gone, vec![1, 2]);
        assert_eq!(t.len(), 1);
        assert_eq!(t.remove("/c"), Some(3));
        assert!(t.is_empty());
    }
}
