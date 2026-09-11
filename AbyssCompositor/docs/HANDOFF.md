# Session handoff

## State as of 2026-09-11 (later) — B3 and B6 are done, both run

Branch `ci-attribution-display-name`. **Working tree clean.** `HEAD` is
`d40aaee`; all five gate commands are green at it. Still nothing pushed.

The previous entry (below) said B3 and B6 were scaffolded, paused and
deliberately outside the workspace `members` list. They are now finished, in
the workspace, committed, and each has been run nested under Hyprland.

| Commit | What |
|---|---|
| `460809d` | `docs/STYLE.md` — the frontend style spec now lives in the repo instead of the owner's Downloads, and `eclipse-ui/src/tokens.rs` points at it |
| `3a6f7a5` | `feat(eclipse-bar)` — the bar, plus the `Cargo.lock` winit pin |
| `1a5d506` | `feat(eclipse-policy-viewer)` — the read-only policy window |
| `d40aaee` | `default-members = ["crates/abyss"]` |

### Things a later session will otherwise rediscover the hard way

**`Cargo.lock` carries a deliberate downgrade — do not "fix" it.**
`iced_layershell 0.19.1` depends on `iced_exdevtools 0.19.1` unconditionally,
and that crate's `keymap.rs` fails `E0004` against `winit-core 0.31.0-beta.3`'s
non-exhaustive `NativeKeyCode`. `winit-common` requires `winit-core` exactly, so
pinning `winit-core` alone is refused; the working incantation is
`cargo update -p winit-common --precise 0.31.0-beta.2`. A `cargo update` that
floats those two back up breaks the whole workspace build.

**`default-members` exists for a reason.** Five binaries in the workspace made
the bare `cargo run -- --backend winit` that `CLAUDE.md` and `docs/BUILDING.md`
both document fail with "could not determine which binary to run".

**`publish = false` on both new crates is load-bearing** — the same reason as
the other `eclipse-*` crates: `cargo deny check bans` reports
`error[wildcard]: found 2 wildcard dependencies` on a publishable crate with
path dependencies.

**The policy viewer has no `tests/coverage.rs` on purpose.** The settings app's
ratchet tests its generated control table against `abyss::config::schema`; the
viewer has no control table, and linking `abyss` would pull Smithay, DRM and GBM
into a small read-only GUI. The equivalent drift check is inline in
`read.rs::the_policy_owned_set_is_mirrored`, which mirrors `schema.rs`'s
`the_policy_owned_set_is_exactly_this` — if the policy-owned set changes, one of
the two fails and names the other.

`ci/gui-coverage-exceptions.txt` needed no change for either crate.

### Runtime verification — what was actually checked

Nested under Hyprland on `wayland-2`, `abyss --backend winit`, with both clients
against it. All of this passed:

- The viewer maps as a plain `xdg_toplevel` (`get_windows` shows it; the bar
  does not appear there, because it is a layer surface — correct).
- `wayland-info | grep foreign_toplevel` → **nothing**, with the bar running.
  The bar did not cause the protocol to be advertised.
- `switch_workspace`, `move_to_workspace`, `focus_window`, `close_window` all
  return `{"ok":true}` and the state moves with them; the bar stayed up across
  every one of them, and both clients logged not one byte.
- `dump_state` confirms abyss reads
  `~/.config/eclipse/{abyss.kdl,policy.kdl}` — the same pair the viewer's
  `search_path()` derives independently. They agree.
- Clean shutdown: `kill <captured pid>` for the bar, then `pkill -x abyss`.

**Not checked, and it needs a human at the machine:** anything visual. The bar's
rendering, its exclusive zone, the accent discipline, and the viewer's three
sections were never seen. There is no capture path from an SSH session —
`grim` against the nested compositor reports *"compositor doesn't support the
screen capture protocol"*, so screencopy is not advertised at all. That is worth
a look on its own: `capture.allow` is enforced, but there appears to be no
protocol behind it yet to enforce against.

### Two disagreements between the plan and the code — do not silently pick

1. **`get_config {file:"policy"}` does not return `DENIED`.** The plan and the
   older handoff both assert it must. What it actually returns is the four
   policy-owned keys with `"readable": false, "value": null, "source": null`.
   No policy content leaks, so the security property holds — but "the method is
   denied" and "the method enumerates the schema and withholds the values" are
   different claims, and the docs assert the first. Decide which is intended and
   fix the other.
2. **`get_windows` lists only the active workspace.** Moving the viewer to
   workspace 2 made `get_windows` return `[]`. That suits the bar, which scopes
   to the focused workspace anyway, but nothing documents it, and a caller
   expecting every window will be quietly wrong.

### One config change was made outside the repo

`~/.config/eclipse/abyss.kdl` still had a `capture` block in it, predating the
ADR 0037 split, and abyss refuses to start on it
(*"capture belongs in policy.kdl, not in this file"*). The block was moved into
a new `~/.config/eclipse/policy.kdl`, which also now carries a
`clipboard.data-control-allow`, a `misc.scripted-input`, and one `app-trust`
windowrule so the viewer has something real to render. This is the owner's live
config, not repo content — mentioned here so it is not mistaken for a code
change.

### Next

1. **The branch story is still wrong and is now the blocking item.** 19 unpushed
   commits covering all of A1→B6 sit on `ci-attribution-display-name`, whose
   open PR #9 is a one-line CI fix. Land #9 with only its attribution commit,
   branch `comp17-de-userland` off freshly-merged `main`, move the DE commits
   there, and open one PR citing `Implements COMP-17 / DP-2 / F-01 §4` — the
   `spec-trail` job blocks a PR without a citation.
2. Visual verification of the bar and the viewer, by a human at the machine.
3. B4 — the control center and the service layer. Everything the bar is
   currently missing (tray, audio, network, bluetooth, battery, notifications,
   media) is blocked on it.


## State as of 2026-09-11 (late) — DE userland, B5 done, B3+B6 scaffolded and paused

Branch `ci-attribution-display-name`. **Working tree: clean except two
untracked directories, `crates/eclipse-bar/` and `crates/eclipse-policy-viewer/`
— deliberately left that way. **Superseded: both were finished and committed
later the same day; see the entry at the top of this file.**
`HEAD` is `770ff33` and all five gate commands are green at it. Nothing is
pushed as a PR yet.

**The work in flight is the plan at `~/.claude/plans/ok-claude-today-we-inherited-cook.md`**
("Usable frontend: taskbar, control center, settings GUI, policy viewer").
Read it before touching anything in `crates/eclipse-*`; it carries the owner's
settled decisions and the B8 order. Its progress markers are kept in the file
itself — they are the authority on where the work stands, this section is the
narrative.

### Where the plan stands

| Unit | State | Commit |
|---|---|---|
| A0 round-trip spike | done | verdict recorded at the foot of this file (2026-09-10) |
| A1 schema | done | `ac3cb4f` |
| A2 `abyss.kdl` / `policy.kdl` split | done | `a95d657`, ADR 0037 |
| A3 config IPC | done | `b63b6ac` |
| A4 self-write vs. watcher | done | `6d8f5a8` |
| A5 `eclipse-ctl config` + coverage ratchet | done | `18b97cf`, `ci/gui-coverage-exceptions.txt` |
| B2 `crates/eclipse-ipc` | done | `5ebe0a9` |
| B1 `crates/eclipse-ui` | done | `a501576`, ADR 0039 |
| B5 settings app | **done** | `770ff33` |
| B3 bar | done (later the same day) | `3a6f7a5` |
| B4 control center + services | not started | — |
| B6 policy viewer | done (later the same day) | `1a5d506` |

B7 is out of scope on purpose: desktop icons (DP-6), `mode wm\|de` profiles
(DP-3), the compositor-drawn policy editor, the first-boot overscan offer, and
bind/windowrule editing.

### Where B3 and B6 actually stand — superseded

This section described both crates as scaffolded, paused and deliberately
outside the workspace `members` list. Both were finished, added to the
workspace and committed later the same day (`3a6f7a5`, `1a5d506`) — read the
entry at the top of this file instead. The one durable point it made survives
below: `publish = false` on both manifests is load-bearing, because
`cargo deny check bans` fails with `error[wildcard]: found 2 wildcard
dependencies` on a publishable crate with path dependencies, which is how B5
first failed the gate.

### The facts the next session should not re-derive

`iced_layershell` 0.19.1, vendored at
`~/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/iced_layershell-0.19.1/src/`
(its `build_pattern/application.md` is the canonical worked example):

- `iced_layershell::build_pattern::application(boot, namespace, update, view)`
  returns a builder with `.run() .settings() .layer_settings() .style()
  .subscription() .theme() .font() .antialiasing() .default_text_size()
  .scale_factor() .executor()`. `namespace` is an `fn() -> String`.
- **The `Message` enum must carry `#[to_layer_message]` above
  `#[derive(Debug, Clone)]`.** The builder's bound is
  `Message: TryInto<LayerShellCustomActionWithId, Error = Message> + Send +
  Debug`, and that proc macro is what supplies it. The macro injects extra
  variants (`AnchorSizeChange(Anchor,(u32,u32))` and friends), so `update`
  needs a trailing arm.
- `LayerShellSettings { anchor, layer, exclusive_zone: i32, size:
  Option<(u32,u32)>, margin, keyboard_interactivity, start_mode,
  events_transparent }`. A top bar is `Anchor::Top | Left | Right`,
  `size: Some((0, H))`, `exclusive_zone: H`, `KeyboardInteractivity::None`.
- `iced_layershell::disable_clipboard()` opts out of the smithay-clipboard
  worker thread. The bar has no clipboard use; call it.
- It tracks the same iced 0.14 family (`iced_core`/`iced_runtime`/`iced_widget`
  0.14, `layershellev` 0.19.1). No version split to manage.

The bar's control-socket surface, verified against `crates/abyss/src/ipc/`:

- `get_workspaces` → bare array of `{index, output, output_name, active,
  windows, owner}`; `get_windows` → bare array of `{handle, app_id, title,
  output, workspace, floating, focused, trust, no_agent}`; `get_focused` →
  an array of **one** `{seat, window|null, output, workspace}`.
- `focus_window {handle}`, `close_window {handle}`, `switch_workspace
  {workspace}`, `set_floating {floating, handle}`, `move_to_workspace
  {workspace, handle?}`. Workspace indices are **1-based on the wire**.
- Events: `window {change: opened|closed|urgent, handle}`, `workspace
  {change: switched, output, workspace}`, `focus {handle}`, `output
  {change: changed, id}`, `config-error {errors}`.
- Every one of those has an `implemented: true` row in `ipc/gate.rs`. A method
  with no row does not exist.

B6's surface, pinned by `schema.rs`'s own `the_policy_owned_set_is_exactly_this`
test: exactly four policy-owned keys (`misc.scripted-input`,
`clipboard.data-control-allow`, `capture.allow`, `capture.redact-app-id`) and
exactly four policy-owned `windowrule` actions (`sensitivity`, `app-trust`,
`seat-compat`, `no-agent`). An empty allowlist **denies everyone** and must
read that way on screen — never as a blank list. The search path is
`/etc/eclipse/policy.kdl` then `$XDG_CONFIG_HOME/eclipse/policy.kdl`
(`config/mod.rs::search_path`), read from disk with the user's own permissions;
the viewer never asks the socket, because the `Policy`/`Read` gate row is
default-closed and stays closed.

### What B5 shipped

`crates/eclipse-settings` at `770ff33`: `src/{app,conn,lib,main,output,pane,schema}.rs`
plus `tests/coverage.rs`, which owns the local half of the GUI-coverage ratchet.
A plain `xdg_toplevel` — `iced::application(...).title(..).theme(..).subscription(..)
.window_size((1100.0, 760.0)).antialiasing(true)` with a `for face in
eclipse_ui::FONTS` loop. **Still untested at runtime** — nothing has drawn a
window yet; that needs `abyss` running, and it is the first thing worth doing
with a live compositor.

### Pins and traps for the new crates

- `iced = { version = "0.14", default-features = false, features = ["wgpu",
  "advanced", "wayland", "thread-pool"] }`. Both non-obvious features are
  mandatory: without `wayland`, winit fails to compile with "The platform
  you're compiling for is not supported by winit"; without a futures executor,
  iced fires a `compile_error!`. `iced_layershell` is `0.19.1`.
- **`iced::time::every` does not exist for us** — it is gated on `tokio`/`smol`
  and we enable only `thread-pool`. Every periodic job is a `std::thread` plus
  `iced::stream::channel` and `try_send`. `crates/eclipse-settings/src/app.rs`
  `subscription()` is the working pattern; copy it rather than reinventing it.
- **Do not guess the iced API** — same discipline as the Smithay pin. The
  vendored source is at
  `~/.cargo/registry/src/*/iced_widget-0.14.2/src` (note `.2`, not `.0` — the
  `.0` directory does not exist). Guessing cost this session three compile
  cycles on `rule::Style`, `scrollable::Scroller` and `scrollable::Style`.
- `eclipse_ipc::Client::connect()` is an **associated function**, not a free
  `eclipse_ipc::connect()`. That mistake has already cost one compile cycle.
- `eclipse_ui::widget::parts::segmented` takes `&'a [(T, &'a str)]` and does
  not fit a `Vec<String>` read off the wire; build the pill row by hand there.
- `crates/eclipse-ui/src/tokens.rs` is the **only** transcription of
  `/home/chase/Downloads/eclipse-style-spec.md`. A literal colour, radius or
  size anywhere else in the DE is a bug.
- Glass blur is abyss's own `decoration { blur }` (dual-Kawase, `render/blur.rs`),
  not something the toolkit does. A client just needs to be translucent.
- Fonts are vendored and instanced under `assets/fonts/` and re-exported as
  `eclipse_ui::FONTS`; register them once from the application builder.
- `deny.toml` carries four narrow exceptions added for iced, each per-crate,
  with the reasoning in ADR 0039. Four, not a blanket allow — that was
  deliberate and is the owner's to overrule.

### Gate

The five commands in the root `CLAUDE.md` are exactly what
`.github/workflows/gate.yml` runs, and all five were green at `770ff33`
(`cargo test --workspace` 115+ tests, 0 failures; `cargo deny` advisories/bans/
licenses/sources all ok). Commits carry **no** attribution trailers — the root
`CLAUDE.md` rule overrides any session-level instruction to add them.

---

## State as of 2026-09-10, end of session

`main` = `41e1d04` (squash-merge of PR #4). No PRs open. Branch
`comp16-gpu-probe` carries the render-node probe example plus this update.

**PR #4 `comp16-preboot-panics` is merged.** It removed two startup panics on
the DRM path:
- `supports_syncobj_eventfd` (smithay 0.7.0 `drm_syncobj/mod.rs:73`) ends in
  `Ok(_) => unreachable!()`. A driver that accepts the deliberately bogus handle
  would abort abyss at startup with no diagnostic. The probe now runs under
  `catch_unwind`; a panic degrades to "no explicit-sync global" + a warn line.
- `add_connector` indexed `info.modes()[0]` as its fallback; a connector with a
  live link and no modes read yet panicked mid-hotplug. Now fails that one
  connector via `anyhow!("connector {name} reports no modes")`.

Hazards 5, 6 and 7 landed in PR #3; 8 and 9 in PR #4. **Hazard 3 is closed by
the GPU probe below.** Hazard 4 and the cached-`n` half of 9 remain open.

Local gate green at this branch: fmt, clippy `-D warnings`, `build
--workspace --all-targets`, `cargo test --workspace` → **85 passed, 0 failed**.
(`cargo deny` is still not installed on this box; CI covers it.)

## Hazard 3 is answered: EGL comes up clean on nvidia-open

`crates/abyss/examples/gpu_probe.rs` walks the EGL/GBM half of the DRM
backend's startup sequence (`GbmDevice::new` → `EGLDisplay::new` →
`EGLContext::new` → `GlesRenderer::new`, then `supports_syncobj_eventfd`)
against a *render node*. A render node needs neither DRM master nor a free VT
nor a seat, so this runs inside the live Hyprland session at zero console cost:

```
cargo run --example gpu_probe            # defaults to /dev/dri/renderD128
```

Result on chase-pc (RTX 4060 Ti, nvidia-open-dkms), 2026-09-10, exit 0:

- node type `Render`, `dev_id 57984`; the render node resolves to the same id
- `gbm: ok`
- `egl: ok, version 1.5` — **this is hazard 3, and it does not bite**
- 672 display dmabuf **texture** formats, 516 dmabuf **render** formats
- 67 EGL extensions, including `EGL_EXT_image_dma_buf_import`,
  `EGL_EXT_image_dma_buf_import_modifiers`, `EGL_MESA_image_dma_buf_export`,
  `EGL_ANDROID_native_fence_sync`, `EGL_KHR_partial_update`,
  `EGL_KHR_swap_buffers_with_damage`, `EGL_WL_bind_wayland_display`
- `egl context: ok`; `gles renderer: ok, 672 dmabuf texture formats`
- `syncobj eventfd: supported` — the **true** branch. No panic.

So the `catch_unwind` added in PR #4 is correct defensive code but is
**unexercised on this driver**: `supports_syncobj_eventfd` probes a DRM *core*
ioctl, not an nvidia path. Explicit sync will be enabled at boot. Do not
"verify" the guard by expecting a warn line — there won't be one.

What the probe deliberately does not do: no `LibSeatSession`, no `UdevBackend`,
no `DrmDevice::new`, no modeset. Those need DRM master and belong to the tty2
boot, which is still the thing that needs the user in front of a monitor.

## Corrections the next agent should not have to re-derive

The old handoff framed the whole KMS bring-up as "blocked on the user at the
console." That is **only true of the parts that need DRM master** — see above.

Verified on chase-pc, 2026-09-10:
- `/dev/dri/renderD128` is `crw-rw-rw-` — openable by `chase` unconditionally.
- `/dev/dri/card1` is `root:video rw-rw----` with an ACL; `chase` is in
  `video`, so it is reachable too. **Note the node is `card1`, not `card0`.**
- `/sys/module/nvidia_drm/parameters/modeset` is **not readable as `chase`**
  (`Permission denied`). The guard at `drm.rs:149` can therefore never see a
  value in normal operation and always takes the "continuing" branch — the
  NVIDIA modeset check is effectively inert. By design (an unreadable param is
  not evidence), but it will not save you at boot.

Smithay paths, since the previous handoff got one wrong: `DrmNode`/`NodeType`
are **not** in smithay at all. `smithay/src/backend/drm/mod.rs:94` re-exports
them from the `drm` crate — the source is `drm-0.14.1/src/node/mod.rs`.
Likewise `allocator/gbm` is `allocator/gbm.rs` (a file, not a directory), and
`GbmDevice` is `gbm-0.18.0`'s `Device::new`. The pin is `=0.7.0`; read the
vendored source, don't guess.

## Plan: first KMS boot, on `chase-pc`

`cbbedroomdesktop` has been offline for hours (`tailscale ping` times out), and
the VM pass is **cancelled by the owner** — "we've done enough VM testing".
So the first real KMS boot happens on `chase-pc` itself, on a second VT,
driven over ssh — **not** from the physical console.

### Why this is safe for a running session

Only the **active** VT's logind session holds DRM master. Making another VT
active revokes Hyprland's master and *suspends* it — it is not killed, every
process inside survives, terminals and long-lived shells included. Switching
back brings it all back. Nothing is `pkill`ed. See "Run it over ssh" below for
how to do that switch without being at the keyboard.

### Preconditions, verified on chase-pc 2026-09-10

- One GPU: `/dev/dri/card1` + `/dev/dri/renderD128` (nvidia). Render node
  present, so `gpu.rs` has something to find. **No `card0`** — anything
  assuming `card0` is wrong, though `drm.rs` hardcodes no path.
- logind session 4 on `seat0`/`tty1` (Hyprland). `seatd.service` is also
  active. `chase` is in `video` and `input` but **not** in a `seat` group —
  which is why the ssh path below runs as root with `LIBSEAT_BACKEND=seatd`.
  libseat prefers logind, and logind will not give a seat to a pty session.
- `nvidia-drm.modeset=1` is **not** on the kernel cmdline, but recent
  `nvidia-open-dkms` defaults it on and Hyprland could not run otherwise.
  Note `/sys/module/nvidia_drm/parameters/modeset` is mode 0400, so the guard
  at `drm.rs:149` cannot read it and silently no-ops — a `modeset=0` boot would
  surface as an opaque EGL error from `EGLDisplay::new(gbm)` at `drm.rs:600`,
  not as the clear message that guard intends.

### Run it over ssh, not from the console

**The previous plan said "sit at the machine and Ctrl+Alt+F2". Don't. Drive it
from an ssh shell instead** — the shell you launch from is the shell you rescue
from, so recovery needs no second machine and no keyboard at the box.

Verified on chase-pc 2026-09-10:

```
session 4  chase  seat0  tty1    <- Hyprland, owns DRM master
session 7  chase  (none) pts/1   <- an ssh session: Seat= empty, VTNr=0
seatd.service: active     openvt + chvt: present
```

**Do not run abyss directly from the ssh shell.** A pty session has no seat, so
`LibSeatSession::new()` fails with
`libseat session (is seatd running, or logind available?)`. That message is a
red herring — seatd *is* running. The real cause is the empty `Seat=`. Budget
an hour if you meet this cold without knowing it.

Allocate a VT instead. `chase` is not in a `seat` group, so this is root work,
and `tailscale ssh root@chase-pc` grants root directly (no password; `sudo -n`
does not work on this box):

```
tailscale ssh root@chase-pc
LIBSEAT_BACKEND=seatd openvt -sw -- /path/to/abyss --backend drm
```

`-s` switches to the freshly allocated VT, `-w` waits for the process to exit.
Recovery, from that same ssh shell:

```
pkill -x abyss        # exact name only. NEVER pkill -f.
chvt 1                # back to Hyprland
```

Two things this does **not** solve:

- **Verification still needs eyes on the monitor.** Over ssh you get logs and an
  exit code, not a picture. Milestones M3/M4/M6 below are visual; someone has to
  look at the screen, or you settle for what the logs assert.
- **VT switching is the flaky part on nvidia-open.** Switching away from tty1
  makes logind mark Hyprland's session inactive; it loses DRM master and is
  *suspended*, not killed — every process inside it survives, and `chvt 1`
  restores it. But nvidia's fbcon-restore path is historically where this
  driver misbehaves, so expect a glitchy resume as the plausible failure rather
  than a wedge.

`chase-laptop` (`100.124.173.22`) is on the tailnet and still works as a second
recovery path if the ssh session itself dies. `cbbedroomdesktop` has been
offline for hours and is not available.

### The run

```
git checkout main       # PR #4 is merged; main = 41e1d04
cargo build
```

Then, as root over ssh, with `XDG_RUNTIME_DIR` pointed at chase's runtime dir
so the wayland socket lands where clients expect it:

```
cd /home/chase/syncedprojects/EclipseOS/AbyssCompositor
LIBSEAT_BACKEND=seatd openvt -sw -- \
  env XDG_RUNTIME_DIR=/run/user/1000 ./target/debug/abyss --backend drm
```

A Claude session left running under Hyprland survives the VT switch and can
tail `journalctl --user -t abyss -f` throughout.

### What to verify, per milestone

This one boot is the gate for three milestones. Verify each explicitly rather
than declaring victory on "it started".

- **M3** — multi-output, hotplug (unplug/replug a DP cable), fractional scale,
  output persistence across a replug. Never pin an output by name; DRM
  connector names shift across driver upgrades on this hardware.
- **M4** — dmabuf, explicit sync, direct scanout, damage tracking, and **VRR**.
  VRR "auto" (`drm.rs:393`) only engages while a fullscreen surface owns the
  output, so it has *never* engaged — M4 cannot close without PR #2's
  fullscreen also being present. Confirm from the log whether `drm_syncobj`
  registered or warned; `supports_syncobj_eventfd` (`drm.rs:629`) probes a DRM
  *core* ioctl and logs on both branches, so it takes the true branch on any
  current Arch kernel regardless of driver — it is **not** the nvidia risk.
- **M6** — session lock, idle, DPMS, and **suspend/resume and lid**, which have
  never run anywhere.

Budget this as debugging, not verification. `drm.rs` is the largest
never-executed surface in the tree.

### Ranked list of what is most likely to bite

Items 1-2 and 5-7 are fixed in PR #3; 8-9 in PR #4; 3 is closed by the GPU
probe. Item 4 and the second
half of 9 are still open and worth recognising fast rather than re-deriving at
a wedged console. Line numbers below predate those fixes -- grep, don't trust.

3. **closed 2026-09-10 by `cargo run --example gpu_probe`.**
   `EGLDisplay::new(gbm)` on nvidia-open comes up at EGL 1.5 with 672 dmabuf
   texture formats, and syncobj eventfd probes `supported`. The unreadable
   modeset guard still makes a hypothetical `modeset=0` failure opaque, but
   modeset is on here.
4. `refresh_mhz` truncation — fixed in PR #3, but it also feeds the advertised
   mode list (`drm.rs:334`/`:339`) and the config mode-match (`:311`).
5. **fixed in PR #3.** `scan_connectors` ordering (`drm.rs:252-274`) + `outputs/mod.rs:563`:
   unplugging the **last** monitor calls `unregister` before `sync_fallback` at
   `:274`, so `fallback_id()` is `None` and `removed.windows` is silently
   dropped. Client loss, not a panic. Fix is to hoist `sync_fallback` above the
   departure loop.
6. **fixed in PR #3** (`teardown_gpu`). `drm.rs:786-804` — `UdevEvent::Removed` for our own GPU re-scans against a
   dead fd instead of tearing down `DrmData`. Warn-spam, then a fallback-only
   compositor.
7. **fixed in PR #3** (`saturating_sub`). `outputs/mod.rs:566` — `target.workspaces.len() - 1` underflow, latent on
   the hotplug-removal path.
8. smithay `drm_syncobj/mod.rs:73` `unreachable!()` is reachable via the
   startup probe — **fixed**: the probe now runs under `catch_unwind` and a
   panic degrades to "no explicit sync" instead of killing the compositor.
9. `info.modes()[0]` in `add_connector` — **fixed**: a connector reporting no
   modes now fails that connector instead of panicking. The cached-`n`
   indexing in the VBlank/render paths is invariant-safe today (every index is
   re-derived through `index_of_crtc`) but remains a refactor hazard.

Other hardcodes worth knowing: `drm.rs:69` `FALLBACK_SIZE (1920,1080)`;
`outputs/mod.rs:298` `refresh: 60_000` (fabricated); `drm.rs:443` `"HEADLESS-1"`
(collides across boots for user config rules); `drm.rs:352`
`GbmBufferFlags::RENDERING|SCANOUT` with no modifier-less fallback;
`render/cursor.rs:24-48` a hardcoded 12x19 scale-1 cursor bitmap, which is a
12-pixel speck on a 2x output (cosmetic). The startup path is `?`-propagated
throughout with no `unwrap`/`expect`/`panic` outside `gpu.rs` tests.

### After the boot

- **Step 3b, bar gate (closes M9)** — no compositor code expected. Point a real
  `ext-foreign-toplevel-list` consumer at the socket; confirm windows appear,
  retitle and disappear. Check whether the existing Quickshell `eclipse` config
  can consume it directly, since that is the bar that would ship. Note
  `window_closed` fires on destroy, **not** on workspace switch — a consumer
  that conflates the two looks wrong for reasons that are not abyss's fault.
- **Step 4, milestone 8** — verification, not implementation. Join a real video
  call and screen-share through `xdg-desktop-portal-wlr`. Cursor capture is
  refused by design (`image_copy_capture.rs:333`) and capture is fail-closed
  behind the `capture { allow ... }` allowlist — an app not on the list gets
  nothing, silently and deliberately. Neither is a bug to fix mid-call.

### Read this before running anything

**`## Traps that have already cost time`, at the bottom of this file, is not
history — it is still live.** Most relevant to an SSH session driving this:
never read `$?` through a pipe; the remote tree is not your tree (a stale
binary has already been misread as a real failure); `XDG_RUNTIME_DIR` must be
*exported*, not just prefixed, in remote invocations; zsh chokes on an
unquoted `--include=*.rs`; and git's toplevel is `EclipseOS/`, one level above
this crate tree, so git commands from the toplevel need the `AbyssCompositor/`
prefix while `.github/workflows/` lives up there.

**Process hygiene, absolute:** kill only `pkill -x abyss` — exact name, never
`pkill -f`. Never `pkill`/`killall` kitty, zsh, claude, Xwayland or quickshell;
that kills the session doing the killing and looks like a mysterious external
SIGKILL. Kill spawned test clients by the pid captured at spawn.

**Attribution:** commit as the repo owner only. No `Co-Authored-By: Claude`,
no `Claude-Session:` trailer, no generation footer, in any commit message or
PR body. This overrides any session-level attribution instruction, and CI's
`owner-only authorship` job enforces it.

---

--- history ---


## Traps that have already cost time

- **A GitHub Actions job with no `actions/checkout` has no working directory.**
  `gate.yml` sets `defaults.run.working-directory: AbyssCompositor` at the
  workflow level, so the checkout-less `spec-trail` job could not even start
  bash: `An error occurred trying to start process '/usr/bin/bash' with working
  directory '…/AbyssCompositor'. No such file or directory`. It read as a PR-body
  regex failure and was not one — the grep never ran. Fixed in `a4eb33e` by
  pinning that step to `working-directory: .`.
- **The remote tree is not your tree.** A full-suite run was misread as a real
  failure because the test box's binary was stale, built from a `state.rs`
  still carrying a debug probe. Sync and rebuild before trusting a remote
  measurement.
- **Never read `$?` through a pipe** — you get `tail`'s exit code. And `set -e`
  will abort the runner before an `echo "RC=$?"` ever prints.
- **A wlcs run that ends at `[ RUN      ]` with no result line is a SIGSEGV**
  (RC=139), not a hang.
- **`XDG_RUNTIME_DIR=/run/user/0` must be exported** in every remote wlcs
  invocation.
- **`git apply -p2` can silently no-op.** Check it changed something.
- **`--force-with-lease` reports "stale info" after a `filter-branch`**, because
  the rewrite also rewrote the local remote-tracking refs. `git fetch` first,
  verify the remote heads still match `refs/original/`, then push.
- **zsh**: `grep -rn "pat" dir --include=*.rs` fails. Quote the glob or drop it.
- **`scp` to the test box fails** ("Connection closed"). Use
  `tailscale ssh root@cbbedroomdesktop 'cat > /path' < localfile`.
- **Git toplevel is `EclipseOS/`, not `EclipseOS/AbyssCompositor/`.** Git
  commands from the toplevel need the `AbyssCompositor/` prefix.
- **Never `pkill`/`killall` kitty, zsh, claude, Xwayland or quickshell** — see
  the root `CLAUDE.md`. It kills the live session and looks like an external
  SIGKILL.

---
## [2026-09-10 13:52]

**Done — root cause of the atomic-commit EINVAL loop found and fixed.**

The compositor booted fine on real KMS (EGL 1.5, GLES 3.2 NVIDIA 610.57.04, two
`Initializing drm surface` lines: crtc 198/plane 52 @1360x768 HDMI-A-1, crtc
390/plane 244 @1280x720 DP-3) and rendered (`queueing frame`), but every atomic
commit returned EINVAL — ~1750-2260 retries in 60-90s, nothing ever scanned out.

Chain: both panels are Sony TVs. Their EDIDs give make `SNY`, monitor name
`SONY TV`, numeric serial `0x01010101`, and no 0xFF serial-string descriptor —
so `outputs::identity()` returned the *same* string for both connectors.
`persist::set_key()` dedupes, so the pair collapsed to one key and shared one
`SavedOutput`; last write wins meant DP-3's 1280x720 got applied to HDMI-A-1.
`apply_settings` only changed the smithay `Output`, never the DRM surface, and
smithay derives the primary plane's `CRTC_W/H` from `OutputModeSource`
(`compositor/mod.rs:1713,1823`) while `SRC_W/H` comes from the surface. src≠dst
on an NVIDIA primary plane, which cannot scale = EINVAL every frame.

Fix is `e024947` on branch `comp16-output-identity-modeset`:
- `outputs/edid.rs` — product code folded into the serial fallback.
- `outputs/mod.rs` — deterministic identity collision guard in `Outputs::add`
  (appends the connector name); `apply_settings` and `apply_change` now ask the
  backend first and refuse the mode rather than half-applying it.
- `backend/mod.rs` — new `set_output_mode` wrapper (winit/headless accept).
- `backend/drm.rs` — new `set_mode` finding the matching connector mode and
  calling `DrmCompositor::use_mode`.

Local gate green: fmt, clippy `-D warnings`, build, 85 tests pass. **Not yet
exercised on hardware.**

Also rewrote `/tmp/abyss-kms/run.sh`: the old socket detection read
`/proc/<pid>/environ`, which can never work — `std::env::set_var` mutates the
in-memory libc environ, not that file (it is a frozen copy of the initial
process stack). It now reads the `SOCKET` field off the `abyss ready on DRM`
journal record, with a `find -newermt` fallback. The boot-log dedupe now keys on
the first 60 chars, since `AtomicRequest`'s debug text is HashMap-ordered and
differs on every iteration, which defeated whole-line dedupe.

**State:**
- Branch `comp16-output-identity-modeset`, one commit, clean tree, no PR yet.
- PR #5 still open with all six checks green — `gh pr merge 5 --squash
  --delete-branch` is classifier-blocked for me, chase has to run it.
- Nothing running; no abyss process left alive.

**Next:**
1. Merge PR #5, then open a PR for this branch (`Implements COMP-03 §2/§4`,
   owner-only attribution).
2. Delete root's stale `/root/.local/state/eclipse/outputs.kdl` — it holds the
   colliding record. The new identity scheme changes the keys anyway, so it is
   dead weight either way.
3. As root: `bash /tmp/abyss-kms/run.sh`, then read `/tmp/abyss-kms/boot.log`,
   `run.log` and `shot.png`. That is the pixels-on-screen confirmation, and it
   needs no one to look at the monitor.

**Open bugs, not fixed:**
- `Failed to drop drm master state Error: Invalid argument (os error 22)` at
  VT-switch-away.
- `Failed to destroy old mode property blob: No such file or directory` after
  each connector add.
- Duplicate entries in the connector mode list; mode selection deserves a look.
  (Corrected 2026-09-10 by `underscan_probe`: only *one* mode reports
  PREFERRED per connector — duplicates, not flag ambiguity.)
  `vram=256MiB` on a 4060 Ti is the PCI BAR size, not real VRAM.
- `refresh_mhz` truncation feeding the advertised mode list.

**Notes for whoever picks this up:**
- `sudo -n` does not work here and `ssh root@chase-pc` / `tailscale ssh
  root@chase-pc` were both refusing connections (port 22) this session.
- `xxd` is not installed; `od -A d -t x1` works for EDID dumps.
- Connecting to a Unix socket needs *write* permission — `chown chase:chase` +
  `chmod 666` on the wayland socket, per chase's call to stop engineering around
  root ownership.

---
## [2026-09-10 16:30]

**Done:**
- The `e024947` modeset fix is **confirmed on real KMS**. Two runs of
  `/tmp/abyss-kms/run.sh` as root, after deleting the stale
  `/root/.local/state/eclipse/outputs.kdl`.
- The atomic-commit EINVAL retry storm is gone: `boot.log` went from 2.4 MB to
  ~11 KB, and the only dedupe bucket left is `[x4] config loaded`.
- Both outputs came up on their *own* native modes — crtc 198 / plane 52 @
  1360x768 on HDMI-A-1 (connector 829), crtc 390 / plane 244 @ 1280x720 on DP-3
  (connector 832). `Setting new mode` fires once per output and sticks, so the
  surface and the smithay `Output` agree and src==dst on the primary plane.
- A kitty client rendered on each run (HDMI-A-1 the first time, DP-3 the
  second), `grim exit=0`, `shot.png` 2640x768. Run 2's shot shows `ls` typed at
  the prompt with full output, i.e. live keyboard input → shell → paint.
- chase confirmed it with his own eyes on the panels.
- Frame stats over the run (`--stats`, now passed by `run.sh`): 465 frames,
  1.0 → 38.1 fps as damage arrives, render p50 ~500 us, submit p50 ~166 us.

**Watch out — journald and message-less spans:**
`stats::maybe_report()` calls `tracing::info!(frames, fps, ...)` with **no
message string**. The journald layer stores those as `F_`-prefixed fields
(`F_FPS`, `F_FRAMES`, `F_RENDER_P50_US`, ...) with an *empty* `MESSAGE`, so they
are invisible to `journalctl -o cat` and to any grep for "fps". Read them with
`journalctl -t abyss -o json | jq 'select(.F_FPS)'`. Their absence from a plain
log dump is not evidence that no frames were submitted — this cost most of a
session. Same trap in reverse at `backend/drm.rs:1083`: `"queueing frame"` is a
`warn!` on the *error* path only, so a silent journal means every queue
succeeded.

**State:**
- Branch `comp16-output-identity-modeset`, clean, one commit + this doc commit.
- PR #5 merged and squashed (`079456a` on main); branch deleted both sides.

**Next:**
- Open the PR for this branch, `Implements COMP-03 §2/§4`.
- Open bugs from the previous entry are all still open and all cosmetic: the
  drop-master EINVAL, the mode-blob ENOENT, duplicate connector modes,
  `vram=256MiB` being the PCI BAR, `refresh_mhz` truncation.

---

## 2026-09-10 — probe results (Task 1) and the KDL round-trip spike (Task 4)

### Overscan: kernel-side compensation is NOT available

`cargo run --example underscan_probe` against `/dev/dri/card1` (nvidia-open,
RTX 4060 Ti). Four connectors — DP-1 (disconnected), DP-2 (disconnected),
HDMI-A-1 (connected), DP-3 (connected) — expose exactly nine properties each:
EDID, DPMS, link-status, non-desktop, TILE, Colorspace, HDR_OUTPUT_METADATA,
dithering mode, vrr_capable.

**No `underscan`, `underscan hborder` or `underscan vborder` on any connector.**

Consequences:

- The DP-1 logical-geometry inset system **cannot be deleted**. Overscan
  compensation has to be done by letterbox compositing in the render path.
- `vrr_capable = false` on all four connectors. VRR is not testable on this
  hardware; it stays on the deferred-hardware list alongside the multi-monitor,
  dock, lid and second-GPU Phase 1 gates.
- Both panels report a bogus 1600mm x 900mm physical size from EDID — worth
  remembering before any DPI-derived scaling lands.

### KDL round-trip: the write API is safe to build

Spike against `kdl` 6.7.1 (KDL v2), throwaway. Parse → `to_string()` is
**byte-identical** (`cmp -s` clean) across own-line `//`, trailing `//`,
`/* */`, `/-` slashdash on nodes/properties/child nodes, mixed 2/4/6/8-space
indentation, aligned values and double blank lines. A single value edit three
levels deep produced a one-line diff and nothing else. "The settings GUI writes
your config file" (DP-5) survives contact: no span surgery, no override file,
no sentinel needed.

API traps for the real implementation:

1. **`set_value()` alone is a silent no-op.** `impl Display for KdlEntry` writes
   `format.value_repr` — the original source text — when a format is present, so
   a parsed document renders the *old* value with no error. Always pair it with
   `entry.clear_format()` (or set `format_mut().unwrap().value_repr`). Wrap this
   in one set-value-and-repr helper or the bug is guaranteed.
2. `KdlValue: From<i128>`, not `From<i64>` — `1920i64` does not compile.
3. **Never call `autoformat()` on a user's file.** It strips blank lines and
   end-of-line comments, reflows block-comment interiors, normalizes
   indentation, and unquotes bare-identifier-valid strings
   (`output "eDP-1"` → `output eDP-1`).
4. `get_mut(name)` returns only the *first* node of that name. Multiple `output`
   nodes need `nodes_mut().iter_mut()` filtered on the first entry's value.
5. `span()` is invalidated after mutation — re-resolve, don't cache.
6. The `query`/`query_all` API is not available in 6.7.1.
7. Booleans must be written `#true` / `#false` (spec gap 5): bare `true` and
   `false` are identifiers in KDL v2.
