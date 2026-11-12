// SPDX-License-Identifier: AGPL-3.0-only
//! PAM off the UI loop.
//!
//! `pam_authenticate` blocks (pam_unix sleeps on failure, faillock may wait),
//! so each attempt runs on its own short-lived thread and its verdict comes
//! back over a oneshot channel. The state machine allows one attempt in flight,
//! so there is never more than one such thread.

use std::ffi::CString;

use iced::futures::channel::oneshot;

use crate::pam::{self, Verdict};
use crate::secret::Secret;

/// Hand `password` to PAM for `user` on a worker thread. The password moves
/// to the worker and is wiped there when PAM is done, or here if the thread
/// cannot start (the closure that owns it is dropped).
pub fn start(user: CString, password: Secret) -> oneshot::Receiver<Verdict> {
    let (tx, rx) = oneshot::channel();
    let spawned = std::thread::Builder::new()
        .name("ec-lock-pam".into())
        .spawn(move || {
            let verdict = pam::authenticate(crate::PAM_SERVICE, &user, password);
            let _ = tx.send(verdict);
        });
    if spawned.is_err() {
        eprintln!("ec-lock: cannot start the PAM worker; the attempt fails");
    }
    rx
}

/// The verdict, or `Unavailable` if the worker went away without one (it
/// panicked, or never started). Never a grant by default.
pub async fn verdict(rx: oneshot::Receiver<Verdict>) -> Verdict {
    rx.await.unwrap_or(Verdict::Unavailable)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_dropped_worker_is_unavailable() {
        let (tx, rx) = oneshot::channel::<Verdict>();
        drop(tx);
        let v = iced::futures::executor::block_on(verdict(rx));
        assert_eq!(v, Verdict::Unavailable);
    }
}
