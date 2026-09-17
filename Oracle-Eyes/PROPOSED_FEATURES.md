# Proposed features

Ideas parked deliberately, not forgotten. Nothing here is scheduled.

## A settings-plugin system for EclipseOS

Oracle-Eyes has no settings UI of its own and cannot get one from
`eclipse-settings`: that crate is closed and schema-driven entirely off the
compositor's `get_config {schema: true}` (see its `CLAUDE.md` — "there is no
hand-written key list in this crate," and `tests/coverage.rs` ratchets its
coverage of *compositor* keys only). There is currently no mechanism anywhere
in EclipseOS for an addon outside `AbyssCompositor/` to contribute a pane,
schema, or control to the DE's settings surface.

When such a plugin system exists, Oracle-Eyes' config (`config.rs`) is the
natural first integration target: it already follows the same KDL syntax and
refusal discipline as `abyss/src/config/mod.rs` (unknown key or malformed
value = positioned error, never a silent skip), so a schema-driven settings
pane could describe it with no format changes on this side.

Until then: keybind and tuning customization for Oracle-Eyes goes through
plain KDL config keys and `abyss.kdl` `bind` entries, same as everything else
in EclipseOS today.
