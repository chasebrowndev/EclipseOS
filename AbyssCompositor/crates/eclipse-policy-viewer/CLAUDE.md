# eclipse-policy-viewer — what the policy currently permits

Read the root `CLAUDE.md` first. Governing spec: COMP-17 (DP-5), B6.

- **Not TCB**, and deliberately powerless: it reads `policy.kdl` off disk and
  renders it. The control socket refuses to serve policy, and this app never
  asks. Editing policy happens in the compositor-drawn policy editor — every
  section carries that affordance, and a test asserts it.
- **Read-only, structurally.** There is no write path in this crate and none
  may be added. A control that would mutate policy from a layer-shell client
  violates the trusted-UI invariant.
- **An absent or malformed file is content, not a crash.** A missing
  `/etc/eclipse/policy.kdl` is normal; a malformed one must name the file and
  the reason. An empty allowlist renders a sentence, never a blank list.
- **Fail-closed reading**: an unreadable file is reported as granting nothing,
  never as granting everything.
- No literal colour, radius or size: `eclipse_ui::tokens` only. `view.rs` is
  currently a stack of uniform panels with no hero block — see
  `docs/COMPOSITION.md`; this is a known composition bug, not the house style.
- Every `.rs` starts with `// SPDX-License-Identifier: AGPL-3.0-only`.
