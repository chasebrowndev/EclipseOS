# Changelog

Every release has a name and a full set of patch notes, written like a game
update: what's new, what changed, what was fixed, and what to know before you
update. The newest release is at the top. Older releases stay below it.

**How an entry is written**

- A codename and a date. The version number is set when the release is tagged.
- *Headline* says what the release is about, in a few lines.
- *New*, *Changed* and *Fixed* describe what you will notice, not the commit log.
- *Upgrade notes* lists anything that moves under you: defaults, config,
  files you may need to re-sign or re-install.
- *Known issues* is honest. If it is not verified, it says so.
- Spec and ADR numbers are cited where they exist.

---

## ASCENSIONISM

**Status:** in preparation, not yet tagged. Date: 2026-11-20.

### Headline

ASCENSIONISM is the release where EclipseOS stops looking like a dark theme with
blur and becomes a material. Glass is the same everywhere you look, in dark and
in light: the launcher, Settings, the lock screen, the login screen, toasts and
even the compositor's own security prompts. One typeface, Onest, runs through all
of it, from the logo to the window titlebars. Settings was rebuilt around an
Overview page with real navigation, drawn icons and live previews. The light
scheme is now paper white.

### New

**Glass**

- **Lens glass in most places.** Small controls (pills, key caps, rows, toast
  chips) use the lens material: a narrow bright rim, refraction that bends the
  backdrop near the edge only, and a soft contact shadow. Large surfaces stay
  clear in the middle.
- **Stacked glass reads as layers.** A pane wholly inside a larger one draws as a
  cell: its own fill, gradient, rim and contact shadow, with no second backdrop
  sample. A control on a card on a window no longer looks like one flat sheet.
- **Size-aware edge shading.** Edge darkening and lightening follow one
  continuous rule based on a pane's effective size, so long skinny panes, short
  fat panes and big windows all read as one family. Small controls look as they
  did.
- **Bend scales with size.** A big sheet barely bends the backdrop, a key cap
  bends it fully. The launcher and toasts line up with the wallpaper behind them.
- **Faster glass rebuilds.** Blur pyramids are kept per depth tier, rebuilds are
  limited to damaged areas, and dragging a window keeps a margin so it does not
  invalidate the cache. These ship as the glass pull request stack, #363 onward.
- **Panes can be clipped.** `eclipse_glass_v1` version 2 adds `set_clip`, so a
  scrolled list can cut glass panes at its viewport with a straight edge.
  Clients on version 1 keep working and fall back to intersecting the pane.
  (ADR 0127)
- **The compositor's trusted prompts are glass too.** The prompt that asks you to
  confirm a command, a secret or a permission is drawn as lens glass, with a lens
  panel and lens buttons. If glass cannot be drawn it falls back to an opaque
  panel, so a prompt always shows. (COMP-10 §5; its ADR is still to be numbered)
- **Lock screen glass.** The session lock draws real glass over your wallpaper
  and nothing else, by construction. A scan test pins that no window content can
  reach it. (ADR 0129)

**Light mode**

- **Paper white.** Light is `#faf9f6`, barely warm, instead of cream. Surfaces,
  frost and titlebar grounds follow it.
- **It looks like glass now.** Light glass lets the wallpaper through (a lighter
  tint and a thinner white veil), has an ink hairline on the outside edge with a
  white specular just inside it, and its cards are translucent instead of grey
  slabs.
- **Light version of the "Private" cover.** The redaction card shown for secret
  windows has a paper version. It is still fully opaque, so nothing shows
  through. (ADR 0106)

**One typeface**

- **Onest everywhere.** The shell, installer, Fog, the Firefox glass extension,
  Apotheosis's chrome, the compositor's titlebars, menus, overview and HUD, and
  the welcome screen all use Onest. Digits are tabular, so clocks and columns
  stay aligned. The terminal cell grid keeps a monospace face.
- **A new logo.** The wordmark is redrawn in Onest, with the eclipse still
  forming the O. The welcome screen's hand-off onto the O is intact. Boot and ISO
  splashes are regenerated.
- **Trusted prompts: Onest for words, mono for evidence.** Headings, warnings,
  labels and buttons are Onest. The command, path, phrase or fact you must read
  character by character stays in fixed cells, where look-alikes cannot hide.
- **A check that keeps it that way.** `abyss/ci/one-font.sh` fails when a
  non-grid crate names a monospace family.

**Settings**

- **Overview.** A landing page with a device card (host, OS, kernel, display,
  network, battery, uptime), Light, Dark and Automatic cards that are miniatures
  of the window, quick switches and your recently opened pages.
- **Five groups, real navigation.** Look & feel, Devices, System, Privacy &
  accounts, Apps & add-ons. Every section has a drawn icon tile. Pages are split
  into titled cards with captions, and rarely changed settings sit behind an
  Advanced expander.
- **Search that explains itself.** Results show where a setting lives
  (`Effects › Blur & glass › Blur`) and cover pages as well as settings. Jumping
  to a hidden setting opens its Advanced expander.
- **Art and live previews.** Each page has a drawn header. Layout, Gaps, Borders
  and Rounding show a live window set, and Blur & glass shows a live glass pane
  that follows the sliders.
- **Lens controls.** Lens strength and rim light, plus bevel and dispersion under
  advanced.
- **Keyboard.** Tab, arrows, Enter and Space work on the sidebar, cards and
  quick switches, with a crisp focus ring.
- **Narrow windows.** Under 640 px the sidebar becomes a page list with a back
  control, and search works there too.

**Login, lock and widgets**

- **Glance.** The login and lock screens are built from content-sized tiles on a
  shared layout engine (`ec-layout`). The Battery Glance shows each device as an
  iOS-style rounded-square gauge, and the tile is as big as its content.
- **Greeter light scheme.** The greeter reads correctly in light.

**Project**

- **Community files.** A code of conduct, issue forms and a pull request
  template.
- **Looser PR rules.** Spec citations are optional, outside authors can open pull
  requests, the CLA wording is plainer, and the 40-minute wlcs run is advisory.
  The owner is still the only person who merges. TCB paths still need the owner's
  line-by-line review.

### Changed

- Default window tint in light drops from .455 to .35 (`LIGHT_TINT_SCALE` .50).
- `decoration.surface.inset-fill` defaults to 0.05 (was 0.03), so insets lift
  off their card more clearly.
- Trusted prompt scrim is .45 (was heavier) so the glass behind it reads. The
  owner has not yet signed this value off.
- Compositor text is proportional: panels are the same pixel width but hold more
  characters, and line boxes are a pixel or two taller.
- Apotheosis's chrome follows the compositor's colour scheme under glass.
- The interface font cache in Apotheosis rebuilds when `font.fallback` changes.

### Fixed

- **The launcher sheet had no glass.** In glass mode the compositor blurs only
  under panes, and the sheet was a plain fill. It is real glass now, along with
  the cheatsheet that shares its code.
- **The lock screen froze after its first frame.** Frame callbacks, presentation
  feedback and fifo/commit-timing release were never serviced for the lock
  surface. They are now.
- **Sidebar glass rows landed in the wrong place** when the sidebar scrolled.
- **Apotheosis's light tab strip was near-black** with unreadable titles under a
  light compositor.
- **Config error carets** were placed by counting mono cells and are now drawn
  under the error span itself.
- **Narrow-mode search** left its results on screen after you picked one.
- Light selected rows no longer render 16% darker than their card.

### Upgrade notes

- The `decoration.surface.inset-fill` default changed. If you set it yourself,
  nothing changes for you.
- Re-sign the Firefox extension with `packaging/firefox/sign.sh`. The shipped
  `glass.xpi` no longer matches `browser/firefox-glass`.
- The ISO must install Onest. The files are in `assets/fonts`; check the ISO
  package list ships them.
- Welcome-screen CJK greetings use Noto Sans CJK from the system, as Onest has no
  CJK glyphs.
- Branch protection on `main` now requires an approving review from a code
  owner. The attribution check's name changed to `no agent attribution`.

### Known issues

- Some windows may still show a square blur corner behind a rounded rim. It was
  reported and could not be reproduced on the release branches. Treat it as open.
- The dark launcher reads as smoked glass more than clear glass.
- Compositor Greek, maths and arrow glyphs still come from the monospace
  fallback, not Onest.
- Tab does not move focus into text fields or sliders in Settings. Taskbar,
  Terminal and Animations pages have no Advanced expander yet.
- Not yet measured: GPU cost per glass preset. Not yet tested: real DRM, greetd,
  PAM and multi-output sessions.
- Apotheosis tab and palette overlays do not render in headless captures.
- The lock screen shows the wallpaper only in `glass` blur mode. In the other
  modes it keeps an opaque ground, by design (ADR 0129).
- The README has no picture of the trusted prompt as glass yet.
