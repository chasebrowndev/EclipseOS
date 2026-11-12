# 0107 — A config key's presentation lives beside its schema row
Status: proposed
Date: 2026-11-11
Deciders: chase (owner), Claude (advisory)

## Context
Adding one `abyss.kdl` key meant editing up to five places: the schema
(`abyss/crates/ec-abyss-config/src/schema.rs`), the Settings label table
(`ec-settings` `schema.rs`), its page rules (`pane.rs`), its fixed captions
(`app.rs`), and, for a policy key, the viewer's mirrored key list
(`ec-policy-viewer` `read.rs`). Each could be missed on its own, and tests
caught the misses only afterwards: a key landed on the wrong page, the viewer's
list lacked `capture.hide-layer`, a search synonym collided. `docs/CONFIG.md`
was already generated from the schema (`tests/config_doc.rs`).

## Options
1. Add label, page, group, caption and keywords as columns on `schema::Key`.
   One row per key, but every one of 221 `k(...)` calls changes, and Settings
   presentation is mixed into the parser's table.
2. One paired table, `ec_abyss_config::meta::META`, one row per `TABLE` key in
   the same order, plain `&'static str` data. A test fails on any schema key
   without a row unless it is in a commented `EXEMPT` list. Same pattern as
   `DRAG_MAX`.
3. Keep the hand tables and add more cross-checks. Leaves five places to edit.

## Decision
Option 2. `META` holds each key's label, Settings page (as its `section/page`
path), group, fixed caption and search keywords. `ec-settings` reads labels,
placement, captions and keywords from it. A small `NODE_FALLBACK` table, used
only for a key a newer compositor sends that this build does not know, places
that key by its top-level node. Whether a key is policy-owned stays the
schema's `owner` column. `ec-policy-viewer` derives its key and rule sets from
the schema, and names the policy items it does not show yet in
`NOT_SHOWN_KEYS` / `NOT_SHOWN_RULES`, which its tests keep honest.
Value-dependent captions (interaction mode, blur mode, GPU mode, colour
scheme, model-command approval) are code and stay in the app.

## Consequences
A new key fails `meta::tests::every_schema_key_has_metadata` until it has a
row, and the label, page and caption are reviewed in the same diff as the
key. The rows were generated from the old tables, so what Settings shows did not
change. Still outside the table: the Taskbar pages' own row labels
(`taskbar.rs`), the bespoke search entries and the word-level `SYNONYMS`
table, and `ec-ctl`'s `migrate::POLICY_KEYS` mirror (checked by
`tests/schema_drift.rs`). `ec-policy-viewer` now links `ec-abyss-config`.

## Revisit when
Settings needs per-value presentation (enum pill labels and blurbs) as data,
or the Taskbar pages' labels move into `META`.
