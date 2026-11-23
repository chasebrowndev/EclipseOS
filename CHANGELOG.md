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

## ASCENSIONISM, update v0.4.2

**Status:** in review. Date: 2026-11-22.

### Headline

The first release run on a real hybrid-graphics laptop with a portrait monitor.
Displays driven by the second GPU show a proper picture, rotated displays no
longer lose half the screen, and windows can be dragged between displays and
land where you drop them.

### New

- **Per-display wallpaper can be set over the control socket.** The
  `wallpaper { output "<connector>" { … } }` override already worked in the
  config file; `set_config_collection` can now create, change and remove it live,
  keeping the rest of the file (comments, order) as written. A display that
  should look different from the others, such as a portrait monitor, no longer
  needs a hand-edited file. (COMP-13 §1.4)
- **Dragging a window across displays.** A window that straddles two displays is
  drawn on both while you drag it, instead of being cut at the seam and vanishing
  once it is fully across. A tiled window can be dropped onto another display's
  layout, the drop guides and the ghost follow the pointer to that display, and
  the edge snap and the screen-edge tile in desktop mode use the display the
  window is on. A display that is switched off is never a drop target, even if it
  went off in the middle of the drag. Output focus follows the pointer again as
  soon as the drag ends. (COMP-05 §5.1, COMP-04 §7)

### Fixed

- **A second GPU's display showed the top row repeated.** On a laptop where an
  external monitor hangs off the discrete GPU, the picture was one row stretched
  down the whole screen (a patterned wallpaper looked like vertical stripes, the
  bar and cursor were missing). The frame handed to the second GPU was flagged
  upside-down, which OpenGL ES draws as a single repeated row, on every driver
  tried. It is now handed over as it is. (COMP-01 §4, ADR 0086)
- **A rotated display on the second GPU showed half the screen and flickered.**
  Only part of the frame was refreshed, so the rest kept stale contents that
  flipped between two buffers. The changed regions are now mapped through the
  display's rotation before they are copied.

### Upgrade notes

- Nothing moves under you. A write over the socket that would leave the config
  file byte-identical no longer rewrites it, reloads, or sends a `config` event,
  and reports `applied: false`.

### Known issues

- Glass panes (the bar's body) are not drawn on a display rotated by 90 or 270
  degrees, so the bar looks plain there. Rounded corners and the border fall back
  to square on those displays, as before. (COMP-02 §9)
- A display on the second GPU is composited on the first and copied; the faster
  path that scans the first GPU's buffer out directly is refused by the NVIDIA
  plane we tested, so every second-GPU display takes the copy. `misc.secondary-compositing`
  (the second GPU composites its own displays) was not measured.
- Right after the login screen hands over, the compositor can lose a short race
  for the display and start on a low-resolution fallback, or the second GPU's
  display can come up empty until a reboot. Seen once each; not fixed.
- The drag changes are covered by tests on a synthetic two-display layout, one of
  them a portrait display. They have not been watched on a real multi-display
  screen.
- A small wallpaper image set on a large or portrait display is scaled up to
  cover it and looks soft. That is how `fill` works; give that display its own
  image.

---

## ASCENSIONISM, hotfix v0.4.1

**Status:** released as v0.4.1. Date: 2026-11-22.

### Fixed

- **Text showing along the bar's edge.** The stronger lens bend on the bar pulled
  sharp pixels from just outside it, so a terminal under the bar showed as a
  coloured smear along its rim. The rim now samples the blurred backdrop where it
  reaches outside the pane, so the bend and rim stay and the text does not. The
  pane lens cap is 0.4 (about 16 px on a 40 px pill). (COMP-02 §9)
- **Login screen on real hardware.** On one laptop the glass greeter showed black
  controls and took no typing. The cause was not found. The compositor now resets
  an output whose page flip never completes, the greeter opens a lone account on
  its own so typing needs no click, and the journal records the first page flip,
  keyboard focus and first key event (never their content).

### Known issues

- The glass greeter failure above was seen once and not reproduced. If it
  happens again, `journalctl -b -t ec-abyss -t greeter` now says where it stopped.

---

## ASCENSIONISM

**Status:** released as v0.4.0. Date: 2026-11-21.

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

**Installer and graphics**

- **The installer is glass.** The setup wizard sits on a glass sheet over the new
  eclipse wallpapers (dark and light, generated by
  `packaging/backgrounds/gen-eclipse.py`), and `ec-setup` is allowed to use glass
  in the live session. (D-03 §5)
- **A graphics profile that picks itself.** `decoration.profile` is `auto` by
  default and chooses `minimal`, `low`, `balanced` or `full` from the GPU, RAM,
  CPU count and screen size, so weak and virtual machines stay smooth. Override
  it with `ec-ctl graphics` or Settings > Graphics. (D-03 §5.1)
- **Erasing a disk takes one decision.** You type the disk's full name on the
  Review page, and that is the consent. The second compositor prompt is gone, and
  the helper still refuses to touch a disk unless the typed name matches it
  exactly. (D-07 §4.2, §6)

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

- Windows and the bar are one material: translucent window bodies use the same
  rim and edge bend as the bar, the pane lens cap goes from 0.16 to 0.45 (the bar
  bends about 18 px instead of 6), and the dark rim is a little brighter.
- Adaptive glass tint defaults to 0.3 (was 0.5): backdrops such as a bright
  corona stay visible through terminals, and measured text contrast stays above
  4.5:1 in dark. (ADR 0092)
- Apotheosis's automatic opacity is 0.55 (was 0.82).
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

- **The ISO would not build.** A stray `abyss.kdl` on the medium collided with
  the one from `eclipseos-meta`; the medium no longer ships its own.
- **Installer defects found by a full QEMU install:** the Welcome button label,
  retrying after a bad entry, install progress that looked frozen, and a few
  cosmetic issues. The launcher cuts long names with an ellipsis, and the greeter
  button reads "Sign in".
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

- Erasing a disk now needs only the typed disk name. The compositor's confirm
  prompt is no longer part of the install path.
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
