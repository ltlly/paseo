import { dirname } from "node:path";
import { fileURLToPath } from "node:url";
import { _electron as electron } from "playwright";
import fs from "node:fs/promises";
const q = process.env.PASEO_NATIVE_QA ?? "/home/ltlly/.cache/paseo-native-qa";
const env = { ...process.env, DISPLAY: ":94" };
delete env.WAYLAND_DISPLAY;
delete env.XAUTHORITY;
delete env.ELECTRON_RUN_AS_NODE;
const app = await electron.launch({
  args: [
    "--no-sandbox",
    "--use-gl=angle",
    "--use-angle=swiftshader",
    "--enable-unsafe-swiftshader",
    dirname(fileURLToPath(import.meta.url)) + "/reference-electron.cjs",
  ],
  env,
});
try {
  const page = await app.firstWindow();
  page.on("pageerror", (e) => console.log("ERROR", e.message));
  await page.setContent(
    '<style>body{margin:0;background:#181b1a}#root{position:absolute;left:320px;top:72px;width:960px;height:728px}#host{width:100%;height:100%;}</style><div id="root"><div id="host"></div></div>',
  );
  await page.addStyleTag({
    path: process.cwd() + "/packages/app/node_modules/@xterm/xterm/css/xterm.css",
  });
  await page.addScriptTag({ path: q + "/terminal-style-runtime.js" });
  await page.evaluate(() => {
    window.runtime = new window.Runtime();
    runtime.mount({
      root: document.getElementById("root"),
      host: document.getElementById("host"),
      scrollback: 100,
      theme: terminalTheme,
      fontSize: codeSize,
    });
  });
  await page.waitForTimeout(1000);
  const scenes = JSON.parse(
    await fs.readFile(dirname(fileURLToPath(import.meta.url)) + "/scenes.json", "utf8"),
  );
  for (const [sceneKey, sceneInput] of Object.entries(scenes)) {
    await page.evaluate(
      async ({ key, input }) => {
        const t = runtime.terminal;
        t.reset();
        t.focus();
        await new Promise((r) =>
          runtime.write({
            data: new TextEncoder().encode(input),
            suppressInput: true,
            onCommitted: r,
          }),
        );
        if (key === "t")
          await new Promise((r) =>
            runtime.write({
              data: new TextEncoder().encode("\x1b[6 q"),
              suppressInput: true,
              onCommitted: r,
            }),
          );
      },
      { key: sceneKey, input: sceneInput },
    );
    await page.waitForTimeout(100);
    await page.screenshot({ path: q + "/style-reference-" + sceneKey + ".png" });
  }
  await fs.writeFile(
    q + "/style-reference-metrics.json",
    JSON.stringify(
      await page.evaluate(() => ({
        fontSize: runtime.terminal.options.fontSize,
        fontFamily: runtime.terminal.options.fontFamily,
        dimensions: runtime.terminal._core._renderService.dimensions,
        rows: runtime.terminal.rows,
        cols: runtime.terminal.cols,
        cursor: {
          x: runtime.terminal.buffer.active.cursorX,
          y: runtime.terminal.buffer.active.cursorY,
          focused: document.hasFocus(),
          hidden: runtime.terminal._core.coreService.isCursorHidden,
        },
      })),
      null,
      2,
    ),
  );
} finally {
  await app.close();
}
