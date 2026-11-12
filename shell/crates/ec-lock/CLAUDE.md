# ec-lock — the screen locker

Read the root `CLAUDE.md` first. Governing decisions: ADR 0095 (this locker),
ADR 0024 (the compositor's fallback when it dies). Protocol: Vol 1's
`ext_session_lock_v1` row ("Screen lock").

- **Security-critical, not TCB.** The compositor holds the session shut; this
  client only decides when to send `unlock_and_destroy`. A bug here can strand
  the human (no way to unlock) or, worse, unlock early. Changes to `app.rs`'s
  unlock gate, `machine.rs` or `pam.rs` deserve line-by-line review.
- **Unlock only on PAM success.** `Message::Unlock(Granted)` is the only
  message `TryInto<UnLockAction>` accepts, and `iced_sessionlock` acts on it
  before `update`. `Granted`'s field is private to `pam.rs`; it is built only
  on `PAM_SUCCESS`. Never add another variant to that `TryInto`, never make
  `Granted` constructible elsewhere (tests use `pam::granted_for_test`).
- **Never `iced::window::close`.** `iced_sessionlock` maps a window-close
  action to `unlock_and_destroy`.
- **Every exit but a grant leaves the session locked.** Errors, panics and
  signals are fine: the compositor draws its fallback (ADR 0024). Anything that
  could make unlocking impossible (unknown user, bad argv, no PAM service
  file) is settled in `main.rs` *before* the lock is taken.
- **No PAM service file, no lock.** Without `/etc/pam.d/ec-lock` (or
  `/usr/lib/pam.d/ec-lock`) libpam uses `other`, which is `pam_deny` on Arch.
  `main.rs` exits non-zero before locking; the lookup is `service::find`,
  directories injectable for its tests.
- **The password.** Only in `secret::Secret` (fixed allocation, wiped, no
  `Debug`/`Clone`) and, for one key, `app::Typed` (fixed array, zeroed on
  drop). No `text_input`: keys come from the keyboard subscription. Never log,
  print or format it; `Message`'s `Debug` is hand-written to redact it.
- **PAM off the UI loop.** `auth::start` runs `pam_authenticate` on a worker
  thread and answers by oneshot channel. One attempt in flight at a time
  (`machine.rs`).
- **No PAM crate.** `pam.rs` links libpam directly (three calls and a
  conversation). Do not add a binding crate beside it.
- **argv is `--help`/`--version` only.** The user is the process uid.
- `view.rs` is eclipse-frontend's. It reads the model only through
  `Scene::of(&App)` (`status, typed, user, clock, can_submit`) and emits only
  `Message::Submit` / `Message::Clear`. `style()`/`ground()` stay opaque: a
  translucent lock surface shows the desktop. `examples/preview.rs` renders
  each state in an ordinary window for screenshots; it never locks.
- `tests/headless.rs` needs `EC_ABYSS_BIN` and is `#[ignore]`d; run it with
  `-- --ignored` after building `ec-abyss`.
- Every `.rs` starts with `// SPDX-License-Identifier: AGPL-3.0-only`.
