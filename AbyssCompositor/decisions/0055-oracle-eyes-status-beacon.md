# 0055 — Oracle-Eyes publishes a status beacon for the taskbar eye
Status: accepted
Date: 2026-09-23
Deciders: chase (owner), Claude (advisory)

## Context
The taskbar's far-left mark is the eclipse ring. The owner wants it to show
what Oracle-Eyes is doing: while automatic mode is on, the ring thickens into
an iris and its empty centre darts around like a pupil; while a model call is
in flight (automatic or select) the pupil narrows to a point; otherwise it is
the plain eclipse. It can be switched off in settings (`bar { eye }`).

That needs three states to cross from Oracle-Eyes to hyperion. The two are
separate workspaces and neither may depend on the other, and Oracle-Eyes'
first invariant forbids the compositor behaving differently because it is
running — so the compositor cannot relay it.

## Options
1. **Relay through abyss** — a new control-socket method and event. Makes the
   compositor carry Oracle-Eyes state; ruled out by ADR 0041's boundary.
2. **A state file** in `$XDG_RUNTIME_DIR`. No liveness: a crashed daemon
   leaves `watch` on disk and the eye stares forever.
3. **An output-only Unix socket owned by Oracle-Eyes.** Disconnect is liveness.

## Decision
Option 3. Oracle-Eyes listens on `$XDG_RUNTIME_DIR/oracle-eyes/eye.sock`
(directory 0700, socket 0600) and writes one word per line — `off`, `watch` or
`think` — on every transition, and the current word to each new client.

- **It never reads from a client.** Nothing a connecting process sends can
  reach the daemon, so the socket adds no input path to the injection surface.
- **Nothing screen-derived crosses it.** No OCR text, no reply, no geometry,
  no timing finer than a state change. The vocabulary is three fixed words.
- **The reader fails to `off`.** EOF, a failed connect, or an unknown word is
  the plain eclipse. The eye is decorative; nothing depends on it.
- **Beacon failure is not daemon failure.** A bind error is logged once and
  the daemon runs without it.

## Consequences
- Oracle-Eyes gains a third, output-only capability; `Oracle-Eyes/CLAUDE.md`
  and `spec.md` §4 list it.
- Any same-uid process can learn when Oracle-Eyes is watching or thinking.
  Same-uid processes can already see the daemon's process and its `claude`
  children, so this discloses nothing new of substance.
- The compositor stores `bar.eye` as a DE setting and knows nothing else.
