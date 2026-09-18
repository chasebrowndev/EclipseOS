# ISO handoff — where the plan stands (2026-09-18)

Roadmap: `~/.claude/plans/what-actually-needs-done-abstract-hennessy.md`
Attribution rule still applies: commit as the repo owner only. No Co-Authored-By,
no Claude-Session, no generated-with footer.

## Done

- **Step 0–3 complete.** PR #19 merged (`ce2e86f`), `v0.1.0` tagged and pushed,
  `makepkg` produces all four packages, `namcap` clean of errors.
- **Step 1 partially:** greeter stylesheet written by `eclipse-frontend`
  (`97e430b`, glassmorphic per `docs/STYLE.md`). **The owner has still not
  logged out and picked Abyss at greetd** — that is the one remaining manual
  verification, and it needs recording in `docs/STATUS.md` either way.

## Step 4 — signed pacman repo (nearly done)

Uncommitted: `dist/repo/build-repo.sh`, `dist/repo/serve.sh` (both `chmod +x`,
syntax-checked), `dist/pacman/eclipseos.conf`, `docs/design/D-02-package-repository.md`.

- Key `3E5C4816F939F249F5D5A6AC5F61A3ED98F23591` was generated in the isolated
  `GNUPGHOME` at `~/.local/share/eclipseos/gnupg`. `~/.gnupg` untouched.
- First `build-repo.sh v0.1.0` run **failed**: PKGBUILD had `_src="$pkgbase/AbyssCompositor"`
  but the git clone lands in `EclipseOS/`. Fixed to `_src="EclipseOS/AbyssCompositor"`.
- A second run was launched in the background and its result was **never checked**.
  **Do this first:** re-run `dist/repo/build-repo.sh v0.1.0` and confirm
  `~/.local/share/eclipseos/repo/x86_64/` has the four `.pkg.tar.zst` files,
  their `.sig` files, and a signed `eclipseos.db.tar.zst`.
  Note the PKGBUILD `check()` runs `cargo test --workspace --release` — that is
  most of the runtime; expect ~10 min cold.

## Step 5 — archiso profile (built, never run through mkarchiso)

All of `dist/iso/` is uncommitted and new:

- `profiledef.sh` rebranded (`iso_name=eclipseos`, `install_dir=eclipseos`), with
  releng's `choose-mirror` and `automated_script` `file_permissions` entries
  removed and two added: `/root/install-eclipseos.sh` (0755) and
  `/root/eclipseos-packages.txt` (0644). Both files now exist.
- `packages.x86_64` rewritten for the Framework 13 AMD (amd-ucode,
  linux-firmware-amdgpu, vulkan-radeon; no DKMS), plus disk tooling,
  the D-Bus services the bar consumes, greeter packages, and `eclipseos-meta`.
- `pacman.conf` gained a build-time `[eclipseos]` repo over `file://` with
  `SigLevel = Optional TrustAll`. Deliberately different from the installed
  system's drop-in, which is `Required DatabaseRequired` over the tailnet —
  rationale in D-02 §3.
- `airootfs/` stripped of all releng `choose-mirror` machinery (binary, script,
  unit, wants-symlink) and `pacman-init.service.d`. `grep -rn choose-mirror .`
  is clean. Live networking stays releng's iwd + systemd-networkd.
- `airootfs/root/install-eclipseos.sh` — a ~115-line pacstrap installer:
  sgdisk GPT, ESP+ext4, pacstrap from `eclipseos-packages.txt`, genfstab,
  locale, `Include = /etc/pacman.d/eclipseos.conf`, `useradd -m -G wheel`,
  systemd-boot with `amd_pstate=active`, `systemctl enable greetd NetworkManager
  bluetooth systemd-timesyncd`. **No group management** — D-01 §5: a logind
  session on a seat is the whole requirement, there is no `seat` group on Arch.
  `bash -n` passes; it has never been executed.
  Decision: this exists *instead of* an archinstall JSON, because `archinstall`
  is not installed on this machine and its JSON schema could not be validated
  locally. `archinstall` is still on the medium as the guided alternative.
- greetd is **not** enabled on the live medium — the live ISO is an installer and
  only root exists on it. `eclipseos-meta` already ships `/etc/greetd/*` and
  `/etc/eclipse/*`, so the airootfs overlay needs neither.

## Next, in order

1. Confirm the Step 4 repo build actually produced signed packages.
2. Write `docs/design/D-03-*.md` (the profile and installer; D-02 is written).
3. `mkarchiso -v -w /var/tmp/eclipseos-work -o /var/tmp/eclipseos-out dist/iso`
   — needs root, so `~/bin/ksudo`. First run will surface missing packages.
4. Commit **all of Step 4 + Step 5 as one batch** (dist/repo, dist/pacman,
   dist/iso, the PKGBUILD `_src` fix, both design docs). One PR, not one per
   file — the owner's explicit pace feedback.
5. Step 6: boot the ISO, install on the Framework 13.
6. Step 7: D-04 update strategy, including what a `policyd` upgrade means for an
   in-flight hash-chained audit chain (ADR 0046).
7. Update `docs/STATUS.md`: the #19 merge, the `v0.1.0` tag, wlcs green, the
   first successful package build (F-07 §7).

## Open, awaiting the owner

- Focus-follows-mouse no longer raises the hovered window (`785db49`). One-line
  carve-out to restore. Never answered — do not treat it as approved.
- `gh pr view 20` state was never re-checked; it was open awaiting `wlcs`.
- The greeter screenshots have not been shown to the owner yet.

## Traps worth carrying forward

- CI: every push makes four runs; the two-workspace gate is ~5 min a round.
  Batch packaging changes; do not PR-cycle a two-line fix.
- `gh pr merge --admin` bypasses a missing review but **not** a required check
  that is still running.
- `namcap` needs `PATH=/usr/bin:/bin` — the pyenv shim shadows the python that
  owns its module.
- `options=(!lto)` in the PKGBUILD is load-bearing: zstd-sys compiles bundled C
  with makepkg's `-flto=auto` and rust-lld cannot resolve the GCC LTO bytecode.
- zsh hard-errors on unmatched globs (`rm -rf *.pkg.tar.zst` with no matches).
- Working directory drifts constantly via `cd` in compound commands; the
  workflows are at the repo root, one level above `AbyssCompositor/`.
