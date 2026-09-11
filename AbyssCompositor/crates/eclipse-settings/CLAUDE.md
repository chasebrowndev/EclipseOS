# eclipse-settings — the settings app

Read the root `CLAUDE.md` first. Governing spec: COMP-17 §3 (DP-5).

- **Not TCB.** It is an ordinary client on the control socket and holds no
  authority of its own; every write goes through `set_config_value`, which
  does its own capability check.
- **Every control is generated** from `get_config {schema: true}`. There is no
  hand-written key list in this crate. `tests/coverage.rs` fails the build if
  the compositor grows a key this app cannot render — the only way to pass
  without rendering it is to add the key to `ci/gui-coverage-exceptions.txt`,
  and that file only ever shrinks.
- **No persistent state of its own.** Window size and the selected pane are
  not remembered. `abyss.kdl` is the only state there is.
- **Scoped to `abyss.kdl`.** Policy-owned keys are shown, read-only, with the
  "edit requires the policy editor" affordance — never writable by any path in
  this app. `policy.kdl` belongs to B6.
- **Never fail silently, never hide that a setting exists.** The two error
  shapes (`DENIED (-32000)` and the `ConfigError` object) render identically
  here and in every other DE app.
- The calibration overlay on the Display pane is compositor-drawn
  (COMP-03 §1.1). This pane sends `calibrate_output` verbs and shows numbers.
- No literal colour, radius or size: `eclipse_ui::tokens` is the only source.
- Every `.rs` starts with `// SPDX-License-Identifier: AGPL-3.0-only`.
