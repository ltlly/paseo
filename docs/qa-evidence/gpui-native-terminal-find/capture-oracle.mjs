import { _electron as electron } from "playwright";
import { createRequire } from "node:module";
import { createHash } from "node:crypto";
import fs from "node:fs/promises";
import path from "node:path";

const root = process.cwd();
const require = createRequire(path.join(root, "packages/app/package.json"));
const xterm = path.dirname(require.resolve("@xterm/xterm/package.json"));
const search = path.dirname(require.resolve("@xterm/addon-search/package.json"));
const unicode = path.dirname(require.resolve("@xterm/addon-unicode11/package.json"));
const env = { ...process.env, DISPLAY: process.env.DISPLAY || ":94" };
delete env.WAYLAND_DISPLAY;
delete env.ELECTRON_RUN_AS_NODE;
const app = await electron.launch({
  args: [
    "--no-sandbox",
    path.join(root, "docs/qa-evidence/gpui-native-terminal-theme/reference-electron.cjs"),
  ],
  env,
});
try {
  const page = await app.firstWindow();
  await page.setContent('<div id="terminal" style="width:800px;height:400px"></div>');
  await page.addStyleTag({ path: path.join(xterm, "css/xterm.css") });
  await page.addScriptTag({ path: path.join(xterm, "lib/xterm.js") });
  await page.addScriptTag({ path: path.join(search, "lib/addon-search.js") });
  await page.addScriptTag({ path: path.join(unicode, "lib/addon-unicode11.js") });
  const cases = [
    ["wrapped-command", 8, "abcd/path/to/project", "d/path/to"],
    ["wrapped-only", 8, "abcd/path/to/project", "to/project"],
    ["three-rows", 5, "abcdefghijklmnop", "cdefghijklmn"],
    ["hard-break", 8, "abcd\r\nefgh", "def"],
    ["wrapped-blank", 5, "abc  def", "c  d"],
    ["trailing-blanks", 8, "abc   ", "c  "],
    ["combining", 12, "cafe\u0301 cafe\u0301", "e\u0301"],
    ["combining-only", 12, "cafe\u0301", "\u0301"],
    ["combining-wrap", 5, "abcde\u0301fgh", "e\u0301fg"],
    ["wide-wrap", 5, "abcd中ef", "d中e"],
    ["wide-characters", 10, "a中b中c", "中b中"],
    ["emoji-wrap", 5, "abcd😀ef", "d😀e"],
    ["case-fold", 12, "Paseo PASEO", "pAsEo"],
    ["greek-case-fold", 12, "ΟΣ ος ΟΣ", "ος"],
    ["expanded-case-fold", 12, "İstanbul", "i\u0307s"],
    ["emoji-in-row", 12, "a😀b😀c", "😀b😀"],
    ["wide-trailing-wrap", 5, "abc中def", "中d"],
    ["multiple-wide-wraps", 3, "中中中中", "中中"],
    ["overlap", 5, "aaaaaaa", "aa"],
    ["history", 5, "abcdefghi\r\nsecond\r\nthird\r\nfourth", "defgh"],
    ["erased-wrap", 5, "abcdefghi\r\x1b[2Kxy", "dex"],
    ["erase-right-at-start-clears-wrap", 5, "abcdefghi\r\x1b[Kxy", "dex"],
    ["erase-left-preserves-wrap", 5, "abcdefghi\r\x1b[1Kxy", "dex"],
  ].map(([name, cols, input, query]) => ({ name, cols, rows: 4, input, query }));
  const captured = await page.evaluate(async (fixtures) => {
    const results = [];
    for (const c of fixtures) {
      const terminal = new Terminal({
        cols: c.cols,
        rows: c.rows,
        scrollback: 100,
        allowProposedApi: true,
      });
      terminal.loadAddon(new Unicode11Addon.Unicode11Addon());
      terminal.unicode.activeVersion = "11";
      const addon = new SearchAddon.SearchAddon({ highlightLimit: 20_000 });
      terminal.loadAddon(addon);
      terminal.open(document.getElementById("terminal"));
      await new Promise((resolve) => terminal.write(c.input, resolve));
      addon.findNext(c.query, {
        regex: false,
        caseSensitive: false,
        decorations: { matchBorder: "#ffff00", activeMatchBorder: "#0000ff" },
      });
      const matches = [];
      const decorations = [];
      const seen = new Set();
      for (const { match, decoration } of addon._decorationManager._highlightDecorations) {
        decorations.push([
          decoration.marker.line - terminal.buffer.active.baseY,
          decoration.options.x ?? 0,
          decoration.options.width ?? 1,
        ]);
        const key = `${match.row}:${match.col}:${match.size}`;
        if (seen.has(key)) continue;
        seen.add(key);
        matches.push([match.row - terminal.buffer.active.baseY, match.col, match.size]);
      }
      results.push({ ...c, matches, decorations });
      terminal.dispose();
    }
    return results;
  }, cases);
  const sources = {};
  for (const name of [
    "SearchAddon.ts",
    "SearchEngine.ts",
    "SearchLineCache.ts",
    "DecorationManager.ts",
  ]) {
    const file = path.join("packages/app/node_modules/@xterm/addon-search/src", name);
    sources[file] = createHash("sha256")
      .update(await fs.readFile(path.join(root, file)))
      .digest("hex");
  }
  for (const file of [
    "packages/app/src/terminal/runtime/terminal-emulator-runtime.ts",
    "packages/app/node_modules/@xterm/addon-unicode11/lib/addon-unicode11.js",
  ]) {
    sources[file] = createHash("sha256")
      .update(await fs.readFile(path.join(root, file)))
      .digest("hex");
  }
  const destination = path.join(root, "packages/desktop/native/src/testdata/terminal-find.json");
  await fs.writeFile(
    destination,
    JSON.stringify(
      {
        source: "Installed Electron xterm search addon, real browser buffer and decorations",
        sources,
        cases: captured,
      },
      null,
      2,
    ) + "\n",
  );
  console.log(`Captured ${captured.length} xterm search cases`);
} finally {
  await app.close();
}
