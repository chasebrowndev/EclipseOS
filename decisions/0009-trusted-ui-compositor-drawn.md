# 0009 — Trusted UI is compositor-drawn, never a layer-shell client
Status: accepted
Date: 2026-09-04
Deciders: chase (owner), Claude (advisory)

## Context
Consent prompts, the agent-activity indicator and the emergency panel are the
last line between an agent and an irreversible action (COMP-10). If any of them
is a Wayland client, a malicious or compromised client can imitate it pixel for
pixel, and the human has no way to tell.

## Options
1. Layer-shell client (a shell/bar draws it) — easy, reuses toolkits, spoofable.
2. Privileged client on the privileged socket — still a client; still spoofable
   from the human's point of view.
3. Compositor-drawn surfaces above all clients.

## Decision
Trusted UI is drawn by `abyss` itself, in surfaces that live above every
client in the scene graph and cannot be occluded, screenshotted by an agent, or
synthesized into. The override chord (`Super+Escape`) reaches it even while a
client holds a shortcuts inhibitor and while a prompt is already up.

## Consequences
- `abyss` carries its own minimal text/shape rendering; no toolkit.
- Anti-spoof tests are mandatory: assert no client can produce a surface above
  trusted UI, and that the chord works with a prompt open.
- Prompt visual design is constrained by what the compositor can draw.

## Revisit when
Never, without a replacement anti-spoof story.
