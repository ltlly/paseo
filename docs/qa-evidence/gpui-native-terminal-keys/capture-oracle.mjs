import { build } from "esbuild";
import fs from "node:fs/promises";
import path from "node:path";
import { createHash } from "node:crypto";
const root = process.cwd();
const xterm = "packages/app/node_modules/@xterm/xterm/src";
const { outputFiles } = await build({
  stdin: {
    contents: `export { evaluateKeyboardEvent } from './${xterm}/common/input/Keyboard.ts'; export { resolveMacTerminalEditingShortcut } from './packages/app/src/utils/terminal-keys.ts';`,
    resolveDir: root,
  },
  bundle: true,
  format: "esm",
  platform: "node",
  write: false,
});
const { evaluateKeyboardEvent, resolveMacTerminalEditingShortcut } = await import(
  `data:text/javascript;base64,${Buffer.from(outputFiles[0].text).toString("base64")}`
);
const keys = [
  ["up", "ArrowUp", 38],
  ["down", "ArrowDown", 40],
  ["left", "ArrowLeft", 37],
  ["right", "ArrowRight", 39],
  ["home", "Home", 36],
  ["end", "End", 35],
  ["insert", "Insert", 45],
  ["delete", "Delete", 46],
  ["pageup", "PageUp", 33],
  ["pagedown", "PageDown", 34],
  ["backspace", "Backspace", 8],
  ["tab", "Tab", 9],
  ["enter", "Enter", 13],
  ["escape", "Escape", 27],
  ...Array.from({ length: 12 }, (_, index) => [`f${index + 1}`, `F${index + 1}`, 112 + index]),
  ...[..."abcdefghijklmnopqrstuvwxyz"].map((key) => [
    key,
    key,
    key.toUpperCase().charCodeAt(0),
    key.toUpperCase(),
  ]),
  ...[...")!@#$%^&*("].map((shifted, index) => [`${index}`, `${index}`, 48 + index, shifted]),
  ["space", " ", 32, " "],
  [";", ";", 186, ":"],
  ["=", "=", 187, "+"],
  [",", ",", 188, "<"],
  ["-", "-", 189, "_"],
  [".", ".", 190, ">"],
  ["/", "/", 191, "?"],
  ["`", "`", 192, "~"],
  ["[", "[", 219, "{"],
  ["\\", "\\", 220, "|"],
  ["]", "]", 221, "}"],
  ["'", "'", 222, '"'],
];
const cases = [];
for (const [native, base, keyCode, shifted] of keys)
  for (let mask = 0; mask < 16; mask++)
    for (const [application, mac] of [
      [false, false],
      [false, true],
      [true, false],
      [true, true],
    ]) {
      let code = base;
      if (keyCode >= 65 && keyCode <= 90) code = `Key${base.toUpperCase()}`;
      else if (keyCode >= 48 && keyCode <= 57) code = `Digit${base}`;
      else if (base === "-") code = "Minus";
      const event = {
        key: mask & 1 && shifted ? shifted : base,
        code,
        keyCode,
        shiftKey: !!(mask & 1),
        altKey: !!(mask & 2),
        ctrlKey: !!(mask & 4),
        metaKey: !!(mask & 8),
        type: "keydown",
      };
      const shortcut = mac ? resolveMacTerminalEditingShortcut(event) : null;
      const result =
        shortcut === null
          ? evaluateKeyboardEvent(event, application, mac, true)
          : { type: 0, key: shortcut };
      let key = native;
      if (native !== "space" && shifted && mask & 1) key = shifted;
      cases.push([
        key,
        mask,
        application,
        mac,
        result.type,
        result.key ?? (!(mask & 14) && event.key.length === 1 ? event.key : null),
        event.key,
      ]);
    }
const sources = {};
for (const file of [
  `${xterm}/common/input/Keyboard.ts`,
  `${xterm}/common/Types.ts`,
  "packages/app/src/utils/terminal-keys.ts",
  "packages/app/src/terminal/runtime/terminal-emulator-runtime.ts",
])
  sources[file] = createHash("sha256")
    .update(await fs.readFile(path.join(root, file)))
    .digest("hex");
await fs.writeFile(
  "packages/desktop/native/src/testdata/terminal-keyboard.json",
  JSON.stringify(
    {
      source:
        "Installed xterm evaluateKeyboardEvent plus authored Mac editing shortcuts; app cursor mode and macOptionIsMeta=true. Plain text is committed by native IME rather than sent twice.",
      columns: [
        "key",
        "modifiers: shift=1 alt=2 ctrl=4 meta=8",
        "applicationCursor",
        "mac",
        "action: send=0 selectAll=1 pageUp=2 pageDown=3",
        "data (ordinary text includes the subsequent DOM input commit)",
        "domKey",
      ],
      sources,
      cases,
    },
    null,
    2,
  ) + "\n",
);
console.log(`Captured ${cases.length} keyboard cases`);
