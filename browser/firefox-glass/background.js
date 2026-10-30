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
// abyss sets the translucent strip itself. We report, per normal window, the
// window size and the active tab's page-area height; abyss computes
// top = window height - content. Only those three numbers leave the browser:
// no URLs, no titles. A native-messaging helper (os.eclipse.glass) relays them.

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
    windows.push({ width: w.width, height: w.height, content: tab.height });
  }
  windows.sort((a, b) => a.width - b.width || a.height - b.height || a.content - b.content);
  const opacity = Math.max(0, Math.min(100, settings.strength)) / 100;
  const payload = JSON.stringify({ type: "strips", opacity, windows });
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
Glass.api.tabs.onUpdated.addListener(report, { properties: ["status"] });
Glass.api.storage.onChanged.addListener((changes, area) => {
  if (area === "sync" && changes.settings) report();
});
// Window resizes and toolbar changes fire no extension event.
setInterval(report, POLL_MS);
connect();
