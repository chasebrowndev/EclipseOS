# Building and running abyss

## Toolchain

Rust **1.85+** (edition 2021, workspace `rust-version = "1.85"`). Install via
`rustup`. `rustfmt` and `clippy` components are required for CI parity.

## System dependencies

Development headers, plus their runtime libraries:

| Library | Arch package | Debian/Ubuntu package |
|---|---|---|
| libseat (seat management) | `seatd` | `libseat-dev` |
| libinput | `libinput` | `libinput-dev` |
| Wayland | `wayland` | `libwayland-dev` |
| libxkbcommon | `libxkbcommon` | `libxkbcommon-dev` |
| libdisplay-info (EDID) | `libdisplay-info` | `libdisplay-info-dev` |
| Mesa / GBM / EGL | `mesa` | `libgbm-dev libegl-dev` |
| libdrm | `libdrm` | `libdrm-dev` |
| libudev | `systemd-libs` | `libudev-dev` |
| pkg-config | `pkgconf` | `pkg-config` |

Arch:

```
sudo pacman -S --needed rust seatd libinput wayland libxkbcommon \
                        libdisplay-info mesa libdrm pkgconf
```

Debian/Ubuntu (what CI installs):

```
sudo apt-get install -y libseat-dev libinput-dev libwayland-dev \
  libxkbcommon-dev libgbm-dev libegl-dev libudev-dev libdrm-dev \
  libdisplay-info-dev pkg-config
```

For a real DRM session, `seatd.service` must be running (or logind present) and
your user must be able to open the seat.

## Build

```
cargo build --workspace
cargo test --workspace
cargo clippy --workspace -- -D warnings
cargo fmt --check
```

## Run

During development, run nested inside your existing session (Hyprland works):

```
cargo run -- --backend winit
```

That opens `abyss` in a window with a winit backend, which is milestone 1's
target. The DRM/udev backend is selected by running from a TTY without a parent
compositor.

Quit the nested compositor with **`Super+Shift+Q`** (interim hardcoded binding —
ADR 0020). `Super+Escape` is reserved for the agent override chord and is never
bound to anything else.

## Running as a login session

`abyss --session` performs the systemd/D-Bus handoff itself, immediately after
its Wayland socket exists — see ADR 0032 for why that is not in the wrapper.
It publishes `WAYLAND_DISPLAY`, `XDG_CURRENT_DESKTOP` and `XDG_SESSION_TYPE` to
the user bus and to `systemd --user`, then starts `abyss-session.target`; on
exit it stops that target again. Every one of those calls is best effort: a
box without systemd or D-Bus still gets a compositor, just no user services.

Install the three files in `dist/`:

| From | To |
|---|---|
| `dist/abyss.desktop` | `/usr/share/wayland-sessions/abyss.desktop` |
| `dist/abyss-session` | `/usr/bin/abyss-session` (mode 0755) |
| `dist/abyss-session.target` | `~/.config/systemd/user/abyss-session.target` |

`greetd`/`regreet` builds its session list from `/usr/share/wayland-sessions`,
so the desktop entry is all that is needed for "Abyss" to appear at the
greeter; it runs `/usr/bin/abyss-session`, which sets the session environment
and `exec`s `abyss --session`. After adding the target, run
`systemctl --user daemon-reload` once.

User services that should come up with the session declare the usual pair:

```
[Unit]
PartOf=graphical-session.target
After=graphical-session.target
```

`abyss-session.target` is `BindsTo=graphical-session.target`, so starting it
starts `graphical-session.target` and those services with it.

## Logs

`abyss` logs through `tracing` to journald under the identifier `abyss`:

```
journalctl --user -t abyss -f
```

Verbosity is controlled by `RUST_LOG`, e.g. `RUST_LOG=abyss=debug cargo run -- --backend winit`.
Human input is never logged by content, at any level. If you find a log line
that would print a keystroke, clipboard contents or a `password`-role value,
that is a bug — fix it before anything else.

## Supply-chain checks

```
cargo deny check
```

`cargo-deny` is not installed by default; `cargo install cargo-deny` if you want
to run it locally. CI runs it on every push. `deny.toml` allows only AGPL-3.0
compatible licences and rejects wildcard version requirements.

## Testing DRM on a second VT

On a machine already running another compositor, the DRM backend can be tested
without logging out. Only the *active* VT's session holds DRM master, so a
second VT gets its own seat session and the first compositor is suspended
rather than killed.

1. From the running desktop, `Ctrl+Alt+F2` and log in as the same user. logind
   makes tty2's session active and revokes DRM master from the desktop, which
   keeps running (and so does everything inside it — terminals, editors,
   long-lived shells).
2. `XDG_RUNTIME_DIR=/run/user/1000 ./target/debug/abyss --backend drm`
3. `Ctrl+Alt+F1` returns to the desktop; the compositor on tty2 is suspended in
   turn.

Preconditions: `seatd.service` active *or* logind (libseat prefers logind, in
which case the `seat` group is not needed — `video` and `input` are). On
NVIDIA, `nvidia-drm.modeset=1`; recent `nvidia-open-dkms` defaults it on, and
if the host compositor is running at all, modeset is on.

**Have an out-of-band recovery path before the first attempt.** A compositor
that wedges while holding DRM master can make the VT switch back fail, leaving
a black screen with the desktop still alive underneath. `ssh` in from another
machine and `pkill -x abyss` recovers it without a power cycle; there is no
recovery from the keyboard once the console is wedged.

## Testing DRM in a VM

The DRM/KMS backend needs a real KMS device, so it cannot run nested under
another compositor. A headless QEMU guest with `virtio-gpu` is enough to
exercise the whole path (libseat session, DRM device, GBM/EGL, libinput,
page flips) without leaving your desktop session.

Guest requirements: `seatd` enabled, the test user in the `seat`, `video` and
`input` groups, `mesa`, `libinput`, `seatd` and a Wayland client to poke at
(`foot`, plus `ttf-dejavu` or foot fails to find a font).

Host side, roughly:

```
qemu-system-x86_64 -enable-kvm -m 4096 -smp 4 \
  -drive if=pflash,format=raw,readonly=on,file=/usr/share/edk2/x64/OVMF_CODE.4m.fd \
  -drive if=pflash,format=raw,file=OVMF_VARS.4m.fd \
  -drive file=disk.qcow2,if=virtio \
  -device virtio-gpu-pci -display none \
  -netdev user,id=n0,hostfwd=tcp:127.0.0.1:2222-:22 -device virtio-net,netdev=n0 \
  -monitor unix:mon.sock,server,nowait -daemonize -pidfile qemu.pid
```

Build and run inside the guest over ssh:

```
ssh -p 2222 user@127.0.0.1
cd ~/abyss-src && cargo build
XDG_RUNTIME_DIR=/run/user/1000 ./target/debug/abyss --backend drm &
XDG_RUNTIME_DIR=/run/user/1000 WAYLAND_DISPLAY=wayland-1 foot &
```

Drive and inspect the guest through the QEMU monitor socket:

```
printf 'screendump shot.ppm\n' | socat -t1 - unix-connect:mon.sock
printf 'sendkey meta_l-shift-q\n' | socat -t1 - unix-connect:mon.sock
```

Notes learned the hard way:

- Use `virtio-gpu-pci`, not `bochs-display`. `bochs-drm` exposes
  `/dev/dri/card0` but **no render node**, so `backend/gpu.rs` finds nothing to
  render on. `virtio-gpu` exposes both a card and a render node; mesa still
  falls back to `llvmpipe` behind it, which is fine for correctness testing and
  useless for performance testing. The DRM backend runs recorded in
  `docs/STATUS.md` were all under `virtio-gpu`.
- The QEMU monitor's `mouse_move` / `mouse_set` do **not** reach the guest with
  `-display none`, with or without `-device usb-tablet`. To test pointer
  handling, create a synthetic device inside the guest with `/dev/uinput`
  (a small C program emitting `EV_REL` motion and `BTN_LEFT`) — that exercises
  the real libinput path. `sendkey` for the keyboard works fine.
- Screendumps are full-frame captures of the scanout buffer, which makes them a
  good way to catch damage-tracking bugs (stale regions, cursor trails). Run a
  known-good compositor such as `weston` in the same guest as a control before
  blaming the hardware.
- Only one compositor can hold DRM master: kill weston before starting abyss.
