# policy-eval — the one policy evaluator, linked by two processes

Read the root `CLAUDE.md` first.

- **TCB.** Every change here is owner-reviewed line by line and is never
  delegated to `eclipse-backend` (F-07 §4).
- Governing spec: S-01 §4 (grants), S-02 (policy language), ADR 0008
  (in-process compiled table, `defer` is tighten-only), ADR 0044 (canonical
  CBOR), ADR 0045 (COSE_Sign1 parameters).
- **Two processes link this crate**, `abyss` and `policyd`, and ADR 0008 makes
  their agreement a CI gate. A change that alters a decision changes both
  evaluators at once — that is the whole reason this crate exists. Do not add
  an API that only one of them can call.
- **No allocation on the check path.** `check()` is a table lookup. If a change
  needs a `Vec`, it belongs in compilation, not evaluation.
- Fail-closed everywhere: an unknown request, a missing table entry, an
  unparseable grant and an expired grant are all the same answer, `Deny`.
- The CBOR decoder **rejects** non-canonical input rather than normalising it.
  See ADR 0044 for why that is a security property and not pedantry.
- No serde. The signed and hashed byte layout is written out by hand so it can
  be read by a reviewer.
- Every `.rs` starts with `// SPDX-License-Identifier: AGPL-3.0-only`.
