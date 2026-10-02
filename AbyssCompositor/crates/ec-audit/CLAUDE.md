<!-- SPDX-License-Identifier: AGPL-3.0-only -->
# ec-audit — `eclipse-audit`, the audit store's reader

Governing spec: S-04 §4 (`verify`), §5 (`query`, `trace`), §1 (JSON
projection). Reads the store `policyd` writes; links `policyd`'s own record
codec so there is one definition of a record.

- **Read-only.** It never repairs, truncates or quarantines: a reader must
  not change what it audits. `policyd::audit::verify` is the walk.
- Every command verifies the whole chain before it answers. Records from a
  store that does not verify are not printed.
- The owner's tool: agents have no access (S-04 §5, B2). It runs as the
  session user against `policyd`'s state directory.
- Not yet: the SQLite index (queries scan), `replay`, `export
  --for-classifier`, retention, external anchors.
- SPDX `AGPL-3.0-only` on every file.
