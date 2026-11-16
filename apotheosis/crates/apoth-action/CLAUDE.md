# apoth-action

Governing spec: T-11 §1–§5 (vocabulary), T-12 §4 (classes), T-10 §1.1 (override classes).
Apache-2.0, pure, no dependencies, `#![forbid(unsafe_code)]`. Not TCB.

## What is here (M4)
- `action.rs`: one declaration list generates `enum Action`, `REGISTRY`, typed parse from
  a `Call`, format back to a `Call`. Add an action = add one entry there; the registry
  completeness test (`tests/registry.rs`) checks it.
- `class.rs`: `Class`/`ClassSet`, `OverrideClass`, `Action::classify(&dyn ClassContext)`
  (argument-dependent), `rule_consent`.
- `argv.rs`: argv arrays; a placeholder is a whole element, never interpolated or re-split.
- `text.rs`: textual action syntax shared by `apoth do`, palette, KDL `do` bodies.
- `key.rs`: chords, sequences, mouse binds, mode conditions, bind options, resolution.
- `macros.rs`: no-recursion check, class union via `WithMacros`.
- `event.rs`: closed event set, fields, trusted/program-triggered.

## Decisions the spec left open (recorded, not silent)
- Bare ids `capture pipe launch open notify` (T-11 §1 table) are exempt from `family.verb`.
- Textual syntax: positional values fill params in order; `name=value`; argv after `--`;
  `;` chains; quoted strings with `\" \\ \n \r \t \e \0 \xHH \u{..}`.
- Argv source form: `{` and `}` doubled to be literal; whole-element `{url}` is a placeholder.
  argv[0] can never be a placeholder.
- Chords: xkb names (`plus`, `greater`, `asciitilde`), `physical:<[A-Za-z0-9_]+>`; mods
  `ctrl|control alt|option shift super|cmd|win|logo`, canonical order ctrl alt shift super.
- Mode conditions: comma = AND, `!` negates.
- `clipboard.paste-to` is `clipboard` only (exactly the T-11 §1 row).
- Extra classes beyond the T-11 table (stricter, asserted in `recorded_class_tightenings`):
  `input.paste-selection` is `input`+`clipboard` (same as `input.paste`; the spec lists only
  `input.paste`); `pane.signal-child` (not in the spec) is `input`; argv containing `{selection}`/`{match}`/`{url}` and
  `launch stdin=…` add `read-content`; `config.persist` of a `grant` key is refused.
- `rule_consent` is applied regardless of event trust (stricter than the spec's wording).
- Absent on purpose: `prompt.*` (R2 sheets), `workflow.run` (R2), undo/redo of closed
  panes' processes, output-match actions (R2 rules), per-surface `media.open`.
- `target=` (AUTOMATION-20) is not an Action parameter: targeting is the socket/dispatcher
  envelope (T-12 §3, match language T-11 §7), so Actions stay closed and typed.
- `User.*` ids (AUTOMATION-02): `is_user_action_id` validates the namespace; layered
  override by id and null-deletion live in `apoth-config`/dispatcher, not the registry.
- `font.ligatures-set` takes `scope` (AUTOMATION-31).
- Macro classification is memoised per (name, depth); build() is iterative and capped at
  `MAX_MACROS`. `Debug` of `Event`/`FieldValue` redacts string content.
- Match language (T-11 §7) and template language (T-11 §8) are not here yet.
