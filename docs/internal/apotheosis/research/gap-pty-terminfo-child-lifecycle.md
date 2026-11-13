# Gap dossier: PTY, terminfo and child-process lifecycle

Status: research input for the Apotheosis spec. Prior-art rule observed: techniques are described in prose; only interface facts (escape sequences, env var names, config keys) are quoted. Source studied locally (shallow clones in `/home/risk/.cache/apotheosis-prior-art/`): alacritty, foot, kitty, wezterm, ghostty (the last for cross-checks). Crate facts were read from the crates.io API on 2026-11-12.

## Summary

- PTY layer: use `rustix` (Apache-2.0 WITH LLVM-exception OR Apache-2.0 OR MIT) plus `rustix-openpty` for `openpty`, or do `posix_openpt`/`grantpt`/`unlockpt` directly in rustix's `pty` module. Spawn the child with `std::process::Command` + `pre_exec` (setsid, TIOCSCTTY, signal reset). Do not take `portable-pty` (MIT, 28M downloads, but pulls anyhow, filedescriptor, nix, serial2, shell-words, and its `spawn_command` closes fds by walking `/dev/fd` and has no pidfd or foreground-process API beyond `tcgetpgrp`). Do not take `pty-process` as the core (MIT, usable, but ties to tokio/futures-style APIs and a fixed spawn model). All three are licence-clean; the choice is about control, not licence.
- The three native prior-art terminals (foot, kitty, alacritty) all do the same ordered dance: openpty, set IUTF8 on the pty, fork, `setsid`, open/ioctl the slave as controlling tty (`TIOCSCTTY`), dup to 0/1/2, reset signal dispositions and mask, close stray fds, exec. wezterm's portable-pty does the same minus IUTF8. We should do all of these.
- Foreground process / cwd: `tcgetpgrp(master_fd)` then `/proc/<pid>/cwd`, `/proc/<pid>/exe`, `/proc/<pid>/cmdline`, `/proc/<pid>/stat` (pgrp, tpgid, session). kitty adds a process-group map built from `/proc/*/stat` with a 1 s TTL cache; alacritty only does tcgetpgrp + `/proc/pid/cwd` and falls back to the shell pid. OSC 7 should be primary, /proc is the fallback (wezterm does exactly this order).
- Resize: `TIOCSWINSZ` on the master with all four fields, including pixel width/height, which foot/alacritty/wezterm fill (cols*cell_w, rows*cell_h) and which kitty-graphics and sixel-aware tools read via `TIOCGWINSZ`. The kernel itself raises SIGWINCH for the foreground process group when the size changes; the terminal does not signal anyone. Also support in-band resize notifications (DEC private mode 2048) which foot implements.
- Child exit: foot uses SIGCHLD via its fd multiplexer plus EPOLLHUP on the ptmx; alacritty uses a signal-hook self-pipe SIGCHLD plus `Child::try_wait`; kitty uses a signal fd style handler setting `child_died`. For Apotheosis (single calloop-style event loop per window, or tokio-less epoll) the best fit is **pidfd** (`pidfd_open`, Linux >= 5.3; rustix exposes it) polled as a normal fd: no signal handlers, no global state, race-free, one fd per child. SIGCHLD stays as a fallback only. Unverified: whether `std::process::Child` exposes the pidfd on our pinned toolchain; rustix `pidfd_open` is the safe route.
- utmp/wtmp: nobody in the native set writes utmp directly. foot shells out to a setgid helper (`utmp-helper`, libutempter on Linux, default off for non-Linux) and alacritty/kitty/wezterm/ghostty write none. xterm writes utmp itself (setgid/`-ut`). Recommendation: **no utmp/wtmp by default**; optional `utmp-helper` config key pointing at an external helper binary, never linking libutempter (C).
- Env sanitising: set `TERM`, `COLORTERM=truecolor`, `TERM_PROGRAM=Apotheosis`, `TERM_PROGRAM_VERSION`; scrub other terminals' leakage (foot scrubs about 30 variables: WEZTERM_*, KITTY_*, GHOSTTY_*, CONTOUR_*, XTERM_*, MLTERM, GNOME_TERMINAL_*, TERM_PROGRAM); scrub `XDG_ACTIVATION_TOKEN`/`DESKTOP_STARTUP_ID` (alacritty) so children do not reuse the launch token; set `TERMINFO` only if we bundle a database outside the system path.
- Backpressure: never block the UI on pty reads, never read unbounded. Everyone reads in bounded batches. Design target: parser thread or in-loop parse of 64 KiB gather buffers, bounded work per wakeup, kernel pty queue provides natural flow control to the child (stop reading and the writer blocks). Add a 3 ms input-coalescing window (kitty `input_delay` 3, wezterm `mux_output_parser_coalesce_delay_ms` 3) that is bypassed for small echo-sized reads, and honour DEC 2026 synchronized output.
- Terminfo without C: three separable problems. (1) The compiled file format (term(5), legacy 16-bit and the ncurses extended-number variant, plus extended user-defined caps) is simple and can be emitted by about 300 lines of Rust from a source description; so a **Rust-only build step is realistic**. (2) The Rust crates for the job are weak: `terminfo` 0.9.0 is a reader under **WTFPL (not in our allowed list)**; `termini` 1.0.0 (MIT) is a reader; `terminfokit` 0.1.0 (published 2026-08, 69 downloads, licence `(MIT OR Apache-2.0) AND X11`) claims compile/decompile but is new and tiny: treat as unvetted. (3) XTGETTCAP does not need a compiled file at all: foot and ghostty generate a build-time static table from the terminfo source; wezterm instead parses its embedded wezterm.terminfo-equivalent via the `terminfo` crate at runtime and special-cases TN/Co/RGB.
- Settled recommendation: ship `xterm-apotheosis` (+ `xterm-apotheosis-direct`) as terminfo **source of truth in-repo (`.ti` text)**, compile it with our own small Rust `apotheosis-terminfo` build tool into `/usr/share/terminfo/x/xterm-apotheosis` (and the hex-named `78/` twin if distro layout needs it) at package time, and generate the XTGETTCAP table from the same source at build time. At runtime, if the database file is absent (`terminfo_exists` probe, same search order as alacritty) fall back to `TERM=xterm-256color` + `COLORTERM=truecolor`. Offer `terminfo-mode` = `path | direct | none` like kitty's `terminfo_type`, where `direct` is out of scope for v1 (not widely supported).

## Architecture (how it is built)

### What each reference does, end to end

alacritty_terminal (`alacritty_terminal/src/tty/unix.rs`: `new`, `from_fd`, `set_controlling_terminal`, `on_resize`; `event_loop.rs`: `pty_read`, `spawn`):
- Depends on `rustix-openpty` 0.2.0, `rustix`, `signal-hook` 0.4, `polling` 3.x, plus raw `libc` for ioctls. Crate licence Apache-2.0.
- `openpty` is called with the initial `Winsize` so the child never sees a 0x0 window. Then `IUTF8` is set via termios on the master.
- Shell: from `SHELL` env else passwd entry; user/home likewise. Plain shell is not a login shell on Linux (argv[0] is not dash-prefixed); on macOS it execs `/usr/bin/login -flp user` with a wrapper `exec -a` line, and checks `~/.hushlogin` itself because login only checks cwd.
- stdio: stdin/stdout/stderr of the `Command` are set to clones of the slave fd. `pre_exec` closure does `setsid`, optional `chdir` (invalid paths ignored), `TIOCSCTTY` on the slave, closes slave+master, and resets SIGCHLD/SIGHUP/SIGINT/SIGQUIT/SIGTERM/SIGALRM to default.
- Env: `TERM` chosen by probing the terminfo search path for an `alacritty` entry, else `xterm-256color`; `COLORTERM=truecolor`; `ALACRITTY_WINDOW_ID`, `WINDOWID` (X11 hack), `USER`, `HOME`; removes `XDG_ACTIVATION_TOKEN` and `DESKTOP_STARTUP_ID`.
- Child exit: `signal_hook::low_level::pipe` registers SIGCHLD into a nonblocking UnixStream registered in the same `polling::Poller` as the pty fd; on wake it reads the byte and calls `try_wait`.
- Reads: single IO thread, level-triggered, 1 MiB read buffer, takes the terminal mutex with `try_lock_unfair` and keeps reading while it can't lock until the buffer is full, then forces a blocking lock; releases the lock after 65535 processed bytes (`MAX_LOCKED_READ`) so the renderer can run. A "fair lease" object reserves the next lock for the reader so the renderer cannot starve it.
- Writes: queue of buffers; write interest is only registered with the poller while a write is pending.
- Resize: `TIOCSWINSZ` with pixel fields computed as cols*cell_width / rows*cell_height; failure is fatal there.
- Foreground cwd: `foreground_process_path` in `alacritty/src/daemon.rs`: `tcgetpgrp(master)`, fall back to shell pid on error, read `/proc/<pid>/cwd` (macOS and OpenBSD use libproc and sysctl). Used for "spawn new window in same directory".

foot (`slave.c`: `slave_spawn`, `login_shell` handling, `del_from_env`; `terminal.c`: `fdm_ptmx`, `fdm_ptmx_out`, `tiocswinsz`, shutdown path; `reaper.c`; `dcs.c`; `scripts/generate-builtin-terminfo.py`):
- `posix_openpt` in the parent, then `fork`. In the child: `grantpt/unlockpt`, close the master, `setsid`, then **open the slave by name after setsid** so the open itself makes it the controlling tty, and also issue `TIOCSCTTY` for BSD. Sets IUTF8 on the slave termios. dup2 to 0/1/2. Login shell = argv[0] prefixed with '-' while exec'ing the original file (config `login-shell`, default no).
- A CLOEXEC pipe from child to parent carries `errno` if exec fails; the parent blocks on it so spawn errors are reported synchronously. Good pattern (std::process does the same internally).
- Env: sets `TERM` (config `term`, default is the foot terminfo name), `COLORTERM=truecolor`; deletes a long list of variables set by other terminals (TERM_PROGRAM and _VERSION, TERMINAL_NAME and TERMINAL_VERSION_*, XTERM_*, MLTERM, WEZTERM_*, KITTY_*, GHOSTTY_*, CONTOUR_PROFILE, GNOME_TERMINAL_*); sets `TERMINFO` to the install path only when built with a private path.
- Initial window: sets 24x80 `TIOCSWINSZ` before the first real layout; later `tiocswinsz` writes rows, cols, `ws_xpixel = cols*cell_width`, `ws_ypixel = rows*cell_height`, then sends the in-band size report (`CSI 48;rows;cols;height;width t`, mode 2048) if enabled.
- Reads: epoll-driven (`fdm`), non-blocking ptmx, up to 10 reads of a 24 KiB stack buffer per wakeup (unbounded when EPOLLHUP is set so final output is drained), EAGAIN and EIO treated as "stop"; reading is suspended during interactive (live) resize. Output rendering is delayed by two timers (lower and upper bound, config `tweak.delayed-render-lower/upper`) so a burst of writes becomes one frame; skipped when app-synchronized-updates (2026) is active.
- Writes to the child (keys, paste) go through a per-terminal queue; EPOLLOUT only armed when the queue is non-empty.
- Shutdown: close ptmx, `kill(-pgid, SIGHUP)` to the whole process group, then escalate SIGTERM then SIGKILL on timers (60 s fallback with SIGALRM in the sync path). A reaper registered on SIGCHLD collects exits.
- utmp: external helper executable (libutempter on Linux); config `utmp-helper`, `none` disables.
- XTGETTCAP (DCS `+ q`): `foot.info` is a template; a Python script at build time turns the source into a sorted table of NUL-delimited name/value pairs embedded in a header; lookup is a linear scan of the sorted table with a unit test asserting sort order. Booleans reply `DCS 1 + r <hexname> ST`, strings `DCS 1 + r <hexname> = <hexvalue> ST`, unknown `DCS 0 + r ST`. The same source is compiled with `tic` at install time.

kitty (`kitty/child.py`: `Child.fork`, `get_final_env`, `foreground_processes`, `get_pid_for_cwd`, `process_group_map`, `CachedProcessData`; `kitty/child.c`: `spawn`; `kitty/child-monitor.c`: `read_bytes`, `handle_signal`, wake logic; `kitty/terminfo.py`; `build-terminfo`):
- Python calls a C `spawn` after `openpty`. Child: reset handled signals and Python-ignored signals (SIGPIPE, SIGXFSZ) to default, empty sigmask, `chdir` (fall back to `/` if cwd unusable), `setsid`, **reopen the slave by name** and `TIOCSCTTY`, dup2 stdio, wait on a "ready" pipe until the parent has built the screen object (so early output cannot be lost), close fds 3..255 except explicitly passed ones, set environ, exec. On exec failure it execs a helper that holds the window open with the error visible (`hold_till_enter`).
- Env: `TERM` (config `term`, default `xterm-kitty`), `COLORTERM=truecolor`, `KITTY_PID`, `KITTY_PUBLIC_KEY`, `KITTY_LISTEN_ON`, `KITTY_INSTALLATION_DIR`, `PWD` set to the (possibly symlinked) cwd so shells show the logical path; `TERMINFO` per config `terminfo_type`: `path` (point at bundled dir if present), `direct` (the whole compiled database base64-encoded in the variable, prefix `b64:`; useful for containers; not all software supports it), `none`.
- Login shell on macOS wraps with `/usr/bin/login -f -l -p user`; on Linux login shells are done by prefixing argv[0] when `shell` is the default and config says so. It tracks "wrapper" processes (login) so `root_pid` can resolve the real shell.
- Foreground info: `tcgetpgrp` on the master then `/proc` scan of `/proc/*/stat` field 5 (pgrp) into a map; a cache with TTL 1 s and explicit start/stop-caching context avoids rescanning when several queries run in one action. When several processes share the foreground group the heuristic picks the highest pid ("newest") for cwd, lowest if `oldest` is requested. Also: cmdline, environ (`/proc/pid/environ`), memory use (smaps_rollup), background processes of same session.
- Reads: separate IO thread using `poll`; parser has a 1 MiB buffer; input wakeup to the main thread is delayed by `input_delay` (3 ms default) but skipped for tiny echo-like reads at most once per delay; the delay is ignored when the buffer is nearly full. On macOS only, a gather loop of up to 64 KiB because the kernel hands out 1 KiB chunks.
- Signals: handled via a signalfd-like loop; SIGCHLD sets `child_died`, SIGINT/TERM/HUP request shutdown, SIGUSR1 reload config.
- Terminfo: Python tables (`kitty/terminfo.py`) generate a source file; `build-terminfo` calls the **external `tic`** to compile it and the output is embedded into the binary as a C array for the `direct` mode. So kitty does need C tooling (ncurses tic) at build time. XTGETTCAP is not implemented as a table lookup in the parts we found; kitty documents its own query kitten instead (unverified whether `+q` is handled, the changelog mentions an XTGETTCAP fix for empty string capabilities, line 1665 of `docs/changelog.rst`).

wezterm (`pty/src/unix.rs`, `pty/src/cmdbuilder.rs`, `procinfo/src/linux.rs`, `mux/src/lib.rs`, `mux/src/localpane.rs`, `term/src/terminalstate/mod.rs: xt_get_tcap`, `config/src/config.rs`):
- `portable-pty` 0.9.0, MIT, from the wezterm repo. `openpty` is libc `openpty` with a `winsize` that includes pixel dimensions. `UnixMasterPty::resize` ioctl TIOCSWINSZ with pixels.
- `spawn_command`: std `Command`, `pre_exec` resets six signal dispositions, clears the sigmask, `setsid`, `TIOCSCTTY` on fd 0 (optional `controlling_tty` flag), then **closes every fd above 2 by listing `/dev/fd`** (works on Linux because `/dev/fd` links to `/proc/self/fd`), optional umask. The `process_group_leader()` method is just `tcgetpgrp`.
- Command builder: env starts from the process env (case-insensitive handling only matters on Windows), always clears and re-applies an explicit env map; default program = login shell from passwd/`$SHELL`, **argv[0] is `-basename`** (always a login shell for the default program); cwd falls back to home if the given dir does not exist; `SHELL` set to the resolved shell.
- Env set at spawn: `TERM` (config `term`, default `xterm-256color`), `COLORTERM=truecolor`, `TERM_PROGRAM=WezTerm`, `TERM_PROGRAM_VERSION`; extra `WEZTERM_*` vars; config `mux_env_remove` list. Terminfo for the `wezterm` name is **not** installed by default: users run `tic -x` themselves (docs `term.md`).
- Reads: a thread per pane reads up to 1 MiB from the pty and forwards through a socketpair (with enlarged socket buffers) to a parser thread; the parser thread uses a buffer of `mux_output_parser_buffer_size` (default 128 KiB) and a coalesce delay `mux_output_parser_coalesce_delay_ms` (default 3 ms) to batch actions before taking the mux lock. When the child exits the reader sees EOF; `exit_behavior` = Close / Hold / CloseOnCleanExit decides pane fate.
- Foreground/cwd: `LocalProcessInfo::with_root_pid` builds a process tree by scanning `/proc` (`stat`, `cmdline`), `current_working_dir` and `executable_path` use `readlink` on `/proc/pid/{cwd,exe}`. The pane first trusts the cwd from OSC 7, only then "divines" from /proc, with a cache policy (`AllowStale`) to limit syscalls.
- XTGETTCAP handled in `xt_get_tcap`: for each requested (hex-decoded) name: TN/name returns the term-program name, Co/colors returns 256, RGB returns 8/8/8, anything else is looked up in the embedded terminfo database read with the `terminfo` crate at runtime; unknown returns `0+r`. Replies are hex-uppercase encoded.

ghostty (cross-check, `src/termio/Exec.zig`, `src/pty.zig`, `src/terminfo/Source.zig`, `src/build/GhosttyResources.zig`): `TERM=xterm-ghostty` only if a resources dir with the terminfo exists, else falls back to `xterm-256color` (same pattern as alacritty); `TERMINFO` set to the bundled directory; `TERM_PROGRAM=ghostty` and `TERM_PROGRAM_VERSION`. Terminfo source is generated by the program itself (`+terminfo` action) then compiled with external `tic` at build/install; `infocmp`-style termcap conversion with `infotocap` is optional. XTGETTCAP responses are produced from a compile-time static string map built out of the same capability list, with TN, Co, RGB injected. Two-stage read pipeline: a gather thread fills 4 rotating 64 KiB buffers, a reader thread parses; spin-then-poll bridging for saturated streams, with at most one display frame of budget. A 200 ms termios poll (while focused) detects echo/canonical-mode changes (used for password-prompt style hints).

### Resulting design for Apotheosis

1. **`apotheosis-pty` crate** (small, in-tree): `Pty { master: OwnedFd, child: Child, pidfd }`, no globals.
   - `openpty` via rustix-openpty (or rustix `openpt`+`grantpt`+`unlockpt`+`ptsname`), set `O_CLOEXEC` and `O_NONBLOCK` on the master, set initial winsize **with pixel fields**, set `IUTF8`.
   - Spawn via `std::process::Command` with `pre_exec`: reset sigmask and six default dispositions, `setsid`, `TIOCSCTTY(0)` on the slave (re-open by `ptsname` after setsid if we want the foot/kitty variant that works without ioctl on Linux), `close_range` (rustix/libc 5.9+) instead of walking `/dev/fd`; fallback to the fd walk. Use `Command::arg0` for the login-shell dash prefix (wezterm does this; foot/kitty emulate it manually).
   - Errors from exec are returned synchronously through std's internal CLOEXEC pipe.
2. **Event loop integration**: master fd and pidfd registered in the window's epoll/calloop source set. Level-triggered reads, bounded batches (see Performance), EAGAIN/EIO = stop, EPOLLHUP = drain-then-finish. Writes via a queue, EPOLLOUT armed only when non-empty (foot, alacritty agree).
3. **Process introspection module** (`/proc` only, no `libproc`): `foreground_pgid`, `foreground_pid` (heuristic: pick highest pid in the group like kitty, with an oldest option), `cwd`, `exe`, `cmdline`, `environ` on request, parent/child walk via `/proc/<pid>/task/<tid>/children` when present (kitty `_children_from_task_files`) else a /proc scan, with a short-TTL cache scoped to one action invocation.
4. **Terminfo pipeline** in-repo: `.ti` source -> Rust tool -> compiled binary files + XTGETTCAP table, all at build/package time (see Feature inventory, terminfo).

## Feature inventory (grouped by category)

### PTY allocation and spawn
- openpty with initial size including pixels (alacritty `new`, wezterm `openpty`).
- IUTF8 on the pty (alacritty, foot, ghostty `pty.zig`; absent in wezterm/kitty main path as far as read).
- setsid + controlling tty: `TIOCSCTTY` (all four); foot/kitty reopen the slave after setsid, wezterm/alacritty ioctl on an inherited fd.
- Signal hygiene in the child: restore default for SIGCHLD, SIGHUP, SIGINT, SIGQUIT, SIGTERM, SIGALRM (alacritty, wezterm); kitty also SIGPIPE/SIGXFSZ and clears the sigmask; foot sets SIGHUP default via sigaction and clears sigmask. Ignored signals are inherited across exec, so this matters (Rust's std ignores SIGPIPE in the parent, a classic pitfall: reset SIGPIPE to default in `pre_exec`, otherwise shell pipelines behave oddly).
- fd hygiene: kitty closes 3..255, wezterm closes everything via `/dev/fd`, alacritty/foot rely on CLOEXEC discipline. Rust std opens with CLOEXEC; still use `close_range`.
- Synchronous exec error reporting (foot errno pipe; std does the same).
- "Ready" handshake so early child output cannot be lost before the screen exists (kitty). With a nonblocking master and kernel buffering, losing output is not a risk in our design; skip.
- Hold-on-failure window: when exec fails, show the error rather than closing (kitty hold kitten; wezterm `exit_behavior = Hold`).
- Shell selection: config shell, else `$SHELL`, else passwd entry; verify `access(X_OK)` (wezterm `get_shell`); login-shell flag (foot `login-shell` default no; wezterm always for default prog; alacritty only on macOS).
- Spawn cwd fallback: if requested cwd is gone, use home (wezterm) or `/` (kitty).
- `PWD` set to logical cwd (kitty).

### Resize and signals
- `TIOCSWINSZ` with ws_row/ws_col/ws_xpixel/ws_ypixel; pixel values are the grid pixel size (cols*cell_w), which foot, alacritty and wezterm define; whether to include padding is a design choice: kitty-graphics clients compute image cell sizes from it, so use the cell grid area, not the window.
- SIGWINCH is kernel-generated to the foreground process group on a size change; the terminal must only call the ioctl, and should skip the ioctl when nothing changed (identical sizes do not signal).
- In-band resize reports, DEC mode 2048 (foot `term_send_size_notification`).
- Resize during live drag: foot suspends pty draining and uses a temporary grid; alacritty resizes immediately. Decide whether to coalesce resize ioctls to one per frame.
- SIGCHLD vs pidfd (see Summary).
- Window close: SIGHUP to the process group (foot `kill(-pgid, SIGHUP)`), escalate TERM then KILL with timers; close master first which also delivers SIGHUP to the session leader via the kernel.

### Introspection
- Foreground pgrp via `tcgetpgrp(master)`; fall back to shell pid if it fails.
- cwd via `/proc/pid/cwd`; exe via `/proc/pid/exe`; cmdline via `/proc/pid/cmdline` (NUL separated); environ via `/proc/pid/environ` (own-uid only; same-uid check is what the kernel enforces).
- Process tree and session/pgrp map from `/proc/*/stat` (pid, comm in parens, state, ppid, pgrp, session, tty_nr, tpgid are fields 1..8; comm can contain spaces and parens so parse from the last ')' — wezterm's `LinuxStat` parser handles this; kitty uses a fixed split on spaces and could mis-split, a known pitfall). Unverified: exact field list quoted from proc(5), not re-fetched.
- OSC 7 reported cwd is preferred when present (wezterm); `/proc` is the fallback.
- "Is a program running" check for close-confirmation: compare `tcgetpgrp` to the shell's pgid; if different, a foreground job exists.
- Termios polling for echo/canonical-off (ghostty, 200 ms while focused) to infer password prompts; also useful for "secure keyboard" like behaviour. Cheap: one `tcgetattr` on the master.

### Environment
- `TERM`: own entry when installed, else `xterm-256color`.
- `COLORTERM=truecolor` always (foot, alacritty, kitty, wezterm, ghostty all agree).
- `TERM_PROGRAM`, `TERM_PROGRAM_VERSION` (wezterm, ghostty set; foot deletes those so a nested foot is not confused; so: set ours, delete theirs).
- Scrub other terminals' variables (foot list); scrub launch tokens `XDG_ACTIVATION_TOKEN`, `DESKTOP_STARTUP_ID` (alacritty).
- `TERMINFO` only for bundled DB; `TERMINFO_DIRS` untouched.
- `WINDOWID`: X11 only; skip (Wayland-only).
- Our own: `APOTHEOSIS_PANE`, `APOTHEOSIS_WINDOW`, `APOTHEOSIS_SOCKET` (control socket path), `APOTHEOSIS_VERSION`; scrubbed from nested instances when not wanted.
- `LANG`/`LC_*`: do not invent; but kitty provides `set_LANG_in_default_env` for the case where the launcher has no locale (menu launchers). Offer an explicit config `env` table instead.

### utmp/wtmp
- Default off. Optional external helper path (foot `utmp-helper`). We must not link libutempter. Writing `/var/run/utmp` directly needs group utmp privileges and is distro policy; defer to the helper binary approach.

### Flow control and backpressure
- Read bounds: foot 10 x 24 KiB per wakeup; alacritty 1 MiB buffer but unlock after 65535 bytes; kitty 1 MiB parser buffer with 3 ms delay; wezterm 1 MiB read + 128 KiB parse batch + 3 ms coalesce; ghostty 4 x 64 KiB pipeline.
- Backpressure is the kernel pty queue: if the terminal stops reading, writers block. No userspace unbounded queue anywhere. Our rule: parse-time per wakeup is time-budgeted (for example 2 ms) and the loop yields to render/input.
- Input path priority: key input and paste writes must never sit behind output parsing (alacritty's "lease" to the reader is the opposite problem; use a separate fd source with higher priority for writes).
- Large paste: write through the nonblocking queue; chunk to the pty buffer size; respect bracketed paste (not this dossier's scope).
- Synchronized output (DEC 2026) stops rendering but not reading (alacritty counts `sync_bytes_count`).
- XON/XOFF: terminals do not implement software flow control themselves; the line discipline's IXON handles ^S/^Q when the child leaves it on. Do not treat ^S as a terminal-level pause. (Terminal-level "pause output" like xterm's scroll lock is a separate feature.)
- Hang protection: a hostile `yes`-style flood is bounded by frame-budgeted parsing; mark a pane as "flooding" in the UI instead of dropping bytes.

### Terminfo and XTGETTCAP
- Entry name `xterm-apotheosis` (prefix `xterm-` keeps heuristics in vim/tmux/etc. matching `xterm*` working; kitty `xterm-kitty`, ghostty `xterm-ghostty`, foot plain `foot` and `foot-direct`, alacritty `alacritty`, wezterm `wezterm`).
- Variants: `xterm-apotheosis` (256 colour, `colors#256`, `setaf/setab` with 38;5), `xterm-apotheosis-direct` (RGB, `colors#16777216`, `setaf` with 38;2 colon form) per foot's pair.
- Extended (user-defined) capabilities: `Su`, `Smulx`, `Setulc`, `Ss`, `Se`, `Ms`, `Cs`, `Cr`, `BE/BD/PE/PS` (bracketed paste), `XM` (mouse), `kxIN/kxOUT` (focus), `Sync` (2026), `Tc`, `RGB`, `XT`, `fullkbd`. Names confirmed present in `foot.info`. `tic -x` is required so these are kept; a Rust compiler must support the extended section.
- Delivery paths: (a) system DB `/usr/share/terminfo/x/xterm-apotheosis` (no conflict with the ncurses package because the filename is ours: consistent with the updates-must-be-clean rule), (b) `TERMINFO` pointing to a bundled dir, (c) `direct` base64 in env (kitty), (d) none.
- Build step options without C: write the compiled file with our own Rust code (preferred), or vet `terminfokit`, or shell out to `tic` only in CI/packaging (ncurses is a C build dependency of the package, not of our binary, but this breaks the "no C compiled by our build" spirit if PKGBUILD runs `tic`; avoid).
- XTGETTCAP: request `DCS + q <hex name> [; <hex name>...] ST`; reply per name `DCS 1 + r <hex name> [= <hex value>] ST` or `DCS 0 + r ST` for unknown/invalid. Booleans reply without `=`; empty string caps must still reply with valid form (kitty fixed this). Hex encoding is upper-case in wezterm; foot/xterm accept either on input. TN, Co (and `colors`), RGB are queried by tools like vim, helix, notcurses and need to be present even if not in terminfo proper (ghostty adds TN, Co, RGB; wezterm special-cases them).
- Source of truth: one `.ti` file; generate (1) compiled binaries, (2) sorted static table for XTGETTCAP at build time via a `build.rs`/xtask in Rust.

### Cross-cutting (control surface)
- Config keys: see Control surface.
- Actions that touch the child: `signal-child`, `send-text`, `close-pane` with policy, `respawn`, `copy-cwd`, `new-pane-same-cwd`, `kill-foreground` (kitty remote command `signal_child`, file `kitty/rc/signal_child.py`).

## Control surface (config, actions, IPC)

Proposed KDL (all editable in the settings GUI; key names are our own proposal except where upstream names are cited):

```
shell {
    program "/bin/zsh"        // default: $SHELL, then passwd
    args ...
    login #false              // foot login-shell; default no (desktop session already ran login)
    cwd "inherit"             // inherit | home | "/path"; inherit = foreground cwd of the focused pane
}
env {
    set "FOO" "bar"
    remove "KITTY_WINDOW_ID"
    scrub-foreign-terminals #true   // foot-style list
}
terminal {
    term "xterm-apotheosis"   // fallback chain handled at spawn; "auto" probes DB then xterm-256color
    colorterm "truecolor"
    terminfo-mode "path"      // path | direct | none (kitty terminfo_type)
    term-program #true        // set TERM_PROGRAM/_VERSION
}
pty {
    read-batch-kib 64
    coalesce-ms 3             // kitty input_delay / wezterm coalesce
    coalesce-bypass-bytes 1024
    parse-budget-ms 2
    iutf8 #true
}
session {
    utmp-helper "none"        // foot utmp-helper
    exit-behavior "close"     // close | hold | close-on-clean-exit (wezterm exit_behavior)
    hup-grace-ms 2000
    term-grace-ms 3000
}
```

Actions (shared vocabulary): `spawn-pane { cwd "foreground"|"root"|"/p" ; program ... }`, `signal-child "SIGINT"|...` (target: foreground group or session leader), `respawn`, `copy-foreground-cwd`, `copy-foreground-cmdline`, `confirm-close-if-busy`.

IPC (JSON control socket + CLI), read side: `pane.info -> { pid, pgid, fg_pgid, fg_pid, cwd, cwd_source: "osc7"|"proc", exe, cmdline, tty, exit_status?, size {rows, cols, px_w, px_h} }`, `pane.processes -> tree`, event stream `pane.child-exited {pane, status, signal}`, `pane.foreground-changed {pane, pid, exe}` (poll on fg_pgid change at 250 ms while visible or on a SIGCHLD/pidfd tick), `pane.cwd-changed`. Write side: `pane.signal`, `pane.resize-pty` (testing), `pane.set-env` (for next spawn only; cannot change a running process env).

Rules (event -> actions): `on child-exited code!=0 -> hold-pane`, `on foreground-changed exe="ssh" -> set-profile remote` (keep to one vocabulary; no scripting).

## Performance (numbers + method)

Numbers below are **constants read from source, not benchmark results**; method: read the named constant in each repo. We have no measured throughput of our own yet.

| Terminal | Per-wakeup read | Parser buffer | Coalesce/delay | Source |
|---|---|---|---|---|
| foot | up to 10 x 24 KiB; unbounded on HUP | stack 24 KiB | delayed-render lower/upper timers (config `tweak.delayed-render-lower/upper`, values not extracted here: unverified) | `terminal.c: fdm_ptmx` |
| alacritty | until EAGAIN or 65535 B processed while locked | 1 MiB read buffer (`READ_BUFFER_SIZE = 0x100000`) | none, lock-based fairness | `event_loop.rs: pty_read` |
| kitty | until short read | 1 MiB (`BUF_SZ`), macOS gather 64 KiB (`PTY_GATHER_MAX`) | `input_delay` 3 ms, bypass < 1 KiB once per delay | `child-monitor.c`, `vt-parser.c`, `options/definition.py` |
| wezterm | 1 MiB read, 128 KiB parse chunk | 128 KiB | 3 ms (`mux_output_parser_coalesce_delay_ms`) | `mux/src/lib.rs`, `config/src/config.rs` |
| ghostty | 64 KiB x 4 rotating | 64 KiB | spin retry then poll, budget below a frame | `termio/Exec.zig` ReadThread |

Method for our own benchmark (to be run later, not run here): `vtebench` (cloned in the prior-art cache) plus `cat` of a 1 GiB file with the renderer idle, measuring bytes/s through the pty read path with parsing stubbed out, to isolate syscall cost; then with parsing; then latency of a single keystroke echo (key event to pty write to read to first damage) with the coalescing window on and off.

Syscall cost notes: one `read` into a 64 KiB buffer per wakeup is the sweet spot on Linux (pty line discipline buffers in 4 KiB tty_buffer pages; unverified exact figure); larger buffers do not increase returned bytes per read. `FIONREAD` is unnecessary.

## What Apotheosis should take

1. rustix + rustix-openpty for allocation; std `Command::pre_exec` for spawn; no `portable-pty`.
2. The full child setup list: sigmask clear, six (plus SIGPIPE) default dispositions, setsid, TIOCSCTTY, close_range, IUTF8, initial winsize with pixels.
3. pidfd for exit detection; SIGCHLD only as fallback.
4. TIOCSWINSZ pixel fields from the grid area; plus mode 2048 in-band resize.
5. `/proc`-based introspection with OSC 7 as primary cwd source; kitty's TTL cache; wezterm's robust `stat` parse; "highest pid in fg group" heuristic with override.
6. Foreign-terminal env scrub (foot) and launch-token scrub (alacritty); `TERM_PROGRAM` set by us.
7. Foot's table-from-source XTGETTCAP, sorted-table unit test, plus TN/Co/RGB injection (ghostty/wezterm).
8. Terminfo emitted by an in-tree Rust tool; `xterm-apotheosis` with `xterm-256color` fallback probe (alacritty `terminfo_exists` search order: `TERMINFO`, `~/.terminfo`, `TERMINFO_DIRS`, `/etc/terminfo`, `/lib/terminfo`, `/usr/share/terminfo`; both letter dir and hex dir).
9. Bounded, time-budgeted reads with a 3 ms coalescing window bypassed for echo-sized reads; kernel queue as the only backpressure.
10. Hold-on-failure and exit-behavior policy (wezterm).
11. External utmp helper option, default off.
12. Process-group HUP on close with TERM/KILL escalation (foot).

## What it should avoid

- `portable-pty`: Windows/serial baggage, fd closing via directory listing, no pidfd; also a runtime dep tree (anyhow, nix, serial2...) for little gain.
- The `terminfo` crate (WTFPL, outside our licence allowlist); `terminfokit` until vetted (published Aug 2026, 69 downloads, licence includes `X11`, which is not on our allowlist: the `AND X11` term needs a licence ruling before use).
- Shelling out to `tic`, `infocmp` or `infotocap` in the build (kitty, ghostty and foot all do; violates "no C compiled by our build" in spirit).
- libutempter or direct utmp writes.
- `TERMINFO` base64 `direct` mode in v1 (poor tool support).
- `WINDOWID` (X11).
- Fatal exit on `TIOCSWINSZ` failure (alacritty `die!`); log and continue.
- Parsing `/proc/pid/stat` by splitting on spaces (comm can contain spaces).
- Parsing under a shared lock that blocks the render thread for unbounded time (alacritty works around it with leases; prefer message passing of parsed damage or a time budget).
- A global SIGCHLD handler with `waitpid(-1)`: steals exits from other crates' children (library code, helper decoders). With pidfd, each child is waited individually.
- Rust std's SIGPIPE ignore leaking into the child: always reset.
- Treating ^S/^Q specially.
- Scraping `/proc` for every frame; cache and rate-limit.

## Sources

Primary source (study paths in the local clones; functions named):
- alacritty: https://github.com/alacritty/alacritty — `alacritty_terminal/src/tty/unix.rs` (`new`, `from_fd`, `set_controlling_terminal`, `on_resize`, `ToWinsize::to_winsize`, `next_child_event`), `alacritty_terminal/src/tty/mod.rs` (`setup_env`, `terminfo_exists`), `alacritty_terminal/src/event_loop.rs` (`pty_read`, `READ_BUFFER_SIZE`, `MAX_LOCKED_READ`), `alacritty/src/daemon.rs` (`foreground_process_path`), `alacritty_terminal/Cargo.toml` (licence Apache-2.0, deps).
- foot: https://codeberg.org/dnkl/foot — `slave.c` (`slave_spawn`, env scrub block, login shell argv0), `terminal.c` (`fdm_ptmx`, `fdm_ptmx_out`, `tiocswinsz`, `term_send_size_notification`, shutdown/SIGHUP/SIGTERM/SIGKILL path), `reaper.c`, `dcs.c` (`lookup_capability`, `xtgettcap_reply`, `xtgettcap_unhook`), `scripts/generate-builtin-terminfo.py`, `foot.info`, `meson_options.txt` (utmp-backend), `doc/foot.ini.5.scd` (`term`, `login-shell`, `utmp-helper`), `doc/foot-ctlseqs.7.scd` (mode 2048, XTGETTCAP).
- kitty: https://github.com/kovidgoyal/kitty — `kitty/child.py` (`Child.fork`, `get_final_env`, `foreground_processes`, `get_pid_for_cwd`, `process_group_map`, `CachedProcessData`, `root_pid`), `kitty/child.c` (`spawn`), `kitty/child-monitor.c` (`read_bytes`, `handle_signal`, input wake logic, `PTY_GATHER_MAX`), `kitty/vt-parser.c` (`BUF_SZ`), `kitty/options/definition.py` (`input_delay`, `terminfo_type`), `kitty/terminfo.py`, `build-terminfo` (uses external `tic`), `docs/changelog.rst` (XTGETTCAP fix). Note kitty is GPL-3.0: studied only, no code reused.
- wezterm: https://github.com/wez/wezterm — `pty/src/unix.rs` (`openpty`, `close_random_fds`, `spawn_command`, `resize`, `process_group_leader`), `pty/src/cmdbuilder.rs` (`as_command`, `get_shell`, argv0 dash), `procinfo/src/linux.rs` (`current_working_dir`, `executable_path`, `with_root_pid`), `mux/src/lib.rs` (`read_from_pane_pty`, `parse_buffered_data`, `BUFSIZE`), `config/src/config.rs` (`mux_output_parser_*`, TERM/COLORTERM/TERM_PROGRAM block ~line 1617), `term/src/terminalstate/mod.rs` (`xt_get_tcap`), `docs/config/lua/config/term.md`.
- ghostty: https://github.com/ghostty-org/ghostty — `src/termio/Exec.zig` (TERM/TERMINFO selection, ReadThread pipeline, termios poll), `src/pty.zig`, `src/build/GhosttyResources.zig`, `src/terminfo/Source.zig` (`xtgettcapMap`).

Crate metadata (crates.io API, 2026-11-12): rustix 1.1.6 (Apache-2.0 WITH LLVM-exception OR Apache-2.0 OR MIT); rustix-openpty 0.2.0 (same licence, published 2025-03-06, https://github.com/sunfishcode/rustix-openpty); nix 0.31.3 (MIT); portable-pty 0.9.0 (MIT); pty-process 0.5.4 (MIT, https://git.tozt.net/pty-process); signal-hook 0.4.6 (MIT OR Apache-2.0); filedescriptor 0.8.3 (MIT); terminfo 0.9.0 (WTFPL); termini 1.0.0 (MIT); terminfokit 0.1.0 ((MIT OR Apache-2.0) AND X11); polling 3.11.0 (Apache-2.0 OR MIT).

Specs and docs (not fetched this session, cited from knowledge: unverified verbatim):
- XTGETTCAP: https://invisible-island.net/xterm/ctlseqs/ctlseqs.html (DCS + q Pt ST; reply DCS 1 + r Pt ST / DCS 0 + r ST).
- terminfo file format: `term(5)`, https://man7.org/linux/man-pages/man5/term.5.html; `terminfo(5)`, https://man7.org/linux/man-pages/man5/terminfo.5.html.
- pty and tty: `pty(7)`, `tty_ioctl(2)` (TIOCSWINSZ, TIOCSCTTY, TIOCGPGRP), `credentials(7)`, `signal(7)`, `pidfd_open(2)`, `proc(5)`: all at https://man7.org/linux/man-pages/.
- Matrix gap: no existing dossier in this directory covers these rows (checked `FEATURE-MATRIX.md` and per-terminal dossiers by grep for terminfo/XTGETTCAP; only foot, xterm, kitty, wezterm, rio, tmux-zellij, contour, topic-escape-registry mention them in passing).
