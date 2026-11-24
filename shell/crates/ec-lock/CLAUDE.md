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
- **Layout and Glances (ADR 0126) are drawing only.** `face.rs` reads
  `$XDG_CONFIG_HOME/eclipse/lock-layout` once at start (`O_NOFOLLOW`, 4 KiB cap,
  `Layout::from_lines` + `validate`; anything wrong is `face::builtin()`) and polls
  battery/wifi off the UI loop under `ec_glance::bounded`. `Message::Frame` and
  `Message::Glance` only set `Face`; they never convert to an unlock and never
  reach auth. The Password tile is always drawn (added if a layout lacks it) and
  never animates. `Users`, `Power`, `NowPlaying` tiles are not drawn here.
- `view.rs` is eclipse-frontend's. It reads the model only through
  `Scene::of(&App)` (`status, typed, user, clock, can_submit, face`) and emits only
  `Message::Submit` / `Message::Clear`. Glass (ADR 0129): under abyss in `glass`
  mode the compositor draws the wallpaper, then this surface's `eclipse_glass_v1`
  panes (blurring the wallpaper only), so `style()`/`ground_for()` draw **no ground**
  while `glass_live()` and every pane pushes a pane. In the fall-back (no global,
  not `glass` mode) the ground is **opaque** and panes are glass-styled, so the
  lock is never see-through. `glass.rs` hands each lock surface to `ec_ui::glass`
  through the vendored `iced_sessionlock` surface hook (`shell/vendor/PATCHES.md`
  hunk 4). The lock draws only the primary and secondary inks (4.5:1 fall-back, 3:1
  on glass; tests in `view.rs`). `examples/preview.rs` renders
  each state in an ordinary window for screenshots; it never locks.
- `tests/headless.rs` needs `EC_ABYSS_BIN` and is `#[ignore]`d; run it with
  `-- --ignored` after building `ec-abyss`.
- Every `.rs` starts with `// SPDX-License-Identifier: AGPL-3.0-only`.
- The clock face follows the login screen's clock preset: read-only from `/var/lib/eclipse/greeter/settings.toml` via
  `ec_greeter::Settings::parse` and `ec_greeter::clock_format`; a missing or invalid file means the default (12-hour with date).
  Strings and sizing come from `ec_glance::clock` and `ec_glance::tile`, shared with the greeter and the builder preview.
- The layout file needs only a Password tile; a Users or Power tile in it is accepted and ignored (`ec_layout::validate_with`
  with `LOCK_REQUIRED`; built-in is `Layout::lock_default()`). Sizes and words come from `ec_glance::cells` (`lock_frame`,
  `lock_greeting`, `lock_password`, `cells::lock`), not from `tokens.rs` or `view.rs`. Settings' lock builder
  (`ec-settings/.../lock_mock.rs`) draws the same plans, so change `view.rs` and the preview together; tests fail on drift.
  The bare clock has no pane and no padding on purpose, and the preview copies that: do not "fix" one side.
