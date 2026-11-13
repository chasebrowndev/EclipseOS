# Topic dossier: built-in multiplexing and remote

Research date 2026-11-12. Subjects: wezterm mux protocol and domains, kitty ssh kitten, tmux control
mode, zellij session model and serialization, session persistence and its security, Rust SSH crates
versus shelling out to system ssh. Source trees inspected (shallow clones, never built):
`~/.cache/apotheosis-prior-art/{wezterm,kitty,tmux,zellij,russh}`. Prose only; no upstream source is
reproduced. Escape sequences, message names and config key names are quoted as interface facts.
Complements `tmux-zellij.md` (general mux model); this file concentrates on the remote, protocol,
persistence and SSH-stack questions.

## Summary

- Three distinct designs exist for "terminal with built-in mux", and they differ in where the screen
  model lives:
  1. wezterm: a headless mux server owns ptys AND the full terminal model (scrollback, cursor,
     images). The GUI is a client that keeps a local mirror of each pane and pulls deltas by sequence
     number. Panes are grouped into "domains" (local, unix, ssh, tls, exec, wsl, serial, tmux-cc).
  2. tmux: server owns the model and renders per-client; control mode (`-C`/`-CC`) turns the server
     into a line-oriented machine API with typed notifications and flow control. A GUI terminal (iTerm2,
     wezterm's tmux domain) maps tmux windows/panes to native tabs/splits.
  3. zellij: server owns the model and sends clients pre-rendered frames (ANSI, or a structured binary
     frame in 0.46); layouts serialize to KDL and sessions are "resurrected" from KDL.
- kitty takes the opposite approach for remote: no mux at all. The `ssh` kitten wraps system OpenSSH,
  pushes terminfo, shell integration and files to the remote over the tty itself (a one-time-password
  gated escape-sequence data request), and shares connections via OpenSSH ControlMaster.
- No mainstream terminal has real persistence of live processes across a server death. Resurrection
  (zellij) restarts commands from a saved layout (optionally with viewport text); live-process
  persistence needs a surviving server (tmux, wezterm-mux-server, zellij server).
- Crates: `russh` 0.65.1 (Apache-2.0, 10.4M downloads, released 2026-11-03) is the only maintained
  pure-Rust async SSH implementation, but it hard-requires `ring` or `aws-lc-rs` for AEAD ciphers
  (compile_error otherwise). Both contain C/asm compiled by build.rs, which violates Apotheosis's
  "no C compiled by our build" rule. Shelling out to system `ssh` (a runtime dependency, not compiled
  by us) satisfies the rule and inherits ssh_config, agents, ProxyJump, FIDO keys, ControlMaster and
  known_hosts for free. Recommendation: system ssh as the default and only v1 transport.
- Recommendation for Apotheosis: GUI-first with a separable session model (`apotheosis-mux` as a
  library plus an optional headless server over a Unix socket); wezterm-style domains but with a
  smaller wire protocol; tmux control mode as a client domain (import, not our own protocol);
  kitty-ssh-style bootstrap over system ssh; zellij-style KDL layout save/restore; strict
  security defaults (0700 socket dir, peer-credential check, no clipboard/remote-control reachability
  from remote panes without a prompt).

## Architecture (how it is built)

### wezterm multiplexer

- Crates: `mux` (domains, panes, tabs, windows, the Mux singleton), `codec` (PDU framing),
  `wezterm-client` (client domain and remote-pane mirror), `wezterm-mux-server-impl` (session
  handler, dispatch, PKI), `wezterm-mux-server` (daemonize, TLS acceptor), `wezterm-ssh`
  (ssh abstraction over libssh-rs or ssh2, both C libraries; libssh-rs is the default backend).
  Sources: `codec/src/lib.rs`, `mux/src/domain.rs`, `wezterm-client/src/pane/renderable.rs`,
  `config/src/{unix,ssh,tls}.rs`.
- Domain concept (`mux/src/domain.rs`): a domain is a set of windows/tabs plus the means to spawn
  panes; trait operations are spawn, split_pane, attach, detach, state, detachable. Domain kinds:
  LocalDomain (pty on this machine), ClientDomain (a remote mux reached via unix socket, ssh stdio
  proxy, or TLS), ExecDomain (wrap every spawn in a user command; used for docker/WSL/podman-like
  wrapping), and the tmux-control-mode domain (`mux/src/tmux.rs`, `tmux_commands.rs`, `tmux_pty.rs`)
  where each tmux pane is mirrored as a wezterm pane via a synthetic pty fed by `%output`.
  Config also defines SshDomain (two flavours auto-generated per `~/.ssh/config` host: plain
  `SSH:host` and multiplexed `SSHMUX:host`) and serial ports.
- Wire protocol (`codec/src/lib.rs`): each message is a PDU framed as unsigned LEB128 length, LEB128
  serial, LEB128 ident, then payload. The top bit of the length marks zstd compression; compression
  is attempted only when the serialized payload exceeds 32 bytes and kept only if smaller.
  Payloads are serde serialized with a varint binary format (`varbincode`, MIT, last release 2019).
  A `CODEC_VERSION` constant (46 at inspection) is exchanged via GetCodecVersion; mismatches are
  rejected, so client and server must be the same wezterm release line. About 60 PDU kinds
  (ListPanes, SpawnV2, SplitPane, WriteToPane, SendKeyDown, SendMouseEvent, SendPaste, Resize,
  GetLines, GetPaneRenderChanges, SearchScrollbackRequest, SetClipboard, GetImageCell, PaneRemoved,
  TabResized, SetWindowWorkspace, GetClientList, AdjustPaneSize, ...). Server-to-client unsolicited
  PDUs (PaneRemoved, PaneFocused, TabResized, TabAddedToWindow, title changes) keep structure in sync.
- Rendering sync is pull-based with delta by sequence number. Every line in the server model carries
  the sequence number of its last change. The client polls GetPaneRenderChanges; the response lists
  dirty line ranges, cursor, dimensions, title, cwd, alt-screen and mouse-grab state, a current
  seqno, and "bonus lines" the server expects the client to want immediately (viewport lines that
  changed). The client then fetches any other stale ranges with GetLines (`hydrate_lines`). Poll
  interval starts at 20 ms and doubles on each idle poll up to 30 s, reset on any input or response.
  Technique: stale-range tracking with a rangeset of stable row indices; lines absent locally render
  blank until fetched.
- Latency masking: client-side predictive local echo (`should_predict`, `apply_prediction`), enabled
  only when measured input round-trip exceeds `local_echo_threshold_ms` (ssh domains default on,
  unix domains off); skips lines containing "sword" as a password-prompt heuristic. Lag indicator
  (`is_tardy`, `overlay_lag_indicator`): shown when sends outpace receives for over 3 s.
- Reconnect: a ClientDomain whose transport dies marks panes tardy, retries with the same
  credentials; TLS domains reconnect with the certificate obtained at bootstrap; unix-socket domains
  reconnect to the same server and re-attach by pane id. Session survival depends entirely on the
  server process staying alive; there is no on-disk state.
- Bootstrap: unix domain auto-starts `wezterm-mux-server --daemonize` unless
  `no_serve_automatically`; `serve_command` and `proxy_command` override; the ssh path runs a
  `wezterm cli proxy` on the remote and pipes the PDU stream through ssh stdio (SshDomain with
  multiplexing = WezTerm). TLS domain `bootstrap_via_ssh` logs in over ssh, starts the server, and
  receives a freshly minted client certificate (`wezterm-mux-server-impl/src/pki.rs`).
- CLI and workspaces: `wezterm cli` subcommands (list, list-clients, spawn, split-pane, send-text,
  get-text, activate-pane/tab/direction, adjust-pane-size, zoom-pane, kill-pane, move-pane-to-new-tab,
  set-tab-title, set-window-title, rename-workspace) talk to the same mux over the unix socket.
  Workspaces are a named grouping of windows inside a mux; switching workspace hides other windows.

### kitty ssh kitten

- Written in Go (`kittens/ssh/main.go`, config in `config.go`, option schema in `main.py`) with
  remote-side POSIX sh bootstrap (`shell-integration/ssh/bootstrap.sh`, Python alternative
  `bootstrap.py`). It is a wrapper around the system `ssh` binary, invocation identical to ssh.
- Flow: parse destination, match `hostname` blocks in `ssh.conf` (globs, optional user@) to get
  per-host options; build a tar (gzip best compression) containing the kitty terminfo (compiled
  entries under `.terminfo` for the `xterm-kitty` name and the `x/` hashed dir layout, plus text
  source for hosts without a compiled-entry reader), shell-integration scripts, user `copy` files,
  and an env-setting script; base64 it into JSON together with a random one-time password.
  The bootstrap script runs on the remote via the ssh command; it asks the local terminal for the
  data with a DCS escape (`ESC P @kitty-ssh|...`) quoting the request id, a password-file name in
  local shared memory, and the password; kitty verifies and replies on the tty with the tarball.
  With OpenSSH 8.4+ the data can be written before the remote script starts reading, avoiding a
  round trip. The remote extracts to a temp dir (`~/.kitty-ssh-kitten-untar-*`), compiles terminfo with
  `tic` if present, sets `TERMINFO`, execs the login shell with the integration env, and cleans up.
- Connection sharing: injects OpenSSH options ControlMaster=auto, a ControlPath under the runtime
  dir keyed by kitty pid and ssh's `%C` hash, ControlPersist=yes, ServerAliveInterval=60,
  ServerAliveCountMax=5, TCPKeepAlive=no. Works around the unix socket path length limit (about 104
  chars) by symlinking a shorter runtime dir when the real one exceeds 35 chars. A stale-master check
  (`master_responds`) asks the master to connect to a closed port, which needs a server round trip but
  runs nothing remote; five second timeout, then a new master is made. `close_shared_ssh_connections`
  action tears masters down; kitty cleans them at exit.
- Other behaviours: native askpass (kitty draws the password/host-key prompt itself, so ssh does not
  need the tty before the connection exists; optional automated `password` and `totp_secret` stored
  in plain text, documented as insecure); `remote_kitty` pushes a tiny script that downloads the
  `kitten` binary on first use; per-host `color_scheme` plus dark/light/no-preference variants sent
  as a `color-scheme` DCS and re-sent when the OS appearance changes; `delegate` hands a host to
  another command; `forward_remote_control` forwards the remote-control socket and carries an explicit
  warning that any process on the remote gains full local control; `env` instructions with copy-from-
  local and recursive expansion; `cwd`, `login_shell`.
- Documented limitations: input typed before the prompt can be lost; BSD default shells may need
  Python; multiplexers in between break the DCS channel and dark/light switching.

### tmux control mode

- Entry: `tmux -C` (line mode, echo on, for testing) or `-CC` (no canonical mode, emits DCS
  `ESC P 1000 p` at start and `%exit` plus ST at end so a hosting terminal can detect it; empty line
  detaches). Source: `control.c`, `control-notify.c`, `cmd-refresh-client.c`.
- Protocol: client sends ordinary tmux commands, one per line. Every command result is wrapped in
  `%begin <epoch> <number> <flags>` and `%end` or `%error` with the same three fields; asynchronous
  notifications never appear inside a guard block (deferred until it closes). Pane output arrives as
  `%output %pane data` with bytes below 0x20 and backslash octal-escaped (`\134`). Notifications:
  %window-add/close/renamed, %window-pane-changed, %layout-change (carries the layout string and
  visible layout), %session-changed/renamed/window-changed, %client-session-changed/detached,
  %pane-mode-changed, %paste-buffer-changed/deleted, %sessions-changed, %subscription-changed,
  and unlinked-window variants for windows outside the attached session.
- Flow control: `refresh-client -f pause-after=N` enables per-pane buffering; if a client lags more
  than N seconds on a pane, tmux emits `%pause %pane` and stops delivering; the client resumes with
  `refresh-client -A '%id:continue'`, receiving `%continue`. With flow control on, output uses
  `%extended-output %pane age : data` including lag in ms. In `control.c` the per-pane write budget
  is gated by low/high watermarks (512 / 8192 bytes), a 32 byte minimum write, a 300000 ms maximum
  age, and a 64 MiB cap on a single command reply.
- Subscriptions: `refresh-client -B name:target:format` makes tmux push `%subscription-changed`
  when a format value changes, at most once per second; target kinds are session, one pane, all
  panes, one window, all windows. This is how a native client tracks cwd, running command or custom
  formats without polling.
- Size: control clients do not influence window size unless they send `refresh-client -C`; then
  they count as a normal client. Optional new layout format carries floating panes.
- Consumers: iTerm2 (`tmux -CC` integration: tmux windows become native tabs/windows), wezterm
  (experimental tmux domain, TODO in config enum). Control mode makes tmux a persistence backend
  while the terminal keeps native rendering, scrollback search and selection.

### zellij session model

- Server/client: server is a multi-threaded Rust process (screen, pty, pty_writer, plugin,
  background_jobs threads); clients connect over a per-session Unix socket in a socket dir created
  with mode 0700 (`zellij-utils/src/consts.rs`). IPC messages (`zellij-utils/src/ipc.rs`):
  ClientToServerMsg variants include AttachClient, AttachWatcherClient (read-only observer),
  DetachSession, Action (the same Action enum used by keybinds and CLI), Key, TerminalResize,
  color and pixel dimension reports. Protobuf contracts exist for plugin API, nested-session and
  client-server frames.
- 0.46 additions relevant here: a structured render frame (`structured_render`) with cells, link
  table, cursor state and pane geometry records so a non-ANSI client (native window, web client)
  can render directly; `pane_mirror` (stream one pane to a client); `session_link` (a session
  forwarding to a peer with explicit 700 ms reply and 2 s action timeouts and three detach attempts);
  `remote_session_tokens` (token store in SQLite for remote/web attachment); `nested_session`
  (input-mode and keybinding reporting so a nested zellij inside zellij can hand keys to the inner
  one); `web_authentication_tokens`; and a native terminal window crate.
- Persistence: a background job serializes every session at `serialization_interval` (default
  60000 ms) into the cache dir as a KDL layout (`session_serialization.rs`, per-pane manifest holds
  geometry, run command and args, cwd, title, borderless flag, focus, optional `pane_contents`,
  default fg/bg, border style, attention flags; tab level has floating panes and focus). Options:
  `session_serialization` (default true), `serialize_pane_viewport` (default false),
  `scrollback_lines_to_serialize` (0 means whole scrollback, capped to scrollback size),
  `disable_session_metadata`, `post_command_discovery_hook` (a shell command used to rewrite the
  discovered command line, for example to strip secrets or resolve wrapper scripts). Resurrection
  re-creates the layout; command panes start "held" (`hold_on_start`, `start_suspended`) showing a
  run-prompt so nothing re-executes unseen. Serialization is only done when the session is "dirty"
  and has non-default-shell content (`is_dirty`). Resurrectable sessions are listed with age.
- Layouts: KDL with tabs, nested splits, percent/fixed sizes, `swap_tiled_layout` and
  `swap_floating_layout` blocks that re-tile by pane count, stacked panes, templates. Same format is
  used for hand-written layouts and for serialized sessions.

### System ssh versus Rust ssh crates

- `russh` 0.65.1 (repo moved to warp-tech/russh; Apache-2.0; edition 2024, MSRV 1.89). Async tokio
  client and server; channels, PTY, port/unix forwarding, ssh-agent client, ssh_config parsing in
  `russh-config` (ProxyCommand and ProxyJump fields parsed, ControlMaster not implemented),
  `known_hosts.rs`, certificates, strict kex (Terrapin mitigation), `mlkem768x25519-sha256`
  hybrid, curve25519, DH-GEX, ciphers aes-ctr/cbc/gcm and chacha20-poly1305@openssh, legacy
  algorithms opt-in. Pure-Rust crypto dependencies from RustCrypto and dalek families (aes, ghash,
  polyval, ed25519-dalek, curve25519-dalek, p256/384/521, rsa, ml-kem, sha2, hmac, pbkdf2, scrypt) -
  but the AEAD ciphers are routed through `aws-lc-rs` (default, brings `aws-lc-sys`, C compiled via
  cc/cmake) or `ring` (C and assembly, licence Apache-2.0 AND ISC); one of them is mandatory by a
  compile_error guard in `lib.rs`. `ssh-key` is pinned to a 0.7.0 release candidate, while crates.io's
  max stable is 0.6.7: a pre-release in the dependency tree. Licences of the whole set fit the
  allow-list (aws-lc-sys: ISC, Apache-2.0, MIT, BSD-3-Clause per crates.io metadata).
- Other Rust options: `ssh2` (MIT/Apache, wraps libssh2 C), `libssh-rs` (MIT, wraps libssh C; what
  wezterm uses), `openssh` crate 0.11.6 (MIT/Apache; a typed wrapper around the system ssh binary and
  ControlMaster, no C), `thrussh` (old fork lineage), `async-ssh2-tokio` (non-standard licence).
- Shelling out: gains every OpenSSH feature (ssh_config Include/Match, ProxyJump, ControlMaster,
  agent and agent forwarding, FIDO2/sk keys, GSSAPI, certificates, known_hosts policy, askpass),
  system security updates, and no crypto in our attack surface. Costs: process management, parsing
  stderr for errors, harder in-GUI prompting (solved with SSH_ASKPASS, as kitty does), no pty
  introspection beyond the child. Windows is out of scope (Wayland-only).

## Feature inventory (grouped by category)

### Domains and transports
- Local domain; unix-socket remote/local mux domain with auto-start; ssh stdio proxy domain;
  TLS-over-TCP domain with ssh bootstrap and per-client certificates; exec domain (wrap spawns);
  serial domain; wsl domain; tmux control-mode domain.
- Default auto-generated domains per ssh_config host (plain and multiplexed variants).
- `connect_automatically`, `no_serve_automatically`, `serve_command`, `proxy_command`,
  `remote_wezterm_path`, `override_proxy_command`, `assume_shell` (Posix vs unknown, to know how to
  `cd` and `exec`), `default_prog` per domain, `ssh_option` map.

### Session and layout persistence
- Detach/attach (tmux, zellij, wezterm-mux-server); layout save/restore as KDL (zellij);
  periodic autosave at 60 s; viewport/scrollback optional serialization; command panes held on
  resurrection; resurrectable-session listing; tmux layout string with checksum; named workspaces.

### Remote bootstrap and environment
- Terminfo push (kitty), shell integration push, file copy with glob/exclude/symlink strategy, env
  injection, cwd, login shell, per-host colour scheme with OS dark/light switching, `remote_kitty`
  helper binary, native askpass, TOTP automation, delegate-to-plain-ssh, connection sharing via
  ControlMaster with stale-master probe.

### Remote UX
- Predictive local echo gated by measured RTT; lag indicator; reconnect with same credentials;
  poll backoff; bonus-line prefetch; client list (`list-clients`); workspace migration between
  clients; per-pane metadata `since_last_response_ms`.

### Programmatic control
- wezterm `cli` over the mux socket; tmux control-mode text protocol with guards, notifications,
  subscriptions; zellij Action enum over IPC, `zellij action`, `zellij pipe`, `zellij subscribe`;
  kitty `@` remote control (with the forward warning).

### Observers and collaboration
- zellij watcher client (read-only attach), multi-client attach with independent focus, web client
  with tokens, `list-clients`, nested-session keybind handoff.

## Control surface (config, actions, IPC)

- wezterm: Lua config tables `unix_domains`, `ssh_domains`, `tls_servers`, `tls_clients`,
  `exec_domains`, `wsl_domains`, `default_domain`, `default_gui_startup_args`; key assignments
  `AttachDomain`, `DetachDomain`, `SpawnTab` with domain scope, `SwitchToWorkspace`,
  `SwitchWorkspaceRelative`; CLI `wezterm connect`, `wezterm ssh`, `wezterm cli`, `wezterm-mux-server`.
- kitty: `ssh.conf` block grammar (`hostname` + options), kitten `ssh`, action
  `close_shared_ssh_connections`, DCS channels `@kitty-ssh`, `@kitty-echo`, `@kitty-print`,
  `@kitty-color-scheme`.
- tmux: `-C/-CC`, `refresh-client` flags `-f pause-after`, `-A`, `-B`, `-C`, `new-session -A`,
  `attach -d/-x`, hooks (`set-hook`), `wait-for`, format language (`#{...}`).
- zellij: options listed above, `zellij attach|list-sessions|delete-session|kill-session`,
  `zellij action <Action>`, `zellij --layout file.kdl`, `zellij run`.
- Apotheosis mapping: every one of these maps onto the single Action vocabulary plus domain/profile
  KDL; the control socket should expose (a) Action dispatch, (b) typed event subscription (tmux
  `%`-notification equivalent, delivered as JSON lines), (c) a pane stream/mirror call for
  observers, (d) session list/attach/detach/save/restore. All settings must also have GUI rows.

## Performance (numbers + method)

- wezterm: poll interval 20 ms base, doubled per idle poll to 30 s cap; compression only above
  32 bytes, zstd default level (3). Source constants in `wezterm-client/src/pane/renderable.rs` and
  `codec/src/lib.rs` (static reading; no benchmark published). Local echo threshold default for ssh
  domains is a small number of milliseconds in config (`default_local_echo_threshold_ms`; exact
  value not extracted: unverified).
- tmux control mode: per-pane output watermarks 512/8192 bytes, minimum write 32, maximum age 300 s,
  reply cap 64 MiB; subscription push rate limit 1 per second (documented).
- zellij: serialization interval default 60 s; session-link timeouts 700 ms/2 s; 0.46 changelog notes
  memory-usage optimizations and kitty image compression to the host terminal, no numbers.
- kitty ssh: stale-master probe timeout 5 s; ServerAliveInterval 60 s x 5; with OpenSSH >= 8.4 the
  bootstrap avoids one round trip; shared-connection startup latency reduced but not quantified in
  docs (unverified).
- russh: `benches/ciphers` exists in the repo; no published throughput figure verified.
- Method note: all numbers above are read from source constants or official docs, not measured.
  Apotheosis should benchmark its own mux protocol (frame delta bytes per keystroke, reattach time to
  first full paint for 10k-line scrollback, ssh bootstrap time) in its own harness.

## What Apotheosis should take

1. Session model separable from windows: panes/tabs/splits/floats are plain data owned by a
   session object; a window is just a view. Needed anyway for layouts, rules, profiles and control socket.
2. Optional headless mux server (`apotheosis-mux`) over a Unix socket in a 0700 directory with
   SO_PEERCRED uid check; auto-start on demand like wezterm; detach/attach; multi-client with
   independent focus and read-only watcher role (zellij).
3. Delta sync keyed by per-line sequence numbers (wezterm) with stable row indices, bonus-line
   prefetch of the viewport, and adaptive poll or better push with a subscribe model; add zstd-like
   compression only above a small threshold. Use a pure-Rust codec (`ruzstd` MIT decodes and now encodes
   at low levels; `zstd` is C) or skip compression on local sockets.
4. Domain abstraction: local, mux-unix, ssh, exec (container wrap), tmux-control; each with
   `connect_automatically`, spawn defaults, and per-domain profile, in KDL and GUI.
5. System `ssh` as transport: spawn with a private ControlPath (kitty recipe: pid-keyed path, %C hash,
   short runtime dir symlink for path length), ControlPersist, ServerAlive options, stale-master probe,
   SSH_ASKPASS pointing at an Apotheosis helper so prompts become glass dialogs with host-key
   confirmation and no tty use.
6. SSH bootstrap push done the kitty way but narrower: terminfo (or advertise `xterm-256color`
   fallback), shell-integration scripts, env, cwd, optional file copy; data pulled by the remote via a
   one-time-password-gated DCS request over the tty so nothing is written until the remote asks;
   cleanup temp dir on exit.
7. tmux control mode client domain (iTerm2-style): `%output` octal decoding, guard-matched command
   queue, `pause-after` flow control, `-B` subscriptions for cwd/command, layout-string parsing with
   checksum. Gives Apotheosis users persistence on any host with tmux with native tabs/search.
8. Zellij-style resurrection: periodic (default 60 s, configurable) KDL layout snapshot of tabs,
   splits, floats, cwd, commands, titles, focus; commands start held with a visible run prompt;
   optional viewport/scrollback text restore; secrets hook (`post_command_discovery_hook` analogue)
   and an exclude list so passwords on command lines are never stored.
9. Predictive local echo for remote domains gated by measured RTT with the password-prompt guard
   (needs a better heuristic than a substring: use terminal echo-off detection via the pty's termios
   on local domains; for remote use OSC 133 prompt state and the ECHO flag only when known).
10. Lag indicator and tardy state in the glass UI; reconnect banner; per-pane `since_last_response`.
11. Event stream and subscriptions on the JSON control socket modelled on tmux notifications with
    guard-correlated replies and rate-limited format subscriptions.
12. Domain-scoped actions: Spawn/Split take an optional domain; `AttachDomain`, `DetachDomain`,
    `SaveSession`, `RestoreSession`, `SwitchWorkspace`, `CloseSharedSshConnections`.
13. Per-host remote colour scheme with light/dark variants sent when OS appearance changes (kitty).
14. Version handshake with an explicit protocol version, refuse-on-mismatch with a clear UI message,
    and a compat window for N-1 so a local update does not strand a long-lived server.

## What it should avoid

- Compiling `russh` with `ring` or `aws-lc-rs`: C/asm in our build breaks the fixed rule. Do not adopt
  `ssh2` or `libssh-rs` either (C). If an in-process SSH client is ever wanted, it needs an
  upstream or forked AEAD path on RustCrypto (`aes-gcm`, `chacha20poly1305`); that is an engineering
  project, not a v1 item, and its security maintenance falls on us.
- Lua or other embedded scripting for domains and events (wezterm); use the Action vocabulary.
- WASM plugins (zellij) for status or pickers.
- A TLS-over-TCP domain in v1: it needs a PKI, certificate issuance, revocation and a listening
  daemon, a large attack surface; wezterm's design (CN must equal unix user, bootstrap over ssh)
  is workable but OpenSSL-based. If ever needed, use `rustls` (Apache-2.0/ISC/MIT; its default
  aws-lc-rs provider is C, ring is C; a pure-Rust provider would be required) or tunnel the
  Unix-socket protocol through `ssh -L`/`ssh -W` instead, which needs no new crypto.
- A client/server version lock with no grace (wezterm's exact CODEC_VERSION match).
- Persisting passwords, TOTP secrets or process environment in saved sessions or in plain config
  (kitty documents `password`/`totp_secret` as plain text; avoid entirely, or store via Secret
  Service, never in KDL).
- Silent re-execution of commands on restore; always hold and ask.
- Forwarding the control socket or clipboard-write/OSC 52-read capability to remote panes by default
  (kitty's `forward_remote_control` warning is the model: off, per-host, with a visible indicator).
- Pull polling at 20 ms for idle panes; prefer server push with a wake-up and keep the backoff only
  as a fallback.
- Relying on a single heuristic substring ("sword") for password detection.
- Using tmux or zellij as hidden implementation detail; users should see which domain a pane is in.

## Persistence and security implications

- Long-lived server = long-lived secrets: scrollback, environment, ssh agent sockets, and decrypted
  content stay in memory after the window closes. Mitigations: per-session idle timeout option,
  explicit "lock session" and "wipe scrollback on detach" actions, mlock-free but zeroize known
  password-role buffers, no scrollback serialization by default (zellij default false for viewport).
- Socket access control: directory 0700 under `$XDG_RUNTIME_DIR`, socket mode 0600, verify peer uid
  with SO_PEERCRED, refuse other-owned sockets (wezterm `skip_permissions_check` exists and is an
  explicit opt-out; tmux checks directory ownership/mode at startup).
- Any process running as the same uid can drive the mux socket, including `send-text` into panes.
  That is equivalent to tmux today; document it and, on Abyss, allow per-capability gating via the
  compositor's policy (no ambient authority invariant on the host OS) rather than inventing our own.
- Remote-originated escape sequences: a remote pane can emit OSC 52, kitty graphics, DCS channels
  (`@kitty-*`) and, with a mux, sequences that address the outer terminal. Control channels like the
  ssh bootstrap request must be honoured only for a request id the terminal itself armed (kitty ties
  it to a one-time password in local shared memory), never for arbitrary DCS.
- Session files on disk: KDL snapshots contain cwd, command lines (may contain tokens) and, if
  enabled, screen text. Write 0600 in `$XDG_STATE_HOME`, offer redaction hook, mark panes opened
  with `password`-role input as excluded (project invariant: password values never stored).
- tmux control-mode trust: a hostile remote tmux can send arbitrary `%output`; treat as normal pane
  output, and never let `%`-lines outside a controlled DCS context be interpreted.
- Agent forwarding and ControlMaster sharing: connection sharing means any local process that can
  read the ControlPath socket can open channels on the master; keep it in the 0700 runtime dir, one
  master per host per Apotheosis instance, torn down on exit (kitty behaviour).
- Version skew attack surface: parsing a hostile or stale mux peer's PDUs; use length caps (tmux has
  64 MiB reply cap), bounded varints, and fuzz the codec.

## Sources

Source findings (upstream path and function or constant):
- wezterm `codec/src/lib.rs` (pdu table, `encode_raw_as_vec`, `serialize`, `COMPRESS_THRESH`,
  `COMPRESSED_MASK`, `CODEC_VERSION`, `GetPaneRenderChangesResponse`, `GetLines`),
  `wezterm-client/src/pane/renderable.rs` (`poll`, `hydrate_lines`, `is_tardy`, `should_predict`,
  `apply_prediction`, `BASE_POLL_INTERVAL`, `MAX_POLL_INTERVAL`), `config/src/{unix,ssh,tls}.rs`,
  `wezterm-mux-server/src/ossl.rs` (`verify_peer_cert`), `wezterm-mux-server-impl/src/pki.rs`,
  `mux/src/{domain,tmux,tmux_commands,tmux_pty}.rs`, `wezterm-ssh/Cargo.toml` (libssh-rs, ssh2).
- wezterm docs: https://wezterm.org/multiplexing.html (local copy `docs/multiplexing.md`).
- kitty `kittens/ssh/main.go` (`connection_sharing_args`, `master_responds`, `make_tarfile`,
  `bootstrap_script`), `kittens/ssh/main.py` (options), `shell-integration/ssh/bootstrap.sh`
  (`dcs_to_kitty`), docs https://sw.kovidgoyal.net/kitty/kittens/ssh/.
- tmux `control.c` (watermarks, pause logic), `control-notify.c`, wiki
  https://github.com/tmux/tmux/wiki/Control-Mode.
- zellij `zellij-utils/src/{ipc,consts,session_serialization,session_link,nested_session,pane_mirror,remote_session_tokens}.rs`,
  `zellij-utils/src/input/options.rs`, `zellij-server/src/{background_jobs,session_layout_metadata}.rs`,
  `CHANGELOG.md` 0.46.0 (2026-11-11).
- russh `russh/Cargo.toml`, `russh/src/lib.rs` (compile_error), `russh/src/cipher/{mod,gcm,chacha20poly1305}.rs`,
  `russh/README.md` (algorithm list, backends), `russh-config/src/lib.rs`.
- crates.io API metadata (licence, downloads, versions) fetched 2026-11-12 for russh, ring, aws-lc-rs,
  aws-lc-sys, ssh-key, openssh, ruzstd, zstd, rustls, ssh2, libssh-rs, thrussh, async-ssh2-tokio, varbincode.
- Not verified: mosh behaviour, exact wezterm default local-echo threshold, any measured throughput
  or latency numbers for the subject tools, russh pure-Rust AEAD feasibility (inferred from source
  layout only: ring/aws-lc used solely in the cipher module).
