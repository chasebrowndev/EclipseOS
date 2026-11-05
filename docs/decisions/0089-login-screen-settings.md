<!-- SPDX-License-Identifier: AGPL-3.0-only -->
# 0089 — The machine chooses its login screen
Status: accepted
Date: 2026-11-04
Deciders: chase (owner), Claude (advisory)

## Context
The login screen was fixed when the package was built. The owner wants a real user
to change the wallpaper, colours, greeting and clock from Settings, and rules out
custom CSS.

The greeter runs as `greeter`, which cannot read `/home`. `/etc/eclipse` is image-owned
and nothing may write it (D-01 §4.1). The login screen is the only way in, so a bad
choice must not be able to lock anyone out.

## Options
1. Generate the files in `/etc/eclipse/greetd` as root. Breaks D-01 §4.1.
2. Store a user's own CSS and image in `/var/lib/eclipse`. Root and the greeter would
   parse whatever the user supplied.
3. Store a small validated settings file and a PNG root encoded itself, and render
   them over the shipped files when the greeter starts. Chosen.

## Decision
**Storage.** `/var/lib/eclipse/greeter/settings.toml` and `wallpaper.png`, root-owned,
0644. Machine state, so it survives an image swap.

**Settings.** Whether to use the stored wallpaper; four theme colours, which must pass
`ec_theme::validate`; a greeting of at most 120 printable characters; a clock preset,
never a free format, because a bad strftime string could break the screen. Everything
is optional. The file is a strict flat subset of TOML, parsed by hand: any unknown
key, repeat or bad value fails the whole file. This adds no TOML dependency to a root
process. The only string that needs escaping has already had control characters
refused.

**Writing.** `/usr/lib/eclipse/ec-greeter-apply`, run through pkexec under
`org.eclipse.greeter.apply`, `auth_admin` with no keep, active session only (D-07 §6).
It takes exactly `apply` or `reset`. `apply` reads a size-capped frame from stdin:
settings text and raw RGBA, at most 3840x2160. The user's process decodes the image;
root only encodes raw pixels, so root never parses a user's image. Root writes its own
serialisation of the settings, never the input bytes. Writes are atomic and do not
follow links.

**Reading.** `/usr/lib/eclipse/ec-greeter-run` replaces `regreet` in the greetd
commands. As `greeter`, it renders `regreet.toml` and `regreet.css` into its runtime
directory, then execs regreet, keeping the pid the kiosk compositor watches. Rendering
at start means a package update to the CSS's shapes still applies. Any problem (bad
settings, an image that does not decode, a missing line in a shipped file, a panic)
gets a journal line and the shipped files, all or nothing.

**Colours.** `regreet.css` is written in terms of its `@define-color` lines, and
rendering replaces only those.

**Package.** `eclipseos-greeter` owns all of this and the greetd config, and has no
edge to any desktop package. `eclipseos-meta` depends on it. Removing a desktop package
used to take `-meta` and so the graphical login with it (PKG-02).

## Consequences
- `/var/lib/eclipse/greeter/settings.toml` is world-readable, as anything the greeter
  shows before login must be. The greeting is public by nature.
- Setup does not seed this: defaults apply until someone changes something.
- Whoever authenticates decides what the login screen shows every user, including a
  greeting that imitates a system message. Admin rights already allow that.
- The wallpaper shipped has the card's blur baked in, because GTK4 has no backdrop
  filter. A user's image does not, so the card's legibility over a bright image is a
  matter for the Settings page, which does the conversion.

## Revisit when
regreet changes its config or stops reading `--config` and `--style`, or someone wants
per-user login screens.
