// SPDX-License-Identifier: Apache-2.0
//! The C ABI, declared by hand in `include/ec_cataclysm_pub.h`. Symbols are
//! `ecpub_*`. Ownership and lifetime rules are stated in the header and are
//! the contract; this file implements them.
//!
//! A panic cannot cross this boundary: `extern "C"` functions abort on
//! unwind, and nothing here is expected to panic on any input.

use std::ffi::c_char;
use std::ptr;

use crate::publisher::{Config, Publisher};

/// Bumped on any incompatible change to the header.
pub const ECPUB_ABI_VERSION: u32 = 1;

pub const ECPUB_OK: i32 = 0;
pub const ECPUB_ERR_NULL: i32 = -1;
pub const ECPUB_ERR_RANGE: i32 = -2;

#[repr(C)]
#[derive(Clone, Copy)]
pub struct EcpubConfig {
    pub min_interval_ns: u64,
    pub boundary_interval_ns: u64,
    pub idle_ns: u64,
    pub downgrade_grace_ns: u64,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct EcpubOp {
    pub kind: u32,
    pub node: u32,
    pub parent: u32,
    pub index: u32,
    pub role: u32,
    pub cursor: i32,
    pub sel_start: i32,
    pub sel_end: i32,
    pub key: *const c_char,
    pub key_len: usize,
    pub str_: *const c_char,
    pub str_len: usize,
}

#[repr(C)]
pub struct EcpubBatch {
    pub ops: *const EcpubOp,
    pub len: usize,
    pub seq: u64,
    pub flags: u32,
}

#[repr(C)]
pub struct EcpubStats {
    pub retained_bytes: u64,
    pub publishes: u64,
    pub structural_publishes: u64,
    pub blocks: u64,
}

/// Opaque to C.
pub struct EcpubPublisher {
    inner: Publisher,
    ops: Vec<EcpubOp>,
}

fn code(r: Result<(), crate::Error>) -> i32 {
    match r {
        Ok(()) => ECPUB_OK,
        Err(crate::Error::Range) => ECPUB_ERR_RANGE,
    }
}

/// # Safety
/// `ptr` must be valid for `len` bytes, or `len` must be 0.
unsafe fn bytes<'a>(ptr: *const c_char, len: usize) -> Option<&'a [u8]> {
    if len == 0 {
        Some(&[])
    } else if ptr.is_null() {
        None
    } else {
        // SAFETY: non-null and valid for `len` bytes per the caller contract.
        Some(unsafe { std::slice::from_raw_parts(ptr.cast::<u8>(), len) })
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn ecpub_abi_version() -> u32 {
    ECPUB_ABI_VERSION
}

#[unsafe(no_mangle)]
pub extern "C" fn ecpub_op_size() -> usize {
    size_of::<EcpubOp>()
}

/// # Safety
/// `out` must be NULL or point to writable `ecpub_config`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ecpub_config_default(out: *mut EcpubConfig) {
    if out.is_null() {
        return;
    }
    let d = Config::default();
    // SAFETY: non-null and writable per the caller contract.
    unsafe {
        out.write(EcpubConfig {
            min_interval_ns: d.min_interval_ns,
            boundary_interval_ns: d.boundary_interval_ns,
            idle_ns: d.idle_ns,
            downgrade_grace_ns: d.downgrade_grace_ns,
        })
    };
}

/// # Safety
/// `cfg` must be NULL or point to a readable `ecpub_config`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ecpub_new(cfg: *const EcpubConfig, rows: u16, cols: u16) -> *mut EcpubPublisher {
    let cfg = if cfg.is_null() {
        Config::default()
    } else {
        // SAFETY: non-null and readable per the caller contract.
        let c = unsafe { cfg.read() };
        Config {
            min_interval_ns: c.min_interval_ns,
            boundary_interval_ns: c.boundary_interval_ns,
            idle_ns: c.idle_ns,
            downgrade_grace_ns: c.downgrade_grace_ns,
        }
    };
    match Publisher::new(cfg, rows, cols) {
        Ok(inner) => Box::into_raw(Box::new(EcpubPublisher {
            inner,
            ops: Vec::new(),
        })),
        Err(_) => ptr::null_mut(),
    }
}

/// # Safety
/// `p` must be NULL or a pointer from `ecpub_new` not yet freed.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ecpub_free(p: *mut EcpubPublisher) {
    if !p.is_null() {
        // SAFETY: from `Box::into_raw` in `ecpub_new`, freed once.
        drop(unsafe { Box::from_raw(p) });
    }
}

/// # Safety
/// `p` must be NULL or a live publisher.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ecpub_resize(p: *mut EcpubPublisher, rows: u16, cols: u16) -> i32 {
    // SAFETY: NULL or live per the caller contract.
    let Some(p) = (unsafe { p.as_mut() }) else {
        return ECPUB_ERR_NULL;
    };
    code(p.inner.resize(rows, cols))
}

/// # Safety
/// `p` must be NULL or a live publisher; `utf8` valid for `len` bytes.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ecpub_set_line(
    p: *mut EcpubPublisher,
    row: u16,
    utf8: *const c_char,
    len: usize,
) -> i32 {
    // SAFETY: NULL or live per the caller contract.
    let Some(p) = (unsafe { p.as_mut() }) else {
        return ECPUB_ERR_NULL;
    };
    // SAFETY: valid for `len` per the caller contract.
    let Some(b) = (unsafe { bytes(utf8, len) }) else {
        return ECPUB_ERR_NULL;
    };
    code(p.inner.set_line(row, b))
}

/// # Safety
/// `p` must be NULL or a live publisher.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ecpub_scroll(p: *mut EcpubPublisher, n: u16) -> i32 {
    // SAFETY: NULL or live per the caller contract.
    let Some(p) = (unsafe { p.as_mut() }) else {
        return ECPUB_ERR_NULL;
    };
    p.inner.scroll(n);
    ECPUB_OK
}

/// # Safety
/// `p` must be NULL or a live publisher.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ecpub_set_cursor(p: *mut EcpubPublisher, row: u16, col: u16) -> i32 {
    // SAFETY: NULL or live per the caller contract.
    let Some(p) = (unsafe { p.as_mut() }) else {
        return ECPUB_ERR_NULL;
    };
    code(p.inner.set_cursor(row, col))
}

/// # Safety
/// `p` must be NULL or a live publisher.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ecpub_set_alt_screen(p: *mut EcpubPublisher, on: bool) -> i32 {
    // SAFETY: NULL or live per the caller contract.
    let Some(p) = (unsafe { p.as_mut() }) else {
        return ECPUB_ERR_NULL;
    };
    p.inner.set_alt_screen(on);
    ECPUB_OK
}

/// # Safety
/// `p` must be NULL or a live publisher.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ecpub_set_echo(p: *mut EcpubPublisher, on: bool, now_ns: u64) -> i32 {
    // SAFETY: NULL or live per the caller contract.
    let Some(p) = (unsafe { p.as_mut() }) else {
        return ECPUB_ERR_NULL;
    };
    p.inner.set_echo(on, now_ns);
    ECPUB_OK
}

/// # Safety
/// `p` must be NULL or a live publisher; `payload` valid for `len` bytes.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ecpub_osc133(
    p: *mut EcpubPublisher,
    payload: *const c_char,
    len: usize,
    now_ns: u64,
) -> i32 {
    // SAFETY: NULL or live per the caller contract.
    let Some(p) = (unsafe { p.as_mut() }) else {
        return ECPUB_ERR_NULL;
    };
    // SAFETY: valid for `len` per the caller contract.
    let Some(b) = (unsafe { bytes(payload, len) }) else {
        return ECPUB_ERR_NULL;
    };
    p.inner.osc133(b, now_ns);
    ECPUB_OK
}

/// # Safety
/// `p` must be NULL or a live publisher; `out` NULL or writable.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ecpub_poll(p: *mut EcpubPublisher, now_ns: u64, out: *mut EcpubBatch) -> i32 {
    // SAFETY: NULL or live per the caller contract.
    let (Some(p), false) = ((unsafe { p.as_mut() }), out.is_null()) else {
        return ECPUB_ERR_NULL;
    };
    let empty = EcpubBatch {
        ops: ptr::null(),
        len: 0,
        seq: 0,
        flags: 0,
    };
    let EcpubPublisher { inner, ops } = p;
    let Some(batch) = inner.poll(now_ns) else {
        // SAFETY: non-null and writable per the caller contract.
        unsafe { out.write(empty) };
        return 0;
    };
    ops.clear();
    for op in batch.ops() {
        let (key, key_len) = op.key.map_or((ptr::null(), 0), |k| (k.as_ptr(), k.count_bytes()));
        let (s, s_len) = batch
            .bytes_with_nul(op)
            .map_or((ptr::null(), 0), |b| (b.as_ptr().cast::<c_char>(), b.len() - 1));
        ops.push(EcpubOp {
            kind: op.kind as u32,
            node: op.node,
            parent: op.parent,
            index: op.index,
            role: op.role,
            cursor: op.cursor,
            sel_start: op.sel_start,
            sel_end: op.sel_end,
            key,
            key_len,
            str_: s,
            str_len: s_len,
        });
    }
    let (seq, flags) = (batch.seq, batch.flags);
    // SAFETY: non-null and writable per the caller contract.
    unsafe {
        out.write(EcpubBatch {
            ops: ops.as_ptr(),
            len: ops.len(),
            seq,
            flags,
        })
    };
    1
}

/// # Safety
/// `p` must be NULL or a live publisher.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ecpub_next_deadline(p: *const EcpubPublisher) -> u64 {
    // SAFETY: NULL or live per the caller contract.
    unsafe { p.as_ref() }.map_or(u64::MAX, |p| p.inner.next_deadline())
}

/// # Safety
/// `p` must be NULL or a live publisher; `out` NULL or writable.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ecpub_get_stats(p: *const EcpubPublisher, out: *mut EcpubStats) -> i32 {
    // SAFETY: NULL or live per the caller contract.
    let (Some(p), false) = ((unsafe { p.as_ref() }), out.is_null()) else {
        return ECPUB_ERR_NULL;
    };
    let s = p.inner.stats();
    let retained = s.retained_bytes + (p.ops.capacity() * size_of::<EcpubOp>()) as u64;
    // SAFETY: non-null and writable per the caller contract.
    unsafe {
        out.write(EcpubStats {
            retained_bytes: retained,
            publishes: s.publishes,
            structural_publishes: s.structural_publishes,
            blocks: s.blocks,
        })
    };
    ECPUB_OK
}
