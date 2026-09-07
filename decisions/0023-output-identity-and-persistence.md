# 0023 — Output identity by EDID, layouts persisted per output-set
Status: accepted
Date: 2026-09-06
Deciders: chase (owner), Claude (advisory)

## Context
COMP-03 §2 requires an output layout that survives dock/undock and reboot, and
§9 asks specifically that a dock/undock cycle restore *both* layouts. That
needs two things the kernel does not give us for free:

1. **A stable name for a monitor.** DRM connector names are not stable — this
   machine's primary panel moved from `DP-1` to `DP-3` across an NVIDIA driver
   upgrade, and the root `CLAUDE.md` already warns never to pin an output by
   name. Connector names are also positional, so unplugging one monitor can
   renumber another.
2. **A place to write layout state.** The user's KDL config is canonical and
   hand-written; COMP-03 §4 says runtime changes are persisted "back to
   `outputs.kdl`, not to the user's config".

Smithay 0.7.0 ships no EDID helper (`smithay-drm-extras` is a separate crate
and not a dependency), so the parse is ours.

## Options
1. **Key on connector name.** Free, and wrong the first time a driver upgrade
   or a hotplug renumbers connectors — silently restoring the wrong layout.
2. **Key on EDID, no fallback.** Correct where EDID exists; EDID-less panels
   (virtual outputs, some KVMs, the winit window) then have no identity at all.
3. **EDID make/model/serial, falling back to the connector name.** Stable where
   the hardware cooperates, degraded-but-working where it does not.

## Decision
Option 3. `outputs::identity()` builds `"<make> <model> <serial>"` from a
hand-rolled EDID 1.x base-block parse (`outputs/edid.rs`: header magic +
checksum validated, packed 3-letter manufacturer, `0xFC` monitor-name and
`0xFF` serial-string descriptors, numeric product/serial as a last resort). A
missing, short or corrupt EDID falls back to the connector name — matching the
spec's "EDID-less panel identified by connector name" test.

Layouts are stored in `$XDG_STATE_HOME/eclipse/outputs.kdl` (machine-written,
atomic tmp+rename, rate-limited, flushed on drop), keyed by the **set** of
identities currently present: `set_key()` sorts, de-dups and joins them with
`" + "`. Laptop-alone and laptop-docked are therefore different keys with
independently remembered position/scale/mode/transform, which is what makes the
dock/undock gate pass. `output` blocks in the user's KDL config always win over
the persisted values; the persisted file is only consulted where the config is
silent.

## Consequences
- Two identical monitors with no serial in their EDID collide on one identity.
  They then share saved settings and can swap positions on re-plug. Accepted:
  the alternative (mixing the connector name back into the key) would break the
  driver-upgrade case this ADR exists to fix. `TODO(COMP-03)` to disambiguate
  by connector as a tiebreak *within* a set once wlr-output-management lands.
- The state file is keyed by set, so an unusual dock configuration is learned
  the first time it is seen and remembered thereafter; there is no migration
  between keys.
- The EDID parser is ~130 lines of our own code in the DRM path. It is
  parse-only, bounds-checked, and never trusts a length field; a bad blob
  yields `None`, never a panic.
- Windows whose output disappears are stashed by identity and re-homed onto the
  fallback output, then taken back when that identity returns — the spec's "no
  workspace loss" requirement, implemented against identity rather than
  connector for the same reason as above.
