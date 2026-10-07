# 0075 — brokerd runs as a user service with a per-user store
Status: accepted
Date: 2026-10-06
Deciders: chase (owner), Claude (advisory)

## Context
S-08 §2 places the secret store at `/var/lib/eclipse/secrets/`, which implies
a system service. Every other Eclipse daemon that holds per-user authority
(`policyd`, its journal and issuer key) is a user service with state under
`~/.local/state/eclipse/` (D-01 §3.4). v1 is single-user (Z-03).

## Options
1. System service, store under `/var/lib/eclipse/secrets/`, as S-08 says.
   Survives a compromised user account reading the files directly, but needs
   a root daemon, a per-user namespace inside it, and a cross-uid socket
   protocol that nothing else in the session has.
2. User service, store under `~/.local/state/eclipse/brokerd/secrets/`
   (`$ECLIPSE_BROKERD_DIR`), mode 0700. Same footing as policyd. The files are
   ciphertext whose master key is sealed (TPM, or Argon2id passphrase), so a
   same-uid reader gets nothing usable without the unlock.

## Decision
Option 2. brokerd is `ec-brokerd.service`, a user unit, and its store is
per-user state. S-08 §2's path is amended to match.

## Consequences
- The at-rest protection is the sealing, not file ownership. The TPM sealer
  (S-08 §9.1) matters more, because the passphrase fallback is the only thing
  between a same-uid reader and an offline guess.
- `LimitMEMLOCK`, `LimitCORE=0` and `NoNewPrivileges` are set on the unit
  (`packaging/ec-brokerd.service`), and brokerd refuses to start unhardened.
- Peer identity follows policyd's exe rule (`ec-policyd/src/peer.rs`).

## Revisit when
Multi-user support (Z-03) is taken up, or a threat-model revision treats the
session uid as hostile to its own secrets.
