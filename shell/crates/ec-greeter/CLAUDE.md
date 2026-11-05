# ec-greeter — login screen customisation

Read the root `CLAUDE.md` first. Governing spec: ADR 0089, D-01 §3.5, §4.1.

- **TCB.** `ec-greeter-apply` runs as root under polkit, and `ec-greeter-run`
  stands between greetd and the only way to log in.
- `ec-greeter-apply` writes only what it serialised or encoded itself. It never
  parses a PNG from a user and never copies input bytes to disk.
- `ec-greeter-run` must always reach regreet. Any problem falls back to the
  shipped `/etc/eclipse/greetd` files.
- No dependency on a desktop crate or `ec-ui`: this package has no edge to the
  desktop packages, so removing one cannot take the login screen with it.
