# Graphical installer handoff — write the full plan (2026-09-24)

Task for the fresh agent: **write a complete implementation plan** for the
graphical installer. Do not implement yet. Owner's goal, verbatim: first boot
from the ISO must not need the CLI.

Attribution: commit as the repo owner only. No `Co-Authored-By`, no
`Claude-Session`, no "Generated with" footer (CLAUDE.md overrides session
attribution). PR bodies cite the spec, e.g. `Implements COMP-17 §2.1, §2.2;
D-07 …`.

## Where things stand

**Specified, not built:** everything the owner actually wants.

- `docs/design/D-07-first-run.md` is the design (the whole file; §2.1 medium,
  §3 what runs, §4 steps 0–14, §5 seed allowlist, §6 helper + polkit, §7 trust,
  §8 failure, §10 tests, §11 open decisions).
- `decisions/0060-setup-profiles.md` is the ADR. COMP-17 §2.1/§2.2 and
  Appendix D DA-01/03/04 in `ECLIPSEOS_SPECS_v2_VOL1.md` are the spec side.
- `docs/design/DP-4-desktop-profiles.md` DP-7 (slots, setup floor) and DP-8
  (installer and setup) are the work items, unstarted.

**Built and working:**

- `crates/eclipse-welcome/` — step 0. iced canvas eclipse animation, ten
  greetings, Space to begin, `Message::Begin` after a 700 ms fade. 54 tests.
  Read its `CLAUDE.md`. Flags: `--reduced-motion --version-label --size
  --scale --at --fade --shot`. Never run `--shot` windowed on the host; the
  offscreen `Headless` path exists for that.
- ISO (D-03): `dist/iso/build-iso.sh` builds `eclipseos-<date>-x86_64.iso`
  (1.7 G) with the signed `[eclipseos]` repo baked into `/root/eclipseos-repo`.
  `dist/repo/build-repo.sh local` builds the repo.
- Fallback installer: `dist/iso/airootfs/root/install-eclipseos.sh`
  (interactive shell script; whole-disk erase, Limine, UEFI only).

**What the ISO actually does today (this is the gap):** it boots to a **text
`archiso login:` prompt on tty1 and tty2**. There is no `liveuser`, no greetd
autologin, no abyss session, no welcome, no GUI. Root logs in with an empty
password (`airootfs/etc/shadow` is `root::`). The only way to install is to
run `/root/install-eclipseos.sh` by hand. That is the opposite of the goal.

## VM smoke test, 2026-09-24 (what it proved)

Headless QEMU (KVM, OVMF, 30 G qcow2), driven over QMP `send-key` and
`screendump`. Note this exercised the **fallback script**, not the GUI.

- ISO boots UEFI, network up (SLIRP), baked repo present. **Pass.**
- `install-eclipseos.sh` end to end: partition, mkfs, pacstrap (428 pkgs, from
  Arch mirrors plus the baked repo), Limine, passwords. **Pass, first ever run
  of this script.** Only ran with default timezone; locale/hostname paths lightly
  covered.
- Installed disk boots via Limine to greetd + regreet. The greeter is themed
  gold on near-black. **Pass.**
- Login as the created user starts Abyss: hyperion bar, `SUPER+Return` opens
  foot. **Pass.**
- `eclipse-welcome` is installed (`/usr/bin/eclipse-welcome`), fonts resolve
  (54 fontconfig entries for Cormorant / Noto Serif CJK), the animation runs and
  the eclipse merges into the O, greetings render in Cormorant. **Pass.**

Not tested: the READY/Space/Begin path, CJK greeting frames, reduced motion, any
input on the live medium, Secure Boot, BIOS, NVMe.

## Findings the plan must absorb

1. **Live medium has no session** (above). Needs `liveuser`, greetd autologin,
   a live-only polkit rule, and an abyss session that autostarts the installer.
   D-07 §2.1 describes this; the ISO has none of it.
2. **D-07 §2.1 says "no sshd" but the live ISO runs `sshd`** (releng default;
   `airootfs/etc/ssh/sshd_config.d/10-archiso.conf`, seen in the boot log).
   Either the ISO disables it or D-07 changes; do not leave them disagreeing.
3. **Empty root password on the live medium.** With a network-reachable sshd
   (finding 2) this is worse. Decide the live auth story together with 1 and 2.
4. **`packages.x86_64` does not include the welcome fonts** for the *live*
   session (`ttf-cormorant`, `noto-fonts-cjk`, static Light cuts preferred).
   They are installed on the target through `eclipseos-desktop`, so the installed
   system is fine.
5. **Offline is not offline.** EclipseOS packages install from the baked repo;
   the Arch closure still needs a mirror (D-03/D-07 §2.2 already say this
   honestly). The installer must handle "no network" cleanly (D-07 §4 step 3).
6. **Stock releng leftovers:** `root/.zlogin` runs a missing
   `~/.automated_script.sh` and prints an error on every root login; the motd is
   the stock Arch installation-guide text.
7. **`install-eclipseos.sh` has no non-interactive mode.** The helper's closed
   plan (D-07 §6) replaces it, but the script stays as the tty2 fallback (§8), so
   decide whether the helper reuses its steps or the script wraps the helper.
8. **No input-injection path for headless abyss.** Testing the live GUI needs
   the VM. Plan the test harness (QMP keys/pointer, screendump diffing) as its
   own work item; a reusable one is at `/tmp/eclipseos-vm/q.py` (not in repo,
   ephemeral; recreate under `dist/iso/test/` or `ci/`).
9. Welcome known diffs: dev-rig variable fonts render Cormorant Regular not
   Light; version label and keycap 1–2 px high; faint glow banding at 2x.

## Work breakdown to plan (per repo rules)

- **`eclipse-setup` (iced GUI, steps 1–14):** `eclipse-frontend` agent, always.
  New crate per ADR 0052. Consumes `eclipse-welcome`'s `Begin`. Ordinary
  client, not TCB; writes `abyss.kdl` only through COMP-13 §1.4.
- **`eclipse-setup-helper` (root, polkit):** `eclipse-backend` agent. Closed
  plan, two polkit actions (`org.eclipse.install.apply` live-medium only,
  `org.eclipse.setup.apply` installed system, `auth_admin`), catalog at
  `/usr/share/eclipse/setup/catalog.kdl`, seed as a key-allowlist copy with
  `O_NOFOLLOW` single read (D-07 §5–§6). Privileged and security-sensitive;
  run `invariant-review` and expect owner review. TCB paths (`policy/`,
  `trusted_ui/`, `audit/`, `render/capture.rs`) are never delegated.
- **COMP-10 trusted surfaces owed** (Appendix D): compositor-drawn
  confirmation for whole-disk erase, and the deferred phrase/preset surfaces.
  Main thread + owner review, not an agent.
- **ISO changes:** `liveuser`, greetd autologin, live-only polkit rule, session
  autostart, disable sshd, fonts in `packages.x86_64`, keep tty2 root fallback.
  Main thread. `dist/iso/`.
- **Packaging:** new crates into `dist/pkg/eclipseos/PKGBUILD` split packages,
  catalog file, polkit rules.
- **Test plan:** D-07 §10 into COMP-15, plus the VM harness (finding 8), plus a
  gate for "the live session reaches the installer with no keystroke".

## Decisions the plan should surface to the owner

Already settled: profiles seed defaults, seed-only (not a live layer), GUI
wizard, `mode wm|de` seeded by profile, Agentic offers locked-down default plus
opt-in curated preset, phrase and preset deferred to first login of the
installed system, whole-disk erase and UEFI only in v1.

Open (D-07 §11 plus new): live-medium auth (findings 2–3); BIOS support;
dual-boot/manual partitioning (v1 says no); Secure Boot; whether Agentic is
hidden or "preview" until the Phase 2 stack exists; helper reusing the shell
script's steps vs reimplementing; mirror selection UI.

## Repo state

Branch `d07-first-run-setup`. Two commits at the top, `a6b29ca` (abyss
annotation) and `fd81b83` (oracle-eyes), are **not this work** and are unpushed;
`20643e8` (oracle-eyes) is also foreign and already pushed. Decide with the
owner whether they belong on this branch before opening the PR. No PR is open.

Local git excludes (in `.git/info/exclude`, not committed):
`dist/pkg/eclipseos/{src/,pkg/,*.pkg.tar.zst*}`.

VM disk and OVMF vars live in the previous session's scratchpad `vm/`
directory; the disk holds a finished fallback install (user `tester`,
throwaway password) and can be discarded.
