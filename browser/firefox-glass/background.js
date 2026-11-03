// SPDX-License-Identifier: AGPL-3.0-only
//
// Builds the browser theme from the stored settings and re-applies it when
// they change or the OS colour scheme flips. Theme building lives in
// shared/glass.js so the options page can preview the very same palette.

"use strict";

function apply(settings) {
  return Glass.api.theme.update(Glass.buildTheme(settings));
}

Glass.watch(apply);

// Seed defaults on first install so the options page has something to edit.
Glass.api.runtime.onInstalled.addListener(async () => {
  const got = await Glass.api.storage.sync.get("settings");
  if (!got.settings) await Glass.save(Glass.DEFAULTS);
});

// ---- live strip reporting ---------------------------------------------------
//
// abyss makes the glass translucent itself. We report the reserved key colours
// with their opacities, and per normal window the window size, the active
// tab's page-area height, and `body`: whether the active tab is a
// browser-owned page (Home, new tab, blank, private-browsing start), so abyss
// keys the whole window. The "tabs" permission is needed to read the URL for
// that test, but ONLY THE BOOLEAN leaves the browser: no URL, no title.
// A native-messaging helper (os.eclipse.glass) relays the message.
const BODY_PAGES = ["about:home", "about:newtab", "about:blank", "about:privatebrowsing"];

function isBodyPage(url) {
  const u = String(url || "");
  return BODY_PAGES.some((p) => u === p || u.startsWith(p + "?") || u.startsWith(p + "#"));
}

const HOST = "os.eclipse.glass";
const POLL_MS = 1000;
const BACKOFF_MIN = 1000;
const BACKOFF_MAX = 30000;

let port = null;
let backoff = BACKOFF_MIN;
let retryTimer = null;
let lastSent = null;
let lastStatus = null;

function setStatus(status) {
  const key = JSON.stringify(status);
  if (key === lastStatus) return;
  lastStatus = key;
  Glass.api.storage.local.set({ status });
}

function connect() {
  if (port) return;
  let p;
  try {
    p = Glass.api.runtime.connectNative(HOST);
  } catch (e) {
    setStatus({ connected: false, error: String(e && e.message ? e.message : e) });
    scheduleReconnect();
    return;
  }
  port = p;
  lastSent = null;
  p.onMessage.addListener((msg) => {
    if (!msg || msg.type !== "status") return;
    if (msg.connected) backoff = BACKOFF_MIN;
    const st = { connected: !!msg.connected };
    if (msg.error) st.error = String(msg.error);
    setStatus(st);
  });
  p.onDisconnect.addListener(() => {
    const err = (p.error && p.error.message) || (Glass.api.runtime.lastError && Glass.api.runtime.lastError.message);
    if (port === p) port = null;
    setStatus({ connected: false, error: err || "The abyss helper disconnected." });
    scheduleReconnect();
  });
  report();
}

function scheduleReconnect() {
  if (retryTimer) return;
  retryTimer = setTimeout(() => {
    retryTimer = null;
    connect();
  }, backoff);
  backoff = Math.min(backoff * 2, BACKOFF_MAX);
}

async function report() {
  if (!port) return;
  let wins;
  let settings;
  try {
    [wins, settings] = await Promise.all([
      Glass.api.windows.getAll({ populate: true, windowTypes: ["normal"] }),
      Glass.load(),
    ]);
  } catch (e) {
    return;
  }
  const windows = [];
  for (const w of wins) {
    if (w.type !== "normal" || w.state === "minimized") continue;
    const tab = (w.tabs || []).find((t) => t.active);
    if (!tab || !(tab.height > 0) || !(w.width > 0) || !(w.height > 0)) continue;
    windows.push({ width: w.width, height: w.height, content: tab.height, body: isBodyPage(tab.url) });
  }
  windows.sort((a, b) => a.width - b.width || a.height - b.height || a.content - b.content || Number(a.body) - Number(b.body));
  // `everywhere`: the popup key is also keyed over the whole window on websites
  // (the URL-bar results dropdown is drawn inside the window, over the page).
  const keys = Glass.keyColors(settings, Glass.resolveDark(settings)).map((k) => {
    const o = { color: k.color, opacity: k.opacity };
    if (k.id === "popup") o.everywhere = true;
    return o;
  });
  const payload = JSON.stringify({ type: "strips", keys, windows });
  if (payload === lastSent || !port) return;
  lastSent = payload;
  try {
    port.postMessage(JSON.parse(payload));
  } catch (e) {
    lastSent = null;
  }
}

for (const ev of [
  Glass.api.windows.onCreated,
  Glass.api.windows.onRemoved,
  Glass.api.windows.onFocusChanged,
  Glass.api.tabs.onActivated,
  Glass.api.tabs.onZoomChange,
]) {
  ev.addListener(report);
}
// Page loads and zoom settle through status changes; filter the noisy rest.
// Navigation (URL) changes flip the body flag, so watch those too.
Glass.api.tabs.onUpdated.addListener(report, { properties: ["status", "url"] });
if (window.matchMedia) {
  window.matchMedia("(prefers-color-scheme: dark)").addEventListener("change", report);
}
Glass.api.storage.onChanged.addListener((changes, area) => {
  if (area === "sync" && changes.settings) report();
});
// Window resizes and toolbar changes fire no extension event.
setInterval(report, POLL_MS);
connect();
