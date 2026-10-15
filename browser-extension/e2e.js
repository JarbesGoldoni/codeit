// The extension against a real codeit: loads it into Playwright's Chromium, runs codeit's
// `site_cookies` example plugin (`cargo build -p codeit --example site_cookies` first), and a
// test site that hands out an expired session first and a good one after.
//
// Checks that the extension sends the site's cookie as it is set, that codeit asks it to reload
// the site when the plugin says the login doesn't work, that the new cookie then arrives and
// works, and that cookie values never reach codeit's output.
//
//   bun install && bunx playwright install chromium   (once)
//   bun run e2e

import assert from "node:assert/strict";
import { spawn } from "node:child_process";
import { mkdtempSync, realpathSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join, resolve } from "node:path";
import { chromium } from "playwright";

const EXT = realpathSync(import.meta.dir);
const BIN = resolve(EXT, "../target/debug/examples/site_cookies");

let visits = 0;
const site = Bun.serve({
  hostname: "127.0.0.1",
  port: 0,
  fetch(req) {
    const headers = new Headers({ "content-type": "text/html" });
    if (new URL(req.url).pathname === "/") {
      visits++;
      headers.append("set-cookie", `session_id=${visits === 1 ? "expired" : `fresh-${visits}`}; Path=/; HttpOnly`);
      headers.append("set-cookie", "theme=dark; Path=/");
    }
    return new Response("<title>app</title><p>app</p>", { headers });
  },
});
const SITE = `http://app.bridge.test:${site.port}/`;

const home = mkdtempSync(join(tmpdir(), "codeit-bridge-e2e-"));
const port = 7900 + Math.floor(Math.random() * 90);
let log = "";
const codeit = spawn(BIN, ["web", "--no-open", "--port", String(port)], {
  cwd: home,
  env: {
    ...process.env,
    HOME: home,
    XDG_DATA_HOME: join(home, "data"),
    XDG_CACHE_HOME: join(home, "cache"),
    XDG_CONFIG_HOME: join(home, "config"),
    CODEIT_NO_CATALOG: "1",
    CODEIT_NO_BROWSER: "1",
    SITE_URL: SITE,
  },
});
codeit.stdout.on("data", (d) => (log += d));
codeit.stderr.on("data", (d) => (log += d));

const profile = mkdtempSync(join(tmpdir(), "codeit-bridge-profile-"));
const context = await chromium.launchPersistentContext(profile, {
  channel: "chromium",
  headless: !process.env.HEADED,
  args: [`--disable-extensions-except=${EXT}`, `--load-extension=${EXT}`, "--host-resolver-rules=MAP *.bridge.test 127.0.0.1"],
});

async function cleanup() {
  await context.close().catch(() => {});
  codeit.kill();
  site.stop(true);
  rmSync(profile, { recursive: true, force: true });
  rmSync(home, { recursive: true, force: true });
}

/** codeit's view of the bridge, as the web app gets it. */
async function bridge() {
  return new Promise((ok, fail) => {
    const ws = new WebSocket(`ws://localhost:${port}/api/ws`, { headers: { Origin: `http://localhost:${port}` } });
    ws.onmessage = (e) => {
      const m = JSON.parse(e.data);
      if (m.type === "hello") {
        ws.close();
        ok(m.bridge);
      }
    };
    ws.onerror = () => fail(new Error("can't reach codeit"));
  });
}

async function until(what, fn, ms = 20_000) {
  const end = Date.now() + ms;
  let last;
  while (Date.now() < end) {
    try {
      last = await fn();
      if (last) return last;
    } catch (e) {
      last = e;
    }
    await Bun.sleep(250);
  }
  throw new Error(`timed out waiting for ${what} (last: ${JSON.stringify(last)})`);
}

let passed = 0;
async function check(name, fn) {
  await fn();
  passed++;
  console.log(`ok - ${name}`);
}

try {
  await until("codeit to start", () => log.includes("codeit is running"));
  const sw = context.serviceWorkers()[0] ?? (await context.waitForEvent("serviceworker"));
  await sw.evaluate((p) => chrome.storage.local.set({ port: p }), port);

  await check("the extension connects and learns the plugin's site", async () => {
    const b = await until("the extension", async () => {
      const b = await bridge();
      return b.browsers.length > 0 && b;
    });
    assert.equal(b.browsers[0], "Chrome");
    assert.equal(b.sites[0].key, "example:app");
    assert.equal(b.sites[0].ok, null);
  });

  const page = await context.newPage();
  await page.goto(SITE);

  await check("an expired login makes codeit reload the site, and the new cookie works", async () => {
    const s = await until("a working login", async () => {
      const s = (await bridge()).sites[0];
      return s.ok === true && s;
    });
    assert.equal(s.detail, "connected");
    // The first visit set the expired cookie; the reload codeit asked for set a good one.
    assert.ok(visits >= 2, `the site was loaded ${visits} times`);
  });

  await check("only the cookie asked for is sent, and no value is printed", async () => {
    assert.match(log, /cookies for app: session_id\n/);
    assert.doesNotMatch(log, /theme/);
    assert.doesNotMatch(log, /expired|fresh-/);
  });

  await check("codeit refuses the bridge to web pages", async () => {
    const res = await fetch(`http://localhost:${port}/api/bridge`, {
      headers: { Origin: "https://evil.example", Connection: "Upgrade", Upgrade: "websocket", "Sec-WebSocket-Version": "13", "Sec-WebSocket-Key": "dGhlIHNhbXBsZSBub25jZQ==" },
    });
    assert.equal(res.status, 403);
  });

  console.log(`\n${passed} passed`);
} catch (e) {
  console.error(`not ok - ${e.message}\n--- codeit output ---\n${log}`);
  process.exitCode = 1;
} finally {
  await cleanup();
}
