# ec-steam-glass — the Steam glass add-on's injector

Read the root `CLAUDE.md` first. An add-on (ADR 0066) with no host hook: its
manifest is `packaging/addons/steam-glass.kdl`, its assets are
`packaging/addons/steam-glass/`.

- **Not TCB.** An ordinary user process. It holds no capability and grants
  none; it styles pixels Steam already draws.
- **Only Steam's own UI.** Its `about:blank?createflags=` windows and `steamloopback.host` pages.
  The store and community are other people's sites and are never touched.
- **Loopback only.** Steam's DevTools port is unauthenticated: connect to
  `127.0.0.1`, never bind or forward it.
- **One connection per page, held open.** `Page.addScriptToEvaluateOnNewDocument`
  registrations die with the DevTools session; that is why `serve` does not
  inject and leave.
- **Key on colour values, not class names.** Steam's classes are hashed and
  change every release; the recolouring in `steam-glass.js` must survive that.
- Hand-rolled WebSocket (`cdp`) on purpose: no new dependency (`cargo deny`).
- Every `.rs` starts with `// SPDX-License-Identifier: AGPL-3.0-only`.
