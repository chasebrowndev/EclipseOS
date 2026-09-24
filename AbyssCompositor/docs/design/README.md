# Design documents

Tier 6 documents (Vol 1 planning index):

| ID | Document | Status |
|---|---|---|
| D-01 | [Base system](D-01-base-system.md) | written 2026-09-18 |
| D-02 | [Package repository and signing](D-02-package-repository.md) | written 2026-09-18, reduced |
| D-03 | [Installation media and installer](D-03-installation-media.md) | written 2026-09-18 |
| D-05 | [Default userland](D-05-userland.md) | written 2026-09-23, partial (no terminal, no portal) |
| D-07 | [First-run setup and agent onboarding](D-07-first-run.md) | written 2026-09-24, specification only |

## Vendored design reference

`eclipse-panes.html` is a copy of the Claude Design canvas for the Eclipse
panes. It ships its own `./support.js`, which is **not** vendored and is not
needed: the file is a *reading* reference, and the markup plus inline styles
are the payload. Opening it in a browser will render the artboards statically
and log one missing-script error; that is expected.

Five artboards, referenced throughout `docs/COMPOSITION.md`:
`4a` Display, `4b` Sound, `4c` Notifications, `4d` Power,
`4e` Privacy & security.

The accompanying style spec was **not** vendored here — it is byte-identical
to `docs/STYLE.md`, which is the copy the code and `tokens.rs` cite.
