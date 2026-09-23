# Building and running abyss

## Toolchain

Rust **1.97.0**, pinned in `rust-toolchain.toml` — that exact channel is what
CI uses, and dev must not drift from it (a bump is its own PR). The workspace's
`rust-version = "1.85"` is the *minimum* the crates claim to compile against,
not the toolchain you should build with. Install via `rustup`; `rustfmt` and
`clippy` components are required for CI parity.

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

No C library is needed for D-Bus: `eclipse-services` uses `zbus` (pure Rust,
already in the tree via `iced_layershell`). At runtime it wants a session bus,
and the status cells want NetworkManager, BlueZ and UPower on the system bus;
each degrades to "unavailable" without its daemon.

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

During development, run nested inside your existing session (the dev host runs
abyss itself; any Wayland session with a host compositor works):

```
cargo run -- --backend winit
```

That opens `abyss` in a window with a winit backend. The DRM/udev backend is
selected by running from a TTY without a parent compositor; `--backend headless`
runs with no display or input hardware at all (COMP-01 §10) and is what the wlcs
conformance suite drives.

Add `--stats` on any backend to get periodic frame counts, fps, and render and
submit latency percentiles. **These are logged with no message string**, so
journald stores them as `F_`-prefixed fields with an empty `MESSAGE` — they are
invisible to `journalctl -o cat` and to a grep for "fps". Read them with:

```
journalctl -t abyss -o json | jq 'select(.F_FPS)'
```

## Running wlcs locally

The `conformance` job in `gate.yml` builds `crates/wlcs-abyss` as a cdylib and
hands it to wlcs, which drives the headless backend in-process. `ci/wlcs-skip.txt`
is a ratchet of the tests known to fail — entries may be removed, never added
without justification. Locally you need wlcs itself installed; without it, the
job is CI-only.

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

Install the three files in `dist/` (or use one of the scripts under
"Development install" / "Packaging" below, which do this and more):

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

### Development install

`dist/install-session.sh` does all of the above against a checkout instead of
against `/usr/bin`, so Abyss can be selected at the greeter while it is still
being worked on:

```
cargo build --release --workspace --bins
sudo ./dist/install-session.sh
```

It symlinks the release binaries into `/usr/local/bin`, installs the wrapper
and the session entry with `Exec` repointed there, and writes the user units
with `/usr/bin` rewritten to match. Because the binaries are symlinks and not
copies, a rebuild is live at the next login and the installer does not have to
be run again — which is the whole point of it, and also the reason it is not
how a distribution should ship Abyss.

It also enables `hyperion`, `eclipse-toasts` and
`eclipse-screensaver`. `dist/install.sh` is the non-symlink variant: it builds
from a fresh clone as your user, installs real binaries into `/usr/bin` and the
units into `/usr/lib/systemd/user`, and enables the same three units. Neither
script installs `eclipse-secret-prompt` yet (`KNOWNBUGS.md` PKG-03).

**Check for pacman-installed packages first.** A machine that ever had the
`eclipseos-*` packages installed keeps their binaries and enabled units, and a
source install lands *beside* them rather than replacing them — two taskbars
was the symptom last time (2026-09-22). Before a from-source install:

```
pacman -Qq | grep '^eclipseos-'
```

If that prints anything, decide which install you want. Removing
`eclipseos-desktop` forces `eclipseos-meta` out with it, and `-meta` owns the
greetd config — reinstate it (or `pacman -S eclipseos-meta`) before rebooting,
or the next boot drops to a text greeter.

The installed wrapper passes `--backend drm` explicitly. `default_backend()`
would infer it, but only from `WAYLAND_DISPLAY` and `DISPLAY` both being unset;
a greeter that leaked either into the session environment would silently get a
nested `winit` compositor rather than a login session.

## Packaging

The distribution path (D-01..D-03), all under `dist/`:

- **`dist/pkg/eclipseos/PKGBUILD`** — split package, built from the pushed
  `v$pkgver` tag (`pkgver=0.1.1`). One package per swappable component
  (ADR 0052): `eclipseos-abyss` (abyss, eclipse-ctl, the session wrapper,
  desktop entry and target; hard-depends on `xorg-xwayland`),
  `-hyperion`, `-toasts`, `-center`, `-launcher`, `-desktop` (settings, policy
  viewer, `eclipse-screensaver`), `-policyd`, and `-meta`, which depends on all
  of them plus greetd/regreet/cage, NetworkManager, BlueZ, UPower, PipeWire
  and foot, and ships `/etc/eclipse/{abyss,policy}.kdl`, the greetd config under
  `/etc/eclipse/greetd` and its `greetd.service.d` drop-in.
- **User units.** `hyperion.service`, `eclipse-toasts.service`,
  `eclipse-screensaver.service` and `policyd.service` are all
  `PartOf=graphical-session.target`; the three GUI-side units are
  `After=`/`Requisite=abyss-session.target`, and `policyd` is
  `Before=abyss-session.target` so abyss still paints without it.
  The packages enable them image-side by shipping
  `/usr/lib/systemd/user/abyss-session.target.wants/` symlinks (`_want` in the
  PKGBUILD); a user-preset would never be applied for an existing account.
- **`dist/repo/`** — `build-repo.sh <tag>|local` builds, signs with a dedicated
  packaging key in its own keyring, and `repo-add`s into
  `~/.local/share/eclipseos/repo`; `serve.sh` serves it on the tailnet address
  only (D-02).
- **`dist/iso/`** — `sudo ./build-iso.sh` bakes the signed packages and key
  into the archiso profile and runs `mkarchiso` (D-03). The medium carries its
  own copy of the repo, so the resulting ISO installs anywhere.

Not packaged yet: `eclipse-secret-prompt` (PKG-03).

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

This is the path the 2026-09-10 real-KMS boot took (scripted with `openvt -sw`
under a `timeout`, as root, with `LIBSEAT_BACKEND=seatd`). Wrapping the run in
`timeout -s TERM 60` is worth doing on a first attempt: it bounds the damage if
the compositor wedges while holding DRM master.

Preconditions: `seatd.service` active *or* a logind session on the seat
(libseat prefers logind, which ACLs the DRM node to the session user — no
`seat`, `video` or `input` group needed; D-01 §5). `openvt` produces no logind
session, which is why the 2026-09-10 run used root and seatd. On
NVIDIA, `nvidia-drm.modeset=1`; recent `nvidia-open-dkms` defaults it on, and
if the host compositor is running at all, modeset is on.

**Have an out-of-band recovery path before the first attempt.** A compositor
that wedges while holding DRM master can make the VT switch back fail, leaving
a black screen with the desktop still alive underneath. `ssh` in from another
machine and `kill` the test compositor's pid recovers it without a power cycle
(not `pkill -x abyss` if the host session is itself abyss); there is no
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
  useless for performance testing. Every DRM run recorded in `docs/STATUS.md`
  before 2026-09-10 was under `virtio-gpu`; the first real-KMS boot (NVIDIA,
  `nvidia-open-dkms`) is written up in that file's "Real-KMS boot record".
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
