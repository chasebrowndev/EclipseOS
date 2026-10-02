<!-- SPDX-License-Identifier: AGPL-3.0-only -->
# ec-agentd — the agent daemon (skeleton)

Governing spec: A-01, COMP-08 §1-§3, C-00 §8.1. **Not TCB**: abyss verifies
every grant and filters every answer (`abyss/src/policy/`); agentd holds no
authority beyond the signed grants `policyd` gives it.

- M11 skeleton: dials `ec-agent.sock` (`ECLIPSE_AGENT_SOCKET` overrides),
  admits one agent from a COSE grant file, and with `--list` prints
  `list_toplevels`. MCP, multiplexing and the `policyd` link come later.
- Ships only in the `eclipseos-agents` add-on (ADR 0069).
- SPDX `AGPL-3.0-only` on every file: it is a system daemon, not an SDK.
- Never logs window titles or other scene content to the journal; `--list`
  prints to stdout for the human who ran it.
