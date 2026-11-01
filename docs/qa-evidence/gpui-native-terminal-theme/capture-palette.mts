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
    '<style>body{margin:0;background:#181b1a}#root{position:absolute;left:0;top:0;width:960px;height:728px}#host{width:100%;height:100%;}</style><div id="root"><div id="host"></div></div>',
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
  const result = await page.evaluate(() => {
    const t = runtime.terminal;
    return {
      theme: terminalTheme,
      fontSize: t.options.fontSize,
      fontFamily: t.options.fontFamily,
      dimensions: t._core._renderService.dimensions,
      webgl: !!t._core._renderService._renderer.value._gl,
    };
  });
  if (!result.webgl) throw new Error("Real WebGL terminal renderer required");
  const cases = [];
  for (let i = 0; i < 256; i++) cases.push({ name: "indexed-" + i, data: `\x1b[0;38;5;${i}m█ ` });
  for (let i = 0; i < 16; i++) {
    const n = i < 8 ? 30 + i : 90 + i - 8;
    cases.push({ name: "ansi-" + i, data: `\x1b[0;${n}m█ ` });
    cases.push({ name: "bold-" + i, data: `\x1b[0;1;${n}m█ ` });
  }
  for (const [name, sgr] of [
    ["default", "0"],
    ["inverse-default", "0;7"],
    ["inverse-bold", "0;31;44;1;7"],
    ["rgb-bold", "0;1;38;2;10;20;30;48;2;40;50;60"],
  ])
    cases.push({ name, data: `\x1b[${sgr}m█ ` });
  const pages = [];
  for (let i = 0; i < cases.length; i += 32) pages.push(cases.slice(i, i + 32));
  for (const [name, seq] of [
    ["osc-index", "\x1b]4;1;rgb:11/22/33\x07\x1b[0;31m"],
    ["osc-foreground", "\x1b]10;#123456\x07\x1b[0m"],
    ["osc-background", "\x1b]11;#28323c\x07\x1b[0m"],
  ])
    pages.push([{ name, data: seq + "█ " }]);
  for (const [name, data, selected, sampleForeground] of [
    ["selected-osc-background", "\x1b]11;#28323c\x07X ", true, false],
    ["selected-default-text", "X ", true, false],
    ["selected-rgb-background", "\x1b[48;2;40;50;60mX ", true, false],
    ["selected-inverse-text", "\x1b[7mX ", true, false],
    ["selected-inverse-bold", "\x1b[1;31;7mX ", true, false],
    ["selected-default-block", "█ ", true, true],
    ["selected-inverse-block", "\x1b[7m█ ", true, true],
    ["selected-dim-block", "\x1b[2m█ ", true, true],
    ["dim-default-block", "\x1b[2m█ ", false, true],
    ["dim-rgb-block", "\x1b[2;38;2;10;20;30;48;2;40;50;60m█ ", false, true],
    ["hidden-block", "\x1b[8;48;2;40;50;60m█ ", false, true],
  ])
    pages.push([{ name, data, selected, sampleForeground }]);
  let index = 0;
  for (const pageRows of pages) {
    await page.evaluate(async (rows) => {
      const t = runtime.terminal;
      t.reset();
      await new Promise((r) =>
        runtime.write({
          data: new TextEncoder().encode(
            "\x1b]104\x07\x1b]110\x07\x1b]111\x07\x1b[0m\x1b[?25l\x1b[2J\x1b[H" +
              rows.map((c) => c.data).join("\r\n"),
          ),
          suppressInput: true,
          onCommitted: r,
        }),
      );
    }, pageRows);
    await page.evaluate((rows) => {
      const selected = rows.findIndex((c) => c.selected);
      if (selected >= 0) runtime.terminal.select(0, selected, 2);
    }, pageRows);
    await page.waitForTimeout(80);
    const image = q + "/style-palette-" + index + ".png";
    await page.screenshot({ path: image });
    pageRows.forEach((c, row) => Object.assign(c, { row, image }));
    index++;
  }
  await fs.writeFile(
    q + "/style-palette-pages.json",
    JSON.stringify({ ...result, pages }, null, 2),
  );
  console.log(
    "Captured real terminal palette pages",
    pages.length,
    "cases",
    pages.reduce((n, p) => n + p.length, 0),
  );
} finally {
  await app.close();
}
