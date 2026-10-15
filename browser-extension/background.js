// codeit bridge: connects to codeit (`codeit web`) on this computer, learns which sites its
// plugins want cookies from, and sends those cookies whenever they change. It also loads each
// site in the background now and then (so the login doesn't expire), and reloads a site when
// codeit says its login stopped working.

import { browserName, fingerprint, pick, sitesFor } from "./lib.js";

const DEFAULT_PORT = 7317;

let ws = null;
let sites = [];
const sent = new Map(); // site key → fingerprint of the cookies last sent
const results = new Map(); // site key → { ok, detail, at }
const timers = new Map(); // site key → debounce timer
let pinger = null;

async function port() {
  const { port } = await chrome.storage.local.get("port");
  return Number(port) || DEFAULT_PORT;
}

async function saveStatus(extra = {}) {
  await chrome.storage.session.set({
    status: {
      connected: ws?.readyState === WebSocket.OPEN,
      port: await port(),
      sites: sites.map((s) => ({ ...s, result: results.get(s.key) ?? null })),
      ...extra,
    },
  });
}

async function connect() {
  if (ws && (ws.readyState === WebSocket.OPEN || ws.readyState === WebSocket.CONNECTING)) return;
  const p = await port();
  let socket;
  try {
    socket = new WebSocket(`ws://localhost:${p}/api/bridge`);
  } catch {
    return;
  }
  ws = socket;
  socket.onopen = () => {
    send({ type: "hello", browser: browserName(navigator.userAgent) });
    // Messages every 20 s keep this service worker alive while connected.
    clearInterval(pinger);
    pinger = setInterval(() => send({ type: "ping" }), 20_000);
    void saveStatus();
  };
  socket.onmessage = (e) => {
    let m;
    try {
      m = JSON.parse(e.data);
    } catch {
      return;
    }
    void handle(m);
  };
  socket.onclose = () => {
    if (ws === socket) ws = null;
    clearInterval(pinger);
    sent.clear();
    void saveStatus();
  };
}

function send(m) {
  if (ws?.readyState === WebSocket.OPEN) ws.send(JSON.stringify(m));
}

async function handle(m) {
  if (m.type === "sites") {
    sites = m.sites;
    sent.clear();
    await scheduleKeepalive();
    for (const s of sites) await sendCookies(s);
  } else if (m.type === "refresh") {
    const s = sites.find((x) => x.key === m.site);
    if (s) await reloadSite(s);
  } else if (m.type === "result") {
    results.set(m.site, { ok: m.ok, detail: m.detail, at: Date.now() });
  }
  await saveStatus();
}

/** Sends a site's cookies unless they are the ones codeit already has. */
async function sendCookies(site, force = false) {
  let all;
  try {
    all = await chrome.cookies.getAll({ url: site.url });
  } catch {
    return;
  }
  const cookies = pick(site, all);
  if (cookies.length === 0) return;
  const print = fingerprint(cookies);
  if (!force && sent.get(site.key) === print) return;
  sent.set(site.key, print);
  send({ type: "cookies", site: site.key, cookies });
}

chrome.cookies.onChanged.addListener(({ cookie, removed }) => {
  if (removed) return;
  for (const s of sitesFor(sites, cookie)) {
    clearTimeout(timers.get(s.key));
    // Logins often set several cookies at once: send them together.
    timers.set(s.key, setTimeout(() => void sendCookies(s), 1500));
  }
});

/** Loads the site's page in the background, with its cookies, so the session stays alive. */
async function keepAlive(site) {
  try {
    await fetch(site.url, { credentials: "include", cache: "no-store", redirect: "follow" });
  } catch {
    /* offline, or the site refused: the next time may work */
  }
  await sendCookies(site);
}

/** Reloads the site's open tabs (in the background), or opens it in one for a moment. */
async function reloadSite(site) {
  const origin = new URL(site.url).origin;
  const tabs = await chrome.tabs.query({ url: `${origin}/*` });
  const idle = tabs.filter((t) => !t.active);
  if (idle.length > 0) {
    for (const t of idle) await chrome.tabs.reload(t.id);
  } else if (tabs.length === 0) {
    const tab = await chrome.tabs.create({ url: site.url, active: false });
    const done = (id, info) => {
      if (id !== tab.id || info.status !== "complete") return;
      chrome.tabs.onUpdated.removeListener(done);
      setTimeout(() => {
        void sendCookies(site, true);
        void chrome.tabs.remove(tab.id).catch(() => {});
      }, 5000);
    };
    chrome.tabs.onUpdated.addListener(done);
    return;
  } else {
    await keepAlive(site);
  }
  setTimeout(() => void sendCookies(site, true), 5000);
}

async function scheduleKeepalive() {
  const alarms = await chrome.alarms.getAll();
  for (const a of alarms) if (a.name.startsWith("keep:")) await chrome.alarms.clear(a.name);
  for (const s of sites) {
    if (s.keepalive) await chrome.alarms.create(`keep:${s.key}`, { periodInMinutes: Math.max(1, s.keepalive) });
  }
}

chrome.alarms.onAlarm.addListener(async (a) => {
  if (a.name === "connect") {
    await connect();
  } else if (a.name.startsWith("keep:")) {
    await connect();
    const s = sites.find((x) => `keep:${x.key}` === a.name);
    if (s) await keepAlive(s);
  }
});

chrome.runtime.onMessage.addListener((m, _sender, reply) => {
  if (m.type === "reconnect") {
    ws?.close();
    ws = null;
    void connect().then(() => reply(true));
    return true;
  }
  if (m.type === "send-now") {
    sent.clear();
    void Promise.all(sites.map((s) => sendCookies(s, true))).then(() => reply(true));
    return true;
  }
  return false;
});

// A new port (from the popup): connect there.
chrome.storage.onChanged.addListener((changes, area) => {
  if (area === "local" && changes.port) {
    ws?.close();
    ws = null;
    void connect();
  }
});

// Try now, and every minute in case codeit starts later.
chrome.alarms.create("connect", { periodInMinutes: 1 });
chrome.runtime.onStartup.addListener(() => void connect());
chrome.runtime.onInstalled.addListener(() => void connect());
void connect();
