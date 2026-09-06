# Building and running helios

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

That opens `helios` in a window with a winit backend, which is milestone 1's
target. The DRM/udev backend is selected by running from a TTY without a parent
compositor.

Quit the nested compositor with **`Super+Shift+Q`** (interim hardcoded binding —
ADR 0020). `Super+Escape` is reserved for the agent override chord and is never
bound to anything else.

## Logs

`helios` logs through `tracing` to journald under the identifier `helios`:

```
journalctl --user -t helios -f
```

Verbosity is controlled by `RUST_LOG`, e.g. `RUST_LOG=helios=debug cargo run -- --backend winit`.
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
