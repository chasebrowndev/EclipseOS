<p align="center">
  <img src="installer/crates/ec-welcome/assets/eclipseos-logo.png" alt="EclipseOS" width="160">
</p>

> Hello, and welcome, to EclipseOS.  
> My gift to the world, my labor of love.  
> I have been working on EclipseOS since 8/28/2026, almost non-stop.  
> The goal of EclipseOS is not recognition, nor is it a quest to surpass others.  
> It is merely my attempt at making something other humans will like.  
> I truly, truly hope you enjoy.

<table>
  <tr>
    <td rowspan="2" width="60%"><img src="docs/screenshots/hero.png" alt="Eclipse Settings on the EclipseOS desktop"></td>
    <td width="40%"><img src="docs/screenshots/desktop.png" alt="The EclipseOS desktop and bar"></td>
  </tr>
  <tr>
    <td><img src="docs/screenshots/tiling.png" alt="Tiled terminals"></td>
  </tr>
</table>

## What is EclipseOS

EclipseOS is an Arch-based operating system where the compositor, the desktop shell, the installer and the security model are designed together, as one thing. It is not a distro with a theme on top. At its heart is **Abyss**, a Wayland compositor written in Rust. The shell around it is written in Rust too.

**Philosophy**

- **Glass first.** Dark by default with a gold accent: translucent, blurred layers, rounded corners, soft depth, fluid motion.
- **Keyboard first.** Tiling windows and workspaces, with gestures, touch and tablet as full citizens.
- **Nothing ambient.** Every sensitive operation crosses a capability check, and the check fails closed.
- **Your input is yours.** Keystrokes, clipboard and IME are never logged by content, not in logs and not in the audit trail.

## Highlights

- **A compositor written from scratch.** Abyss is Rust on Smithay with a single-threaded core, handle-based state and no locks on the hot path. It has booted on real KMS hardware with two outputs, and it passes the Wayland conformance suite (WLCS) in CI.
- **Liquid-glass rendering.** Dual-Kawase blur, soft SDF shadows, rounded corners and animated focus, workspace and window transitions.
- **Tiling that stays out of the way.** Radiant, dwindle and master layouts, workspaces, window rules, three-finger workspace swipes and touch/tablet support.
- **Config you can't break.** One KDL file that hot-reloads and never takes the session down on a bad edit. The Settings app edits the same file in place.
- **A complete desktop, not a parts bin.** The Hyperion bar, launcher with search, toasts and notification centre, policy viewer and secret prompt are all native and share one design system.
- **Trusted UI that can't be spoofed.** Consent and confirmation prompts are drawn by the compositor above every app, never by a client. Screen capture is redacted fail-closed, and a visible indicator shows whenever something is capturing.
- **A tamper-evident audit trail.** Grants are signed, and decisions go into an append-only, hash-chained journal you can verify with `ec-audit`.
- **Agents as guests, not owners.** AI agents get their own input seats and a policy-checked protocol, so they never steal your focus and everything they do is attributable.
- **Installs like an OS should.** A graphical installer with Minimal, Standard, Full and Agentic profiles, and updates through a signed `pacman` repository.

## Built on

- **Spec first.** Two spec volumes and a set of [ADRs](docs/decisions) govern the code, and where they disagree it is treated as a bug.
- **Hard invariants.** No ambient authority, a ratchet that can only tighten a decision, no allocation on the input and policy hot paths, and backends isolated behind one trait.
- **A real gate.** `rustfmt`, `clippy -D warnings`, the full test suite, `cargo-deny` and WLCS conformance run on every PR, with a pinned toolchain and a pinned Smithay.
- **Reviewed trust boundary.** The enforcement path, policy daemon and sandbox get line-by-line owner review.

**Status:** early. The installer and desktop work, but EclipseOS is not a daily driver yet. [STATUS.md](docs/STATUS.md) lists what is verified and what is still stubbed.

## Install

**You will need:** a 64-bit UEFI PC, a USB stick (the ISO is a few GB), and an internet connection during install. Turn Secure Boot off; it isn't supported yet. The installer erases the disk you pick, so back up first.

1. **Download** the latest ISO from the [Releases page](https://github.com/chasebrowndev/EclipseOS/releases) (`eclipseos-YYYY.MM.DD-x86_64.iso`, plus its `.sig` and sha256).
   *(Release not published yet — this link goes live with the first ISO.)*
2. **Verify** it:
   ```
   sha256sum -c eclipseos-*.iso.sha256
   ```
3. **Write it to a USB stick.** Replace `/dev/sdX` with your stick (check with `lsblk`); everything on it is erased:
   ```
   sudo dd if=eclipseos-*.iso of=/dev/sdX bs=4M status=progress oflag=sync
   ```
   On Windows or macOS, use [balenaEtcher](https://etcher.balena.io/) or Rufus (DD mode).
4. **Boot from it.** Restart and open your firmware's boot menu (usually F12, F10, F2 or Esc) and choose the USB stick.
5. **The installer starts by itself.** It is graphical and walks you through the rest.

Once installed, updates arrive through the signed EclipseOS package repository with `pacman`.

Want to build the ISO yourself? See [D-03](docs/design/D-03-installation-media.md) and [BUILDING.md](docs/BUILDING.md).

## Learn more

| | |
|---|---|
| [Abyss compositor](abyss/README.md) | the window manager at the heart of EclipseOS |
| [STATUS.md](docs/STATUS.md) | what is built, stubbed and verified |
| [ARCHITECTURE.md](docs/ARCHITECTURE.md) | how the pieces fit together |
| [CONFIG.md](docs/CONFIG.md) | configuring the desktop |
| [STYLE.md](docs/STYLE.md) | the visual language |

## Licence

System code is **AGPL-3.0-only** ([LICENSE](LICENSE)). Protocol definitions and the agent SDKs are **Apache-2.0** ([LICENSE-APACHE](LICENSE-APACHE)). The name and logo are not licensed by the AGPL: [TRADEMARK.md](TRADEMARK.md).
