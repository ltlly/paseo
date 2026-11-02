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
m.port = m.daemonPort;
if (m.port === 6767) throw Error("Use a private fixture");
const ids = JSON.parse(await fs.readFile(root + "/terminal-ids.json", "utf8"));
const status = await (await fetch(`http://127.0.0.1:${m.daemonPort}/api/status`)).json();
const base = "/home/ltlly/.cache/paseo-native-qa/header-20261102/reference-dist";
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
const referenceConfig = root + "/reference-config";
await fs.mkdir(referenceConfig + "/gtk-3.0", { recursive: true });
await fs.writeFile(
  referenceConfig + "/gtk-3.0/settings.ini",
  "[Settings]\ngtk-font-name=Noto Sans 10\n",
);
const env = { ...process.env, DISPLAY: ":99", XDG_CONFIG_HOME: referenceConfig };
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
try {
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
  for (const terminal of [ids.main, ids.pi, ids.grokA]) {
    await page.goto(
      `http://127.0.0.1:${port}/h/${status.serverId}/workspace/${ids.a}?open=terminal:${terminal}`,
    );
    await page.getByTestId(`workspace-tab-terminal_${terminal}`).waitFor({ timeout: 30000 });
  }
  await page.getByTestId("workspace-explorer-toggle").click();
  const dock = page.getByTestId("workspace-explorer-sidebar");
  await dock.waitFor();
  for (const terminal of [ids.pi, ids.grokA]) {
    const source = page.getByTestId(`workspace-tab-terminal_${terminal}`);
    const sb = await source.boundingBox();
    const db = await dock.boundingBox();
    if (!sb || !db) throw Error("Missing drag bounds");
    await page.mouse.move(sb.x + sb.width / 2, sb.y + sb.height / 2);
    await page.mouse.down();
    await page.mouse.move(db.x + db.width / 2, db.y + Math.min(260, db.height / 2), { steps: 30 });
    await page.waitForTimeout(150);
    await page.mouse.up();
    await page.waitForTimeout(500);
    await dock.getByTestId(`workspace-tab-terminal_${terminal}`).waitFor();
  }
  const handle = page.getByTestId("workspace-explorer-sidebar-resize-handle");
  const hb = await handle.boundingBox();
  if (!hb) throw Error("Missing Explorer resize handle");
  await page.mouse.move(hb.x + hb.width / 2, hb.y + 160);
  await page.mouse.down();
  await page.mouse.move(hb.x + hb.width / 2 - 160, hb.y + 160, { steps: 20 });
  await page.mouse.up();
  await page.waitForTimeout(500);
  await dock.getByTestId(`workspace-tab-terminal_${ids.pi}`).click();
  await page.mouse.move(1100, 740);
  await page.waitForTimeout(600);
  await page.screenshot({ path: root + "/reference-dock.png", caret: "initial" });
  const metrics = await page
    .locator(
      '[data-testid="workspace-explorer-sidebar"],[data-testid="workspace-explorer-sidebar-resize-handle"],[data-testid^="workspace-tab-terminal_"],[data-testid="workspace-tab-files"],[data-testid="workspace-tab-changes_tree"]',
    )
    .evaluateAll((elements) =>
      elements.map((e) => {
        const b = e.getBoundingClientRect(),
          s = getComputedStyle(e);
        return {
          id: e.getAttribute("data-testid"),
          text: e.textContent,
          box: { x: b.x, y: b.y, width: b.width, height: b.height },
          font: s.font,
          background: s.backgroundColor,
        };
      }),
    );
  await fs.writeFile(root + "/reference-explorer-metrics.json", JSON.stringify(metrics, null, 2));
  await dock.getByTestId("workspace-new-tab-button").click();
  await page.getByTestId("workspace-new-tab-menu").waitFor();
  await page.mouse.move(1100, 740);
  await page.waitForTimeout(300);
  await page.screenshot({ path: root + "/reference-dock-menu.png", caret: "initial" });
} catch (error) {
  const page = await browser.firstWindow();
  console.log("STATE", page.url(), (await page.locator("body").innerText()).slice(0, 5000));
  await page.screenshot({ path: root + "/reference-failed.png" });
  throw error;
} finally {
  try {
    await browser.close();
  } finally {
    await new Promise<void>((resolve) => server.close(() => resolve()));
  }
}
