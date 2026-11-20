# ec-glance — data for Glances that need no session

Read the root `CLAUDE.md` first. Governing decision: ADR 0126. Linked by the greeter.

- sysfs and `/proc` only. No bus, no zbus, no new privilege pre-auth.
- Device-reported text is untrusted: it goes through `clean` before any screen.
- Reads run through `bounded`; the UI never waits on a device.
- Session-bound data (now playing) does not belong here; `ec-services` owns it.
