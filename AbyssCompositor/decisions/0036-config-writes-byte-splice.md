# 0036 — Config writes byte-splice the file; the KDL document model does not
Status: accepted
Date: 2026-09-10
Deciders: chase (owner), Claude (advisory)

## Context
COMP-13 §1.4 specifies a write API for `abyss.kdl` and carries a blocking
VERIFY: can the `kdl` v2 document model round-trip a human's file — comments,
blank lines, indentation, number bases, raw strings — across an edit? Every
GUI surface planned in F-01 §4 writes through this one path, so the answer
gates all of them.

Reading the vendored `kdl-6.7.1` source (not docs) settles it:

* `KdlEntry::set_value` mutates only `self.value` (`entry.rs:80`).
* `Display for KdlEntry` prints `format.value_repr` — the *original source
  text* — whenever `format` is `Some`, and falls back to `self.value` only
  when it is `None` (`entry.rs:418-441`).

So on a parsed document `entry.set_value(8); doc.to_string()` re-serializes the
**old** value. It compiles, it returns `Ok`, and it writes a file that is
byte-identical to the input. A silent no-op. The escape hatch,
`entry.clear_format()`, writes the right value but discards that entry's
`leading`/`trailing` — the inline comment and the alignment on that line.

## Options
1. Document model + `clear_format()` on the edited entry — correct value, loses
   the comment and spacing on exactly the line the user was looking at.
2. Document model + `autoformat()` — whole-file reformat on every slider drag.
3. Byte-splice against `KdlEntry::span()` — parse only to *locate*, then
   replace those bytes and copy the rest verbatim.

## Decision
Option 3. `config::edit::set_value` parses the file solely to resolve a dotted
path to a byte range, replaces exactly that run of bytes with the newly
rendered literal, and copies everything else through untouched. The document is
never re-serialized on the write path, so the `value_repr` footgun is not
reachable from it at all.

Supporting facts, verified in `v2_parser.rs:543`: an entry's span starts
*after* its leading trivia and ends immediately after the value, so
`end - value_repr.len() .. end` is an exact value span — and it works for
`key=value` properties without touching the key.

Prohibited on this path, ever: `autoformat*`, `clear_format_recursive`,
`ensure_v1`/`ensure_v2` on the whole document. They are whole-document
reformatters; a config file is a human's file. The document model is used only
by `eclipse-ctl config migrate`, where a one-time reformat of the moved node is
acceptable and stated up front.

Absent keys are inserted at the end of the owning block, indented from the
previous sibling. Writing the value a key already holds is byte-identical to
the input — a slider dragged back to where it started must not churn the file.

## Consequences
- COMP-13 §1.4's VERIFY is closed: yes, faithfully — by splicing, not by the
  obvious API call.
- `crates/abyss/src/config/edit.rs` carries
  `round_trip_preserves_everything_else` over a fixture holding a license
  comment, a `/* block */`, a `//` trailing comment on the edited line, a `/-`
  slashdashed node, tab and 4-space indentation in different blocks,
  `0xDEADbeef`, a raw string, a multi-line string, `key   =   value` spacing
  and no trailing newline. It asserts the output differs in exactly one run of
  bytes, re-parses, and yields the new value.
- `document_model_would_have_lied` is a tripwire: it asserts the no-op
  behaviour directly, so a future refactor back to the document model fails
  loudly instead of silently dropping writes.
- List-valued constructs (`bind`, `windowrule`) are out of scope for v1; they
  need list-identity semantics and start on the GUI-coverage exception list.

## Revisit when
`kdl` changes `Display for KdlEntry` to prefer `self.value`, or ships a
format-preserving setter. Until then the tripwire test is the guard.
