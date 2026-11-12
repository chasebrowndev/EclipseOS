# Security policy

EclipseOS is pre-release software. Its security model is partly implemented;
`docs/KNOWNBUGS.md` (the `SEC-`, `SEAT-`, `TASK-`, `PROT-`, `AGENTD-` entries)
and `docs/STATUS.md` list what is enforced today and what is not. Do not rely
on it to contain hostile software yet.

## Reporting a vulnerability

Report privately through GitHub: **Security → Report a vulnerability** on this
repository (private vulnerability reporting). Please do not open a public
issue for anything that bypasses a policy decision, leaks input, clipboard or
window content, forges a grant, or escapes the agent sandbox.

Include the commit, how to reproduce it, and what an attacker gains. Gaps
already listed in `docs/KNOWNBUGS.md` are known; a working exploit for one is
still welcome.

## Scope

In scope: the compositor (`abyss/`), `ec-policyd`, `ec-policy-eval`,
`ec-agentd` and its sandbox, the trusted UI, the control and agent sockets,
the installer's privileged helper, and the packaging that sets permissions.

Out of scope: an attacker who already has root, and physical access to an
unlocked session.

## Supported versions

Only the tip of `main` receives fixes.
