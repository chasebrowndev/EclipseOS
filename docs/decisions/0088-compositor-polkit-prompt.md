<!-- SPDX-License-Identifier: AGPL-3.0-only -->
# 0088 — The compositor collects polkit passwords
Status: accepted
Date: 2026-11-04
Deciders: chase (owner), Claude (advisory)

## Context
polkit asks for a password through an authentication agent, and the session had
none. So `auth_admin` could not be answered from a graphical session: any feature
built on `pkexec` (the login screen settings, the boot image, `--reconfigure`) had
nothing to ask with. D-07 §6 and DA-05 record the agent as owed, and rule out a
client prompt: an admin password in a client window is spoofable.

The usual agent holds the password itself and starts polkit's helper. Here that
would put the password in a session process, which the root invariant "`password`
values are never delivered, logged, or stored" forbids.

## Options
1. Agent draws its own window, as polkit-gnome does. A client prompt; D-07 rules
   it out.
2. Compositor draws, then passes the password to the agent, which runs the helper.
   Compositor-drawn, but the password crosses a same-uid process.
3. Compositor draws and talks to the helper itself. The agent relays the request
   only. Chosen.

## Decision
`ec-polkit-agent`, a user unit, registers with polkitd and relays cookie, user,
message and action id to the compositor. It never sees the password and hears only
ok, failed or cancelled.

The compositor draws the prompt (COMP-10 §3.14), connects to
`/run/polkit/agent-helper.socket`, checks with `SO_PEERCRED` that the peer is uid 0,
and writes the user, cookie and password there. PAM runs in the helper, which then
tells polkitd.

**The owner's ruling on the invariant:** "delivered" means delivered to anything
except the verifying authority. The password reaches a root process whose only job
is to verify it, as it does when typed at a console login. It reaches no session
process, is not logged, and is not stored: the entry zeroizes, and so do the copies
made to send it.

This works because polkitd accepts the helper's word for a cookie when the helper's
peer has the uid that registered the agent (polkit 128,
`get_authentication_session_for_uid_and_cookie`). The compositor and the agent are
the same user. It relies on the socket-activated helper, the one polkit uses when
that socket exists, and Arch ships polkit that way.

What the helper reports back grants nothing. polkitd has already heard it directly.

The agent runs as a user unit, in no logind session, so it registers for logind's
`Display` session of its user. That is the session polkitd falls back to for such a
process, and it refuses any other.

### The door
`polkit.sock`, 0600, beside `trusted.sock`. The peer must be the session owner and
`/proc/<pid>/exe` must be `/usr/bin/ec-polkit-agent`. It is not bound in kiosk mode.
One request per connection; hanging up withdraws it, which is how
`CancelAuthentication` reaches the compositor.

polkit's bus policy keeps every other user, root included, off the agent interface.
The agent also answers only `org.freedesktop.PolicyKit1`'s current owner, as a second
check should that policy change.

### Enter
§3.2 and §3.11 keep Enter off anything that grants, so a reflexive keypress lands on
the safe answer. Here Enter submits once something is typed, because otherwise
typing a password and pressing Enter would cancel. A reflexive Enter meets an empty
field and still cancels, and after a Tab Enter follows the focus, so tabbing to
Cancel cancels. Authenticate with nothing typed asks again without asking PAM, so it
costs no try. The Authenticate button has role `Other`, since the password
is what grants. This is limited to this prompt (`Modal::submit_on_enter`); the secret
store's prompt is unchanged.

### Limits, stated
- A same-uid process can put a prompt up, by passing the `/proc` check through the
  real binary, or by registering its own polkit agent before ours. It can then learn
  pass or fail, for a user the prompt names. The prompt's text is marked unverified,
  and the personal secret shows it is the real compositor. It cannot read the
  password. As in COMP-10 §3.10, a process that can ptrace the compositor is outside
  this defence.
- A same-uid process that rebinds `polkit.sock` sees cookies. A cookie is useless
  without passing PAM through the helper.
- Printable ASCII passwords of up to 60 characters only: the trusted font's range and
  the panel's width. Anyone else uses a TTY.
- PAM stacks that ask a second question (an OTP) fail. The password is sent once, and
  only to a no-echo prompt.

## Consequences
- `pkexec` and every other `auth_admin` action now work from the session.
- `/usr/bin/ec-polkit-agent` must be a real binary. Dev installs that symlink into a
  checkout are refused at the door, so `ec-update` installs a copy.
- The secret store's prompt still cancels on Enter if the Tab is skipped. Worth
  moving to `submit_on_enter` separately.
- `org.eclipse.update.apply` uses `auth_admin_keep`, against D-07 §6's reasoning for
  `auth_admin`. Left alone here, and now reachable, so it deserves its own look.

## Revisit when
polkit changes how the helper checks its peer, a PAM stack needs more than a password
on a supported install, or the trusted font gains non-ASCII.
