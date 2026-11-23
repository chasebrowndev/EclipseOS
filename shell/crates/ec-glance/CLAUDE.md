# ec-glance — data for Glances that need no session

Read the root `CLAUDE.md` first. Governing decision: ADR 0126. Linked by the greeter.

- sysfs and `/proc` only. No bus, no zbus, no new privilege pre-auth.
- Device-reported text is untrusted: it goes through `clean` before any screen.
- Reads run through `bounded`; the UI never waits on a device.
- Session-bound data (now playing) does not belong here; `ec-services` owns it.
- `tile` is pure sizing: how a Glance and the clock fit a tile (variant, sizes, which parts show). No
  iced, no I/O, no dependencies. The greeter, the lock and Settings' builder preview all ask it, so they cannot drift.
  Plans never exceed the tile and are sized against the widest string, not the live one.
- `clock` is pure formatting: `Format {twelve, date}` and `render(...)` build the time and date strings for a preset. The greeter, the lock and the
  Settings builder sample all use it; `ec_greeter::clock_format` is the one preset-to-format mapping.
