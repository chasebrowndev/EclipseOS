<!-- SPDX-License-Identifier: AGPL-3.0-only -->
# 0103 — One workspace and one lockfile for the OS; fog joins later, Oracle Eyes stays out
Status: proposed
Date: 2026-11-11
Deciders: chase (owner), Claude (advisory)

## Context
ADR 0070 split the old single workspace into one cargo workspace per product:
`abyss/`, `shell/`, `installer/`, `fog/`, `oracle-eyes/`. Each has its own
`Cargo.lock` and `deny.toml`. Shared client crates are linked by path. That ADR's
own trigger was "a product needs its own release cadence or repo". None has.
The cost showed up somewhere else: lockfiles drift.

**The incident.** `cc4e1555` (PR #274, open) bumps rustix to 1.1.6 for
RUSTSEC-2026-0486. That one-crate bump edits five lockfiles (77 lines). Before
it, fog was already on rustix 1.1.5 and the other four were on 1.1.4.

**Workspaces (as of `fea55ca7`).**

| Workspace | Members | Lock entries | `[workspace.dependencies]` | `[patch.crates-io]` |
|---|---|---|---|---|
| abyss | 21 (20 crates + `bench`) | 340 | smithay `=0.7.0` no-default, calloop, tracing*, anyhow, xkbcommon, kdl, regex, serde_json, blake3, rustix `[net,process,time]` | smithay → `vendor/smithay` |
| shell | 17 | 568 | kdl, serde_json | iced_layershell, layershellev → `vendor/` |
| installer | 4 | 440 | kdl, serde_json | none |
| fog | 6 | 357 | iced 0.14 `+tokio`, iced_wgpu, tokio, rustix `[fs,net,process]`, serde, postcard, jiff, … | none |
| oracle-eyes | 1 | 58 | ec-ipc (path), regex, kdl, wayland-client/protocols, … | none |

- All five use `resolver = "2"`, edition 2021, `rust-version = "1.85"`, and
  toolchain `1.97.0` (five identical `rust-toolchain.toml` files).
- abyss, shell, installer and fog have identical `[profile.*]` blocks.
  oracle-eyes has only `[profile.release]`.
- abyss, shell and installer have virtual root manifests (no `[package]`).
- `.cargo/config.toml` points every workspace at `<repo>/target`.
- `gate.yml` sets `CARGO_INCREMENTAL=0`.

**Cross-workspace path dependencies.** All of them point at abyss or shell crates:
- shell → abyss `ec-ipc` (12 crates), `ec-abyss-config` (settings, bar,
  cheatsheet), `ec-theme` (ui, settings, greeter), `ec-protocols` (ui).
- installer → shell `ec-ui`, abyss `ec-theme`, abyss `ec-ipc` (`ec-setup`).
- oracle-eyes → abyss `ec-ipc`.
- fog → nothing. It is self-contained.

So abyss, shell and installer already compile one another's crates. Each one
resolves those crates against a different lockfile.

**Shared heavy dependencies, per lockfile.**

| Crate | abyss | shell | installer | fog | oracle-eyes |
|---|---|---|---|---|---|
| smithay (patched) | 0.7.0 | — | — | — | — |
| iced / iced_wgpu | — | 0.14.0 | 0.14.0 | 0.14.0 | — |
| iced_winit | — | 0.14.0 | 0.14.0 | **0.14.1** | — |
| wgpu | — | 27.0.1 | 27.0.1 | 27.0.1 | — |
| iced_layershell, layershellev (patched) | — | 0.19.1 | — | — | — |
| zbus | — | 5.19.0 | — | — | — |
| tokio | — | — | — | 1.54.1 | — |
| rustix 1.x | 1.1.4 | 1.1.4 | 1.1.4 | **1.1.5** | 1.1.4 |
| wayland-client / -protocols / -backend | 0.31.15 / 0.32.13 / 0.3.17 everywhere | | | | |
| wayland-server | 0.31.14 | — | — | — | — |
| calloop | 0.13.0 + 0.14.4 in all four that have it | | | | |

Other patch-level drift: fog is ahead of the rest on about 20 crates (thiserror
2.0.21, syn 3.0.6, zerocopy 0.8.59, wasm-bindgen 0.2.129, cc 1.5.1, bitflags
2.13.2, …). abyss is ahead on zlib-rs 0.6.8 and crc32fast 1.5.2. Across the five
locks there are 1763 entries but only 732 distinct name+version pairs. abyss,
shell and installer alone have 1348 entries and 684 distinct pairs.

**CI and packaging.**
- `gate.yml` runs three build jobs: a matrix over `[abyss, shell, installer]`
  plus separate `oracle-eyes` and `fog` jobs. Each one has its own
  `Swatinem/rust-cache` keyed on its workspace.
- `supply-chain` runs a five-entry cargo-deny matrix with
  `--manifest-path <ws>/Cargo.toml`.
- The five `deny.toml` files have the same structure but have drifted. Their
  advisory ignore lists are:
  - abyss: 4 ignores.
  - shell: abyss's 4, plus the three zbus advisories (RUSTSEC-2026-0370/0371/0372)
    and RUSTSEC-2026-0441.
  - installer: abyss's 4 plus 0441.
  - fog: 3 ignores.
  - oracle-eyes: none.
  - The licence allow-lists are the same, except that fog and oracle-eyes drop
    `Unicode-DFS-2016`. fog also drops the libfuzzer-sys exception and the ring
    clarify. oracle-eyes has no exceptions at all.
- `packaging/pkg/eclipseos/PKGBUILD` loops `cargo build --release --workspace
  --bins --locked` over abyss, shell and installer, then builds fog with
  `-p ec-fog-daemon -p ec-fog-ui`. It does not build oracle-eyes. That fits
  Oracle Eyes being an add-on that does not ship with the OS (ADR 0041, 0066).

**Why the split exists.** ADR 0070 gives the reason: ec-settings and ec-ctl had
depended on the whole compositor library. That was fixed by extracting
`ec-abyss-config`, not by the workspace boundary.

The split does not carry the licence boundary. F-05 §3 and ADR 0005 put the
protocol crates and SDKs under Apache-2.0. The `license` field is already set per
crate, and Apache-2.0 crates already live inside AGPL workspaces:
`ec-protocols`, `ec-inference-wire`, `ec-cataclysm-pub`, `ec-ref-agent` and
`ec-claude-agent` in abyss, and `ec-fog-proto` in fog.

Nor does the split carry add-on independence. The agent stack is an add-on
(ADR 0069), yet its crates (`ec-agentd`, `ec-ref-agent`, `ec-claude-agent`) are
members of abyss. ADR 0066 defines an add-on as a package with a manifest and
hooks, not as a build unit. ADR 0052 (one crate and one package per swappable
component) is about crates and pacman packages, not workspaces.

**Spec drift.** F-07 §1 specifies a monorepo with one Cargo workspace and a root
`Cargo.toml`, `deny.toml` and `rust-toolchain.toml`. ADR 0070 departed from that
without recording that it amends F-07 §1. `docs/ARCHITECTURE.md` "Workspace
layout" still describes one workspace under `crates/`, and says the Apache-2.0
half has no code. Both are stale.

**Verified here.** `cargo metadata --offline --no-deps` on a scratch tree, cargo
from the 1.97 toolchain, showed two things:
- A nested directory with its own `[workspace]` stays its own root under a root
  workspace that does not list it.
- Running cargo from a non-member subdirectory (like `abyss/`) resolves to the
  root workspace.

`iced_futures-0.14.0/src/backend/default.rs` picks `tokio` over `thread-pool`
when both features are enabled.

## Options
1. **Status quo plus tooling.** Add a `ci/lock-drift.sh` gate job that fails when
   one crate appears at different semver-compatible versions across lockfiles,
   for a watched list or for every crate (open question 3). Add a
   `ci/cargo-update-all.sh -p <crate> [--precise v]` script that runs the update
   in every workspace that has the crate.
   - Build: no change. abyss, shell and installer still compile the shared path
     crates and the iced/wgpu stack separately whenever their locks differ.
   - Features: no change.
   - CI: one cheap job added. The matrix and five caches stay.
   - Packaging and deny: no change. Five `deny.toml` files keep drifting.
   - Migration risk: none.
   - Cost: a security bump is still five edits in one PR, just scripted. The
     check only catches drift once it has happened.
2. **One root virtual workspace for all 49 crates, one lockfile.** Use
   `default-members = ["abyss/crates/ec-abyss"]` and select products in CI and
   packaging with `-p` sets.
   - Build: one resolution, and shared deps compile once.
   - **Features:** `cargo build/clippy/test --workspace` unifies across every
     member. Fog enables iced's `tokio` feature, so every shell and installer
     pane would silently switch from the thread-pool executor to tokio
     (verified in iced_futures source). Either CI tests a different unification
     than the one shipped, or fog must stop enabling `tokio` first. A code
     change in fog would be a precondition.
   - smithay: unaffected. Only the 3 abyss crates that use smithay (`ec-abyss`,
     `ec-abyss-render`, `bench`) depend on it, through the `default-features =
     false` workspace pin. No other member can switch its features on.
   - `--all-features` would spread `ec-console`'s `shot = ["iced/tiny-skia"]` and
     `ec-abyss-config/test-hooks` into the clippy and test builds of every
     product. Shipped builds don't use `--all-features`, so they are unaffected.
   - Add-ons: Oracle Eyes' third-party versions get pinned to the OS's.
   - deny: one file, one graph. Shell's zbus ignores then cover the whole graph.
     That is harmless while only shell pulls zbus.
   - Matches F-07 §1 literally.
3. **Partial merge (as briefed): abyss and shell into one workspace.** fog,
   installer and oracle-eyes stay separate under option 1's drift check.
   - installer would stay separate even though it path-depends on both halves.
     Its lockfile would keep resolving `ec-ui`, `ec-theme` and `ec-ipc` on its
     own.
   - Its iced features (`wgpu advanced wayland thread-pool canvas image`) are a
     subset of `ec-ui`'s, which every iced pane in shell already links. Merging
     installer therefore changes no iced feature.
   - **Variant 3b: merge abyss, shell and installer.** These are the three that
     already path-depend on one another and are already built together by the
     PKGBUILD loop. It introduces no new unification hazard: abyss has no
     iced/wgpu/zbus/tokio, and the only union is rustix features, `net process
     time` ∪ default.
   - fog stays out until it drops iced `tokio`, then joins (decided 2026-11-11).
   - oracle-eyes stays out because it is an add-on (58 locked crates, one path
     dependency).

## Decision
Option 3b, reached in stages, with option 1's drift check covering what stays
outside.

The workspace root is the repository root: a virtual `Cargo.toml`, plus one
`Cargo.lock`, one `deny.toml` and one `rust-toolchain.toml` there.
- **Members.** The existing crate paths under `abyss/`, `shell/` and
  `installer/`. No crate directory moves.
- **Exclude.** `fog`, `oracle-eyes`, `abyss/vendor` and `shell/vendor`.
- **`[patch.crates-io]`.** The union of today's entries, with root-relative
  paths.
- **`[workspace.dependencies]`.** The union of today's tables. It has no
  conflicting keys.
- **Profiles.** Today's identical block, moved to the root.
- **`resolver`.** Stays `"2"`.

Fog does not stay separate permanently (owner, 2026-11-11: "Change fog so it's
able to be merged into the others cleanly, fog needs a lot of work anyhow.").
Fog drops iced's `tokio` feature, so it runs on the same iced executor
(thread-pool) as the other panes; any fog async work that needs tokio moves to
its own thread or runtime owned by fog, not through iced's feature. Fog then
joins the root workspace as stage 3, after the abyss+shell+installer merge.

Until fog joins, and permanently for oracle-eyes (an add-on, not shipped with
the OS), they keep their own workspaces and locks. `ci/lock-drift.sh` fails the
gate when a watched crate differs between the root lock and theirs. Oracle Eyes
stays separate (owner, 2026-11-12); it joins only by its own ADR amendment.

This ADR amends ADR 0070 (layout) and brings the tree back in line with
F-07 §1 for the OS proper.

## Vendored forks
| Fork | Change vs crates.io | Purpose | Upstreamable | Exit |
|---|---|---|---|---|
| `abyss/vendor/smithay` (0.7.0) | 2 files, 33 changed diff lines. `src/xwayland/xwm/mod.rs` from `docs/decisions/0079-smithay-argb-frame.patch` (86-line patch file), plus `#![allow(warnings, clippy::all)]` in `lib.rs`. | Depth-32 frame windows for ARGB X11 clients (ADR 0079 `xwayland-argb-frame`). | Yes. ADR 0079 says to upstream it; Weston does the same. Whether an upstream PR exists is **unverified**. | Open the upstream PR. Drop the fork with the next smithay bump that contains the fix, in its own PR and ADR (root `CLAUDE.md` pin rule). A 0.7.x point release is unlikely. |
| `shell/vendor/iced_layershell` + `layershellev` (0.19.1) | 3 files, 39 changed diff lines, marked `// eclipse-patch:` (`shell/vendor/PATCHES.md`). A `wl_surface()` accessor, a `SurfaceHook` called in `WindowManager::insert`, and lint allows. | Give the app a layer surface's `wl_surface` for `eclipse_glass_v1`. | Yes: "expose the wl_surface of a layer surface" (PATCHES.md). Upstream status is **unverified**. | Open the upstream PR. Drop the fork at the first iced_layershell release that has an equivalent hook, which likely comes with the next iced bump. |

Neither fork changes in the merge. `[patch]` applies workspace-wide, but only
abyss crates reach smithay and only shell crates reach iced_layershell.
installer's lock has no iced_layershell.

## Migration (each stage is one PR)
0. **Drift tooling.** Add `ci/lock-drift.sh`, `ci/cargo-update-all.sh` and a
   `lock-drift` gate job, starting in report mode. Land #274 through the script.
   - Check: the script reports zero drift on the watched list after #274.
1. **Converge the three deny.toml files.** Make abyss, shell and installer
   `deny.toml` byte-identical, using the union of ignores, each ignore keeping
   its comment.
   - Check: `cmp` succeeds, and the three cargo-deny jobs pass unchanged.
2. **The merge.** All of these change in one atomic PR, because the gate's
   working directories stop resolving otherwise:
   - root `Cargo.toml`, `rust-toolchain.toml`, `deny.toml` and `Cargo.lock`;
   - delete `abyss/`, `shell/` and `installer/` `Cargo.toml`, `Cargo.lock`,
     `deny.toml` and `rust-toolchain.toml`;
   - `gate.yml`: the matrix collapses to one job run from the root (open
     question 4), the conformance job and supply-chain matrix are updated, and
     the "five workspaces" comment is fixed;
   - PKGBUILD: one root `cargo build --release --workspace --bins --locked`;
   - `CLAUDE.md` build commands, `docs/ARCHITECTURE.md` layout, BUILDING.md,
     and ADR 0070's status set to "amended by 0103".

   Seed the root lock from `shell/Cargo.lock` (the largest), then let cargo add
   what is missing. Checks:
   - (a) A script lists every name+version in the old three locks that is
     absent from the new one. The PR body carries the list, which should be
     empty except for deduplication.
   - (b) The set of binary names under `target/release` matches the pre-merge
     PKGBUILD build.
   - (c) The gate is green.
   - (d) `cargo metadata --locked` succeeds from `abyss/`, `shell/` and
     `installer/`.
3. **Fog joins (decided 2026-11-11).** Two PRs, after stage 2.
   - 3a. Fog-only PR: remove iced `tokio` from fog's `[workspace.dependencies]`
     and move its async work off iced's tokio executor (own thread/runtime in
     fog). Check: `cargo tree -e features -i iced_futures` in `fog/` shows no
     `tokio`; fog's tests pass.
   - 3b. Join PR: add fog's members to the root workspace, drop `fog/` manifest,
     lock, deny and toolchain files, fold its `deny.toml` ignores into the root
     one, update `gate.yml` and PKGBUILD. Same checks as stage 2 (a)-(d), plus
     the `cargo tree` check from the root.
   - The `fog` entry leaves `ci/lock-drift.sh`'s outside set.

   Oracle Eyes stays separate (decided 2026-11-12).

**Coordination.** On 2026-11-11 there are 8 open PRs. Only #273 (`de-ec-lock`
adds a shell member) and #274 touch cargo files. Land both before stage 2, or
rebase #273 by moving its member line to the root manifest and regenerating its
lock entries. Branches in worktrees on another machine conflict only on deleted
manifests and locks. To resolve, drop their lock hunk and run `cargo update -p
<new dep>`. Announce stage 2 as a short freeze. It is not reversible by revert
once other PRs land on top of it.

## Consequences
- **Easier.**
  - A security bump in the OS is one lockfile edit, plus at most two for fog and
    oracle-eyes, and the drift check flags those.
  - abyss, shell and installer compile the shared path crates and the iced/wgpu
    stack once per profile.
  - Cross-product refactors such as `ec-ipc` changes are one atomic
    `cargo check`.
- **Harder.**
  - The gate builds the whole OS in one job. The three-way parallel matrix is
    lost unless open question 4 picks per-product `-p` jobs. Wall-clock change
    is **unverified**: not measured, since nothing was compiled for this ADR.
  - `cargo build --workspace` inside `abyss/` now builds the shell and installer
    too.
  - One `deny.toml` means an ignore justified for one product covers the whole
    graph.
  - Per-product CI timing is no longer visible.
- **Forbidden.**
  - New nested `[workspace]` roots inside `abyss/`, `shell/` or `installer/`.
  - A member enabling smithay default features. The `default-features = false`
    pin lives in root `[workspace.dependencies]`.
- **Owed.**
  - The two drift and fan-out scripts.
  - A `ci/` check that the root `deny.toml` exists and that no member directory
    has its own deny file.
  - The doc updates listed in stage 2.
  - An F-07 §1 note that fog and oracle-eyes remain separate workspaces.

## Open owner questions
1. ~~Fog: join after dropping iced `tokio`, or stay separate permanently?~~
   **Decided 2026-11-11 (owner):** it joins; see Decision and migration stage 3.
2. ~~Oracle Eyes: separate workspace, or join the root?~~ **Decided 2026-11-12
   (owner):** it stays separate, with its own lockfile, behind the drift check.
3. Drift check scope: should it fail on a watched list (rustix, iced*, wgpu*,
   wayland-*, calloop, kdl, serde_json, regex), or on any crate present in two
   locks at different semver-compatible versions? The second fails today on
   about 20 fog crates.
4. CI after the merge: one root job that runs the F-07 §3 commands, which is
   simple, serial, and the same unification as packaging? Or per-product `-p`
   jobs, which run in parallel but each lint a feature set the shipped build
   does not use?

## Revisit when
- A product needs its own release cadence or repo (ADR 0070's trigger).
- Fog stops enabling iced `tokio` (stage 3a; triggers stage 3b).
- Oracle Eyes is published or versioned on its own.
- The merged gate job exceeds the CI time budget.
- smithay or iced_layershell upstream absorbs a fork.
