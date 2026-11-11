// SPDX-License-Identifier: AGPL-3.0-only
//! PAM, in this process (ADR 0095).
//!
//! The repository had no PAM binding: the greeter leaves PAM to greetd and the
//! polkit prompt leaves it to polkit's root helper (ADR 0088). Rather than a
//! binding crate for three calls, this links libpam directly. The surface is
//! `pam_start`, `pam_authenticate`, `pam_end` and one conversation callback.
//!
//! What crosses the conversation: the password, once, to the first no-echo
//! prompt, as a `calloc` copy PAM owns and frees (Linux-PAM overwrites replies
//! before freeing them). Any echoed prompt, a second no-echo prompt, or a
//! binary/radio prompt fails the conversation: the locker asks for one thing.
//! Module text (`PAM_ERROR_MSG`, `PAM_TEXT_INFO`) is counted, never printed.
//!
//! [`Granted`] is the only proof of success, and it is built in exactly one
//! place: [`authenticate`], on `PAM_SUCCESS`. Its field is private to this
//! module, so no other code in the crate, `view.rs` included, can make one.

use std::ffi::{c_char, c_int, c_void, CStr, CString};
use std::fmt;

use crate::secret::Secret;

/// Proof that PAM said yes. `Message::Unlock` carries one, and is the only
/// message `iced_sessionlock` turns into `unlock_and_destroy`.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct Granted {
    _sealed: (),
}

impl fmt::Debug for Granted {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Granted")
    }
}

/// A test-only grant, for the state machine's tests.
#[cfg(test)]
pub(crate) fn granted_for_test() -> Granted {
    Granted { _sealed: () }
}

/// What one PAM attempt came to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Verdict {
    Granted(Granted),
    /// PAM refused the password (or the account, or too many tries).
    Denied,
    /// PAM could not decide: no service file it can use, a conversation it
    /// could not finish, a module error. Never a grant.
    Unavailable,
}

// Linux-PAM, <security/_pam_types.h>.
const PAM_SUCCESS: c_int = 0;
const PAM_BUF_ERR: c_int = 5;
const PAM_PERM_DENIED: c_int = 6;
const PAM_AUTH_ERR: c_int = 7;
const PAM_CRED_INSUFFICIENT: c_int = 8;
const PAM_USER_UNKNOWN: c_int = 10;
const PAM_MAXTRIES: c_int = 11;
const PAM_CONV_ERR: c_int = 19;
const PAM_DISALLOW_NULL_AUTHTOK: c_int = 0x0001;
const PAM_PROMPT_ECHO_OFF: c_int = 1;
const PAM_ERROR_MSG: c_int = 3;
const PAM_TEXT_INFO: c_int = 4;
const PAM_MAX_NUM_MSG: c_int = 32;

#[repr(C)]
struct PamHandle {
    _opaque: [u8; 0],
}

// The C layout, field for field; not every field is read from Rust.
#[repr(C)]
#[allow(dead_code)]
struct PamMessage {
    msg_style: c_int,
    msg: *const c_char,
}

#[repr(C)]
#[allow(dead_code)]
struct PamResponse {
    resp: *mut c_char,
    resp_retcode: c_int,
}

type ConvFn = unsafe extern "C" fn(
    num_msg: c_int,
    msg: *mut *const PamMessage,
    resp: *mut *mut PamResponse,
    appdata: *mut c_void,
) -> c_int;

#[repr(C)]
struct PamConv {
    conv: Option<ConvFn>,
    appdata_ptr: *mut c_void,
}

#[link(name = "pam")]
extern "C" {
    fn pam_start(
        service: *const c_char,
        user: *const c_char,
        conv: *const PamConv,
        pamh: *mut *mut PamHandle,
    ) -> c_int;
    fn pam_authenticate(pamh: *mut PamHandle, flags: c_int) -> c_int;
    fn pam_end(pamh: *mut PamHandle, status: c_int) -> c_int;
}

/// The conversation's state, reached through `appdata_ptr`.
struct Conv<'a> {
    password: &'a [u8],
    answered: bool,
}

/// Authenticate `user` against PAM service `service` with `password`. Blocks
/// for as long as the PAM stack does (pam_unix's failure delay, faillock):
/// call it from [`crate::auth`]'s worker, never from the UI loop. `password`
/// is wiped when this returns, whatever the outcome.
pub fn authenticate(service: &str, user: &CStr, password: Secret) -> Verdict {
    let Ok(service) = CString::new(service) else {
        return Verdict::Unavailable;
    };
    let mut conv = Conv {
        password: password.as_bytes(),
        answered: false,
    };
    let pam_conv = PamConv {
        conv: Some(converse),
        appdata_ptr: (&mut conv as *mut Conv<'_>).cast(),
    };
    let mut handle: *mut PamHandle = std::ptr::null_mut();
    // SAFETY: every pointer is valid for the call: the C strings and
    // `pam_conv` live on this frame until `pam_end`, and `handle` is an
    // out-pointer PAM fills.
    let started = unsafe { pam_start(service.as_ptr(), user.as_ptr(), &pam_conv, &mut handle) };
    if started != PAM_SUCCESS || handle.is_null() {
        return Verdict::Unavailable;
    }
    // SAFETY: `handle` came from a successful `pam_start`; the conversation
    // it holds points at `conv`, which outlives this call.
    let code = unsafe { pam_authenticate(handle, PAM_DISALLOW_NULL_AUTHTOK) };
    // SAFETY: as above; `pam_end` is the last use of `handle`.
    unsafe { pam_end(handle, code) };
    drop(password);
    verdict(code)
}

/// Fail-closed mapping: only `PAM_SUCCESS` grants; every other code, known or
/// not, is a refusal.
fn verdict(code: c_int) -> Verdict {
    match code {
        PAM_SUCCESS => Verdict::Granted(Granted { _sealed: () }),
        PAM_AUTH_ERR | PAM_USER_UNKNOWN | PAM_MAXTRIES | PAM_PERM_DENIED | PAM_CRED_INSUFFICIENT => {
            Verdict::Denied
        }
        _ => Verdict::Unavailable,
    }
}

/// The PAM conversation. Answers the first no-echo prompt with the password,
/// lets module text through unanswered, fails on anything else.
///
/// # Safety
/// Called by libpam with `msg` an array of `num_msg` message pointers
/// (Linux-PAM layout) and `appdata` the `Conv` set in [`authenticate`].
unsafe extern "C" fn converse(
    num_msg: c_int,
    msg: *mut *const PamMessage,
    resp: *mut *mut PamResponse,
    appdata: *mut c_void,
) -> c_int {
    if num_msg <= 0 || num_msg > PAM_MAX_NUM_MSG || msg.is_null() || resp.is_null() || appdata.is_null() {
        return PAM_CONV_ERR;
    }
    let n = num_msg as usize;
    // SAFETY: `appdata` is the `Conv` `authenticate` passed, alive for the
    // whole `pam_authenticate` call.
    let conv = unsafe { &mut *appdata.cast::<Conv<'_>>() };
    // PAM frees the array and each reply with free(), so both come from the C
    // allocator. calloc leaves every `resp` null.
    // SAFETY: plain allocation; checked for null below.
    let out = unsafe { libc::calloc(n, std::mem::size_of::<PamResponse>()) }.cast::<PamResponse>();
    if out.is_null() {
        return PAM_BUF_ERR;
    }
    for i in 0..n {
        // SAFETY: `msg` holds `n` pointers.
        let m = unsafe { *msg.add(i) };
        if m.is_null() {
            // SAFETY: `out` holds `n` entries, each null or our own copy.
            unsafe { free_responses(out, n) };
            return PAM_CONV_ERR;
        }
        // SAFETY: a non-null message from PAM.
        let style = unsafe { (*m).msg_style };
        match style {
            PAM_PROMPT_ECHO_OFF if !conv.answered => {
                let len = conv.password.len();
                // SAFETY: plain allocation, one spare byte for the NUL that
                // calloc already wrote.
                let copy = unsafe { libc::calloc(len + 1, 1) }.cast::<u8>();
                if copy.is_null() {
                    // SAFETY: as above.
                    unsafe { free_responses(out, n) };
                    return PAM_BUF_ERR;
                }
                // SAFETY: `copy` has `len + 1` bytes, the source `len`, and
                // they cannot overlap; `out` has `n` entries and `i < n`.
                unsafe {
                    std::ptr::copy_nonoverlapping(conv.password.as_ptr(), copy, len);
                    (*out.add(i)).resp = copy.cast();
                }
                conv.answered = true;
            }
            // Module text: counted by nobody, shown nowhere. Its content can
            // name the account or the lockout; the view says what it needs to
            // from the verdict alone.
            PAM_ERROR_MSG | PAM_TEXT_INFO => {}
            _ => {
                // SAFETY: as above.
                unsafe { free_responses(out, n) };
                return PAM_CONV_ERR;
            }
        }
    }
    // SAFETY: `resp` is PAM's out-pointer; it takes ownership of `out`.
    unsafe { *resp = out };
    PAM_SUCCESS
}

/// Wipe and free every reply in `out`, then `out` itself. Only for a
/// conversation that is failing, so PAM never sees `out`.
///
/// # Safety
/// `out` is a `calloc`'d array of `n` responses, each `resp` null or a
/// NUL-terminated `calloc` copy made by [`converse`].
unsafe fn free_responses(out: *mut PamResponse, n: usize) {
    for i in 0..n {
        // SAFETY: within the array.
        let r = unsafe { (*out.add(i)).resp };
        if !r.is_null() {
            // SAFETY: our own NUL-terminated copy; volatile so the overwrite
            // is not elided ahead of free().
            unsafe {
                let len = libc::strlen(r);
                for j in 0..len {
                    std::ptr::write_volatile(r.add(j), 0);
                }
                libc::free(r.cast());
            }
        }
    }
    // SAFETY: the array itself, from calloc.
    unsafe { libc::free(out.cast()) };
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_success_grants() {
        assert!(matches!(verdict(PAM_SUCCESS), Verdict::Granted(_)));
        for code in [
            PAM_AUTH_ERR,
            PAM_USER_UNKNOWN,
            PAM_MAXTRIES,
            PAM_PERM_DENIED,
            PAM_CRED_INSUFFICIENT,
        ] {
            assert_eq!(verdict(code), Verdict::Denied, "{code}");
        }
        for code in [
            -1,
            1,
            2,
            3,
            4,
            PAM_BUF_ERR,
            9,
            12,
            13,
            PAM_CONV_ERR,
            26,
            99,
            c_int::MAX,
        ] {
            assert_eq!(verdict(code), Verdict::Unavailable, "{code}");
        }
    }

    #[test]
    fn granted_debug_says_nothing_else() {
        assert_eq!(format!("{:?}", granted_for_test()), "Granted");
        assert_eq!(
            format!("{:?}", Verdict::Granted(granted_for_test())),
            "Granted(Granted)"
        );
    }

    /// Drive `converse` the way libpam would.
    fn run(styles: &[c_int], password: &[u8]) -> (c_int, Vec<Option<Vec<u8>>>) {
        let text = CString::new("Password: ").unwrap();
        let msgs: Vec<PamMessage> = styles
            .iter()
            .map(|&s| PamMessage {
                msg_style: s,
                msg: text.as_ptr(),
            })
            .collect();
        let mut ptrs: Vec<*const PamMessage> = msgs.iter().map(|m| m as *const _).collect();
        let mut conv = Conv {
            password,
            answered: false,
        };
        let mut resp: *mut PamResponse = std::ptr::null_mut();
        // SAFETY: the same contract libpam keeps.
        let code = unsafe {
            converse(
                ptrs.len() as c_int,
                ptrs.as_mut_ptr(),
                &mut resp,
                (&mut conv as *mut Conv<'_>).cast(),
            )
        };
        let mut replies = Vec::new();
        if code == PAM_SUCCESS {
            for i in 0..styles.len() {
                // SAFETY: `resp` is our calloc'd array of `styles.len()`.
                let r = unsafe { (*resp.add(i)).resp };
                replies.push((!r.is_null()).then(|| {
                    // SAFETY: NUL-terminated copy from `converse`.
                    unsafe { CStr::from_ptr(r) }.to_bytes().to_vec()
                }));
            }
            // SAFETY: what PAM would do: free the replies and the array.
            unsafe { free_responses(resp, styles.len()) };
        } else {
            assert!(resp.is_null(), "a failed conversation hands PAM nothing");
        }
        (code, replies)
    }

    #[test]
    fn answers_one_no_echo_prompt() {
        let (code, r) = run(&[PAM_TEXT_INFO, PAM_PROMPT_ECHO_OFF], b"pw");
        assert_eq!(code, PAM_SUCCESS);
        assert_eq!(r, vec![None, Some(b"pw".to_vec())]);
    }

    #[test]
    fn refuses_a_second_secret_or_an_echoed_prompt() {
        assert_eq!(
            run(&[PAM_PROMPT_ECHO_OFF, PAM_PROMPT_ECHO_OFF], b"pw").0,
            PAM_CONV_ERR
        );
        assert_eq!(
            run(&[2], b"pw").0,
            PAM_CONV_ERR,
            "echo-on (e.g. an OTP or a user name)"
        );
        assert_eq!(run(&[7], b"pw").0, PAM_CONV_ERR, "binary prompt");
        assert_eq!(run(&[], b"pw").0, PAM_CONV_ERR);
    }
}
