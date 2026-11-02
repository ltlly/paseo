import { _electron as electron } from "playwright";
import { createRequire } from "node:module";
import { createServer } from "node:http";
import fs from "node:fs/promises";
import path from "node:path";
// The E2E helpers are CommonJS under the checkout's package boundaries.
const require = createRequire(import.meta.url);
const { buildSeededHost } =
  require("../../../packages/app/e2e/support/helpers/daemon-registry.ts") as typeof import("../../../packages/app/e2e/support/helpers/daemon-registry.ts");
const { installDesktopRuntime } =
  require("../../../packages/desktop/e2e/support/runtime.ts") as typeof import("../../../packages/desktop/e2e/support/runtime.ts");
const root = process.env.PASEO_NATIVE_QA;
if (!root) throw new Error("Set PASEO_NATIVE_QA to the isolated fixture directory");
const m = JSON.parse(await fs.readFile(root + "/fixture.json", "utf8"));
const status = await (await fetch(`http://127.0.0.1:${m.port}/api/status`)).json();
const base = root + "/reference-dist";
const server = createServer(async (req, res) => {
  try {
    const requested = decodeURIComponent(new URL(req.url!, "http://localhost").pathname);
    let file = path.join(base, requested);
    let data;
    try {
      data = await fs.readFile(file);
    } catch {
      file = path.join(base, "index.html");
      data = await fs.readFile(file);
    }
    const type = {
      ".html": "text/html",
      ".js": "text/javascript",
      ".css": "text/css",
      ".svg": "image/svg+xml",
      ".png": "image/png",
      ".ttf": "font/ttf",
      ".woff2": "font/woff2",
    }[path.extname(file)];
    if (type) res.setHeader("Content-Type", type);
    res.end(data);
  } catch {
    res.statusCode = 500;
    res.end();
  }
});
await new Promise<void>((r) => server.listen(0, "127.0.0.1", r));
const address = server.address();
if (!address || typeof address === "string") throw new Error("Expected a private TCP listener");
const port = address.port;
const env = { ...process.env, DISPLAY: ":99" };
delete env.WAYLAND_DISPLAY;
delete env.XAUTHORITY;
delete env.ELECTRON_RUN_AS_NODE;
const browser = await electron.launch({
  args: [
    "--no-sandbox",
    process.cwd() + "/docs/qa-evidence/gpui-native-terminal-theme/reference-electron.cjs",
  ],
  env,
});
const page = await browser.firstWindow();
page.on("pageerror", (e) => console.log("PAGEERROR", e.message));
page.on("console", (msg) => {
  if (msg.type() === "error") console.log("CONSOLE", msg.text());
});
await page.route(/:(6767)\b/, (r) => r.abort());
await page.routeWebSocket(/:(6767)\b/, (ws) => ws.close());
await page.addInitScript("globalThis.__name = (target) => target;");
await installDesktopRuntime(
  {
    addInitScript: async (fn, arg) =>
      page.addInitScript({
        content: `globalThis.__name=(f)=>f;(${fn.toString()})(${JSON.stringify(arg)});window.paseoDesktop.platform='linux';window.paseoDesktop.windowChromeMode='custom-linux';window.paseoDesktop.window={isFullscreen:async()=>false,isMaximized:async()=>false,onResized:async()=>()=>{}};`,
      }),
  },
  { serverId: status.serverId, daemonListen: `127.0.0.1:${m.port}` },
);
const seededHost = buildSeededHost({
  serverId: status.serverId,
  endpoint: `127.0.0.1:${m.port}`,
  label: "localhost",
  nowIso: new Date().toISOString(),
});
await page.addInitScript(
  ({ host }) => {
    localStorage.setItem("@paseo:daemon-registry", JSON.stringify([host]));
    localStorage.setItem(
      "@paseo:app-settings",
      JSON.stringify({
        theme: "dark",
        chatDisplay: "full",
        fontSize: 14,
        contentFontSize: 15,
        codeFontSize: 12,
      }),
    );
  },
  { host: seededHost },
);
await page.goto(
  `http://127.0.0.1:${port}/h/${status.serverId}/workspace/${m.workspaceId}?open=terminal:${m.terminalId}`,
);
await page
  .getByTestId("workspace-header-title")
  .waitFor({ timeout: 30000 })
  .catch(async (e) => {
    console.log("STATE", page.url(), (await page.locator("body").innerText()).slice(0, 6000));
    await page.screenshot({ path: root + "/reference-failed.png" });
    await browser.close();
    await new Promise<void>((r) => server.close(() => r()));
    throw e;
  });
await page.waitForTimeout(2000);
await page.mouse.move(1000, 700);
await page.waitForTimeout(50);
await page.screenshot({ path: root + "/linux-electron-reference.png" });
const metrics = await page.evaluate(() =>
  [...document.querySelectorAll("[data-testid],textarea")]
    .filter((e) => {
      const b = e.getBoundingClientRect();
      return b.width > 0 && b.height > 0;
    })
    .map((e) => {
      const b = e.getBoundingClientRect(),
        s = getComputedStyle(e);
      return {
        id: e.getAttribute("data-testid") || e.tagName,
        text: e.textContent?.slice(0, 80),
        box: { x: b.x, y: b.y, width: b.width, height: b.height },
        style: {
          font: s.font,
          color: s.color,
          background: s.backgroundColor,
          padding: s.padding,
          border: s.border,
          borderRadius: s.borderRadius,
          gap: s.gap,
        },
      };
    }),
);
await fs.writeFile(
  root + "/linux-electron-reference.json",
  JSON.stringify({ status, viewport: { width: 1280, height: 800 }, metrics }, null, 2),
);
await page.getByTestId("workspace-explorer-toggle").click();
await page.waitForTimeout(500);
await page.mouse.move(700, 700);
await page.screenshot({ path: root + "/reference-changes.png" });
await page.getByTestId("workspace-tab-files").click();
await page.waitForTimeout(500);
await page.mouse.move(700, 700);
await page.screenshot({ path: root + "/reference-files.png" });
await fs.writeFile(
  root + "/files-metrics.json",
  JSON.stringify(
    await page.evaluate(() =>
      [...document.querySelectorAll('[data-testid^="file"], [data-testid^="workspace-tab"]')].map(
        (e) => {
          const b = e.getBoundingClientRect(),
            s = getComputedStyle(e);
          return {
            id: e.getAttribute("data-testid"),
            text: e.textContent?.slice(0, 80),
            x: b.x,
            y: b.y,
            w: b.width,
            h: b.height,
            font: s.font,
            color: s.color,
            padding: s.padding,
            gap: s.gap,
          };
        },
      ),
    ),
    null,
    2,
  ),
);
await browser.close();
await new Promise<void>((r) => server.close(() => r()));
