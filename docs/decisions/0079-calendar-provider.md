# 0079 — The bar's calendar reads events from a provider on the session bus
Status: accepted
Date: 2026-10-22
Deciders: chase (owner), Claude (advisory)

## Context
The clock gains a month calendar drawer (PROPOSEDFEATURES, "Clock → calendar
drawer"). Dates alone need no data source: the grid is libc date arithmetic.
Events do need one. The plan is a calendar add-on, codename Ethereal, that
holds the user's calendars and syncs them with external services. Ethereal
isn't written yet. The bar needs a data path now that Ethereal can fill later
without the bar changing.

Constraints:
- The bar never talks to D-Bus itself. Bus access lives in `ec-services`
  (ADR 0052, ADR 0053).
- Add-ons are a manifest plus host hooks that are off by default (ADR 0066).
  A hook gates an existing host feature and grants nothing new.
- Event titles and locations are `private` by default (S-05 §2). The bar is
  an ordinary layer-shell client (ADR 0056), so whole-output capture sees it.

## Options
1. **The provider owns a well-known name on the session bus, and the bar is
   a client.**
   - Pros: same shape as the tray (StatusNotifierWatcher) and notifications;
     bus activation and name-owner tracking come with it; any provider can
     implement it, not only Ethereal.
   - Cons: a D-Bus interface is a public contract and has to be versioned.
2. **The bar serves a socket and the provider pushes events into it.**
   - Pros: simple wire format.
   - Cons: reconnect, back-pressure and change-push all have to be written by
     hand; the bar ends up owning state that belongs to the provider; it is
     the reverse of every other data path in the shell.
3. **The bar reads a file the provider writes**, such as an ICS cache.
   - Pros: no IPC.
   - Cons: polling; no "open this event" hand-off; the format is lossy.

## Decision
Option 1. A provider owns `org.eclipse.Calendar1` at object path
`/org/eclipse/Calendar1`, interface `org.eclipse.Calendar1`:

| Member | Signature | Meaning |
|---|---|---|
| `Events(start: x, end: x)` | `→ a(ssxxbsa{sv})` | Events overlapping `[start, end)`, unix seconds UTC. Each entry is id, title, start, end, all-day, calendar id, extras. |
| `Calendars()` | `→ a(ssu)` | id, display name, colour as `0xRRGGBB`. |
| `Open(id: s)` | | Show this event in the provider's own app. |
| `OpenDay(day: x)` | | Show this day (local midnight, unix seconds) in the provider's own app. |
| signal `Changed(start: x, end: x)` | | Events in this range changed; refetch if visible. |

- `extras` keys known in v1: `location` (s), `colour` (u, overriding the
  calendar's colour), `notes` (s). Unknown keys are ignored. New fields are
  added as extras, not by changing the struct.
- All-day events: `start` and `end` are local midnights, and `end` is
  exclusive.
- `ec-services::calendar` is the only client. It runs on its own thread,
  watches `NameOwnerChanged`, fetches the visible month plus one month either
  side, refetches on `Changed`, and reports to the bar over a channel. With no
  owner, the bar shows the calendar without events and says that no calendar
  is connected.
- The bar only reads. Creating and editing events happen in the provider's app,
  reached through `Open`/`OpenDay`.
- Event data is `private`. It is shown only in the drawer, and never logged:
  not titles, not locations, not counts per day. A provider that wants its
  events hidden from capture raises the class through policy
  (`sensitivity secret`, CONFIG.md), the same way as any other app.
- No add-on hook. The interface is a public contract that any provider may
  implement, and reading it grants the provider nothing. Ethereal's manifest
  (ADR 0066) names no calendar hook.

## Consequences
- Ethereal, or any other provider such as an evolution-data-server bridge,
  only has to implement one small interface. The bar needs no change when it
  arrives.
- `org.eclipse.Calendar1` is versioned by its name. An incompatible change
  means `Calendar2`, and the bar may then speak both.
- Owed: a mock provider in `ec-services` tests; CONFIG.md entries for
  `bar.clock.week-start` and `bar.clock.week-numbers`.
- Any process in the user's session can own the name and so put text into the
  drawer. That is the same trust level as the tray and notifications: text
  only, no actions run without the user clicking `Open`.

## Revisit when
- Ethereal needs to write through the bar, for example quick-add from the
  drawer.
- A second consumer, such as a lock screen or ec-center, needs events.
- Event data needs a class above `private` by default.
