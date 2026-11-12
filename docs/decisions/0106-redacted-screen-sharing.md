<!-- SPDX-License-Identifier: AGPL-3.0-only -->
# 0106 — Redacted screen sharing as a desktop feature
Status: proposed
Date: 2026-11-11
Deciders: chase (owner), Claude (advisory)

## Context
The owner's priority is desktop first. Abyss already has redaction machinery that was
built for the agent stack. This ADR turns it into a feature a person sees: when they
share their screen in a call, in OBS, or with a screenshot tool, `secret` windows such as
password managers and auth prompts are replaced in the shared image. They stay visible on
the real monitor.

### What exists today (read from code)
- **One pass list, one redaction decision.** `abyss/crates/ec-abyss/src/render/capture.rs`
  (TCB) builds every capture from `capture_elements`, never from the on-screen list.
  `classify()` takes the decision before any GPU work and records it in `Redacted`. The
  secret window is never rendered into the target. An opaque black quad covers its
  `bbox()` and its titlebar (COMP-02 §7, §8 C-24).
- **Every capture client gets the same treatment.** `zwlr_screencopy_v1`
  (`protocols/standard/screencopy.rs`) and `ext_image_copy_capture_v1`
  (`protocols/standard/image_copy_capture.rs`) share one `decide()` gate and one
  `capture::service`. Nothing tells a human-side capture apart from any other.
  `grim`, `wf-recorder`, xdg-desktop-portal-wlr (xdpw) and OBS through PipeWire all get
  the redacted frame. ADR 0029 verified a black `redact-app-id` window in a portal frame.
  No agent capture path exists yet: `eclipse_capture_v1` is unbuilt, and
  `protocols/agent/leakage.rs` carries the gap as an ignored test. `wlr_export_dmabuf` is
  deliberately absent (COMP-06 §2). `protocols/standard/dmabuf.rs` is only the client
  buffer path.
- **The gate.** `capture.allow` is empty by default (ADR 0027), so a stock install shares
  nothing. To share at all, the user types `capture { allow "xdg-desktop-portal-wlr" }`
  (ADR 0029). Screen sharing streams continuously over ext-image-copy-capture (ADR 0030).
  Only outputs can be sources. There is no foreign-toplevel capture source, so a
  per-window share is impossible today.
- **Built-in screenshot** (ADR 0093). It uses the same pass list and redacts the same
  windows, before any effect is applied. It shows no indicator.
- **How a window becomes sensitive.** Four inputs, combined by `or`, so each one can
  only raise:
  1. A `windowrule "sensitivity …"` in `policy.kdl` inserts the window into
     `state.sensitive` on map (`shell/rules.rs:416`).
  2. `capture.redact-app-id` (exact match, checked on map, `capture::mark_sensitive`).
  3. A `secret` node in the semantic tree. A stale or absent tree covers the whole
     window (A-10).
  4. `policy::classes::class_of() == Secret`, but only while a `policyd` table is live.
     With no table, `classify()` ignores the class on purpose, so that S-05 §8's "all
     secret" does not black out the human's own captures.

  `foreign_toplevel::refresh_all` reads no class. It publishes titles and app_ids
  unfiltered.
- **Shipped defaults** (`packaging/etc/policy.kdl`). Only `^ec-secret-prompt$` is
  `secret`. No password manager is listed. `redact-app-id` is empty.
- **What each kind of surface receives today:**

| Surface | In a capture today |
|---|---|
| `secret` toplevel | Opaque black over its bbox and titlebar. The title never reaches the target. |
| `private` toplevel (default) | Shown in full. S-05 §2 says the same. |
| Popups, subsurfaces | Inherit the parent's class by construction: `still.rs` pushes popups inside `fx.window`, and that call is skipped for a redacted window. |
| Layer surfaces (bar, toasts) | Only the sensitive set or the semantic tree can redact them, because they have no app_id. Toasts with message previews are **shown in full**. `capture.hide-layer` omits icon-sized surfaces only (≤64 px, ADR 0056). |
| Taskbar chips | Captured as part of the bar. Whether a chip shows a `secret` window's title is **unverified**. `CONFIG.md` says only that a secret window is never grouped or named in the `de` group list. |
| Overview, switcher, previews | Never in a capture pass list. On screen, a padlock card with no title (`overview/thumbs.rs` → `classify`). |
| Trusted UI, lock screen, polkit prompt, capture indicator, annotations | Drawn by the backends only, so they cannot be captured, by construction. |
| COMP-19 protected surfaces | Omitted entirely, with no placeholder. |
| Cursor | wlr: baked in only if `overlay_cursor` is set, and suppressed while a prompt holds the seat. ext: the cursor session is stopped. |
| XWayland windows | `redact-app-id` never matches them (`app_id_of` reads xdg data only). A `sensitivity` windowrule *does* put them in the set, so they are redacted. The classifier clamps them to `private` (`ec-policy-eval/src/classify.rs:127`). |
| Capture indicator | A red bar, 48×8 px, nameless (`capture::indicator`). It lingers `INDICATOR_LINGER` after the last serviced frame. |

### Spec (sections opened)
- **COMP-02 §7.** Redaction replaces the window during composition. It never applies to
  the physical output. It is decided before GPU work and recorded in the pass list.
  Popups and subsurfaces inherit the parent's class. Every capture path must prove that a
  `secret` surface contributes no pixel.
- **COMP-02 §8.** A titlebar is redacted with its window. `capture_toplevel` is specified
  but unbuilt.
- **COMP-06 §3.** "A `secret` window in a shared screen is a placeholder, always."
  Active capture is shown in trusted UI, persistently. Agents never use these protocols.
- **COMP-10 §3.6.** The indicator must name the consumer, for portal and agent streams
  alike.
- **COMP-07 §2.** X11 windows are never `secret`, and a rule that tries is refused.
- **S-05 §2.** `secret` is redacted from output capture. For `private`, whole-output
  capture redacts nothing and records the region list.
- **S-05 §5.** A raise takes effect before the frame reaches a capture consumer. A
  downgrade waits out a grace period.
- **S-05 §7.** Notification content is `private` unless a rule says otherwise.
- **S-05 §8.** If the class cannot be computed, treat the target as `secret`.
- **Threat model.** S6 lists the capture paths. T5 says the default class is `private`
  and `secret` is for password managers and auth prompts.
- **COMP-11.** Opened only its headings. Its §6 says X11 can never be `secret`. No
  capture-specific rule was read.

### Spec and code disagreements (must be resolved, not picked)
1. **Indicator.** COMP-10 §3.6 says the indicator names the consumer. The code draws a
   nameless red bar.
2. **Picker.** COMP-06 §3 treats the portal's picker as part of consent. xdpw's picker is
   an out-of-tree UI we do not trust (ADR 0029).
3. **X11 and `secret`.** COMP-07 §2 refuses `secret` on X11, and the classifier agrees.
   Yet a `sensitivity secret` windowrule on an X11 window still redacts it from capture.
   That is tighter, not a leak, but the two paths disagree. **Resolved 2026-11-12
   (owner):** amend COMP-07 §2 (see Decision, "X11 password managers").
4. **`sensitivity private`.** `shell/rules.rs:416` matches `Sensitivity(_)` and ignores
   the argument, so `sensitivity private` redacts the window as if it were `secret`.
   S-05 §2 says `private` capture redacts nothing.
5. **No table.** S-05 §8 says that with no table, every surface is `secret`. The code
   applies that to agents only. That is defensible, but no spec says how a capture the
   human started should be treated (ADR 0093 already notes this).

## Options
1. **Keep it config-only.** Document `redact-app-id` and stop. It works today, but the
   human cannot see that it is on, nothing ships for real password managers, and toasts
   leak.
2. **A human-share mode in the compositor.** A redaction feature built on the existing
   single pass list, with a trusted share indicator and shipped default rules. It ships
   first over xdpw. The native portal comes later.
3. **Redact in the portal.** Build our own ScreenCast and redact there. This is a second
   redaction implementation, which ADR 0030 forbids. It would also miss `grim` and every
   other direct client.

## Decision
**Class rename (owner, 2026-11-11).** `private` is a misleading name, so the class
is renamed to `normal`: public / normal / secret. The old `private` keeps parsing
as a deprecated alias. The rename is a separate change, not part of this ADR's
milestones: it touches the spec's sensitivity-class definition (S-05, threat
model, COMP-11), policy files and KDL rule values (`sensitivity`,
`capture.redact-class`). Until it lands, code and spec text still say `private`;
this ADR uses "`normal` (formerly `private`)" for the class.

Option 2, built entirely on `capture_elements`/`classify`. No second redaction path is
added.

**Defaults per class.**
- `secret` is always redacted in every capture: shares, screenshots, and direct clients.
  No setting relaxes it (COMP-06 §3).
- `normal` (formerly `private`) is shown in human screen shares, as S-05 §2 says
  (owner, 2026-11-11: today's behaviour stays).
- A raise-only knob, `capture.redact-class "normal"` (formerly `"private"`, still
  accepted; policy.kdl, default `secret`),
  lets an owner redact more. Nothing can redact less.

**Placeholder.** The secret window stays out of the pass list. A themed card is drawn in
its geometry instead of plain black:
- **Decided 2026-11-12 (owner):** a glass card the size of the window, with diagonal
  stripes and the label "Private: Redacted" ("glass police tape"). The label word
  "Private" is the owner's wording for the tape, not the class name (now `normal`);
- no title and no app icon;
- it replaces the earlier padlock-card sketch.

Opacity is required. A backdrop blur samples only the pass list, and the secret window
is not in it, so a glass tile would be safe. But it would let a window that moves under
the placeholder show through, and that reads as a leak. Geometry and stacking still
leak. That is accepted (see Consequences).

**Share indicator.** A compositor-drawn pill replaces the red bar while a stream is
being serviced. It is trusted UI and cannot be captured.
- It names the consumer, by the exe basename of the bound client (the ADR 0027 identity).
- It shows "N windows hidden" and, on hover, the app names of the redacted windows.
  These appear on screen only, never in the share.
- A click opens the emergency panel, as today.
- It is not shown for the built-in screenshot (ADR 0093).

**"Share anyway".** **Decided 2026-11-12 (owner): dropped.** There is no in-share
override. An earlier answer the same day allowed a per-share trusted-UI prompt, but no
way to reach it fit (from the share pill, a chip on the window, or a dialog at share
start), so the owner withdrew it. COMP-06 §3's "always" stands unamended.

Removing the rule in `policy.kdl` remains the standing way to stop redacting an app.

**Shipped rules.** `packaging/etc/policy.kdl` gains anchored
`windowrule "sensitivity secret"` lines for native password managers, pinentry and
keyring apps. Candidates (app_ids **unverified**, to be checked against each package):
- `org.keepassxc.KeePassXC`
- `org.gnome.World.Secrets`
- `org.gnome.seahorse.Application`
- `org.kde.kwalletmanager`
- `pinentry-qt`, `pinentry-gtk`
- `Bitwarden`

**Decided 2026-11-12 (owner):** these rules ship ON by default in
`/etc/eclipse/policy.kdl`, not as commented examples.

These lines are a security default, so they get the same owner line-by-line review as
TCB code.

**X11 password managers.** **Decided 2026-11-12 (owner):** XWayland password managers
(1Password, possibly Bitwarden) ARE redacted in screen shares. COMP-07 §2 is amended so
an X11 window can be `secret` for capture redaction (code path 3 stays). The amendment
must say plainly that redaction covers compositor capture only: other X11 clients can
still read the window through the X server (COMP-07 §2's existing limit).

**Toasts during a share.** **Decided 2026-11-12 (owner):** every behaviour is
configurable in Settings: show, redact, hold until the share ends, or straight to
history. Default: **redact**, with the same glass police tape drawn over the toast in
the shared stream. The human still sees the toast normally.

The compositor emits a `capture_active` control-socket event (start/stop, no consumer
detail) so ec-toasts can hold or divert banners. The bell history keeps them as usual.
`hide-layer` is capped at icon size by ADR 0056, and raising that cap needs its own ADR,
so redaction of a toast is a placeholder over its layer rect in the capture pass list.
The built-in screenshot does not set `capture_active`.

**Content-type hints.** `wp_content_type_v1` is a scheduling hint (photo/video/game),
not a sensitivity signal. It is not used. Wayland has no DRM/HDCP protected-content
protocol, and none is invented here.

**Screenshots vs streams.** They share the same pass list, the same rules and the same
placeholder. Only streams get the indicator and the toast hold.

**Hot path.**
- `classify` stays a set lookup per window per serviced frame, inside the capture
  composite. That is not the input or policy hot path.
- The indicator's redacted-name list is rebuilt only when the `Redacted` app_id set
  changes. The pill is a cached texture, re-rasterised on change.
- The on-screen frame gains no allocation.
- The existing per-capture `Vec`s in `capture_elements` are not made worse.

**Failure mode.** Anything that leaves a window's class unknown while a frame is being
built redacts it whole:
- a dead window: `class_of` already returns `Secret`;
- a recompute error;
- a table swap mid-build.

S-05 §8 applies to capture as well as to agents. A window that has no `wl_surface` yet
(an unassociated X11 window) produces no elements at all. That is believed safe and
**unverified** by test.

**Portal.** The feature ships first on xdpw. Redaction is compositor-side and does not
care who the consumer is, so whole-output shares are covered the day the rules ship.

The native ScreenCast/Screenshot in `ec-portal` comes later. It would add:
- a source picker in trusted UI. That is a new COMP-10 surface and needs an ADR, because
  the surface list is closed (ADR 0053);
- per-session consent that turns `capture.allow` into a "may ask" permission
  (discharges ADR 0029);
- per-output share, which works today;
- per-window share, which needs an `ext_foreign_toplevel_image_capture_source` and
  `capture_toplevel` (COMP-02 §8).

Choosing a `secret` window as the sole source of a share yields placeholder frames for
the whole session. It never ends the stream mid-call. S-05 §5 rule 5's terminate-on-raise
is for agent `capture.stream`.

## Gap list (milestone order)
| # | Slice | Where | Owner |
|---|---|---|---|
| M0 | **Smallest shippable.** Default `sensitivity secret` rules for the verified password-manager app_ids in `packaging/etc/policy.kdl`, plus a CONFIG/README note on sharing via xdpw. Works today and needs no code. | packaging | main, owner review |
| M1a | Fix disagreement 4: `sensitivity private` must not insert into `state.sensitive`. | `shell/rules.rs` | main (small) |
| M1b | Glass police-tape placeholder in `placeholders()`; pixel and record tests unchanged in what they cover. | `render/capture.rs` | **TCB**, main |
| M1c | Share pill: consumer name, hidden-window count, cached texture. COMP-10 §3.6 text amended to match. | `trusted_ui/indicator.rs`, `render/capture.rs` | **TCB**, main |
| M2a | `capture_active` event on the control socket. | `ec-abyss-wire` EVENTS (gate.rs is owner review), `ipc/` | backend agent |
| M2b | Toast share behaviour (show / redact / hold / history, Settings), and a check that bar chips never show a `secret` title. | `ec-toasts`, `ec-hyperion-bar` | **frontend agent** (visual), backend (event plumbing) |
| M3 | Per-window share: foreign-toplevel capture source, `capture_toplevel`, and a `secret` source shown as placeholder frames. | `protocols/standard/`, `render/capture.rs` | backend + **TCB** for capture.rs |
| M4 | Native ScreenCast/Screenshot portal with a trusted-UI picker; retire the xdpw allowlist line. | `ec-services/ec-portal`, `trusted_ui/` | backend + **TCB** + frontend; spec amendment |
| M5 | S-05 §5 dynamic raise on title or URL change (for example, a browser tab navigating to a bank login) once the `policyd` table is live. | `policy/classes.rs` | **TCB** |

## What it does not protect
- A camera or a person pointed at the monitor.
- X11 clients reading each other inside XWayland (COMP-07 §2). An XWayland screenshot
  tool reads X11 windows directly, so an X11 password manager redacted in a share is
  still readable through the X server.
- A process that borrows an allowlisted exe name (ADR 0027 identity is containment, not
  authentication).
- Secrets inside a window that is not classified `secret`: a password in a terminal, an
  unclassified browser tab. Node-level redaction needs COMP-09 trees.
- A browser sharing its own tab from inside its own process.
- Clipboard, audio, and the placeholder's geometry, position and timing, which reveal
  that a secret window exists.
- Toast previews and titles in bar chips until M2.

## Consequences
- Whole-output sharing with password managers redacted works after M0 with no code.
  That is a desktop feature built from the agent stack's parts.
- The human can see that redaction is on, and what it hid, only after M1c.
- No "share anyway": a `secret` window cannot be shown in a share except by removing
  its rule.
- We owe:
  - tests: placeholder card pixels; the indicator is absent from captures; a toast
    is held while `capture_active` is set; an unassociated X11 window contributes no
    pixel;
  - the CONFIG.md entries for `capture.redact-class` and the toast-during-share setting;
  - the COMP-10 §3.6 and COMP-07 §2 amendments, as the owner decides.

## Open owner questions
1. ~~In human shares, are `private` windows shown, or redacted by default?~~
   **Decided 2026-11-11 (owner):** `normal` (formerly `private`) windows are shown.
2. ~~"Share anyway"?~~ **Decided 2026-11-12 (owner):** dropped; no in-share override.
   Removing the rule is the only way to share a `secret` app. See Decision.
3. ~~Placeholder look?~~ **Decided 2026-11-12 (owner):** glass police tape, "Private:
   Redacted", no title, no icon.
4. ~~X11 password managers?~~ **Decided 2026-11-12 (owner):** redacted; amend COMP-07 §2.
5. ~~Toasts during a share?~~ **Decided 2026-11-12 (owner):** configurable; default
   redact with police tape.
6. ~~Shipped password-manager rules?~~ **Decided 2026-11-12 (owner):** ON by default.
7. Should the share pill list redacted apps by name on hover, or show only a count?

## Revisit when
The `policyd` table goes live (class_of then feeds human captures), `eclipse_capture_v1`
lands (agent and human shares diverge), or the native ScreenCast portal replaces xdpw.
