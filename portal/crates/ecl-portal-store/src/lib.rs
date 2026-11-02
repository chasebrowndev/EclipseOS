// SPDX-License-Identifier: AGPL-3.0-only

//! Permission store: `org.freedesktop.impl.portal.PermissionStore` semantics
//! (`table / id / app -> permissions, data`) plus grant records with expiry and
//! ask-every-time, kept as atomic postcard files under
//! `$XDG_DATA_HOME/ecl-portal`.
//!
//! Stateless over the files: every operation reads fresh and writes through,
//! so `ecl-portal-ctl` and the daemon never disagree. Records are claims we
//! store for the user's convenience; they are not enforcement (ADR 0084). A
//! missing or unreadable record means "ask", never "allow".

use std::collections::BTreeMap;
use std::fs::{self, DirBuilder};
use std::io::Write;
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

const FILE_VERSION: u16 = 1;

#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error("invalid table or id name")]
    InvalidName,
    #[error("no such entry")]
    NotFound,
    #[error("entry already exists")]
    Exists,
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("encode: {0}")]
    Encode(#[from] postcard::Error),
}

/// Value attached to a store entry (the `v` of the D-Bus API). Only plain
/// types are storable; the daemon rejects anything else.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub enum Data {
    #[default]
    None,
    Bool(bool),
    U32(u32),
    I64(i64),
    F64(f64),
    Str(String),
    Bytes(Vec<u8>),
    Strs(Vec<String>),
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Entry {
    pub data: Data,
    /// app id -> permission strings.
    pub permissions: BTreeMap<String, Vec<String>>,
}

type Table = BTreeMap<String, Entry>;

#[derive(Serialize, Deserialize)]
struct TableFile {
    version: u16,
    entries: Table,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Decision {
    Allow,
    Deny,
    /// Ask the user (also the answer for no record, expired, or unreadable).
    Ask,
}

/// What the user chose and for how long.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Standing {
    Allow,
    Deny,
    /// Always ask, remembered only so the user can see the choice.
    AskEveryTime,
}

/// One remembered answer for `(app, table, resource)`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Grant {
    pub app: String,
    /// Portal area, e.g. `screencast`, `account`, `email`.
    pub table: String,
    /// Resource within the area; empty for the whole area.
    pub resource: String,
    pub standing: Standing,
    pub granted_at: u64,
    /// Unix seconds; `None` never expires.
    pub expires_at: Option<u64>,
}

impl Grant {
    fn key_eq(&self, app: &str, table: &str, resource: &str) -> bool {
        self.app == app && self.table == table && self.resource == resource
    }

    fn live(&self, now: u64) -> bool {
        self.expires_at.is_none_or(|e| now < e)
    }
}

#[derive(Serialize, Deserialize)]
struct GrantFile {
    version: u16,
    grants: Vec<Grant>,
}

/// `$XDG_DATA_HOME/ecl-portal`, falling back to `$HOME/.local/share/ecl-portal`.
pub fn data_root(xdg_data_home: Option<&str>, home: Option<&str>) -> Option<PathBuf> {
    let base = match xdg_data_home.filter(|s| s.starts_with('/')) {
        Some(x) => PathBuf::from(x),
        None => Path::new(home.filter(|s| s.starts_with('/'))?).join(".local/share"),
    };
    Some(base.join("ecl-portal"))
}

fn name_ok(s: &str) -> bool {
    !s.is_empty() && s.len() <= 255 && !s.starts_with('.') && !s.contains('/') && !s.contains('\0')
}

/// Write `bytes` to `path` via a same-directory temp file and rename.
fn write_atomic(path: &Path, bytes: &[u8]) -> Result<(), StoreError> {
    let dir = path.parent().ok_or(StoreError::InvalidName)?;
    DirBuilder::new().recursive(true).mode(0o700).create(dir)?;
    let tmp = dir.join(format!(
        ".{}.{}.tmp",
        path.file_name().and_then(|n| n.to_str()).unwrap_or("f"),
        std::process::id()
    ));
    {
        let mut f = fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(&tmp)?;
        f.write_all(bytes)?;
        f.sync_all()?;
    }
    if let Err(e) = fs::rename(&tmp, path) {
        let _ = fs::remove_file(&tmp);
        return Err(e.into());
    }
    if let Ok(d) = fs::File::open(dir) {
        let _ = d.sync_all();
    }
    Ok(())
}

/// Read and decode; a corrupt file is moved aside and treated as empty so one
/// bad write cannot wedge the portal (fail toward "ask").
fn read_or_aside<T: for<'de> Deserialize<'de>>(path: &Path) -> Option<T> {
    let bytes = fs::read(path).ok()?;
    match postcard::from_bytes(&bytes) {
        Ok(v) => Some(v),
        Err(_) => {
            let mut aside = path.as_os_str().to_owned();
            aside.push(".corrupt");
            let _ = fs::rename(path, aside);
            None
        }
    }
}

#[derive(Debug, Clone)]
pub struct Store {
    root: PathBuf,
}

impl Store {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Store { root: root.into() }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    fn table_path(&self, table: &str) -> Result<PathBuf, StoreError> {
        if !name_ok(table) {
            return Err(StoreError::InvalidName);
        }
        Ok(self.root.join("tables").join(format!("{table}.bin")))
    }

    fn load(&self, table: &str) -> Result<Table, StoreError> {
        let p = self.table_path(table)?;
        Ok(read_or_aside::<TableFile>(&p)
            .filter(|f| f.version == FILE_VERSION)
            .map(|f| f.entries)
            .unwrap_or_default())
    }

    fn save(&self, table: &str, t: &Table) -> Result<(), StoreError> {
        let p = self.table_path(table)?;
        let bytes = postcard::to_stdvec(&TableFile {
            version: FILE_VERSION,
            entries: t.clone(),
        })?;
        write_atomic(&p, &bytes)
    }

    // ---- PermissionStore semantics ----

    /// `Lookup`: `None` when the table or id does not exist.
    pub fn lookup(&self, table: &str, id: &str) -> Result<Option<Entry>, StoreError> {
        Ok(self.load(table)?.get(id).cloned())
    }

    /// `Set`: replace the whole entry. `create == false` requires that the
    /// table already exists.
    pub fn set(
        &self,
        table: &str,
        create: bool,
        id: &str,
        permissions: BTreeMap<String, Vec<String>>,
        data: Data,
    ) -> Result<(), StoreError> {
        self.mutate(table, create, id, |e| {
            e.permissions = permissions;
            e.data = data;
        })
    }

    /// `SetValue`: replace only the data.
    pub fn set_value(
        &self,
        table: &str,
        create: bool,
        id: &str,
        data: Data,
    ) -> Result<(), StoreError> {
        self.mutate(table, create, id, |e| e.data = data)
    }

    /// `SetPermission`: set one app's permissions.
    pub fn set_permission(
        &self,
        table: &str,
        create: bool,
        id: &str,
        app: &str,
        perms: Vec<String>,
    ) -> Result<(), StoreError> {
        if !name_ok(app) {
            return Err(StoreError::InvalidName);
        }
        self.mutate(table, create, id, |e| {
            e.permissions.insert(app.to_owned(), perms);
        })
    }

    fn mutate(
        &self,
        table: &str,
        create: bool,
        id: &str,
        f: impl FnOnce(&mut Entry),
    ) -> Result<(), StoreError> {
        if !name_ok(id) {
            return Err(StoreError::InvalidName);
        }
        let exists = self.table_path(table)?.exists();
        if !exists && !create {
            return Err(StoreError::NotFound);
        }
        let mut t = self.load(table)?;
        f(t.entry(id.to_owned()).or_default());
        self.save(table, &t)
    }

    /// `Delete`: remove an entry. A table left empty is removed.
    pub fn delete(&self, table: &str, id: &str) -> Result<(), StoreError> {
        let mut t = self.load(table)?;
        if t.remove(id).is_none() {
            return Err(StoreError::NotFound);
        }
        self.save_or_remove(table, &t)
    }

    /// `DeletePermission`: drop one app's permissions from an entry.
    pub fn delete_permission(&self, table: &str, id: &str, app: &str) -> Result<(), StoreError> {
        let mut t = self.load(table)?;
        let e = t.get_mut(id).ok_or(StoreError::NotFound)?;
        if e.permissions.remove(app).is_none() {
            return Err(StoreError::NotFound);
        }
        self.save(table, &t)
    }

    /// `GetPermission`: empty when the app has none.
    pub fn get_permission(
        &self,
        table: &str,
        id: &str,
        app: &str,
    ) -> Result<Vec<String>, StoreError> {
        let t = self.load(table)?;
        let e = t.get(id).ok_or(StoreError::NotFound)?;
        Ok(e.permissions.get(app).cloned().unwrap_or_default())
    }

    /// `List`: ids in a table (empty if the table does not exist).
    pub fn list(&self, table: &str) -> Result<Vec<String>, StoreError> {
        Ok(self.load(table)?.into_keys().collect())
    }

    /// Names of all tables on disk.
    pub fn tables(&self) -> Vec<String> {
        let Ok(rd) = fs::read_dir(self.root.join("tables")) else {
            return Vec::new();
        };
        let mut v: Vec<String> = rd
            .filter_map(|e| e.ok())
            .filter_map(|e| {
                e.file_name()
                    .to_str()
                    .and_then(|n| n.strip_suffix(".bin"))
                    .map(str::to_owned)
            })
            .collect();
        v.sort();
        v
    }

    fn save_or_remove(&self, table: &str, t: &Table) -> Result<(), StoreError> {
        if t.is_empty() {
            let _ = fs::remove_file(self.table_path(table)?);
            Ok(())
        } else {
            self.save(table, t)
        }
    }

    // ---- grants ----

    fn grants_path(&self) -> PathBuf {
        self.root.join("grants.bin")
    }

    fn load_grants(&self) -> Vec<Grant> {
        read_or_aside::<GrantFile>(&self.grants_path())
            .filter(|f| f.version == FILE_VERSION)
            .map(|f| f.grants)
            .unwrap_or_default()
    }

    fn save_grants(&self, g: &[Grant]) -> Result<(), StoreError> {
        let bytes = postcard::to_stdvec(&GrantFile {
            version: FILE_VERSION,
            grants: g.to_vec(),
        })?;
        write_atomic(&self.grants_path(), &bytes)
    }

    /// Record (or replace) an answer.
    pub fn grant(&self, g: Grant) -> Result<(), StoreError> {
        if !name_ok(&g.app) || !name_ok(&g.table) {
            return Err(StoreError::InvalidName);
        }
        let mut all = self.load_grants();
        all.retain(|x| !x.key_eq(&g.app, &g.table, &g.resource));
        all.push(g);
        self.save_grants(&all)
    }

    /// What to do for `(app, table, resource)` at time `now`. A record for the
    /// whole table (empty resource) applies to every resource without its own.
    pub fn check(&self, app: &str, table: &str, resource: &str, now: u64) -> Decision {
        let all = self.load_grants();
        let find = |res: &str| {
            all.iter()
                .find(|g| g.key_eq(app, table, res) && g.live(now))
        };
        match find(resource).or_else(|| find("")).map(|g| g.standing) {
            Some(Standing::Allow) => Decision::Allow,
            Some(Standing::Deny) => Decision::Deny,
            Some(Standing::AskEveryTime) | None => Decision::Ask,
        }
    }

    /// All records, expired ones included (the user may want to see them).
    pub fn list_grants(&self) -> Vec<Grant> {
        let mut g = self.load_grants();
        g.sort_by(|a, b| (&a.app, &a.table, &a.resource).cmp(&(&b.app, &b.table, &b.resource)));
        g
    }

    /// Revoke an app's records, optionally only in one table. Returns how many
    /// were removed.
    pub fn revoke(&self, app: &str, table: Option<&str>) -> Result<usize, StoreError> {
        let mut all = self.load_grants();
        let before = all.len();
        all.retain(|g| !(g.app == app && table.is_none_or(|t| g.table == t)));
        let n = before - all.len();
        if n > 0 {
            self.save_grants(&all)?;
        }
        Ok(n)
    }

    /// Also drop the app's permission-store permissions on revoke.
    pub fn revoke_permissions(&self, app: &str, table: Option<&str>) -> Result<usize, StoreError> {
        let mut n = 0;
        let tables = match table {
            Some(t) => vec![t.to_owned()],
            None => self.tables(),
        };
        for t in tables {
            let mut tab = self.load(&t)?;
            let mut changed = false;
            for e in tab.values_mut() {
                if e.permissions.remove(app).is_some() {
                    n += 1;
                    changed = true;
                }
            }
            if changed {
                self.save(&t, &tab)?;
            }
        }
        Ok(n)
    }

    /// Drop expired records; returns how many.
    pub fn prune_expired(&self, now: u64) -> Result<usize, StoreError> {
        let mut all = self.load_grants();
        let before = all.len();
        all.retain(|g| g.live(now));
        let n = before - all.len();
        if n > 0 {
            self.save_grants(&all)?;
        }
        Ok(n)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn store() -> (tempfile::TempDir, Store) {
        let d = tempfile::tempdir().unwrap();
        let s = Store::new(d.path().join("ecl-portal"));
        (d, s)
    }

    fn perms(app: &str, p: &[&str]) -> BTreeMap<String, Vec<String>> {
        BTreeMap::from([(app.to_owned(), p.iter().map(|s| s.to_string()).collect())])
    }

    #[test]
    fn root_resolution() {
        assert_eq!(
            data_root(Some("/x"), Some("/h")),
            Some(PathBuf::from("/x/ecl-portal"))
        );
        assert_eq!(
            data_root(None, Some("/h")),
            Some(PathBuf::from("/h/.local/share/ecl-portal"))
        );
        assert_eq!(
            data_root(Some("rel"), Some("/h")),
            Some(PathBuf::from("/h/.local/share/ecl-portal"))
        );
        assert_eq!(data_root(None, None), None);
    }

    #[test]
    fn set_lookup_delete() {
        let (_d, s) = store();
        assert_eq!(s.lookup("t", "id").unwrap(), None);
        assert!(matches!(
            s.set("t", false, "id", perms("a", &["yes"]), Data::None),
            Err(StoreError::NotFound)
        ));
        s.set("t", true, "id", perms("a", &["yes"]), Data::Str("d".into()))
            .unwrap();
        let e = s.lookup("t", "id").unwrap().unwrap();
        assert_eq!(e.data, Data::Str("d".into()));
        assert_eq!(s.get_permission("t", "id", "a").unwrap(), vec!["yes"]);
        assert!(s.get_permission("t", "id", "b").unwrap().is_empty());
        s.set_permission("t", false, "id", "b", vec!["no".into()])
            .unwrap();
        s.set_value("t", false, "id", Data::U32(3)).unwrap();
        assert_eq!(s.list("t").unwrap(), vec!["id"]);
        s.delete_permission("t", "id", "a").unwrap();
        assert!(matches!(
            s.delete_permission("t", "id", "a"),
            Err(StoreError::NotFound)
        ));
        s.delete("t", "id").unwrap();
        assert!(s.list("t").unwrap().is_empty());
        assert!(s.tables().is_empty());
        assert!(matches!(s.delete("t", "id"), Err(StoreError::NotFound)));
    }

    #[test]
    fn names_are_validated() {
        let (_d, s) = store();
        for bad in ["", "../x", "a/b", ".hidden"] {
            assert!(matches!(
                s.set(bad, true, "id", BTreeMap::new(), Data::None),
                Err(StoreError::InvalidName)
            ));
        }
        assert!(matches!(
            s.set("t", true, "a/b", BTreeMap::new(), Data::None),
            Err(StoreError::InvalidName)
        ));
        assert!(matches!(
            s.set_permission("t", true, "id", "../app", vec![]),
            Err(StoreError::InvalidName)
        ));
    }

    #[test]
    fn grants_expiry_and_ask() {
        let (_d, s) = store();
        assert_eq!(s.check("app", "screencast", "", 100), Decision::Ask);
        let g = |res: &str, st, exp| Grant {
            app: "app".into(),
            table: "screencast".into(),
            resource: res.into(),
            standing: st,
            granted_at: 50,
            expires_at: exp,
        };
        s.grant(g("", Standing::Allow, Some(200))).unwrap();
        assert_eq!(s.check("app", "screencast", "x", 100), Decision::Allow);
        assert_eq!(s.check("app", "screencast", "x", 200), Decision::Ask);
        assert_eq!(s.check("other", "screencast", "x", 100), Decision::Ask);
        // A resource-specific deny beats the table-wide allow.
        s.grant(g("x", Standing::Deny, None)).unwrap();
        assert_eq!(s.check("app", "screencast", "x", 100), Decision::Deny);
        assert_eq!(s.check("app", "screencast", "y", 100), Decision::Allow);
        s.grant(g("x", Standing::AskEveryTime, None)).unwrap();
        assert_eq!(s.check("app", "screencast", "x", 100), Decision::Ask);
        assert_eq!(s.list_grants().len(), 2);
        assert_eq!(s.prune_expired(300).unwrap(), 1);
        assert_eq!(s.list_grants().len(), 1);
    }

    #[test]
    fn revoke_scopes() {
        let (_d, s) = store();
        for (t, a) in [("screencast", "a"), ("account", "a"), ("account", "b")] {
            s.grant(Grant {
                app: a.into(),
                table: t.into(),
                resource: String::new(),
                standing: Standing::Allow,
                granted_at: 0,
                expires_at: None,
            })
            .unwrap();
        }
        assert_eq!(s.revoke("a", Some("account")).unwrap(), 1);
        assert_eq!(s.revoke("a", Some("account")).unwrap(), 0);
        assert_eq!(s.revoke("a", None).unwrap(), 1);
        assert_eq!(s.list_grants().len(), 1);
        s.set_permission("devices", true, "camera", "b", vec!["yes".into()])
            .unwrap();
        assert_eq!(s.revoke_permissions("b", None).unwrap(), 1);
        assert!(s
            .get_permission("devices", "camera", "b")
            .unwrap()
            .is_empty());
    }

    #[test]
    fn corrupt_file_is_moved_aside_and_asks() {
        let (_d, s) = store();
        s.grant(Grant {
            app: "a".into(),
            table: "t".into(),
            resource: String::new(),
            standing: Standing::Allow,
            granted_at: 0,
            expires_at: None,
        })
        .unwrap();
        fs::write(s.grants_path(), b"\xff\xff garbage").unwrap();
        assert_eq!(s.check("a", "t", "", 1), Decision::Ask);
        assert!(s.root().join("grants.bin.corrupt").exists());
        assert!(s.list_grants().is_empty());
    }

    #[test]
    fn files_are_private_and_no_tmp_left() {
        use std::os::unix::fs::PermissionsExt;
        let (_d, s) = store();
        s.set("t", true, "id", BTreeMap::new(), Data::None).unwrap();
        let p = s.table_path("t").unwrap();
        assert_eq!(
            fs::metadata(&p).unwrap().permissions().mode() & 0o777,
            0o600
        );
        let leftovers = fs::read_dir(p.parent().unwrap())
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| e.file_name().to_string_lossy().ends_with(".tmp"))
            .count();
        assert_eq!(leftovers, 0);
    }
}
