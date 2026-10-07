# brokerd — secrets broker

Read the root `CLAUDE.md` first.

- **TCB.** Owner-reviewed line by line; never delegated to `eclipse-backend`
  (F-07 §4, and `gate.yml` already lists `ec-brokerd`).
- Governing spec: S-08 (Vol 2), COMP-08 §4.3 (`secret_fill`), S-04 §1.1
  (`secret` audit kind), A-06.1 (statuses 20, 21).
- **No value is read before every precondition passes** (`broker.rs`, "Order
  of checks"). Tests prove it by corrupting the value blobs and requiring each
  precondition to still answer with its own status.
- **A value and a hash of one never reach audit** (`audit::SecretRecord` has no
  field for either). No free-text errors on the wire: a failure is a number.
- Plaintext lives in `hygiene::LockedBuf`. `hygiene.rs` is the only module with
  `unsafe`; the crate is `#![deny(unsafe_code)]`.
- **Journal before release.** If the audit record cannot be sent, the use is
  denied. Fail closed on locked, unknown, unauthorised.
- The TPM backend is a third `Sealer`; do not add `tss-esapi` here without an
  ADR and a `cargo deny` pass.
- Wire formats are canonical CBOR from `ec-policy-eval::cbor` (ADR 0044). No
  serde on anything signed or hashed.
- Every `.rs` starts with `// SPDX-License-Identifier: AGPL-3.0-only`.
