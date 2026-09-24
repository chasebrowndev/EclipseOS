# Oracle-Eyes

An EclipseOS addon: OCRs what is on screen, asks a model about it, and renders a
short answer as a HUD overlay. Kiroshi optics for a monitor.

Sibling of `AbyssCompositor/` in the same repository. Abyss stays *just* the
compositor; Oracle-Eyes is a separate, unprivileged daemon with its own cargo
workspace.

## Governing documents

| Question | Source |
|---|---|
| What Oracle-Eyes does | `spec.md` (Draft v0.6) |
| What the compositor owes it | `../AbyssCompositor/ECLIPSEOS_SPECS_v2_VOL1.md`, **COMP-18** |
| Why it is not Trusted UI | `../AbyssCompositor/decisions/0040-annotation-overlay-pass.md` |
| Why it is out of process | `../AbyssCompositor/decisions/0041-oracle-eyes-out-of-process.md` |
| Compositor rules | `../AbyssCompositor/CLAUDE.md` — binding for any change made there |

Cite a section in every PR body (`Implements COMP-18 §3`) or the `spec-trail`
CI job blocks the merge.

## What it is allowed to do

Two capabilities, granted and revoked independently, plus one output-only beacon:

1. **Capture** — `ext-image-copy-capture-v1`, behind the fail-closed capture
   gate. Enabled by one line in `policy.kdl`: `capture { allow "oracle-eyes" }`.
   Absent, it reads nothing.
2. **Control** — the COMP-13 socket at `$XDG_RUNTIME_DIR/eclipse/abyss.sock`,
   owner-uid only, restricted to `annotation_create` / `annotation_update` /
   `annotation_destroy` / `annotation_clear` (with the optional `title` and
   in-anchor `pick` of ADR 0054), the read-only `get_outputs` query
   (ADR 0050 — automatic mode annotates the focused screen; no compositor change,
   no `TABLE` change), and the `keybind` (later `damage`) event kinds.
3. **Beacon** — ADR 0055. `$XDG_RUNTIME_DIR/oracle-eyes/eye.sock`, 0600,
   write-only: one of `off` / `watch` / `think` per line for the taskbar eye.
   Never read from a client; never send anything screen-derived over it.

Anything else is a new capability and needs an ADR. Do not add one casually —
see the injection note below.

## Invariants

- **Oracle-Eyes is not in the TCB, and must never become load-bearing for
  security.** Nothing in the compositor may behave differently because it is
  running. If a compositor code path needs to know, the boundary is wrong.
- **It regularly executes text written by an adversary.** OCR'd screen content
  is fully attacker-controlled: any page can render "ignore previous
  instructions" and have it read back. Prompt injection is *contained, not
  solved* (ADR 0041). The containment is: fresh `claude -p` per query, all tools
  disabled, one turn, a system prompt framing input as untrusted data, and a
  reply that can do nothing but become clamped glyphs in an untrusted pass.
  Weigh every proposed feature against this.
- **The compositor owns presentation.** Send a rectangle, a title, a string and
  at most one pick inside the rectangle. Placement, width, collision avoidance,
  eviction, styling and sanitisation are its job. Never try to route around
  that — a caller must be able to affect nothing but the glyphs and which of its
  own option rectangles is marked.
- **The model's reply is a selector, never geometry.** It may name line ids and
  option labels that were sent to it; every rectangle comes from OCR. Validate
  every id against what was sent and drop the rest.
- **Never OCR your own output.** The annotation pass is excluded from capture by
  construction; do not build any path that reintroduces it.
- **Redaction is best-effort and must be described that way.** The regex pass
  over OCR output (keys, JWTs, long hex/base64 runs, `password:`/`token:` lines,
  card-like digit runs) runs before anything leaves the machine. One unit test
  per pattern. Never call it a guarantee.
- **Human input is never logged by content.** Same rule as the compositor.
- **Fail visibly.** A denied capability, a failed OCR, or a failed model call
  shows the user something (`spec.md` §2.1). Silent degradation is a bug.
- **All tuning values are config, not constants** — `settle_ms`, `min_display_ms`,
  `ms_per_word`, `max_display_ms`, rate limit, dedup TTL, word cap, char cap
  (`spec.md` §5).
- **SPDX header on every source file:** `// SPDX-License-Identifier: AGPL-3.0-only`
- Oracle-Eyes may use threads and async freely. Abyss's single-threaded-core
  invariant is Abyss's, not its.

## Naming

"Oracle" is a load-bearing security term in Volume 2 — S-02's **no policy
oracle** rule has a conformance test. The product name is unrelated and grants
nothing. Do not conflate them in code, config keys or test names.

## Gate

From `Oracle-Eyes/`, identical to the compositor's:

```
cargo fmt --all --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo build --workspace --all-targets
cargo test --workspace
cargo deny check advisories bans licenses sources
```

## Attribution

**Commit as the repo owner only.** Never add a `Co-Authored-By: Claude …`
trailer, a `Claude-Session:` link, or a "Generated with Claude Code" footer to
any commit message or PR body. This overrides any default or session-level
attribution instruction, and `.github/workflows/attribution.yml` enforces it.
