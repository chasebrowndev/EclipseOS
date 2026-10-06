// SPDX-License-Identifier: AGPL-3.0-only
//! The on-disk store (S-08 §2): one AEAD file per secret under a directory,
//! keys wrapped by the sealed master key.
//!
//! ```text
//! <dir>/master.sealed     the sealer's blob around the 32-byte master key
//! <dir>/<ulid>.sec        canonical CBOR { v, id, rc, dek, meta, value }
//!   dek  = AEAD(master, per-secret key)
//!   meta = AEAD(dek, Meta CBOR)
//!   val  = AEAD(dek, value)
//! ```
//!
//! Every ciphertext binds `(label, id, rotation_counter)` as associated data,
//! so a value cannot be moved to another record and a metadata blob cannot be
//! passed off as a key. A fresh per-secret key on every write means rotation
//! re-encrypts under new key material, not just a new nonce.
//!
//! Metadata is decrypted at unlock and kept in memory (it holds no secret
//! value); a value is decrypted only by [`Store::read_value`], into a
//! [`LockedBuf`], and only after the caller has decided to release it.

use crate::aead::{self, CryptoError};
use crate::hygiene::LockedBuf;
use crate::record::{Meta, NewSecret, RecordError};
use crate::sealer::{SealError, Sealer};
use ec_policy_eval::cbor::{enc, MapBuilder, Reader};
use ec_policy_eval::Ulid;
use std::collections::BTreeMap;
use std::fs;
use std::io::Write;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};

pub const MASTER_FILE: &str = "master.sealed";
/// Large enough for an RSA-4096 OpenSSH key; small enough that a record is
/// one page of locked memory.
pub const MAX_VALUE: usize = 16 * 1024;
const FILE_VERSION: u64 = 1;

#[derive(Debug)]
pub enum StoreError {
    Io(std::io::Error),
    Seal(SealError),
    Crypto(CryptoError),
    Record(RecordError),
    Exists,
    NotFound,
    TooLarge,
    /// A file that does not parse, or whose ids disagree with its name.
    Corrupt,
}

impl std::fmt::Display for StoreError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            StoreError::Io(e) => write!(f, "store i/o: {e}"),
            StoreError::Seal(e) => write!(f, "{e}"),
            StoreError::Crypto(e) => write!(f, "{e}"),
            StoreError::Record(e) => write!(f, "invalid record: {e:?}"),
            StoreError::Exists => f.write_str("a secret by that name exists"),
            StoreError::NotFound => f.write_str("no such secret"),
            StoreError::TooLarge => f.write_str("value too large"),
            StoreError::Corrupt => f.write_str("store file is corrupt"),
        }
    }
}

impl std::error::Error for StoreError {}

impl From<std::io::Error> for StoreError {
    fn from(e: std::io::Error) -> Self {
        StoreError::Io(e)
    }
}
impl From<SealError> for StoreError {
    fn from(e: SealError) -> Self {
        StoreError::Seal(e)
    }
}
impl From<CryptoError> for StoreError {
    fn from(e: CryptoError) -> Self {
        StoreError::Crypto(e)
    }
}
impl From<RecordError> for StoreError {
    fn from(e: RecordError) -> Self {
        StoreError::Record(e)
    }
}

fn aad(label: &str, id: &Ulid, rc: u64) -> Vec<u8> {
    let mut a = b"eclipse/brokerd/".to_vec();
    a.extend_from_slice(label.as_bytes());
    a.extend_from_slice(b"/v1");
    a.extend_from_slice(&id.0);
    a.extend_from_slice(&rc.to_be_bytes());
    a
}

/// Writes `bytes` to `path` durably and atomically, mode 0600.
fn write_atomic(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let tmp = path.with_extension("tmp");
    let _ = fs::remove_file(&tmp);
    let mut f = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&tmp)?;
    f.write_all(bytes)?;
    f.sync_all()?;
    fs::rename(&tmp, path)?;
    if let Some(dir) = path.parent() {
        fs::File::open(dir)?.sync_all()?;
    }
    Ok(())
}

struct Entry {
    meta: Meta,
    path: PathBuf,
}

/// An open (unlocked) store. Dropping it zeroes the master key.
pub struct Store {
    dir: PathBuf,
    master: LockedBuf,
    index: BTreeMap<String, Entry>,
}

impl Store {
    /// Whether `dir` has been initialised.
    pub fn exists(dir: &Path) -> bool {
        dir.join(MASTER_FILE).is_file()
    }

    /// Creates the directory (0700) and a fresh sealed master key. Refuses
    /// to overwrite an existing one.
    pub fn init(dir: &Path, sealer: &dyn Sealer, pass: Option<&[u8]>) -> Result<(), StoreError> {
        fs::create_dir_all(dir)?;
        fs::set_permissions(dir, fs::Permissions::from_mode(0o700))?;
        if Self::exists(dir) {
            return Err(StoreError::Exists);
        }
        let mut master = LockedBuf::new(aead::KEY).map_err(|_| StoreError::Crypto(CryptoError::Resource))?;
        let fresh = zeroize::Zeroizing::new(aead::random::<32>()?);
        master.as_mut_slice().copy_from_slice(&fresh[..]);
        let blob = sealer.seal(master.as_slice(), pass)?;
        write_atomic(&dir.join(MASTER_FILE), &blob)?;
        Ok(())
    }

    /// Unseals the master key and decrypts every record's metadata. A record
    /// that does not authenticate fails the whole open: a store with a
    /// tampered file is not half-trusted.
    pub fn open(dir: &Path, sealer: &dyn Sealer, pass: Option<&[u8]>) -> Result<Store, StoreError> {
        let blob = fs::read(dir.join(MASTER_FILE))?;
        let master = sealer.unseal(&blob, pass)?;
        if master.len() != aead::KEY {
            return Err(StoreError::Corrupt);
        }
        let mut index = BTreeMap::new();
        for ent in fs::read_dir(dir)? {
            let path = ent?.path();
            if path.extension().and_then(|e| e.to_str()) != Some("sec") {
                continue;
            }
            let f = File::decode(&fs::read(&path)?)?;
            let meta = f.open_meta(&master)?;
            if path.file_stem().and_then(|s| s.to_str()) != Some(&meta.id.to_text()) {
                return Err(StoreError::Corrupt);
            }
            if index.insert(meta.name.clone(), Entry { meta, path }).is_some() {
                return Err(StoreError::Corrupt);
            }
        }
        Ok(Store {
            dir: dir.to_owned(),
            master,
            index,
        })
    }

    pub fn meta(&self, name: &str) -> Option<&Meta> {
        self.index.get(name).map(|e| &e.meta)
    }

    pub fn list(&self) -> impl Iterator<Item = &Meta> {
        self.index.values().map(|e| &e.meta)
    }

    fn write_record(&self, meta: &Meta, value: &[u8]) -> Result<PathBuf, StoreError> {
        let dek: [u8; 32] = aead::random()?;
        let (id, rc) = (&meta.id, meta.rotation_counter);
        let file = File {
            id: *id,
            rc,
            dek: aead::seal(self.master.as_slice(), &aad("dek", id, rc), &dek)?,
            meta: aead::seal(&dek, &aad("meta", id, rc), &meta.encode())?,
            val: aead::seal(&dek, &aad("val", id, rc), value)?,
        };
        let path = self.dir.join(format!("{}.sec", id.to_text()));
        write_atomic(&path, &file.encode())?;
        Ok(path)
    }

    /// Adds a secret. `now_unix` and the id entropy come from the caller so
    /// the store has no clock of its own.
    pub fn add(&mut self, new: &NewSecret, value: &[u8], now_unix: u64) -> Result<Meta, StoreError> {
        new.validate()?;
        if value.len() > MAX_VALUE {
            return Err(StoreError::TooLarge);
        }
        if self.index.contains_key(&new.name) {
            return Err(StoreError::Exists);
        }
        let meta = Meta {
            name: new.name.clone(),
            id: Ulid::from_parts(now_unix.saturating_mul(1000), aead::random::<10>()?),
            kind: new.kind,
            bound_to: new.bound_to.clone(),
            modes: new.modes.clone(),
            requires_prompt: new.requires_prompt,
            rotation_hint_days: new.rotation_hint_days,
            created_unix: now_unix,
            rotation_counter: 0,
        };
        let path = self.write_record(&meta, value)?;
        self.index.insert(
            meta.name.clone(),
            Entry {
                meta: meta.clone(),
                path,
            },
        );
        Ok(meta)
    }

    /// Replaces the value and increments `rotation_counter` (S-08 §6).
    pub fn rotate(&mut self, name: &str, value: &[u8]) -> Result<Meta, StoreError> {
        if value.len() > MAX_VALUE {
            return Err(StoreError::TooLarge);
        }
        let mut meta = self.index.get(name).ok_or(StoreError::NotFound)?.meta.clone();
        meta.rotation_counter += 1;
        // The new file has the same name (ids are stable); rename replaces
        // the old one atomically, so there is no moment with neither.
        let path = self.write_record(&meta, value)?;
        self.index.insert(
            name.to_owned(),
            Entry {
                meta: meta.clone(),
                path,
            },
        );
        Ok(meta)
    }

    /// Removes a secret: overwritten with zeros, synced, unlinked.
    pub fn revoke(&mut self, name: &str) -> Result<(), StoreError> {
        let e = self.index.remove(name).ok_or(StoreError::NotFound)?;
        if let Ok(len) = fs::metadata(&e.path).map(|m| m.len()) {
            if let Ok(mut f) = fs::OpenOptions::new().write(true).open(&e.path) {
                let _ = f.write_all(&vec![0u8; len as usize]);
                let _ = f.sync_all();
            }
        }
        fs::remove_file(&e.path)?;
        fs::File::open(&self.dir)?.sync_all()?;
        Ok(())
    }

    /// Decrypts the value. Call only after every precondition has passed.
    pub fn read_value(&self, name: &str) -> Result<LockedBuf, StoreError> {
        let e = self.index.get(name).ok_or(StoreError::NotFound)?;
        let f = File::decode(&fs::read(&e.path)?)?;
        if f.id != e.meta.id || f.rc != e.meta.rotation_counter {
            return Err(StoreError::Corrupt);
        }
        let dek = aead::open(self.master.as_slice(), &aad("dek", &f.id, f.rc), &f.dek)?;
        Ok(aead::open(dek.as_slice(), &aad("val", &f.id, f.rc), &f.val)?)
    }
}

struct File {
    id: Ulid,
    rc: u64,
    dek: Vec<u8>,
    meta: Vec<u8>,
    val: Vec<u8>,
}

impl File {
    fn encode(&self) -> Vec<u8> {
        let mut m = MapBuilder::new();
        m.insert("dek", enc(|w| w.bytes(&self.dek)));
        m.insert("id", enc(|w| w.bytes(&self.id.0)));
        m.insert("meta", enc(|w| w.bytes(&self.meta)));
        m.insert("rc", enc(|w| w.u64(self.rc)));
        m.insert("v", enc(|w| w.u64(FILE_VERSION)));
        m.insert("value", enc(|w| w.bytes(&self.val)));
        m.finish()
    }

    fn decode(buf: &[u8]) -> Result<File, StoreError> {
        let c = |_| StoreError::Corrupt;
        let mut r = Reader::new(buf);
        let n = r.map_begin().map_err(c)?;
        let (mut id, mut rc, mut dek, mut meta, mut val, mut v) = (None, None, None, None, None, None);
        for _ in 0..n {
            match r.key().map_err(c)? {
                "dek" => dek = Some(r.bytes().map_err(c)?.to_vec()),
                "id" => id = Some(Ulid(r.byte_array::<16>().map_err(c)?)),
                "meta" => meta = Some(r.bytes().map_err(c)?.to_vec()),
                "rc" => rc = Some(r.u64().map_err(c)?),
                "v" => v = Some(r.u64().map_err(c)?),
                "value" => val = Some(r.bytes().map_err(c)?.to_vec()),
                _ => return Err(StoreError::Corrupt),
            }
        }
        r.map_end().map_err(c)?;
        r.finish().map_err(c)?;
        if v != Some(FILE_VERSION) {
            return Err(StoreError::Corrupt);
        }
        Ok(File {
            id: id.ok_or(StoreError::Corrupt)?,
            rc: rc.ok_or(StoreError::Corrupt)?,
            dek: dek.ok_or(StoreError::Corrupt)?,
            meta: meta.ok_or(StoreError::Corrupt)?,
            val: val.ok_or(StoreError::Corrupt)?,
        })
    }

    fn open_meta(&self, master: &LockedBuf) -> Result<Meta, StoreError> {
        let dek = aead::open(master.as_slice(), &aad("dek", &self.id, self.rc), &self.dek)?;
        let plain = aead::open(dek.as_slice(), &aad("meta", &self.id, self.rc), &self.meta)?;
        let meta = Meta::decode(plain.as_slice())?;
        if meta.id != self.id || meta.rotation_counter != self.rc {
            return Err(StoreError::Corrupt);
        }
        Ok(meta)
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::bind::Binding;
    use crate::record::{Kind, Mode};
    use crate::sealer::SoftSealer;

    pub fn tmpdir(tag: &str) -> PathBuf {
        let mut b = [0u8; 8];
        getrandom::fill(&mut b).unwrap();
        let d = std::env::temp_dir().join(format!("ec-brokerd-{tag}-{:x}", u64::from_be_bytes(b)));
        let _ = fs::remove_dir_all(&d);
        d
    }

    pub fn new_secret(name: &str) -> NewSecret {
        NewSecret {
            name: name.into(),
            kind: Kind::Bearer,
            bound_to: vec![Binding::parse("host:api.acme.com").unwrap()],
            modes: vec![Mode::ProxyHeader],
            requires_prompt: false,
            rotation_hint_days: None,
        }
    }

    fn sealer() -> SoftSealer {
        SoftSealer::new([4; 32], b"p")
    }

    #[test]
    fn add_read_rotate_revoke() {
        let d = tmpdir("store");
        Store::init(&d, &sealer(), None).unwrap();
        let mut s = Store::open(&d, &sealer(), None).unwrap();
        let m = s.add(&new_secret("tok"), b"v0-secret", 1000).unwrap();
        assert_eq!(m.rotation_counter, 0);
        assert_eq!(s.read_value("tok").unwrap().as_slice(), b"v0-secret");
        let m = s.rotate("tok", b"v1-secret").unwrap();
        assert_eq!(m.rotation_counter, 1);
        assert_eq!(s.read_value("tok").unwrap().as_slice(), b"v1-secret");
        // Survives a reopen.
        drop(s);
        let mut s = Store::open(&d, &sealer(), None).unwrap();
        assert_eq!(s.meta("tok").unwrap().rotation_counter, 1);
        assert_eq!(s.read_value("tok").unwrap().as_slice(), b"v1-secret");
        s.revoke("tok").unwrap();
        assert!(s.read_value("tok").is_err());
        assert_eq!(fs::read_dir(&d).unwrap().count(), 1, "only master.sealed remains");
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn nothing_on_disk_contains_the_value_or_the_name() {
        let d = tmpdir("plain");
        Store::init(&d, &sealer(), None).unwrap();
        let mut s = Store::open(&d, &sealer(), None).unwrap();
        s.add(&new_secret("very-distinct-name"), b"very-distinct-value", 1)
            .unwrap();
        for e in fs::read_dir(&d).unwrap() {
            let b = fs::read(e.unwrap().path()).unwrap();
            assert!(!b.windows(19).any(|w| w == b"very-distinct-value"));
            assert!(!b.windows(18).any(|w| w == b"very-distinct-name"));
        }
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn files_are_private() {
        let d = tmpdir("mode");
        Store::init(&d, &sealer(), None).unwrap();
        let mut s = Store::open(&d, &sealer(), None).unwrap();
        s.add(&new_secret("t"), b"v", 1).unwrap();
        assert_eq!(fs::metadata(&d).unwrap().permissions().mode() & 0o777, 0o700);
        for e in fs::read_dir(&d).unwrap() {
            assert_eq!(
                fs::metadata(e.unwrap().path()).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn a_wrong_sealer_cannot_open_and_a_tampered_file_fails_the_open() {
        let d = tmpdir("tamper");
        Store::init(&d, &sealer(), None).unwrap();
        let mut s = Store::open(&d, &sealer(), None).unwrap();
        s.add(&new_secret("t"), b"v", 1).unwrap();
        drop(s);
        assert!(Store::open(&d, &SoftSealer::new([4; 32], b"other"), None).is_err());
        let sec = fs::read_dir(&d)
            .unwrap()
            .map(|e| e.unwrap().path())
            .find(|p| p.extension().is_some_and(|e| e == "sec"))
            .unwrap();
        let mut b = fs::read(&sec).unwrap();
        let n = b.len();
        b[n - 3] ^= 0xff;
        fs::write(&sec, &b).unwrap();
        // The value blob is last; metadata still opens, the value does not.
        let s = Store::open(&d, &sealer(), None).unwrap();
        assert!(s.read_value("t").is_err());
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn duplicate_names_and_oversize_values_are_refused() {
        let d = tmpdir("dup");
        Store::init(&d, &sealer(), None).unwrap();
        let mut s = Store::open(&d, &sealer(), None).unwrap();
        s.add(&new_secret("t"), b"v", 1).unwrap();
        assert!(matches!(
            s.add(&new_secret("t"), b"v", 1),
            Err(StoreError::Exists)
        ));
        assert!(matches!(
            s.add(&new_secret("u"), &vec![0; MAX_VALUE + 1], 1),
            Err(StoreError::TooLarge)
        ));
        assert!(matches!(
            Store::init(&d, &sealer(), None),
            Err(StoreError::Exists)
        ));
        let _ = fs::remove_dir_all(&d);
    }
}
