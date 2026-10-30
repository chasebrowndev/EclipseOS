// SPDX-License-Identifier: AGPL-3.0-only
//
// Shared by the background script, the options page and the new-tab page.
// Classic script (no modules) so the manifest can list it ahead of
// background.js. Everything lives on the single global `Glass`.
//
// Colour values below mirror shell/crates/ec-ui/src/tokens.rs. The theme is
// opaque on purpose: Firefox on Wayland submits an opaque toplevel, so the
// compositor (abyss) applies alpha + blur to the top strip of the window, and
// these colours are what that alpha is applied to.

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
    { id: "tabstrip", name: "Tab strip", hint: "frame, tabs" },
    { id: "toolbar", name: "Toolbar", hint: "URL bar, buttons" },
    { id: "sidebar", name: "Sidebar", hint: "bookmarks, history" },
    { id: "popups", name: "Popups and menus", hint: "context menu, URL dropdown" },
    { id: "newtab", name: "New tab", hint: "ground of the new-tab page" },
  ];

  const DEFAULTS = {
    mode: "auto", // auto | dark | light
    tint: "#12100b", // SURFACE_0
    strength: 60, // 0..100, how far surfaces lift off the tint
    accent: "#f2c33c",
    newtab: true,
    // A WebExtension without the "search" permission cannot use the user's
    // default engine, so the new-tab box searches this template.
    searchUrl: "https://duckduckgo.com/?q=",
    regions: {
      tabstrip: { lift: 20, accent: true },
      toolbar: { lift: 45, accent: true },
      sidebar: { lift: 30, accent: true },
      popups: { lift: 55, accent: true },
      newtab: { lift: 20, accent: true },
    },
    pinned: [
      { title: "Docs", url: "https://developer.mozilla.org/" },
      { title: "Mail", url: "https://mail.example.org/" },
      { title: "Maps", url: "https://www.openstreetmap.org/" },
      { title: "Notes", url: "https://notes.example.org/" },
    ],
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
    return Object.assign({}, DEFAULTS, s, {
      regions,
      pinned: Array.isArray(s.pinned) ? s.pinned : DEFAULTS.pinned,
    });
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

  /**
   * Every colour the extension paints, opaque `#rrggbb`, for one scheme.
   * `k` scales the ink-over-ground steps: a light ground needs lighter steps
   * than a dark one to read as the same material.
   */
  function palette(settings, dark) {
    const s = merge(settings);
    const fg = dark ? "#ffffff" : INK;
    const k = dark ? 1 : 0.7;
    const baseTint = dark ? s.tint : LIGHT_BASE;
    const strength = clamp(s.strength, 0, 100) / 100;

    const ground = (id) => {
      const amount = (clamp(s.regions[id].lift, 0, 100) / 100) * strength * LIFT_SPAN;
      // Dark lifts toward white; light lifts toward white too (brighter glass).
      return mix(baseTint, "#ffffff", dark ? amount : amount * 2.5);
    };
    const g = {
      tabstrip: ground("tabstrip"),
      toolbar: ground("toolbar"),
      sidebar: ground("sidebar"),
      popups: ground("popups"),
      newtab: ground("newtab"),
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
      text2: step(g.toolbar, 0.64 / k),
      text3: step(g.toolbar, 0.4 / k),
      ground: g,
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

    return {
      colors: {
        frame: strip,
        frame_inactive: mix(strip, dark ? "#000000" : "#ffffff", 0.25),
        tab_background_text: p.text2,
        tab_selected: step(strip, 0.12),
        tab_text: p.text,
        // Selection is a crisp neutral rim, never gold; the accent lives on the
        // loading throbber only.
        tab_line: step(strip, 0.45),
        tab_loading: on("tabstrip") ? p.accent : p.text2,

        toolbar: t,
        toolbar_text: p.text,
        toolbar_field: step(t, 0.06),
        toolbar_field_text: p.text,
        toolbar_field_border: step(t, 0.1),
        toolbar_field_focus: step(t, 0.12),
        toolbar_field_text_focus: p.text,
        // Focus = brighter fill + crisp rim. Never the accent.
        toolbar_field_border_focus: step(t, 0.5),
        toolbar_field_highlight: step(t, 0.22),
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

        ntp_background: g.newtab,
        ntp_text: p.text,
        ntp_card_background: step(g.newtab, 0.05),
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
    applyPageVars,
    resolveDark,
    load,
    save,
    watch,
    api,
  };
})();
