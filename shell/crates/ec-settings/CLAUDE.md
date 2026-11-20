# ec-settings — the settings app

Read the root `CLAUDE.md` first. Governing spec: COMP-17 §3 (DP-5).

- **Not TCB.** It is an ordinary client on the control socket and holds no
  authority of its own; every write goes through `set_config_value`, which
  does its own capability check.
- **Every control is generated** from `get_config {schema: true}`. There is no
  hand-written key list in this crate. `tests/coverage.rs` fails the build if
  the compositor grows a key this app cannot render — the only way to pass
  without rendering it is to add the key to `ci/gui-coverage-exceptions.txt`,
  and that file only ever shrinks.
- **No persistent state of its own, bar search history.** Window size and the
  selected pane are not remembered. `abyss.kdl` is the only config state. The one
  exception is `settings-usage.json` in `$XDG_STATE_HOME/eclipse/` (frecency for
  search results: config path, count, last-used; never query text), gated by
  `settings.search.frecency` and cleared from the Settings pane.
- **Scoped to `abyss.kdl`.** Policy-owned keys are shown, read-only, with the
  "edit requires the policy editor" affordance — never writable by any path in
  this app. `policy.kdl` belongs to B6.
- **Never fail silently, never hide that a setting exists.** The two error
  shapes (`DENIED (-32000)` and the `ConfigError` object) render identically
  here and in every other DE app.
- The calibration overlay on the Display pane is compositor-drawn
  (COMP-03 §1.1). This pane sends `calibrate_output` verbs and shows numbers.
- No literal colour, radius or size: `ec_ui::tokens` is the only source.
- Every `.rs` starts with `// SPDX-License-Identifier: AGPL-3.0-only`.
- **The sitemap is presentation.** `pane.rs` owns it: `Group` (five sidebar headings) over `Section` over `Page`. A key's
  page path (`effects/blur-glass`), the CLI argument and every config key are unchanged by how the sidebar is grouped; the
  presentation table is `ec_abyss_config::meta`. Pages that group differently from the meta (`schema::card`), captions
  (`schema::group_caption`) and the advanced settings (`schema::ADVANCED`) are in `schema.rs`, and still searchable. Tests in
  `pane.rs` and `search.rs` fail on a duplicate id, a section with no group or icon, a key or page search cannot reach.
- **The overview** (`overview.rs`) reads the machine without privilege (hostname, os-release, `/proc/uptime`, battery
  `capacity`) once, on reload and when opened, never from a `view`. Quick settings are drawn only for keys the compositor
  reports as switches. "Recently opened" is the search's own frecency, shown only while it is on and not empty.
- **Art** is `ec_ui::widget::art` (tiles, vignettes) and `previews.rs` (live diagrams). No asset, no new dependency, nothing
  drawn while idle, nothing gold. See STYLE.md > Art.
