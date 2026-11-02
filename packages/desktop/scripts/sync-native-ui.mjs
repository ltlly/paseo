import { build as bundle } from "esbuild";
import fs from "node:fs/promises";
import path from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";
import { createRequire } from "node:module";
import { runInNewContext } from "node:vm";
import { createElement } from "react";
import { renderToStaticMarkup } from "react-dom/server";
import { execFileSync } from "node:child_process";
import { createHash } from "node:crypto";
import { characterEntities } from "character-entities";
import ts from "typescript";

// The authored app theme remains the source of truth. Cargo consumes the checked-in output,
// so building the native client does not need a JS runtime on the target machine.
const root = fileURLToPath(new URL("../../../", import.meta.url));
const require = createRequire(import.meta.url);
const check = process.argv.includes("--check");
// Retain the published parser and exactly one local UTF-8 consumption fix.
// Reversing that line must reproduce the crate's original source digest.
const vteRoot = path.join(root, "packages/desktop/native/vendor/vte");
const vteSource = JSON.parse(await fs.readFile(path.join(vteRoot, "source.json"), "utf8"));
for (const [file, expected] of Object.entries(vteSource.upstreamFiles)) {
  let bytes = await fs.readFile(path.join(vteRoot, file));
  if (file === "src/lib.rs") {
    const patched = "                    return c.len_utf8() - old_bytes;";
    const text = bytes.toString("utf8");
    if (text.split(patched).length !== 2)
      throw new Error("Native VTE UTF-8 patch is missing or ambiguous");
    bytes = Buffer.from(
      text.replace(patched, "                    return valid_bytes - old_bytes;"),
    );
  }
  if (createHash("sha256").update(bytes).digest("hex") !== expected) {
    throw new Error(
      `Vendored VTE source drift: ${file}. Preserve the published parser and recorded patch.`,
    );
  }
}
async function output(file, content) {
  if (file.endsWith(".rs"))
    content = execFileSync("rustfmt", ["--edition", "2024", "--emit", "stdout"], {
      input: content,
      encoding: "utf8",
    });
  const destination = path.join(root, file);
  if (check) {
    if ((await fs.readFile(destination, "utf8")) !== content)
      throw new Error(`Native UI asset is stale: ${file}. Run npm run sync:desktop:native-ui.`);
  } else {
    await fs.mkdir(path.dirname(destination), { recursive: true });
    await fs.writeFile(destination, content);
  }
}
const { outputFiles } = await bundle({
  stdin: {
    contents:
      'export { darkTheme as default, lightTheme } from "./packages/app/src/styles/theme.ts"; import { IDENTITY_COLOR_NAMES, identityColor } from "./packages/app/src/styles/identity-colors.ts"; export const identities = IDENTITY_COLOR_NAMES.map(identityColor); export { resolveExplorerSidebarWidth } from "./packages/app/src/components/explorer-sidebar-layout.ts"; export { MENU_ITEM_HEIGHT } from "./packages/app/src/components/ui/menu/menu-geometry.ts"; export {DEFAULT_TERMINAL_FONT_FAMILY, WINDOWS_TERMINAL_FONT_FAMILY} from "./packages/app/src/terminal/runtime/terminal-font.ts";',
    resolveDir: root,
  },
  bundle: true,
  write: false,
  format: "esm",
  platform: "node",
  plugins: [
    {
      name: "web-platform",
      setup(build) {
        build.onResolve({ filter: /^react-native$/ }, () => ({
          path: "platform",
          namespace: "native-ui",
        }));
        build.onLoad({ filter: /.*/, namespace: "native-ui" }, () => ({
          contents:
            'export const Platform = { OS: "web", select: value => value.web ?? value.default };',
        }));
      },
    },
  ],
});
const {
  default: theme,
  lightTheme,
  identities,
  DEFAULT_TERMINAL_FONT_FAMILY,
  WINDOWS_TERMINAL_FONT_FAMILY,
} = await import(
  `data:text/javascript;base64,${Buffer.from(outputFiles[0].text).toString("base64")}`
);
const clipboardOracle = JSON.parse(
  await fs.readFile(
    path.join(root, "packages/desktop/native/src/testdata/terminal-clipboard.json"),
    "utf8",
  ),
);
const clipboardXtermPackage = createRequire(path.join(root, "packages/app/package.json")).resolve(
  "@xterm/xterm/package.json",
);
const clipboardSource = await fs.readFile(
  path.join(path.dirname(clipboardXtermPackage), "src/browser/Clipboard.ts"),
);
if (createHash("sha256").update(clipboardSource).digest("hex") !== clipboardOracle.sourceSha256) {
  throw new Error(
    "Xterm clipboard implementation changed. Recapture the native terminal clipboard oracle with the installed browser terminal.",
  );
}
const mouseOracle = JSON.parse(
  await fs.readFile(
    path.join(root, "packages/desktop/native/src/testdata/terminal-mouse.json"),
    "utf8",
  ),
);
for (const [source, expected] of Object.entries(mouseOracle.sources)) {
  const bytes = await fs.readFile(path.join(path.dirname(clipboardXtermPackage), "src", source));
  if (createHash("sha256").update(bytes).digest("hex") !== expected) {
    throw new Error(
      `Xterm mouse implementation changed: ${source}. Recapture the native pointer oracle with the installed browser terminal.`,
    );
  }
}
const copyOracle = JSON.parse(
  await fs.readFile(
    path.join(root, "packages/desktop/native/src/testdata/terminal-copy.json"),
    "utf8",
  ),
);
for (const [source, expected] of Object.entries(copyOracle.sources)) {
  const bytes = await fs.readFile(path.join(path.dirname(clipboardXtermPackage), "src", source));
  if (createHash("sha256").update(bytes).digest("hex") !== expected) {
    throw new Error(
      `Xterm selection copy implementation changed: ${source}. Recapture the native terminal copy oracle with the installed browser terminal.`,
    );
  }
}
const paletteOracle = JSON.parse(
  await fs.readFile(
    path.join(root, "packages/desktop/native/src/testdata/terminal-palette.json"),
    "utf8",
  ),
);
const findOracle = JSON.parse(
  await fs.readFile(
    path.join(root, "packages/desktop/native/src/testdata/terminal-find.json"),
    "utf8",
  ),
);
const keyboardOracle = JSON.parse(
  await fs.readFile(
    path.join(root, "packages/desktop/native/src/testdata/terminal-keyboard.json"),
    "utf8",
  ),
);
for (const [source, expected] of Object.entries(keyboardOracle.sources)) {
  const bytes = await fs.readFile(path.join(root, source));
  if (createHash("sha256").update(bytes).digest("hex") !== expected) {
    throw new Error(
      `Terminal keyboard reference changed: ${source}. Recapture the native keyboard oracle.`,
    );
  }
}
for (const [source, expected] of Object.entries(findOracle.sources)) {
  const bytes = await fs.readFile(path.join(root, source));
  if (createHash("sha256").update(bytes).digest("hex") !== expected) {
    throw new Error(
      `Xterm search changed: ${source}. Recapture the native terminal search oracle.`,
    );
  }
}
for (const [source, expected] of Object.entries(paletteOracle.sources)) {
  const bytes = await fs.readFile(path.join(root, source));
  if (createHash("sha256").update(bytes).digest("hex") !== expected) {
    throw new Error(
      `Desktop terminal reference changed: ${source}. Recapture the real terminal palette/geometry oracle.`,
    );
  }
}
const name = (key) =>
  key
    .replace(/([a-z])([A-Z])/g, "$1_$2")
    .replaceAll("-", "_")
    .replaceAll(".", "_")
    .toUpperCase();
// Action menus reuse the authored row geometry and theme shadow.
const menuItemSource = await fs.readFile(
  path.join(root, "packages/app/src/components/ui/menu/menu-item.tsx"),
  "utf8",
);
const menuLineHeight = menuItemSource.match(/const MENU_ITEM_LINE_HEIGHT = (\d+);/);
if (!menuLineHeight) throw new Error("Authored menu line-height declaration changed");
const headerHighlight = (source, key) => {
  const rgba = source.match(/^rgba\((\d+),\s*(\d+),\s*(\d+),\s*([\d.]+)\)$/);
  if (!rgba) throw new Error("Unsupported authored header interaction highlight");
  const color = rgba
    .slice(1, 4)
    .map((value) => Number(value).toString(16).padStart(2, "0"))
    .join("");
  return `pub const ${key}_COLOR:u32=0x${color}; pub const ${key}_OPACITY:f32=${Number(rgba[4])};`;
};
const menuShadow = (source, key) => {
  const rgba = source.shadowColor.match(/^rgba\((\d+),\s*(\d+),\s*(\d+),\s*([\d.]+)\)$/);
  if (!rgba) throw new Error("Unsupported authored menu shadow color");
  const color = [...rgba.slice(1, 4).map(Number), Math.round(Number(rgba[4]) * 255)]
    .map((n) => n.toString(16).padStart(2, "0"))
    .join("");
  return `pub const ${key}_COLOR:u32=0x${color}; pub const ${key}_X:f32=${Number(source.shadowOffset.width).toFixed(1)}; pub const ${key}_Y:f32=${Number(source.shadowOffset.height).toFixed(1)}; pub const ${key}_BLUR:f32=${Number(source.shadowRadius).toFixed(1)};`;
};
const { MENU_ITEM_HEIGHT } = await import(
  `data:text/javascript;base64,${Buffer.from(outputFiles[0].text).toString("base64")}`
);
await output(
  "packages/desktop/native/src/action_menu_presentation.rs",
  `// Generated by npm run sync:desktop:native-ui from authored menu geometry and theme shadows.\n// Do not edit.\npub const ITEM_HEIGHT:f32=${MENU_ITEM_HEIGHT.md.toFixed(1)};\npub const LINE_HEIGHT:f32=${Number(menuLineHeight[1]).toFixed(1)};\n${menuShadow(theme.shadow.md, "DARK_SHADOW")}\n${menuShadow(lightTheme.shadow.md, "LIGHT_SHADOW")}\n${headerHighlight(theme.colors.interactionHighlight, "DARK_HEADER_HIGHLIGHT")}\n${headerHighlight(lightTheme.colors.interactionHighlight, "LIGHT_HEADER_HIGHLIGHT")}\n`,
);
const { resolveExplorerSidebarWidth } = await import(
  `data:text/javascript;base64,${Buffer.from(outputFiles[0].text).toString("base64")}`
);
const explorerSource = "packages/app/src/components/explorer-sidebar-layout.ts";
const explorerSha = createHash("sha256")
  .update(await fs.readFile(path.join(root, explorerSource)))
  .digest("hex");
const explorerCases = [];
for (const requestedWidth of [undefined, -1, 0, 239, 240, 320, 320.5, 500, 10000])
  for (const containerWidth of [-1, 0, 400, 599, 600, 800, 960, 1280])
    explorerCases.push([
      requestedWidth ?? null,
      containerWidth,
      resolveExplorerSidebarWidth({ requestedWidth, containerWidth }),
    ]);
const explorerOraclePath = path.join(
  root,
  "packages/desktop/native/src/testdata/explorer-geometry.json",
);
const explorerOracle = { source: explorerSource, sourceSha256: explorerSha, cases: explorerCases };
if (check) {
  if (
    JSON.stringify(JSON.parse(await fs.readFile(explorerOraclePath, "utf8"))) !==
    JSON.stringify(explorerOracle)
  )
    throw new Error("Native Explorer width oracle is stale. Run npm run sync:desktop:native-ui.");
} else await fs.writeFile(explorerOraclePath, JSON.stringify(explorerOracle, null, 2) + "\n");
const explorerDefault = resolveExplorerSidebarWidth({ containerWidth: 0 });
const explorerMin = resolveExplorerSidebarWidth({ requestedWidth: 0, containerWidth: 10000 });
const explorerBody =
  10000 - resolveExplorerSidebarWidth({ requestedWidth: 10000, containerWidth: 10000 });
await output(
  "packages/desktop/native/src/explorer_geometry.rs",
  `// Generated by npm run sync:desktop:native-ui from the authored Explorer width resolver. Do not edit.\npub const DEFAULT_WIDTH:f64=${explorerDefault.toFixed(1)};\npub const MIN_WIDTH:f64=${explorerMin.toFixed(1)};\npub const MIN_BODY_WIDTH:f64=${explorerBody.toFixed(1)};\n`,
);
const colors = Object.entries(theme.colors).filter(
  ([, value]) => typeof value === "string" && /^#[\da-f]{6}$/i.test(value),
);
const numbers = (label, values) =>
  `pub mod ${label} {\n${Object.entries(values)
    .map(
      ([key, value]) =>
        `    pub const ${/^\d/.test(key) ? "S_" : ""}${name(key)}: f32 = ${value}.0;`,
    )
    .join("\n")}\n}\n`;
const terminalSelectionRgba = theme.colors.terminal.selectionBackground.match(
  /^rgba\((\d+),\s*(\d+),\s*(\d+),\s*([\d.]+)\)$/,
);
if (!terminalSelectionRgba) throw new Error("Unsupported authored terminal selection color");
const terminalSelectionColor = terminalSelectionRgba
  .slice(1, 4)
  .map(Number)
  .concat(Math.round(Number(terminalSelectionRgba[4]) * 255))
  .map((value) => value.toString(16).padStart(2, "0"))
  .join("");
const terminalSelectionForeground = theme.colors.terminal.selectionForeground;
if (!/^#[\da-f]{6}$/i.test(terminalSelectionForeground))
  throw new Error("Unsupported terminal selection foreground");
const terminalAnsiKeys = [
  "black",
  "red",
  "green",
  "yellow",
  "blue",
  "magenta",
  "cyan",
  "white",
  "brightBlack",
  "brightRed",
  "brightGreen",
  "brightYellow",
  "brightBlue",
  "brightMagenta",
  "brightCyan",
  "brightWhite",
];
const terminalColors = ["background", "foreground", "cursor", "cursorAccent"];
const settingsFile = path.join(root, "packages/app/src/hooks/use-settings/storage.ts");
const settingsSource = ts.createSourceFile(
  settingsFile,
  await fs.readFile(settingsFile, "utf8"),
  ts.ScriptTarget.Latest,
  true,
);
const codeSizeDeclaration = settingsSource.statements
  .filter(ts.isVariableStatement)
  .flatMap((statement) => statement.declarationList.declarations)
  .find((declaration) => declaration.name.getText(settingsSource) === "DEFAULT_CODE_FONT_SIZE");
if (!codeSizeDeclaration?.initializer || !ts.isNumericLiteral(codeSizeDeclaration.initializer))
  throw new Error("Desktop terminal code-size default requires a numeric settings declaration");
const terminalCodeSize = Number(codeSizeDeclaration.initializer.text);
const terminalHex = (key) => {
  const value = theme.colors.terminal[key];
  if (!/^#[\da-f]{6}$/i.test(value)) throw new Error(`Unsupported authored terminal color: ${key}`);
  return `0x${value.slice(1).toLowerCase()}`;
};
const fontStack = (stack) =>
  stack
    .split(",")
    .map((family) => JSON.stringify(family.trim().replace(/^['"]|['"]$/g, "")))
    .join(",");
const terminalRust = `pub mod terminal {\n${terminalColors.map((key) => `pub const ${name(key)}: u32 = ${terminalHex(key)};`).join("\n")}\npub const ANSI: [u32;16] = [${terminalAnsiKeys.map(terminalHex).join(",")}];\npub const FONT_SIZE: f32 = ${terminalCodeSize}.0;\npub const FONT_FAMILIES: &[&str] = &[${fontStack(DEFAULT_TERMINAL_FONT_FAMILY)}];\npub const WINDOWS_FONT_FAMILIES: &[&str] = &[${fontStack(WINDOWS_TERMINAL_FONT_FAMILY)}];\n}\n`;
function syntaxConstants(palette) {
  return Object.entries(palette.colors.syntax)
    .filter(([, value]) => /^#[\da-f]{6}$/i.test(value))
    .map(([role, value]) => `    pub const ${name(role)}: u32 = 0x${value.slice(1).toLowerCase()};`)
    .join("\n");
}
const syntax = syntaxConstants(theme);
const lightSyntax = syntaxConstants(lightTheme);
const rust = `// Generated by npm run sync:desktop:native-ui from packages/app/src/styles/theme.ts.\n// Do not edit.\n#![allow(dead_code)]\n\npub mod colors {\n${colors.map(([key, value]) => `    pub const ${name(key)}: u32 = 0x${value.slice(1).toLowerCase()};`).join("\n")}\n}\n\n${numbers("font_size", theme.fontSize)}\n${numbers("spacing", theme.spacing)}\n${numbers("radius", theme.borderRadius)}\npub mod syntax {\n${syntax}\n}\npub mod syntax_light {\n${lightSyntax}\n}\npub mod terminal_selection {\n    pub const BACKGROUND: u32 = 0x${terminalSelectionColor};\n    pub const FOREGROUND: u32 = 0x${terminalSelectionForeground.slice(1)};\n}\npub const CONTENT_MAX_WIDTH: f32 = ${theme.contentMaxWidth}.0;\n`;
await output("packages/desktop/native/src/theme.rs", rust + terminalRust);
await output(
  "packages/desktop/native/src/identity_colors.rs",
  `// Generated by npm run sync:desktop:native-ui from styles/identity-colors.ts.\n// Do not edit.\npub const COLORS: [u32; ${identities.length}] = [${identities.map((hex) => `0x${hex.slice(1)}`).join(",")}];\n`,
);

const builtins = [
  "antigravity",
  "claude",
  "codex",
  "copilot",
  "kiro",
  "minimax",
  "omp",
  "opencode",
  "pi",
];

async function authoredNode(file, symbolName) {
  const source = ts.createSourceFile(
    file,
    await fs.readFile(path.join(root, file), "utf8"),
    ts.ScriptTarget.Latest,
    true,
    ts.ScriptKind.TSX,
  );
  for (const statement of source.statements) {
    if (ts.isFunctionDeclaration(statement) && statement.name?.text === symbolName)
      return statement.getText(source);
    if (ts.isVariableStatement(statement)) {
      const declaration = statement.declarationList.declarations.find(
        (node) => ts.isIdentifier(node.name) && node.name.text === symbolName,
      );
      if (declaration) return `const ${declaration.getText(source)};`;
    }
  }
  throw new Error(`Native UI source is missing: ${file}:${symbolName}`);
}
// Select authored style properties without importing the app or its native modules.
async function authoredStyles(file, keys) {
  const text = await authoredNode(file, "styles");
  const source = ts.createSourceFile(file, text, ts.ScriptTarget.Latest, true, ts.ScriptKind.TSX);
  const selected = new Map();
  function visit(node) {
    if (ts.isPropertyAssignment(node) && keys.includes(node.name.getText(source)))
      selected.set(node.name.getText(source), node.getText(source));
    ts.forEachChild(node, visit);
  }
  visit(source);
  if (selected.size !== keys.length) throw new Error(`Missing native UI styles in ${file}`);
  return [...selected.values()].join(",\n");
}
// Evaluate only authored scalar styles; native framework imports stay outside the generator.
const filesStyle = await authoredStyles("packages/app/src/components/file-explorer-pane.tsx", [
  "sortTrigger",
  "sortTriggerText",
  "entriesContent",
]);
const toolbarControlsStyle = await authoredStyles(
  "packages/app/src/components/ui/pane-content-toolbar.tsx",
  ["controls"],
);
const explorerPresentationScript = ts.transpileModule(
  `
${await authoredNode("packages/app/src/components/ui/icon-button-chrome.ts", "SMALL_ICON_BUTTON_SIZE")}
${await authoredNode("packages/app/src/components/ui/icon-button-chrome.ts", "iconButtonChromeGlyphSize")}
${await authoredNode("packages/app/src/components/ui/icon-button-chrome.ts", "smallIconButtonChromeFrameSize")}
${await authoredNode("packages/app/src/components/ui/pane-content-toolbar.tsx", "paneContentToolbarIconSize")}
${await authoredNode("packages/app/src/components/ui/pane-content-toolbar.tsx", "paneContentToolbarTrailingPadding")}
${await authoredNode("packages/app/src/constants/layout.ts", "WORKSPACE_SECONDARY_HEADER_HEIGHT")}
${await authoredNode("packages/app/src/components/tree-primitives.tsx", "TREE_INDENT_PER_LEVEL")}
${await authoredNode("packages/app/src/components/tree-primitives.tsx", "WORKSPACE_FILE_ROW_VERTICAL_PADDING")}
${await authoredNode("packages/app/src/components/tree-primitives.tsx", "WORKSPACE_TREE_ICON_SIZE")}
${await authoredNode("packages/app/src/components/tree-primitives.tsx", "WORKSPACE_TREE_ICON_LABEL_GAP")}
${await authoredNode("packages/app/src/components/tree-primitives.tsx", "WORKSPACE_PANE_TRAILING_GLYPH_RAIL")}
${await authoredNode("packages/app/src/components/tree-primitives.tsx", "treeRowPaddingLeft")}
const fileStyles=({${filesStyle}});
const toolbarControls=({${toolbarControlsStyle}});
const glyphSize=iconButtonChromeGlyphSize("small",false);
globalThis.values={
 TOOLBAR_HEIGHT:WORKSPACE_SECONDARY_HEADER_HEIGHT, CONTROL_SIZE:SMALL_ICON_BUTTON_SIZE,
 CONTROL_GAP:toolbarControls.controls.gap, GLYPH_SIZE:glyphSize,
 TRAILING_PADDING:paneContentToolbarTrailingPadding(false,"glyph"),
 SORT_HEIGHT:fileStyles.sortTrigger.height,SORT_MARGIN_LEFT:fileStyles.sortTrigger.marginLeft,
 SORT_PADDING_X:fileStyles.sortTrigger.paddingHorizontal,SORT_GAP:fileStyles.sortTrigger.gap,
 SORT_RADIUS:fileStyles.sortTrigger.borderRadius,SORT_FONT:fileStyles.sortTriggerText.fontSize,
 TREE_TOP:fileStyles.entriesContent.paddingTop,TREE_BOTTOM:fileStyles.entriesContent.paddingBottom,
 TREE_LEFT:treeRowPaddingLeft(0),TREE_RIGHT:WORKSPACE_PANE_TRAILING_GLYPH_RAIL,
 TREE_INDENT:treeRowPaddingLeft(1)-treeRowPaddingLeft(0),TREE_ICON:WORKSPACE_TREE_ICON_SIZE,
 TREE_GAP:WORKSPACE_TREE_ICON_LABEL_GAP,TREE_PADDING_Y:WORKSPACE_FILE_ROW_VERTICAL_PADDING
};`,
  { compilerOptions: { target: ts.ScriptTarget.ES2022, module: ts.ModuleKind.CommonJS } },
).outputText;
const explorerPresentationContext = { theme, SPACING: theme.spacing, exports: {} };
runInNewContext(explorerPresentationScript, explorerPresentationContext, { timeout: 1000 });
await output(
  "packages/desktop/native/src/explorer_presentation.rs",
  `// Generated from authored Files toolbar and tree presentation. Do not edit.\n#![allow(dead_code)]\n${Object.entries(
    explorerPresentationContext.values,
  )
    .map(([key, value]) => `pub const ${key}:f32=${Number(value).toFixed(1)};`)
    .join("\n")}\n`,
);
const commandFile = "packages/app/src/command-center/command-center.tsx";
const commandStyle = await authoredStyles(commandFile, [
  "overlay",
  "panel",
  "header",
  "searchRow",
  "scopeChip",
  "rowContent",
  "row",
  "title",
  "subtitle",
  "fileLine",
  "sectionLabel",
  "emptyText",
]);
const commandMetricsFile = "packages/app/src/command-center/list-metrics.ts";
const commandContext = { theme, SPACING: theme.spacing, exports: {} };
runInNewContext(
  ts.transpileModule(
    `
${await authoredNode(commandMetricsFile, "COMMAND_CENTER_LINE_HEIGHT")}
${await authoredNode(commandMetricsFile, "COMMAND_CENTER_SECTION_GAP")}
${await authoredNode(commandMetricsFile, "SECTION_DIVIDER_THICKNESS")}
${await authoredNode(commandMetricsFile, "listMetrics")}
${await authoredNode(commandMetricsFile, "COMMAND_CENTER_LIST_METRICS")}
const styles=({${commandStyle}});const metrics=COMMAND_CENTER_LIST_METRICS.desktop;
globalThis.values={WIDTH:styles.panel.width,HEIGHT:styles.panel.height,MAX_WIDTH_RATIO:parseFloat(styles.panel.maxWidth)/100,MAX_HEIGHT_RATIO:parseFloat(styles.panel.maxHeight)/100,TOP:styles.overlay.paddingTop,RADIUS:styles.panel.borderRadius,
 HEADER_PADDING_X:styles.header.paddingHorizontal,HEADER_PADDING_Y:styles.header.paddingVertical,HEADER_GAP:styles.searchRow.gap,
 CHIP_PADDING_X:styles.scopeChip.paddingHorizontal,CHIP_MARGIN_RIGHT:styles.scopeChip.marginRight,CHIP_RADIUS:styles.scopeChip.borderRadius,CHIP_GAP:styles.scopeChip.gap,
 ROW_PADDING_X:styles.row.paddingHorizontal,ROW_GAP:styles.rowContent.gap,ROW_INSET:(metrics.singleLineRow-COMMAND_CENTER_LINE_HEIGHT.singleLine)/2,
 SINGLE_ROW_HEIGHT:metrics.singleLineRow,TWO_ROW_HEIGHT:metrics.twoLineRow,SECTION_HEIGHT:metrics.sectionTitle,DIVIDER_HEIGHT:metrics.sectionDivider,SECTION_GAP:COMMAND_CENTER_SECTION_GAP,
 TITLE_FONT:styles.title.fontSize,SUBTITLE_FONT:styles.subtitle.fontSize,TITLE_LINE_HEIGHT:styles.title.lineHeight,SUBTITLE_LINE_HEIGHT:styles.subtitle.lineHeight,SINGLE_LINE_HEIGHT:styles.fileLine.lineHeight,SECTION_LINE_HEIGHT:styles.sectionLabel.lineHeight,EMPTY_PADDING_Y:styles.emptyText.paddingVertical};`,
    { compilerOptions: { target: ts.ScriptTarget.ES2022, module: ts.ModuleKind.CommonJS } },
  ).outputText,
  commandContext,
  { timeout: 1000 },
);
await output(
  "packages/desktop/native/src/command_center_presentation.rs",
  `// Generated from authored Command Center styles/list metrics. Do not edit.\n${Object.entries(
    commandContext.values,
  )
    .map(
      ([key, value]) =>
        `pub const ${key}:f32=${Number.isInteger(Number(value)) ? Number(value).toFixed(1) : Number(value)};`,
    )
    .join(
      "\n",
    )}\n${menuShadow(theme.shadow.lg, "DARK_SHADOW")}\n${menuShadow(lightTheme.shadow.lg, "LIGHT_SHADOW")}\n`,
);
const commandMatchBundle = await bundle({
  stdin: {
    contents:
      'export {scoreTextFields} from "./packages/protocol/src/search/text-match.ts"; export {findHighlightRanges} from "./packages/app/src/components/ui/highlighted-text-segments.ts";',
    resolveDir: root,
  },
  bundle: true,
  write: false,
  format: "esm",
  platform: "node",
  alias: {
    "@getpaseo/protocol/search/text-match": path.join(
      root,
      "packages/protocol/src/search/text-match.ts",
    ),
  },
});
const { scoreTextFields, findHighlightRanges } = await import(
  `data:text/javascript;base64,${Buffer.from(commandMatchBundle.outputFiles[0].text).toString("base64")}`
);
const commandMatchCases = [];
for (const query of [
  "",
  " ",
  "\ufeff",
  "auto",
  "aut",
  "file",
  "work",
  "rk",
  "lbl dsgn",
  "labdes",
  "你好",
  "😀",
  "mode auto",
  "SIDEBAR",
  "fi le",
  "foo-bar",
  "i",
  "σ",
])
  for (const fields of [
    ["Auto mode", "Mode"],
    ["Label as Design"],
    ["workspace file sidebar"],
    ["foo-bar.ts"],
    ["你好 世界", "😀file"],
    ["İstanbul", "Σίγμα"],
    ["x file auto y"],
    ["lab des"],
    ["e\u0301", "文件"],
  ]) {
    const score = scoreTextFields(query, fields);
    commandMatchCases.push({
      query,
      fields,
      score,
      highlights: fields.map((text) => findHighlightRanges(query, text)),
    });
  }
const commandMatchOracle = { cases: commandMatchCases };
const commandMatchPath = path.join(
  root,
  "packages/desktop/native/src/testdata/command-center-match.json",
);
if (check) {
  if (
    JSON.stringify(JSON.parse(await fs.readFile(commandMatchPath, "utf8"))) !==
    JSON.stringify(commandMatchOracle)
  )
    throw new Error("Native Command Center matcher oracle is stale");
} else await fs.writeFile(commandMatchPath, JSON.stringify(commandMatchOracle, null, 2) + "\n");
const fileIconBundle = await bundle({
  stdin: {
    contents:
      'export {getFileIconSvg} from "./packages/app/src/components/file-icon-svg.ts"; export {getRawFileIconSvg} from "./packages/app/src/components/material-file-icons.ts";',
    resolveDir: root,
  },
  bundle: true,
  write: false,
  format: "esm",
  platform: "node",
  alias: { "@": path.join(root, "packages/app/src") },
});
const { getFileIconSvg, getRawFileIconSvg } = await import(
  `data:text/javascript;base64,${Buffer.from(fileIconBundle.outputFiles[0].text).toString("base64")}`
);
const vendorFile = "packages/app/src/components/material-file-icons.ts";
const fileIconContext = { exports: {} };
runInNewContext(
  ts.transpileModule(
    `${await authoredNode(vendorFile, "SVG_ICONS")}\n${await authoredNode(vendorFile, "EXTENSION_TO_ICON")}\nglobalThis.icons=SVG_ICONS;globalThis.extensions=EXTENSION_TO_ICON;`,
    { compilerOptions: { target: ts.ScriptTarget.ES2022, module: ts.ModuleKind.CommonJS } },
  ).outputText,
  fileIconContext,
  { timeout: 1000 },
);
const representatives = new Map([["_default", "unmapped"]]);
for (const [ext, iconName] of Object.entries(fileIconContext.extensions))
  if (!representatives.has(iconName)) representatives.set(iconName, `icon.${ext}`);
if (representatives.size !== Object.keys(fileIconContext.icons).length)
  throw new Error("Unmapped authored file icon requires a representative filename");
const fileIconData = `// Generated from the app's material icon table and canonical desaturation. Do not edit.\npub fn extension_icon(extension:&str)->Option<&'static str>{match extension{${Object.entries(
  fileIconContext.extensions,
)
  .map(([ext, iconName]) => `${JSON.stringify(ext)}=>Some(${JSON.stringify(iconName)}),`)
  .join(
    "\n",
  )}_=>None}}\npub fn svg(name:&str)->&'static str{match name{${[...representatives].map(([iconName, file]) => `${JSON.stringify(iconName)}=>r###"${getFileIconSvg(file)}"###,`).join("\n")}_=>unreachable!("generated file icon name")}}\n`;
await output("packages/desktop/native/src/file_icon_data.rs", fileIconData);
const fileIconCases = [
  "",
  "unmapped",
  "trailing.",
  ".gitignore",
  "README",
  "README.MD",
  "foo.tar.gz",
  "目录/图.Ts",
  "foo.bar.TSX",
  ...Object.keys(fileIconContext.extensions).flatMap((ext) => [
    `icon.${ext}`,
    `icon.${ext.toUpperCase()}`,
  ]),
].map((file) => [
  file,
  Object.entries(fileIconContext.icons).find(([, svg]) => svg === getRawFileIconSvg(file))?.[0],
]);
if (fileIconCases.some(([, iconName]) => !iconName)) throw new Error("Unknown authored file icon");
const fileIconOracle = JSON.stringify({ cases: fileIconCases });
const fileIconOraclePath = path.join(root, "packages/desktop/native/src/testdata/file-icons.json");
if (check) {
  if (JSON.stringify(JSON.parse(await fs.readFile(fileIconOraclePath, "utf8"))) !== fileIconOracle)
    throw new Error("Native file-icon oracle is stale");
} else
  await fs.writeFile(
    fileIconOraclePath,
    JSON.stringify(JSON.parse(fileIconOracle), null, 2) + "\n",
  );
const syncStyles = await authoredStyles("packages/app/src/panels/agent-panel.tsx", [
  "timelineSyncCalloutRail",
  "timelineSyncCalloutContent",
  "timelineSyncCallout",
  "timelineSyncCalloutText",
]);
const buttonStyles = await authoredStyles("packages/app/src/components/ui/button.tsx", [
  "base",
  "secondary",
  "pressed",
  "disabled",
  "text",
]);
const toastStyles = await authoredStyles("packages/app/src/components/toast-host.tsx", [
  "container",
  "widthBoundary",
  "toast",
  "message",
]);
const geometrySource = await fs.readFile(
  path.join(root, "packages/app/src/components/ui/control-geometry.ts"),
  "utf8",
);
const geometryBody = geometrySource.replace(/^import[^;]+;\s*/gm, "");
const script = ts.transpileModule(
  `
${await authoredNode("packages/app/src/styles/theme.ts", "ICON_SIZE")}
${geometryBody}
${await authoredNode("packages/app/src/components/toast-host.tsx", "TOAST_MAX_WIDTH")}
const geometry=createControlGeometry(theme);
const sync=({${syncStyles}}),button=({${buttonStyles}}),toast=({${toastStyles}});
globalThis.presentation={sync,button,toast,geometry};
`,
  { compilerOptions: { target: ts.ScriptTarget.ES2022, module: ts.ModuleKind.CommonJS } },
).outputText;
const context = { theme, OVERLAY_Z: { toast: 0 }, exports: {} };
runInNewContext(script, context, { timeout: 1000 });
const { sync, button, toast, geometry: controls } = context.presentation;
const syncCallout = sync.timelineSyncCallout,
  r = sync.timelineSyncCalloutRail,
  t = toast.toast;
const values = {
  RAIL_PADDING_X: r.paddingHorizontal,
  RAIL_PADDING_TOP: r.paddingTop,
  MAX_WIDTH: sync.timelineSyncCalloutContent.maxWidth,
  CALLOUT_GAP: syncCallout.gap,
  CALLOUT_BORDER: syncCallout.borderWidth,
  CALLOUT_RADIUS: syncCallout.borderRadius,
  CALLOUT_PADDING_X: syncCallout.paddingHorizontal,
  CALLOUT_PADDING_Y: syncCallout.paddingVertical,
  BUTTON_HEIGHT: controls.buttonSm.minHeight,
  BUTTON_PADDING_X: controls.buttonSm.paddingHorizontal,
  BUTTON_RADIUS: controls.buttonSm.borderRadius,
  BUTTON_BORDER: button.base.borderWidth,
  BUTTON_PRESSED_OPACITY: button.pressed.opacity,
  BUTTON_DISABLED_OPACITY: button.disabled.opacity,
  TOAST_TOP: toast.container("panel", 0).top,
  TOAST_INSET_X: toast.container("panel", 0).left,
  TOAST_MAX_WIDTH: toast.widthBoundary.maxWidth,
  TOAST_WIDTH_RATIO: parseFloat(toast.widthBoundary.width) / 100,
  TOAST_GAP: t.gap,
  TOAST_RADIUS: t.borderRadius,
  TOAST_BORDER: t.borderWidth,
  TOAST_PADDING_X: t.paddingHorizontal,
  TOAST_PADDING_Y: t.paddingVertical,
  TOAST_SHADOW_X: t.shadowOffset.width,
  TOAST_SHADOW_Y: t.shadowOffset.height,
  TOAST_SHADOW_BLUR: t.shadowRadius,
};
const syncStatusFile = "packages/app/src/timeline/sync-status.tsx";
const syncStatus = ts.createSourceFile(
  syncStatusFile,
  await authoredNode(syncStatusFile, "TimelineSyncStatus"),
  ts.ScriptTarget.Latest,
  true,
  ts.ScriptKind.TSX,
);
function findStatusSpinner(node) {
  if (
    ts.isJsxSelfClosingElement(node) &&
    node.tagName.getText(syncStatus) === "ThemedLoadingSpinner"
  ) {
    const size = node.attributes.properties.find(
      (p) => ts.isJsxAttribute(p) && p.name.text === "size",
    );
    values.SPINNER_TOAST = Number(size.initializer.expression.getText(syncStatus));
  }
  ts.forEachChild(node, findStatusSpinner);
}
findStatusSpinner(syncStatus);
if (!Number.isFinite(values.SPINNER_TOAST)) throw new Error("Timeline status spinner has no size");
values.CALLOUT_TEXT_SIZE = sync.timelineSyncCalloutText.fontSize;
values.BUTTON_TEXT_SIZE = button.text.fontSize;
values.TOAST_TEXT_SIZE = toast.message.fontSize;
const componentColors = {
  CALLOUT_FILL: syncCallout.backgroundColor,
  CALLOUT_BORDER_COLOR: syncCallout.borderColor,
  CALLOUT_TEXT_COLOR: sync.timelineSyncCalloutText.color,
  BUTTON_FILL: button.secondary.backgroundColor,
  BUTTON_BORDER_COLOR: button.secondary.borderColor,
  BUTTON_TEXT_COLOR: button.text.color,
  TOAST_FILL: t.backgroundColor,
  TOAST_BORDER_COLOR: t.borderColor,
  TOAST_TEXT_COLOR: toast.message.color,
};
const shadowColor = t.shadowColor.match(/[\d.]+/g).map(Number);
const shadowRgba = shadowColor
  .slice(0, 3)
  .concat(Math.round(shadowColor[3] * 255))
  .map((n) => n.toString(16).padStart(2, "0"))
  .join("");
const enSource = await authoredNode("packages/app/src/i18n/resources/en.ts", "en");
const labelsContext = {};
runInNewContext(
  ts.transpileModule(enSource + ";globalThis.labels=en;", {
    compilerOptions: { target: ts.ScriptTarget.ES2022 },
  }).outputText,
  labelsContext,
  { timeout: 1000 },
);
const labels = {
  ERROR: labelsContext.labels.agentPanel.states.timelineSyncFailed,
  RETRY: labelsContext.labels.common.actions.retry,
  RETRYING: labelsContext.labels.agentPanel.states.timelineSyncRetrying,
  UPDATING: labelsContext.labels.agentPanel.states.updating,
};
const spinnerFile = "node_modules/react-native-web/dist/exports/ActivityIndicator/index.js";
const spinnerContext = { React: { createElement }, StyleSheet: { create: (v) => v } };
runInNewContext(
  `${await authoredNode(spinnerFile, "createSvgCircle")}\n${await authoredNode(spinnerFile, "indicatorSizes")}\nglobalThis.circle=createSvgCircle;globalThis.sizes=indicatorSizes;`,
  spinnerContext,
  { timeout: 1000 },
);
values.SPINNER_LARGE = spinnerContext.sizes.large.width;
values.SPINNER_SMALL = spinnerContext.sizes.small.width;
// Read the installed renderer's rotation duration instead of using the agent-ring timing.
const activityStyle = await authoredStyles(spinnerFile, ["animation"]);
const animationContext = {};
runInNewContext(`globalThis.styles=({${activityStyle}})`, animationContext, { timeout: 1000 });
values.SPINNER_DURATION_MS = parseFloat(animationContext.styles.animation.animationDuration) * 1000;
await output(
  "packages/desktop/native/src/sync_presentation.rs",
  `// Generated from agent-panel, Button, ToastViewport and ActivityIndicator. Do not edit.\n${Object.entries(
    values,
  )
    .map(([key, value]) => `pub const ${key}:f32=${Number(value).toFixed(3)};`)
    .join("\n")}\npub const TOAST_SHADOW_RGBA:u32=0x${shadowRgba};\n${Object.entries(
    componentColors,
  )
    .map(([key, value]) => `pub const ${key}:u32=0x${value.slice(1)};`)
    .join("\n")}\n${Object.entries(labels)
    .map(([key, value]) => `pub const ${key}:&str=${JSON.stringify(value)};`)
    .join("\n")}\n`,
);
const spinnerComponent = ts.createSourceFile(
  spinnerFile,
  await authoredNode(spinnerFile, "ActivityIndicator"),
  ts.ScriptTarget.Latest,
  true,
);
let spinnerElement;
function findSpinnerSvg(node) {
  if (ts.isVariableDeclaration(node) && node.name.getText(spinnerComponent) === "svg")
    spinnerElement = node.initializer.getText(spinnerComponent);
  ts.forEachChild(node, findSpinnerSvg);
}
findSpinnerSvg(spinnerComponent);
if (!spinnerElement) throw new Error("Installed ActivityIndicator has no SVG");
spinnerContext.color = "#000000";
runInNewContext(`globalThis.svg=${spinnerElement}`, spinnerContext, { timeout: 1000 });
const loadingSpinnerSvg = renderToStaticMarkup(spinnerContext.svg).replace(
  "<svg ",
  '<svg xmlns="http://www.w3.org/2000/svg" ',
);

await output(
  "packages/desktop/native/assets/icons/ACTIVITY-INDICATOR-LICENSE",
  await fs.readFile(path.join(root, "node_modules/react-native-web/LICENSE"), "utf8"),
);

// Vendor the pinned app parser so Cargo builds do not need Node or node_modules.
for (const file of ["md4c.c", "md4c.h"]) {
  const source = await fs.readFile(
    path.join(root, "packages/app/node_modules/react-native-enriched-markdown/cpp/md4c", file),
  );
  const destination = path.join(root, "packages/desktop/native/vendor/md4c", file);
  if (check) {
    const current = await fs.readFile(destination);
    if (!current.equals(source)) throw new Error(`Native Markdown parser drift: ${file}`);
  } else await fs.writeFile(destination, source);
}
await output(
  "packages/desktop/native/vendor/md4c/character-entities.LICENSE",
  await fs.readFile(path.join(root, "node_modules/character-entities/license"), "utf8"),
);
await output(
  "packages/desktop/native/src/markdown_entities.json",
  JSON.stringify(characterEntities, null, 2) + "\n",
);
const markdownStylesSource = await authoredNode(
  "packages/app/src/components/markdown/styles.ts",
  "createMarkdownStyles",
);
const { outputFiles: markdownOutput } = await bundle({
  stdin: {
    contents: `const isWeb=true;const WEB_CONTAINER={};${markdownStylesSource}`,
    loader: "ts",
    resolveDir: root,
  },
  write: false,
  format: "esm",
  platform: "node",
});
const { createMarkdownStyles } = await import(
  `data:text/javascript;base64,${Buffer.from(markdownOutput[0].text).toString("base64")}`
);
const densitySource = await authoredNode("packages/app/src/styles/density.ts", "density");
const { outputFiles: densityOutput } = await bundle({
  stdin: {
    contents: `const byLayout=(xs,md)=>({xs,md});${densitySource};export {density};`,
    loader: "ts",
    resolveDir: root,
  },
  write: false,
  format: "esm",
  platform: "node",
});
const { density } = await import(
  `data:text/javascript;base64,${Buffer.from(densityOutput[0].text).toString("base64")}`
);
const markdownPresentation = createMarkdownStyles(theme, { blockGap: density.blockGap.md });
const markdownStyles = markdownPresentation.markdownStyle;
const inlineCodeSource = await authoredNode(
  "packages/app/node_modules/react-native-enriched-markdown/src/web/styles.ts",
  "codeStyle",
);
const { outputFiles: inlineCodeOutput } = await bundle({
  stdin: {
    contents: `const normalizeFontFamily=f=>f;${inlineCodeSource};export {codeStyle};`,
    loader: "ts",
    resolveDir: root,
  },
  write: false,
  format: "esm",
  platform: "node",
});
const { codeStyle } = await import(
  `data:text/javascript;base64,${Buffer.from(inlineCodeOutput[0].text).toString("base64")}`
);
const inlineCode = codeStyle({ code: markdownStyles.code });
const inlinePadding = inlineCode.padding.split(" ").map(parseFloat);
const inlineBorder = parseFloat(inlineCode.border);
const selectionComponents = markdownPresentation.selectionColor.match(/[\d.]+/g).map(Number);
const selectionRgba =
  selectionComponents
    .slice(0, 3)
    .map((value) => value.toString(16).padStart(2, "0"))
    .join("") +
  Math.round(selectionComponents[3] * 255)
    .toString(16)
    .padStart(2, "0");
await output(
  "packages/desktop/native/src/message_geometry.rs",
  `// Generated from the existing message and Markdown presentation. Do not edit.\npub const BLOCK_GAP:f32=${markdownStyles.paragraph.marginBottom}.0;\npub const BODY_SIZE:f32=${markdownStyles.paragraph.fontSize}.0;\npub const BODY_LINE:f32=${markdownStyles.paragraph.lineHeight}.0;\npub const HEADINGS:[(f32,f32);6]=[${[1, 2, 3, 4, 5, 6].map((level) => `(${markdownStyles["h" + level].fontSize}.0,${markdownStyles["h" + level].lineHeight}.0)`).join(",")}];\npub const INLINE_CODE_SCALE:f32=${markdownStyles.code.fontScale};\npub const INLINE_CODE_PADDING_X:f32=${inlinePadding[1] + inlineBorder}.0;\npub const INLINE_CODE_PADDING_Y:f32=${inlinePadding[0] + inlineBorder}.0;\npub const INLINE_CODE_RADIUS:f32=${inlineCode.borderRadius}.0;\npub const SELECTION_RGBA:u32=0x${selectionRgba};\npub const CODE_SIZE:f32=${markdownStyles.codeBlock.fontSize}.0;\npub const CODE_PADDING:f32=${markdownStyles.codeBlock.padding}.0;\npub const QUOTE_PADDING:f32=${markdownStyles.blockquote.padding}.0;\npub const QUOTE_BORDER:f32=${markdownStyles.blockquote.borderWidth}.0;\npub const LIST_INDENT:f32=${markdownStyles.list.marginLeft}.0;\npub const LIST_ITEM_GAP:f32=${markdownStyles.list.itemSpacing}.0;\n`,
);
const parserHashes = Object.fromEntries(
  await Promise.all(
    ["md4c.c", "md4c.h"].map(async (file) => [
      file,
      createHash("sha256")
        .update(await fs.readFile(path.join(root, "packages/desktop/native/vendor/md4c", file)))
        .digest("hex"),
    ]),
  ),
);
await output(
  "packages/desktop/native/vendor/md4c/source.json",
  JSON.stringify(
    {
      source: "react-native-enriched-markdown/cpp/md4c",
      version: JSON.parse(
        await fs.readFile(
          path.join(root, "packages/app/node_modules/react-native-enriched-markdown/package.json"),
          "utf8",
        ),
      ).version,
      sha256: parserHashes,
    },
    null,
    2,
  ) + "\n",
);
const attachmentSource = [
  `const SPACING=${JSON.stringify(theme.spacing)};`,
  await authoredNode("packages/app/src/components/attachment-lightbox.tsx", "LIGHTBOX_FIT"),
  await authoredNode(
    "packages/app/src/components/ui/icon-button-chrome.ts",
    "SMALL_ICON_BUTTON_SIZE",
  ),
  await authoredNode(
    "packages/app/src/components/attachment-pill.tsx",
    "ATTACHMENT_CONTENT_HEIGHT",
  ),
  await authoredNode("packages/app/src/attachments/upload-limit.ts", "FILE_UPLOAD_MAX_BYTES"),
  await authoredNode(
    "packages/app/src/attachments/file-types.ts",
    "RASTER_IMAGE_MIME_TYPE_BY_EXTENSION",
  ),
].join("\n");
const { outputFiles: attachmentOutput } = await bundle({
  stdin: {
    contents:
      attachmentSource +
      "\nexport {ATTACHMENT_CONTENT_HEIGHT,FILE_UPLOAD_MAX_BYTES,RASTER_IMAGE_MIME_TYPE_BY_EXTENSION,LIGHTBOX_FIT,SMALL_ICON_BUTTON_SIZE};",
    loader: "ts",
    resolveDir: root,
  },
  write: false,
  format: "esm",
  platform: "node",
});
const attachmentGeometry = await import(
  `data:text/javascript;base64,${Buffer.from(attachmentOutput[0].text).toString("base64")}`
);
await output(
  "packages/desktop/native/src/attachment_geometry.rs",
  `// Generated from the authored attachment pill, upload limit and file-type table. Do not edit.\npub const CONTENT_HEIGHT:f32=${attachmentGeometry.ATTACHMENT_CONTENT_HEIGHT}.0;\npub const PREVIEW_MAX_WIDTH:f32=${attachmentGeometry.LIGHTBOX_FIT.maxWidth}.0;\npub const PREVIEW_MAX_HEIGHT:f32=${attachmentGeometry.LIGHTBOX_FIT.maxHeight}.0;\npub const PREVIEW_PADDING:f32=${attachmentGeometry.LIGHTBOX_FIT.padding}.0;\npub const PREVIEW_CONTROL_SIZE:f32=${attachmentGeometry.SMALL_ICON_BUTTON_SIZE}.0;\npub const MAX_UPLOAD_BYTES:u64=${attachmentGeometry.FILE_UPLOAD_MAX_BYTES};\npub const IMAGE_TYPES:&[(&str,&str)]=&[${Object.entries(
    attachmentGeometry.RASTER_IMAGE_MIME_TYPE_BY_EXTENSION,
  )
    .map(([extension, mime]) => `(${JSON.stringify(extension.slice(1))},${JSON.stringify(mime)})`)
    .join(",")}];\n`,
);
// Render the original glyph component at build time; its React/Unistyles container never
// enters the native binary. AST selection preserves the authored SVG and platform geometry.
const windowGlyph = await authoredNode(
  "packages/app/src/components/desktop/window-controls.tsx",
  "WindowControlGlyph",
);
const layoutFile = "packages/app/src/constants/layout.ts";
const windowLayout = [
  await authoredNode(layoutFile, "HEADER_INNER_HEIGHT"),
  await authoredNode(layoutFile, "DESKTOP_WINDOW_CONTROLS_HEIGHT"),
  await authoredNode(
    "packages/app/src/desktop/window-chrome-presentation.ts",
    "PRESENTATION_BY_MODE",
  ),
  await authoredNode("packages/app/src/stores/panel-store/state.ts", "DEFAULT_SIDEBAR_WIDTH"),
].join("\n");
const { outputFiles: vectors } = await bundle({
  stdin: {
    loader: "tsx",
    contents:
      builtins
        .map(
          (id, index) =>
            `import {${{ opencode: "OpenCode", minimax: "MiniMax" }[id] ?? id[0].toUpperCase() + id.slice(1)}Icon as icon${index}} from "./packages/app/src/components/icons/${id}-icon.tsx";`,
        )
        .join("\n") +
      `\nexport const icons={${builtins.map((id, index) => `${id}:icon${index}`).join(",")}};\nimport Svg,{Line,Rect} from "react-native-svg";const styles={glyph:{}};\n${windowGlyph}\nexport {WindowControlGlyph};\n${windowLayout}\nexport const windowPresentation=PRESENTATION_BY_MODE;export const defaultSidebarWidth=DEFAULT_SIDEBAR_WIDTH;`,
    resolveDir: root,
  },
  bundle: true,
  write: false,
  format: "esm",
  platform: "node",
  jsx: "automatic",
  plugins: [
    {
      name: "svg-elements",
      setup(build) {
        build.onResolve({ filter: /^react-native-svg$/ }, () => ({
          path: "svg",
          namespace: "svg",
        }));
        build.onLoad({ filter: /.*/, namespace: "svg" }, () => ({
          contents: 'export default "svg"; export const Path="path",Line="line",Rect="rect";',
        }));
        build.onResolve({ filter: /^react(?:\/.*)?$/ }, (args) => ({
          path: pathToFileURL(require.resolve(args.path)).href,
          external: true,
        }));
      },
    },
  ],
});
const { icons, WindowControlGlyph, windowPresentation, defaultSidebarWidth } = await import(
  `data:text/javascript;base64,${Buffer.from(vectors[0].text).toString("base64")}`
);
const assets = new Map([["loading-spinner", loadingSpinnerSvg]]);
for (const [mode, presentation] of Object.entries(windowPresentation)) {
  for (const kind of ["minimize", "maximize", "restore", "close"]) {
    assets.set(
      `window-${mode}-${kind}`,
      renderToStaticMarkup(
        createElement(WindowControlGlyph, {
          kind,
          color: "#000000",
          size: 16,
          strokeWidth: presentation.glyphStrokeWidth,
        }),
      ).replace("<svg ", '<svg xmlns="http://www.w3.org/2000/svg" '),
    );
  }
}
const geometry = Object.entries(windowPresentation)
  .map(
    ([mode, p]) =>
      `pub const ${mode.replace("custom-", "").toUpperCase()}: Geometry = Geometry { control_width:${p.controlWidth}.0,rail_height:${p.railHeight}.0,control_height:${p.controlHeight}.0,glyph_size:${p.glyphSize}.0,circular:${p.hoverShape === "circle"} };`,
  )
  .join("\n");
await output(
  "packages/desktop/native/src/window_geometry.rs",
  `// Generated by npm run sync:desktop:native-ui from the authored desktop chrome. Do not edit.\n#![allow(dead_code)]\npub struct Geometry {pub control_width:f32,pub rail_height:f32,pub control_height:f32,pub glyph_size:f32,pub circular:bool}\n${geometry}\npub const DEFAULT_SIDEBAR_WIDTH:f32=${defaultSidebarWidth}.0;\n`,
);
assets.set(
  "status-ring-arc",
  '<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24"><path d="M4.04505 4.04505 A11.25 11.25 0 0 1 19.95495 4.04505" fill="none" stroke="#000000" stroke-width="1.5"/></svg>',
);
for (const [id, Icon] of Object.entries(icons)) {
  assets.set(
    id,
    renderToStaticMarkup(createElement(Icon, { size: 24, color: "#000000" })).replace(
      "<svg ",
      '<svg xmlns="http://www.w3.org/2000/svg" ',
    ),
  );
}
// Consume the exact installed RN icon package, including its geometry, rather than another
// Lucide package whose version may differ. Only trusted, generated package source is evaluated.
const lucideRoot = path.join(root, "packages/app/node_modules/lucide-react-native");
for (const id of [
  "plus",
  "history",
  "search",
  "calendar-clock",
  "folder-plus",
  "gauge",
  "server",
  "circle-help",
  "settings",
  "sliders-horizontal",
  "panel-left",
  "panel-right",
  "ellipsis",
  "chevron-down",
  "chevron-right",
  "triangle-alert",
  "circle",
  "circle-dot",
  "circle-check",
  "scissors",
  "file-symlink",
  "check-square",
  "pencil",
  "wrench",
  "summary",
  "brain",
  "bot",
  "check",
  "shield-check",
  "shield",
  "shield-alert",
  "shield-ellipsis",
  "shield-off",
  "shield-plus",
  "shield-question-mark",
  "arrow-up",
  "square",
  "mic",
  "audio-lines",
  "square-terminal",
  "paperclip",
  "image",
  "file-text",
  "copy",
  "archive",
  "files",
  "file-diff",
  "x",
  "folder",
  "folder-open",
  "chevron-right",
  "refresh-cw",
  "eye",
  "eye-off",
  "git-branch",
  "arrow-down",
  "arrow-up-down",
  "play",
  "file-plus",
  "save",
]) {
  const exports = {};
  const sourceId =
    {
      history: "rotate-ccw-clock",
      "circle-help": "circle-question-mark",
      "check-square": "square-check-big",
    }[id] ?? id;
  runInNewContext(
    await fs.readFile(path.join(lucideRoot, `dist/cjs/icons/${sourceId}.js`), "utf8"),
    { exports, require: () => () => null },
    { timeout: 1000 },
  );
  const data = exports.__iconData;
  if (!data?.node || data.size !== 24) throw new Error(`Unknown Lucide icon format: ${id}`);
  assets.set(
    id,
    renderToStaticMarkup(
      createElement(
        "svg",
        {
          xmlns: "http://www.w3.org/2000/svg",
          viewBox: "0 0 24 24",
          fill: "none",
          stroke: "#000000",
          strokeWidth: 1.5,
          strokeLinecap: "round",
          strokeLinejoin: "round",
        },
        data.node.map(([tag, props], index) => createElement(tag, { ...props, key: index })),
      ),
    ),
  );
}
for (const [id, svg] of assets)
  await output(`packages/desktop/native/assets/icons/${id}.svg`, svg + "\n");
await output(
  "packages/desktop/native/assets/icons/LUCIDE-LICENSE",
  await fs.readFile(path.join(lucideRoot, "LICENSE"), "utf8"),
);
await output(
  "packages/desktop/native/src/assets.rs",
  `// Generated by npm run sync:desktop:native-ui. Do not edit.\nuse std::borrow::Cow;\nuse gpui::{AssetSource, SharedString};\n\npub struct Assets;\n\nimpl AssetSource for Assets {\n    fn load(&self, path: &str) -> anyhow::Result<Option<Cow<'static, [u8]>>> {\n        let bytes: &'static [u8] = match path {\n${[...assets.keys()].map((id) => `            "icons/${id}.svg" => include_bytes!("../assets/icons/${id}.svg"),`).join("\n")}\n            _ => return Ok(None),\n        };\n        Ok(Some(Cow::Borrowed(bytes)))\n    }\n\n    fn list(&self, path: &str) -> anyhow::Result<Vec<SharedString>> {\n        Ok([${[...assets.keys()].map((id) => `"icons/${id}.svg"`).join(", ")}].into_iter().filter(|name| name.starts_with(path)).map(SharedString::from).collect())\n    }\n}\n`,
);

// ---- Native locale catalogs ----
// The authored app translation resources stay the source of truth for the native
// client. They are evaluated once here (including the pluginSettings spread) and
// serialized to flat dotted-key JSON files that Cargo embeds with include_str!,
// so the native binary needs no JavaScript runtime for translations.
const localeExports = {
  en: "en",
  ar: "ar",
  es: "es",
  fr: "fr",
  ja: "ja",
  ko: "ko",
  "pt-BR": "ptBR",
  ru: "ru",
  "zh-CN": "zhCN",
};
const { outputFiles: localeOutput } = await bundle({
  stdin: {
    contents:
      Object.entries(localeExports)
        .map(
          ([locale, symbol]) =>
            `import { ${symbol} } from "./packages/app/src/i18n/resources/${locale}.ts";`,
        )
        .join("\n") +
      `\nexport const catalogs = { ${Object.entries(localeExports)
        .map(([locale, symbol]) => `${JSON.stringify(locale)}: ${symbol}`)
        .join(",")} };`,
    resolveDir: root,
  },
  bundle: true,
  write: false,
  format: "esm",
  platform: "node",
});
const { catalogs } = await import(
  `data:text/javascript;base64,${Buffer.from(localeOutput[0].text).toString("base64")}`
);

function flattenCatalog(value, prefix, into) {
  for (const [key, child] of Object.entries(value)) {
    const dotted = prefix ? `${prefix}.${key}` : key;
    if (typeof child === "string") {
      into[dotted] = child;
    } else if (child && typeof child === "object") {
      flattenCatalog(child, dotted, into);
    } else {
      throw new Error(`Native locale catalog has a non-string value at ${dotted}`);
    }
  }
  return into;
}
// i18next v4 plural suffixes collapse to the canonical key when the base key
// also exists; locales may add the explicit forms their plural rules need.
const pluralSuffix = /_(zero|one|two|few|many|other)$/;
const canonicalKey = (key) => {
  const base = key.replace(pluralSuffix, "");
  return base !== key && key in catalogs.en ? base : key;
};
const placeholderPattern = /\{\{[^}]+\}\}/g;
const englishFlat = flattenCatalog(catalogs.en, "", {});
const englishCanonical = new Map(Object.keys(englishFlat).map((key) => [canonicalKey(key), key]));
const generatedHashes = [];
for (const locale of Object.keys(localeExports)) {
  const flat = flattenCatalog(catalogs[locale], "", {});
  const missing = [...englishCanonical.keys()]
    .filter((key) => !Object.keys(flat).some((candidate) => canonicalKey(candidate) === key))
    .sort();
  if (missing.length > 0)
    throw new Error(`Native locale catalog ${locale} is missing keys: ${missing.join(", ")}`);
  for (const [key, value] of Object.entries(flat)) {
    const english = englishFlat[englishCanonical.get(canonicalKey(key))];
    if (english === undefined) continue;
    const expected = [...english.matchAll(placeholderPattern)].map((match) => match[0]).sort();
    const actual = [...value.matchAll(placeholderPattern)].map((match) => match[0]).sort();
    if (expected.join("|") !== actual.join("|"))
      throw new Error(
        `Native locale catalog ${locale} key ${key} interpolation mismatch: ${expected.join(", ")} -> ${actual.join(", ")}`,
      );
  }
  const serialized = JSON.stringify(
    Object.fromEntries(Object.entries(flat).sort(([a], [b]) => (a < b ? -1 : 1))),
    null,
    2,
  );
  await output(`packages/desktop/native/src/locales/${locale}.json`, serialized + "\n");
  generatedHashes.push([
    locale,
    createHash("sha256")
      .update(await fs.readFile(path.join(root, `packages/app/src/i18n/resources/${locale}.ts`)))
      .digest("hex"),
  ]);
}
await output(
  "packages/desktop/native/src/locales_data.rs",
  `// Generated by npm run sync:desktop:native-ui from packages/app/src/i18n/resources. Do not edit.\n#![allow(dead_code)]\npub const RESOURCE_SOURCE_SHA256: &[(&str, &str)] = &[${generatedHashes
    .map(([locale, hash]) => `(${JSON.stringify(locale)}, ${JSON.stringify(hash)})`)
    .join(", ")}];\n`,
);
