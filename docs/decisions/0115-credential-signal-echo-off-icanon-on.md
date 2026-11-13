# 0115 — The credential signal is ECHO off and ICANON on
Status: proposed
Date: 2026-11-12
Deciders: chase (owner), Claude (advisory)

## Context
P-04 raises a terminal's sensitivity to `secret` when the PTY has echo off. Every full-screen program (vim, htop, less, a shell's own line editor in raw mode) runs with echo off; under the present rule nearly every terminal session is `secret` nearly all the time, which makes the signal useless and blinds legitimate capture.

## Options
1. Keep echo-off alone. Safe and useless.
2. Heuristics on output text ("password:"). Locale-dependent and spoofable.
3. `ECHO` off **and** `ICANON` on: the termios state of `getpass`-style prompts (sudo, ssh, passwd, gpg pinentry-tty), which raw-mode programs do not share.

## Decision
Option 3 (T-17 §5). Sampled on output-idle edges and before any content leaves the terminal process. It only raises, for the duration of the state, for its duration plus downgrade_grace (S-05 §5.2, 500 ms). This amends S-05 §5 rule 6 and P-04 §6.1, in addition to P-04. The wire signal is `ext.echo=false` on the pane's terminal node, driven by the credential predicate, the only field published in the state; ext.credential is not used (T-17 §5).

## Consequences
Programs that read secrets in raw mode with their own echo suppression are not caught by this signal; they remain covered by app-declared sensitivity and by the user. Fixture programs for both states join the redaction suite. Remote panes (T-18 §10) have a raw local PTY, so remote sudo/passwd prompts are invisible to the predicate; therefore the keys stream is suppressed for remote panes and broadcast out of them requires confirmation unless the profile opts in with `security { remote-keys #true }` (T-17 §5). This is a TCB-adjacent change and gets owner review in its own PR.

## Revisit when
A common credential prompt is found that runs in raw mode, or the kernel exposes a better signal.
