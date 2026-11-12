// SPDX-License-Identifier: AGPL-3.0-only
//! Who to authenticate: the user running this process, by uid.
//!
//! Not `$USER`, not argv. Resolved once, before the lock is taken: a locker
//! that cannot name its user could never unlock, and locking with no way back
//! would strand the human (ADR 0024, ADR 0095).

use std::ffi::{CStr, CString};

#[derive(Debug, PartialEq, Eq)]
pub enum Refusal {
    /// Real and effective uid differ: something set-uid is running us.
    MixedIds,
    /// The uid has no passwd entry.
    Unknown,
}

/// The login name of the real uid, which must equal the effective uid.
pub fn current() -> Result<CString, Refusal> {
    // SAFETY: getuid and geteuid only read process credentials.
    let (uid, euid) = unsafe { (libc::getuid(), libc::geteuid()) };
    if uid != euid {
        return Err(Refusal::MixedIds);
    }
    name_of(uid)
}

fn name_of(uid: libc::uid_t) -> Result<CString, Refusal> {
    let mut buf = vec![0 as libc::c_char; 4096];
    loop {
        // SAFETY: `passwd` is plain C data (integers and raw pointers), for
        // which all-zero is a valid value.
        let mut pw: libc::passwd = unsafe { std::mem::zeroed() };
        let mut result: *mut libc::passwd = std::ptr::null_mut();
        // SAFETY: `pw`, `buf` (with its true length) and `result` are live and
        // owned; getpwuid_r writes only into them and sets `result` to `&pw`
        // or to null.
        let rc = unsafe { libc::getpwuid_r(uid, &mut pw, buf.as_mut_ptr(), buf.len(), &mut result) };
        if rc == libc::ERANGE && buf.len() < 1 << 20 {
            buf.resize(buf.len() * 2, 0);
            continue;
        }
        if rc != 0 || result.is_null() || pw.pw_name.is_null() {
            return Err(Refusal::Unknown);
        }
        // SAFETY: getpwuid_r succeeded, so `pw_name` is a NUL-terminated
        // string inside `buf`, which is still alive.
        let name = unsafe { CStr::from_ptr(pw.pw_name) };
        if name.to_bytes().is_empty() {
            return Err(Refusal::Unknown);
        }
        return Ok(name.to_owned());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolves_the_test_runner() {
        let name = current().expect("the test runner has a passwd entry");
        assert!(!name.to_bytes().is_empty());
    }

    #[test]
    fn an_unknown_uid_is_refused() {
        // Far outside any allocated range; nss returns no entry.
        assert_eq!(name_of(0x7fff_fff0), Err(Refusal::Unknown));
    }
}
