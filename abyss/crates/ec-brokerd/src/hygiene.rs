// SPDX-License-Identifier: AGPL-3.0-only
//! Memory hygiene (S-08 §2): `PR_SET_DUMPABLE=0`, no core dumps, `mlock`ed
//! plaintext, zeroize on drop.
//!
//! This is the only module in the crate that uses `unsafe`: each block is a
//! single libc call whose contract is stated next to it.
//!
//! What this does and does not buy. With dumpable cleared the kernel refuses
//! `ptrace` attach and `/proc/<pid>/mem` to a same-uid process without
//! `CAP_SYS_PTRACE`, and writes no core file. `RLIMIT_CORE=0` is the second
//! lock on the core file, for a `core_pattern` that pipes. Neither stops root.
//! The sandbox is not root and has no `CAP_SYS_PTRACE` (S-03), which is the
//! threat this addresses.
#![allow(unsafe_code)]

use std::fmt;
use std::io;
use zeroize::Zeroize;

fn last() -> io::Error {
    io::Error::last_os_error()
}

fn page_size() -> usize {
    // SAFETY: sysconf has no preconditions.
    let n = unsafe { libc::sysconf(libc::_SC_PAGESIZE) };
    if n > 0 {
        n as usize
    } else {
        4096
    }
}

/// Clears dumpable, zeroes `RLIMIT_CORE` and sets `no_new_privs`, then reads
/// the settings back. A daemon that cannot make these true must not hold
/// secrets, so the caller treats an error as fatal.
pub fn harden() -> io::Result<()> {
    // SAFETY: PR_SET_DUMPABLE takes one integer argument; the rest are unused.
    if unsafe { libc::prctl(libc::PR_SET_DUMPABLE, 0 as libc::c_ulong, 0, 0, 0) } != 0 {
        return Err(last());
    }
    let lim = libc::rlimit {
        rlim_cur: 0,
        rlim_max: 0,
    };
    // SAFETY: `lim` is a valid rlimit that outlives the call.
    if unsafe { libc::setrlimit(libc::RLIMIT_CORE, &lim) } != 0 {
        return Err(last());
    }
    // SAFETY: PR_SET_NO_NEW_PRIVS takes 1 and three unused arguments.
    if unsafe { libc::prctl(libc::PR_SET_NO_NEW_PRIVS, 1 as libc::c_ulong, 0, 0, 0) } != 0 {
        return Err(last());
    }
    verify()
}

/// `PR_GET_DUMPABLE`: 0 not dumpable, 1 dumpable, 2 root-only.
pub fn dumpable() -> io::Result<i32> {
    // SAFETY: PR_GET_DUMPABLE takes no arguments and returns the value.
    let r = unsafe { libc::prctl(libc::PR_GET_DUMPABLE, 0 as libc::c_ulong, 0, 0, 0) };
    if r < 0 {
        Err(last())
    } else {
        Ok(r)
    }
}

/// `(soft, hard)` `RLIMIT_CORE`.
pub fn core_limit() -> io::Result<(u64, u64)> {
    let mut lim = libc::rlimit {
        rlim_cur: 0,
        rlim_max: 0,
    };
    // SAFETY: `lim` is a valid out-pointer for the call.
    if unsafe { libc::getrlimit(libc::RLIMIT_CORE, &mut lim) } != 0 {
        return Err(last());
    }
    Ok((lim.rlim_cur, lim.rlim_max))
}

/// Whether `harden` has taken effect in this process.
pub fn verify() -> io::Result<()> {
    if dumpable()? != 0 {
        return Err(io::Error::other("process is still dumpable"));
    }
    if core_limit()? != (0, 0) {
        return Err(io::Error::other("RLIMIT_CORE is not zero"));
    }
    Ok(())
}

/// Kibibytes this process has `mlock`ed (`VmLck` in `/proc/self/status`).
pub fn vm_locked_kib() -> Option<u64> {
    let s = std::fs::read_to_string("/proc/self/status").ok()?;
    let l = s.lines().find(|l| l.starts_with("VmLck:"))?;
    l.split_whitespace().nth(1)?.parse().ok()
}

/// A fixed-size plaintext buffer that is never swapped, never dumped, never
/// inherited by a fork, and zeroed before it is returned to the kernel.
///
/// It owns whole pages from an anonymous mapping rather than a slice of the
/// heap: `munlock` on drop would otherwise unlock a page that a neighbouring
/// allocation also asked to have locked. There is no `Clone` and no `Deref`
/// to a growable type, so a second, unlocked copy cannot be made by accident.
///
/// Creation fails if the pages cannot be locked: a secret that might reach
/// swap is not stored. (`RLIMIT_MEMLOCK` bounds the total; the systemd unit
/// raises it.)
pub struct LockedBuf {
    ptr: *mut u8,
    map_len: usize,
    len: usize,
}

// SAFETY: the mapping is private to this value and nothing aliases it.
unsafe impl Send for LockedBuf {}

impl LockedBuf {
    /// A zeroed buffer of `len` bytes.
    pub fn new(len: usize) -> io::Result<Self> {
        let page = page_size();
        let map_len = len.max(1).div_ceil(page) * page;
        // SAFETY: an anonymous private mapping with no address hint; the
        // result is checked against MAP_FAILED before use.
        let p = unsafe {
            libc::mmap(
                std::ptr::null_mut(),
                map_len,
                libc::PROT_READ | libc::PROT_WRITE,
                libc::MAP_PRIVATE | libc::MAP_ANONYMOUS,
                -1,
                0,
            )
        };
        if p == libc::MAP_FAILED {
            return Err(last());
        }
        let undo = |e: io::Error| {
            // SAFETY: `p`/`map_len` are exactly the mapping made above.
            unsafe { libc::munmap(p, map_len) };
            e
        };
        // SAFETY: the range is the mapping made above.
        if unsafe { libc::mlock(p, map_len) } != 0 {
            return Err(undo(last()));
        }
        // SAFETY: as above. DONTDUMP keeps the pages out of any core file;
        // DONTFORK keeps them out of a child.
        if unsafe { libc::madvise(p, map_len, libc::MADV_DONTDUMP) } != 0
            || unsafe { libc::madvise(p, map_len, libc::MADV_DONTFORK) } != 0
        {
            let e = last();
            // SAFETY: as above.
            unsafe { libc::munlock(p, map_len) };
            return Err(undo(e));
        }
        Ok(LockedBuf {
            ptr: p.cast(),
            map_len,
            len,
        })
    }

    /// A locked copy of `src`.
    pub fn from_slice(src: &[u8]) -> io::Result<Self> {
        let mut b = Self::new(src.len())?;
        b.as_mut_slice().copy_from_slice(src);
        Ok(b)
    }

    pub fn len(&self) -> usize {
        self.len
    }

    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    pub fn as_slice(&self) -> &[u8] {
        // SAFETY: `ptr` is a live mapping of at least `len` bytes.
        unsafe { std::slice::from_raw_parts(self.ptr, self.len) }
    }

    pub fn as_mut_slice(&mut self) -> &mut [u8] {
        // SAFETY: as above, and `&mut self` makes the borrow unique.
        unsafe { std::slice::from_raw_parts_mut(self.ptr, self.len) }
    }

    /// The whole mapping, including the page tail past `len`. Only for tests
    /// that check what `Drop` will wipe.
    #[cfg(test)]
    fn whole_mut(&mut self) -> &mut [u8] {
        // SAFETY: the mapping is live and `map_len` covers it.
        unsafe { std::slice::from_raw_parts_mut(self.ptr, self.map_len) }
    }
}

impl Drop for LockedBuf {
    fn drop(&mut self) {
        // SAFETY: the whole mapping is ours; zero it, unlock it, return it.
        unsafe {
            std::slice::from_raw_parts_mut(self.ptr, self.map_len).zeroize();
            libc::munlock(self.ptr.cast(), self.map_len);
            libc::munmap(self.ptr.cast(), self.map_len);
        }
    }
}

impl fmt::Debug for LockedBuf {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "LockedBuf(<{} bytes>)", self.len)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn harden_clears_dumpable_and_core_limit() {
        harden().unwrap();
        assert_eq!(dumpable().unwrap(), 0);
        assert_eq!(core_limit().unwrap(), (0, 0));
        verify().unwrap();
    }

    #[test]
    fn a_locked_buffer_is_counted_in_vmlck() {
        // VmLck is per process and tests run in parallel threads, so only the
        // direction is asserted: while this buffer lives, at least one page
        // is locked.
        let b = LockedBuf::from_slice(b"hunter2").unwrap();
        assert_eq!(b.as_slice(), b"hunter2");
        let during = vm_locked_kib().unwrap();
        assert!(
            during >= (page_size() / 1024) as u64,
            "mlock did not take: {during} KiB locked"
        );
    }

    #[test]
    fn the_wipe_drop_performs_zeroes_the_whole_page() {
        let mut b = LockedBuf::from_slice(&[0xAA; 64]).unwrap();
        b.whole_mut().zeroize();
        assert!(b.as_slice().iter().all(|&x| x == 0));
    }

    #[test]
    fn debug_never_prints_contents() {
        let b = LockedBuf::from_slice(b"s3cret").unwrap();
        assert!(!format!("{b:?}").contains("s3cret"));
    }
}
