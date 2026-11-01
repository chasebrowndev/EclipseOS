// SPDX-License-Identifier: AGPL-3.0-only
//
// Shared by the background script and the options page.
// Classic script (no modules) so the manifest can list it ahead of
// background.js. Everything lives on the single global `Glass`.
//
// Colour values below mirror shell/crates/ec-ui/src/tokens.rs. The theme is
// opaque on purpose: Firefox on Wayland submits an opaque toplevel. Glass
// surfaces are painted in reserved KEY colours (see keyColors) and abyss makes
// exactly the pixels of those colours translucent, with blur behind; text and
// icons are other colours and stay opaque.

"use strict";

var Glass = (function () {
  const api = typeof browser !== "undefined" ? browser : chrome;

  // ec-ui accents: ACCENT, CONNECTED, OK, DANGER, NEUTRAL.
  const ACCENTS = [
    { id: "gold", name: "Gold", value: "#f2c33c" },
    { id: "blue", name: "Blue", value: "#4f9fe0" },
    { id: "green", name: "Green", value: "#7fae5e" },
    { id: "red", name: "Red", value: "#e0553f" },
    { id: "neutral", name: "Neutral", value: "#96918a" },
  ];

  const REGIONS = [
    { id: "tabstrip", name: "Base glass", hint: "frame, tab strip, toolbar, Home ground" },
    { id: "toolbar", name: "URL field and tab", hint: "URL bar, selected tab" },
    { id: "sidebar", name: "Sidebar and cards", hint: "bookmarks, history, Home cards" },
    { id: "popups", name: "Popups and menus", hint: "context menu, URL dropdown" },
  ];

  const DEFAULTS = {
    mode: "auto", // auto | dark | light
    tint: "#12100b", // SURFACE_0
    strength: 35, // 0..100, glass opacity; 35 gives about 35% on the base glass
    accent: "#f2c33c",
    regions: {
      tabstrip: { lift: 20, accent: true },
      toolbar: { lift: 45, accent: true },
      sidebar: { lift: 30, accent: true },
      popups: { lift: 55, accent: true },
    },
  };

  const LIGHT_BASE = "#efeadf";
  const INK = "#1a1712";
  // Fraction of white a lift of 100 at strength 100 mixes into the tint.
  const LIFT_SPAN = 0.16;

  // ---- colour math -------------------------------------------------------

  function parseHex(hex) {
    const h = String(hex).replace("#", "");
    const n = parseInt(h.length === 3 ? h.replace(/./g, "$&$&") : h.slice(0, 6), 16);
    if (Number.isNaN(n)) return [0, 0, 0];
    return [(n >> 16) & 255, (n >> 8) & 255, n & 255];
  }

  function toHex(rgb) {
    return (
      "#" +
      rgb
        .map((v) => Math.max(0, Math.min(255, Math.round(v))).toString(16).padStart(2, "0"))
        .join("")
    );
  }

  /** `fg` over `bg` at alpha `a`; opaque result. */
  function mix(bg, fg, a) {
    const b = parseHex(bg);
    const f = parseHex(fg);
    return toHex(b.map((v, i) => v * (1 - a) + f[i] * a));
  }

  function clamp(v, lo, hi) {
    return Math.max(lo, Math.min(hi, v));
  }

  function merge(stored) {
    const s = stored || {};
    const regions = {};
    for (const r of REGIONS) {
      regions[r.id] = Object.assign({}, DEFAULTS.regions[r.id], (s.regions || {})[r.id]);
    }
    const out = Object.assign({}, DEFAULTS, s, { regions });
    // Leftovers from v1 (the new-tab override is gone).
    delete out.newtab;
    delete out.searchUrl;
    delete out.pinned;
    return out;
  }

  // ---- palette -----------------------------------------------------------

  function systemIsDark() {
    return !window.matchMedia || window.matchMedia("(prefers-color-scheme: dark)").matches;
  }

  function resolveDark(settings) {
    if (settings.mode === "dark") return true;
    if (settings.mode === "light") return false;
    return systemIsDark();
  }

  // ---- key colours -------------------------------------------------------

  const KEY_IDS = ["base", "field", "popup", "panel"];
  // Region whose lift/opacity drives each key.
  const KEY_REGION = { base: "tabstrip", field: "toolbar", popup: "popups", panel: "sidebar" };
  // Each key nudges a different channel so equal intended colours still differ.
  const KEY_CHANNEL = { base: 0, field: 1, popup: 2, panel: 0 };

  function nudge(rgb, ch, dir) {
    const out = rgb.slice();
    out[ch] = clamp(out[ch] + (out[ch] >= 255 ? -1 : dir), 0, 255);
    return out;
  }

  function dist(a, b) {
    return Math.max(...a.map((v, i) => Math.abs(v - b[i])));
  }

  /**
   * The 4 reserved key colours, in KEY_IDS order, each `{id, color, opacity}`.
   *
   * abyss keys on exact pixel colour (+-1/255 for rounding), so a glass
   * surface must be a colour nothing else on a page is likely to be. Each key
   * is the intended colour moved by exactly one unit in one channel: invisible
   * to the eye, yet not the plain tint a page may paint. Intended colours are
   * first pushed apart (lighter on dark, darker on light) until every key and
   * the plain tint are at least 2 units apart, so the +-1 tolerance cannot
   * confuse one key with another. The base key stays a pure +1 nudge of the
   * tint. Opacity is Strength scaled by the key's region lift.
   */
  function keyColors(settings, dark) {
    const s = merge(settings);
    const tint = parseHex(dark ? s.tint : LIGHT_BASE);
    const strength = clamp(s.strength, 0, 100) / 100;
    const span = dark ? LIFT_SPAN : LIFT_SPAN * 2.5;
    const white = [255, 255, 255];
    const placed = [tint]; // plain tint is reserved too
    const keys = [];
    for (const id of KEY_IDS) {
      const lift = clamp(s.regions[KEY_REGION[id]].lift, 0, 100) / 100;
      let want = id === "base" ? tint.slice() : tint.map((v, i) => v + (white[i] - v) * lift * span);
      want = want.map(Math.round);
      const sign = dark ? 1 : -1;
      let nudged = nudge(want, KEY_CHANNEL[id], 1);
      for (let n = 0; n < 40 && id !== "base" && placed.some((p) => dist(p, nudged) < 2); n++) {
        want = want.map((v) => clamp(v + 2 * sign, 0, 255));
        nudged = nudge(want, KEY_CHANNEL[id], 1);
      }
      if (id === "base" && dist(tint, nudged) < 1) nudged = nudge(tint, 0, -1);
      placed.push(nudged);
      const opacity = clamp(strength * (0.8 + lift), 0.05, 1);
      keys.push({ id, color: toHex(nudged), opacity: Math.round(opacity * 100) / 100 });
    }
    return keys;
  }

  /**
   * Every colour the extension paints, opaque `#rrggbb`, for one scheme.
   * `k` scales the ink-over-ground steps: a light ground needs lighter steps
   * than a dark one to read as the same material.
   */
  function palette(settings, dark) {
    const s = merge(settings);
    const fg = dark ? "#ffffff" : INK;
    const k = dark ? 1 : 0.7;
    const keys = keyColors(s, dark);
    const col = (id) => keys.find((x) => x.id === id).color;
    // Glass surfaces are exactly their key colour.
    const g = {
      tabstrip: col("base"),
      toolbar: col("base"),
      field: col("field"),
      popups: col("popup"),
      sidebar: col("panel"),
      newtab: col("base"),
      cards: col("panel"),
    };
    const accent = s.accent;
    // ACCENT_TEXT is the accent lifted a step; on light it is darkened.
    const accentText = dark ? mix(accent, "#ffffff", 0.1) : mix(accent, INK, 0.35);
    const on = (id) => !!s.regions[id].accent;
    const step = (bg, a) => mix(bg, fg, a * k);

    return {
      dark,
      accent,
      accentText,
      text: fg,
      // Contrast is against the tint (the glass over a dark blur), not the wallpaper.
      text2: step(g.toolbar, 0.64 / k),
      text3: step(g.toolbar, 0.4 / k),
      ground: g,
      keys,
      on,
      step,
      base: dark ? "#0b0906" : LIGHT_BASE,
    };
  }

  /** The object `browser.theme.update()` takes. */
  function buildTheme(settings, mode) {
    const s = merge(settings);
    const dark = typeof mode === "boolean" ? mode : resolveDark(s);
    const p = palette(s, dark);
    const { step, ground: g, on } = p;
    const t = g.toolbar;
    const strip = g.tabstrip;
    const pop = g.popups;
    const side = g.sidebar;

    const f = g.field;

    return {
      colors: {
        frame: strip,
        frame_inactive: strip,
        frame_incognito: strip,
        tab_background_text: p.text2,
        tab_selected: f,
        tab_text: p.text,
        // Selection is a crisp neutral rim, never gold; the accent lives on the
        // loading throbber only.
        tab_line: step(strip, 0.45),
        tab_loading: on("tabstrip") ? p.accent : p.text2,

        toolbar: t,
        toolbar_text: p.text,
        toolbar_field: f,
        toolbar_field_text: p.text,
        toolbar_field_border: step(f, 0.1),
        // The focused field and its results dropdown (drawn over the page) are the popup key.
        toolbar_field_focus: pop,
        toolbar_field_text_focus: p.text,
        // Focus = brighter fill + crisp rim. Never the accent.
        toolbar_field_border_focus: step(pop, 0.5),
        toolbar_field_highlight: step(pop, 0.22),
        toolbar_field_highlight_text: p.text,
        toolbar_top_separator: step(t, 0.07),
        toolbar_bottom_separator: step(t, 0.055),
        toolbar_vertical_separator: step(t, 0.1),
        icons: p.text,
        icons_attention: on("toolbar") ? p.accent : p.text,
        button_background_hover: step(t, 0.09),
        button_background_active: step(t, 0.14),
        bookmark_text: p.text,

        popup: pop,
        popup_text: p.text,
        popup_border: step(pop, 0.1),
        popup_highlight: step(pop, 0.12),
        popup_highlight_text: on("popups") ? p.accentText : p.text,

        sidebar: side,
        sidebar_text: p.text,
        sidebar_border: step(side, 0.08),
        sidebar_highlight: step(side, 0.1),
        sidebar_highlight_text: on("sidebar") ? p.accentText : p.text,

        // Firefox Home: ground and cards are glass keys too.
        ntp_background: g.newtab,
        ntp_text: p.text,
        ntp_card_background: g.cards,
      },
      properties: {
        color_scheme: s.mode === "auto" ? "system" : dark ? "dark" : "light",
      },
    };
  }

  /** Paint CSS custom properties on a page so it follows the same palette. */
  function applyPageVars(root, settings, mode) {
    const s = merge(settings);
    const dark = typeof mode === "boolean" ? mode : resolveDark(s);
    const p = palette(s, dark);
    root.dataset.scheme = dark ? "dark" : "light";
    const set = (k, v) => root.style.setProperty(k, v);
    set("--accent", p.accent);
    set("--accent-text", p.accentText);
    set("--base", p.ground.newtab);
    set("--surface-0", p.step(p.ground.newtab, 0.025));
    set("--surface-1", p.step(p.ground.newtab, 0.045));
    set("--surface-2", p.step(p.ground.newtab, 0.065));
    return p;
  }

  // ---- storage -----------------------------------------------------------

  async function load() {
    const got = await api.storage.sync.get("settings");
    return merge(got.settings);
  }

  async function save(settings) {
    await api.storage.sync.set({ settings });
  }

  /** Calls `fn` once now and whenever settings or the OS scheme change. */
  function watch(fn) {
    const run = () => load().then(fn);
    run();
    api.storage.onChanged.addListener((changes, area) => {
      if (area === "sync" && changes.settings) run();
    });
    if (window.matchMedia) {
      window.matchMedia("(prefers-color-scheme: dark)").addEventListener("change", run);
    }
  }

  return {
    ACCENTS,
    REGIONS,
    DEFAULTS,
    merge,
    mix,
    palette,
    buildTheme,
    keyColors,
    applyPageVars,
    resolveDark,
    load,
    save,
    watch,
    api,
  };
})();
