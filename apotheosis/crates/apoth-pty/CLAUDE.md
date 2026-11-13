# apoth-pty

AGPL-3.0-only. Governing spec: T-18 §7 (processes, fds), T-00 §4.2 (write path), T-18 §10
(remote detection), ADR 0115 (credential signal). Not pure: real ptys, pidfds, /proc. The only
crate besides `apoth-wl` and the media FFI allowed `unsafe` (each block has `// SAFETY:`).
No logger, never logs content. `Debug` of `SpawnConfig` and `OutQueue` prints counts only.

## Public API (for apoth-term / apotheosis)
- `SpawnConfig { argv, login, cwd, env, size, iutf8 }`; `build_env(&EnvSpec)` makes the
  scrubbed env. `PtySize::from_cells(rows, cols, cell_w, cell_h)` (saturating pixels).
- `Pty::spawn(&cfg)`: new session, slave as ctty, master O_NONBLOCK+CLOEXEC, close_range
  CLOEXEC above fd 2, pidfd for exit. `master_fd()`/`pidfd()` to register in the loop.
- Read: `read(buf)` (WouldBlock = drained, EIO = slave closed; drain fully on HUP).
- Write (main thread only): `send(Vec<u8>)` for replies/keys, `send_paste(Vec<u8>)` for a
  whole bracketed paste. Both return `Enqueue::{Written, Queued, Dropped}`. Arm POLLOUT only
  while `wants_pollout()`; call `drain_out()` on POLLOUT (`Ok(true)` = empty). `dropped_units()`
  is a counter only.
- Queue bound (T-00 §4.2): 1 MiB (`OUT_QUEUE_BOUND`) of pending bytes of whole units plus the
  paste in flight (front unit, if a paste, is not counted; a paste behind other units counts).
  Overflow drops the new unit. Units are never interleaved.
- `resize(PtySize)` (no kill; kernel sends SIGWINCH), `kernel_winsize()`, `termios_state()`
  (`is_credential()` = ECHO off and ICANON on).
- `foreground_pgid()`, `foreground_cwd()`, `foreground_is_remote(&user_names)`: walks from every
  member of the foreground process group (falls back to the shell's group), depth <= 8, <= 256
  procs, unreadable or bound hit = remote.
- `try_wait()` reaps via pidfd. `close(&CloseTimings)` blocks (call off the main loop):
  SIGHUP, wait 1 s, SIGTERM, wait 1 s, SIGKILL, bounded reap (2 s; no spec number, a gap).
  Exit is observed on the pidfd without reaping, so the leader stays a zombie while the final
  group SIGKILL goes out and the pgid cannot be recycled. After the leader is reaped
  (`try_wait` returned a status) `signal_group` and `close` send nothing.
- `pid()` is child pid = pgid = sid. Tests identify children by the captured pid only.
