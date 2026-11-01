import { _electron as electron } from "playwright";
import fs from "node:fs/promises";
import { createHash } from "node:crypto";
import { dirname } from "node:path";
import { fileURLToPath } from "node:url";
// Run from the checkout root on an isolated display with node --import tsx.
const q = dirname(fileURLToPath(import.meta.url)),
  env = { ...process.env };
delete env.WAYLAND_DISPLAY;
delete env.XAUTHORITY;
delete env.ELECTRON_RUN_AS_NODE;
const app = await electron.launch({ args: ["--no-sandbox", q + "/reference-electron.cjs"], env });
try {
  const page = await app.firstWindow();
  await page.setContent('<div id="t"></div>');
  await page.addStyleTag({ path: "packages/app/node_modules/@xterm/xterm/css/xterm.css" });
  await page.addScriptTag({ path: "packages/app/node_modules/@xterm/xterm/lib/xterm.js" });
  await page.evaluate(() => {
    window.t = new Terminal({ cols: 300, rows: 24, allowProposedApi: true });
    t.open(document.getElementById("t"));
  });
  const inputEvents = [];
  const add = (button, action, col = 4, row = 2, x = 36, y = 45, mods = {}) =>
    inputEvents.push({
      button,
      action,
      col,
      row,
      x,
      y,
      shift: false,
      alt: false,
      control: false,
      ...mods,
    });
  add("left", "press");
  add("left", "release");
  add("right", "press", 4, 2, 36, 45, { alt: true, control: true, shift: true });
  add("right", "release", 4, 2, 36, 45, { alt: true, control: true, shift: true });
  add("none", "move", 4, 2, 40, 45);
  add("none", "move", 4, 2, 41, 45);
  add("none", "move", 4, 2, 41, 45);
  add("left", "move", 5, 3, 45, 65);
  add("left", "move", 5, 3, 45, 65);
  add("middle", "move", 7, 3, 63, 65, { control: true });
  add("wheel_up", "press");
  add("wheel_up", "press");
  add("wheel_down", "press", 4, 2, 36, 45, { alt: true });
  for (const col of [94, 95, 222, 223, 299, 300]) add("left", "press", col, 2, col * 9, 45);
  add("none", "press");
  const traces = [];
  for (const protocolValue of [0, 9, 1000, 1002, 1003])
    for (const encodingValue of [0, 1006, 1016]) {
      const result = await page.evaluate(
        async ({ protocol, encoding, events }) => {
          t.reset();
          await new Promise<void>((r) =>
            t.write(
              (protocol ? `\x1b[?${protocol}h` : "") + (encoding ? `\x1b[?${encoding}h` : ""),
              r,
            ),
          );
          const output = [];
          const data = t.onData((v) => output.push(...new TextEncoder().encode(v)));
          const binary = t.onBinary((v) => output.push(...Array.from(v, (c) => c.charCodeAt(0))));
          const rows = [];
          for (const event of events) {
            output.length = 0;
            const button = { left: 0, middle: 1, right: 2, none: 3, wheel_up: 4, wheel_down: 4 }[
              event.button
            ];
            let action = { press: 1, release: 0, move: 32 }[event.action];
            if (event.button === "wheel_up") action = 0;
            else if (event.button === "wheel_down") action = 1;
            const handled = t._core._mouseService._triggerMouseEvent({
              col: event.col,
              row: event.row,
              x: event.x,
              y: event.y,
              button,
              action,
              ctrl: event.control,
              alt: event.alt,
              shift: event.shift,
            });
            rows.push({ event, handled, bytes: [...output] });
          }
          data.dispose();
          binary.dispose();
          return rows;
        },
        { protocol: protocolValue, encoding: encodingValue, events: inputEvents },
      );
      traces.push({
        modes: { protocol: protocolValue, encoding: encodingValue },
        size: { cols: 300, rows: 24 },
        events: result,
      });
    }
  const sequences = [
    ["\x1b", "[?9;1016h"],
    ["\x1b[?1003h", "\x1b[?9l"],
    ["\x1b[?1016h", "\x1b[?1006l"],
    ["\x1b[?1002;1006h", "\x1b[?1005h"],
    ["\x1b]title ?1003h\x07", "\x1bPpayload ?1000h\x1b\\"],
    ["\x1b[0?9h"],
    ["\x1b[?1003:5;1006h"],
    ["\x1b[?99999999999999999;1003h"],
    ["\x1b[?" + Array(40).fill("1000").join(";") + ";1003h"],
    ["\x1b[?1003;1006h", "\x1b[!p"],
    ["\x1b[?1003;1006h", "\x1bc"],
  ];
  const modes = [];
  for (const fragments of sequences) {
    const result = await page.evaluate(async (chunks) => {
      t.reset();
      for (const chunk of chunks) await new Promise<void>((r) => t.write(chunk, r));
      const service = t._core.mouseStateService;
      return {
        protocol: { NONE: 0, X10: 9, VT200: 1000, DRAG: 1002, ANY: 1003 }[service.activeProtocol],
        encoding: { DEFAULT: 0, SGR: 1006, SGR_PIXELS: 1016 }[service.activeEncoding],
      };
    }, fragments);
    modes.push({ chunks: fragments, expected: result });
  }
  const sources = {};
  for (const file of [
    "common/services/MouseStateService.ts",
    "browser/services/MouseService.ts",
    "common/InputHandler.ts",
  ])
    sources[file] = createHash("sha256")
      .update(await fs.readFile("packages/app/node_modules/@xterm/xterm/src/" + file))
      .digest("hex");
  const result = { sources, traces, modes };
  await fs.writeFile(
    "packages/desktop/native/src/testdata/terminal-mouse.json",
    JSON.stringify(result, null, 2) + "\n",
  );
  console.log(
    "Captured xterm pointer traces",
    traces.length,
    "events",
    traces.reduce((n, t) => n + t.events.length, 0),
    "mode traces",
    modes.length,
  );
} finally {
  await app.close();
}
