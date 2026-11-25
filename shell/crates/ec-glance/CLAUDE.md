# ec-glance — data for Glances that need no session

Read the root `CLAUDE.md` first. Governing decision: ADR 0126. Linked by the greeter.

- sysfs and `/proc` only. No bus, no zbus, no new privilege pre-auth.
- Device-reported text is untrusted: it goes through `clean` before any screen.
- Reads run through `bounded`; the UI never waits on a device.
- Session-bound data (now playing) does not belong here; `ec-services` owns it.
- `tile` is pure sizing: how a Glance and the clock fit a tile (variant, sizes, which parts show). No
  iced, no I/O; its only dependency is the constants-only `ec-style-tokens`. The greeter, the lock and Settings' builder preview all ask it, so they cannot drift.
  Plans never exceed the tile and are sized against the widest string, not the live one.
- `cells` is the same for the other tile kinds: the greeting, the account list, the password field and the power pills
  (`greeting`, `users`, `password`, `power` plans), the radii, and the words they draw; and the lock's (`lock_frame`, `lock_greeting`, `lock_password`, `cells::lock`). Nothing about how one of those tiles
  looks lives in the greeter or the builder alone: both draw the plan, so a change to the screen is a change to the preview.
- `clock` is pure formatting: `Format {twelve, date}` and `render(...)` build the time and date strings for a preset. The greeter, the lock and the
  Settings builder sample all use it; `ec_greeter::clock_format` is the one preset-to-format mapping.
