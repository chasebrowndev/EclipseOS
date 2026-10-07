# 0076 — Inference router with an API backend and a Claude Code backend
Status: accepted
Date: 2026-10-07
Deciders: chase (owner), Claude (advisory)

## Context
A-06 §8 forbids agents from calling models: inference goes through the router
(I-02) so `Model` provenance links are stamped, and S-08 §1 forbids agents
from handling credentials. Neither the router nor an inference path existed,
so the only installable agent was the model-free `ec-ref-agent`.

The owner wants real Claude agents reachable two ways: an Anthropic API key,
and their own Claude Code login, with one warm Claude Code session per task.

brokerd (S-08) releases a value only to the egress proxy (`Proxy` peer,
`proxy_header`, bound to the secret's host); the proxy (S-09, M20) does not
exist yet.

## Options
1. Agents call the API directly with a key in their sandbox. Breaks A-06 §8
   and S-08 §1 outright.
2. agentd makes model calls. Puts TLS and a credential path into the console
   backend, which brokerd deliberately never hands plaintext.
3. A separate router process, `ec-inferenced`, recognised by brokerd as a
   `Proxy` peer, so it takes a credential exactly the way the egress proxy
   will: as a header value for the secret's bound host, at the network
   boundary, never stored and never visible to an agent. Two backends behind
   one API-shaped contract.

## Decision
Option 3.

- `ec-inferenced` (I-02) serves `inferenced.sock` to `ec-agentd` only. agentd
  forwards an agent's `inference.complete` (offered only to packages whose
  manifest has an `inference { }` block) with the backend and model taken from
  the manifest, never from the agent. The contract is the Messages API's shape:
  `system`, `messages`, `tools` in; `content` (`text`, `tool_use`),
  `stop_reason`, `usage` out (`ec-inference-wire`).
- **API backend**: `POST /v1/messages`; the key is released per request by
  brokerd (`substitute`, host `api.anthropic.com:443`).
- **Claude Code backend**: one warm `claude -p` per task, stream-json in and
  out, every built-in tool off (`--tools ""`), no user or project settings
  (`--setting-sources ""`, a private empty `CLAUDE_CONFIG_DIR`, an empty
  working directory), MCP limited to a stdio shim (`--strict-mcp-config`)
  that turns Claude's tool calls into `tool_use` blocks returned to the
  agent, and `--dangerously-skip-permissions` so it never waits on a prompt:
  every action is the agent's, and goes through agentd, policy and consent
  like any other. Its OAuth token (`claude setup-token`) is released by
  brokerd the same way and handed to that process as `CLAUDE_CODE_OAUTH_TOKEN`.
  The process runs in its own bwrap sandbox: read-only root, no home, the
  agents' seccomp profile.
- The agent loop always runs in the agent's own sandbox; neither backend gives
  a model any tool the agent does not have.

## Consequences
- The Claude Code process holds a credential and makes its own TLS
  connections. That is what A-06 §8 and S-08 §1 forbid an *agent*; it is
  router-side here, outside every agent's sandbox, and receives only text the
  agent sent. VOL2 A-06 §8 and S-08 §1 are amended to name the router as the
  one place a model credential is used.
- Until the S-09 egress proxy (M20) exists, the Claude Code process has an
  unrestricted network namespace. It has no files and no secrets beyond its
  own token, so what it could leak is what the agent already sent it.
- brokerd must be initialised and unlocked before any model call (`ec-secret`).
  Until a compositor unlock dialog exists, that is a passphrase on a TTY each
  login.
- Driving a subscription login headless is not explicitly covered by
  Anthropic's published terms as of this decision; the owner accepts that for
  personal use on their own machine. The API backend has no such question.
- `--tools ""`, `--setting-sources ""` and the stream-json shapes are Claude
  Code behaviour that can change between releases; the backend pins what it
  relies on in tests against a fake `claude`, and refuses to start a session
  whose `system/init` event lists any built-in tool.

## Revisit when
The S-09 egress proxy lands (restrict the Claude Code process to Anthropic's
hosts); a compositor unlock dialog exists; Anthropic publishes terms or an SDK
surface for programmatic use of a subscription login.
