# 0042 — The EclipseOS root is the root; the DE is its own workspace
Status: accepted
Date: 2026-09-16
Deciders: chase (owner), Claude (advisory)

## Context

The git root is `EclipseOS/`, but nothing lived there. `ECLIPSEOS_SPECS_v2_VOL1.md`
and `VOL2.md`, `decisions/`, `docs/`, `CLAUDE.md`, `.claude/`'s agents and skills,
and the toolchain pins were all inside `AbyssCompositor/`, one level down, beside
the compositor they only partly describe. `Oracle-Eyes/` already sat as a sibling
of that directory and had to reach up and over for every shared rule.

At the same time `AbyssCompositor/crates/` had grown seven `eclipse-*` crates.
Four of them — `eclipse-ui`, `eclipse-bar`, `eclipse-settings`,
`eclipse-policy-viewer` — plus the headless `eclipse-services` are the desktop
userland (COMP-17, F-01 §4). They are not compositor code, they are not in the
TCB, and they drag iced 0.14 (ADR 0038) and with it wgpu, cosmic-text and a font
shaper into the compositor's workspace: one `Cargo.lock`, one `deny.toml`, one
audited dependency graph shared between a GUI stack and an enforcement path.
ADR 0039 exists only because of that collision — the iced advisory and license
exceptions had to be granted workspace-wide, to `abyss` and `policyd` as much as
to the settings app.

F-07 §1 asserted a single monorepo Cargo workspace with `/Cargo.toml` at the
root. That had already been false since Oracle-Eyes landed.

## Options

1. **Leave it.** Zero work. The specs stay misfiled under one of the three things
   they govern, the DE keeps sharing a dependency policy with the TCB, and every
   new ADR 0039-shaped exception keeps widening the compositor's audited surface.
2. **Separate repositories.** Real isolation, but the control protocol crate is
   shared by four components and cross-cutting changes are constant; separate
   repos mean version-juggling, and F-07 §1 rejected them for good reasons that
   have not changed.
3. **One repo, several workspaces, specs at the root.** Hoist everything that
   describes the whole system to `EclipseOS/`, leave `AbyssCompositor/` the
   compositor and its socket contract, and give the userland its own workspace
   in `EclipseDE/`. Exactly the shape Oracle-Eyes already proved (ADR 0041).

## Decision

Option 3.

- The repository root carries the specs, `decisions/`, `docs/`, `CLAUDE.md`,
  `.github/`, and a single `rust-toolchain.toml` and `rustfmt.toml` — both tools
  resolve by walking ancestors, so one copy at the root is what actually stops
  the drift the gate warns about. The per-workspace copies are deleted.
- `EclipseDE/` is a new cargo workspace holding `eclipse-ui`, `eclipse-bar`,
  `eclipse-settings`, `eclipse-policy-viewer` and `eclipse-services`, with its
  own `Cargo.lock` and its own `deny.toml`. The ADR 0039 iced exceptions move
  there; `AbyssCompositor/deny.toml` keeps only `RUSTSEC-2026-0196` (cgmath, via
  smithay) and an empty license exception list.
- `eclipse-ipc` and `eclipse-ctl` stay with the compositor. They are the client
  half of the COMP-13 §2 socket contract, versioned with the server that defines
  it, and Oracle-Eyes already path-deps
  `../AbyssCompositor/crates/eclipse-ipc` — leaving them put means that edge and
  `Oracle-Eyes/Cargo.toml` need no change at all.
- `eclipse-settings` keeps its dev-dependency on `abyss` across the boundary, as
  a path dep. That edge is what keeps the settings GUI's schema coverage test
  honest against the compositor's own config table; it is worth carrying, and a
  dev-dependency does not enter the shipped graph.
- `dist/` splits: the session units, the desktop entry and the install scripts
  stay in `AbyssCompositor/dist/`; the DE units and `applications/` move to
  `EclipseDE/dist/`. The install scripts learn two target roots and one command
  still produces a working session.
- CI gates each workspace with its own job (`gate`, `eclipse-de`, `oracle-eyes`,
  and a `cargo-deny` job apiece). There is no workflow-level
  `working-directory` any more; every job states its own.
- F-07 §1 is amended to describe this tree.

## Consequences

- The DE's supply chain is audited on its own terms. A future iced or wgpu
  advisory is exempted in `EclipseDE/deny.toml` and the compositor's graph never
  sees it — which is the whole point, and the reason ADR 0039's exceptions were
  uncomfortable where they were.
- Nothing is lost to the split, because there was no compile-time coupling to
  lose: not one crate under `AbyssCompositor/crates/` depends on a DE crate. The
  compositor builds, gates and runs with `EclipseDE/` absent.
- Three workspaces means three of everything — three lock files, three
  `deny.toml`s, three gate jobs, and `cargo build --workspace` at the root finds
  no manifest. `CLAUDE.md` and `docs/BUILDING.md` carry the loop that builds all
  three; the `gate` skill runs it.
- Cross-tree path deps are relative and therefore brittle to further moves:
  `EclipseDE` → `../AbyssCompositor/crates/eclipse-ipc`, `eclipse-settings` →
  `../../../AbyssCompositor/crates/abyss`. Moving a tree is now a manifest edit,
  not just a `git mv`.
- The working directory for everyday work is `EclipseOS/`, not
  `AbyssCompositor/`. Agent briefs, skills and the routing rules were repointed
  in the same change; a path in an older transcript that starts at `crates/` is
  now relative to a workspace, not to the repo.
- Every move was `git mv`, so `git log --follow` still reaches the full history
  of a moved file.

## Revisit when

A DE crate needs a compile-time dependency on compositor internals beyond the
socket contract — that would mean the boundary is in the wrong place, not that
the split was wrong. Or the relative path deps become a recurring edit cost, at
which point a workspace-per-repo with a published `eclipse-ipc` is the next
shape, not a return to one workspace.
