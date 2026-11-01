import { build } from "esbuild";
import fs from "node:fs/promises";
import path from "node:path";
import ts from "typescript";
const root = process.cwd();
const output = process.env.PASEO_NATIVE_QA ?? "/home/ltlly/.cache/paseo-native-qa";
await fs.mkdir(output, { recursive: true });
const settingsFile = path.join(root, "packages/app/src/hooks/use-settings/storage.ts");
const settings = ts.createSourceFile(
  settingsFile,
  await fs.readFile(settingsFile, "utf8"),
  ts.ScriptTarget.Latest,
  true,
);
const codeSize = settings.statements
  .filter(ts.isVariableStatement)
  .flatMap((statement) => statement.declarationList.declarations)
  .find((declaration) => declaration.name.getText(settings) === "DEFAULT_CODE_FONT_SIZE");
if (!codeSize?.initializer || !ts.isNumericLiteral(codeSize.initializer))
  throw new Error("Missing numeric default code font size");
await build({
  stdin: {
    contents: `import {TerminalEmulatorRuntime} from './packages/app/src/terminal/runtime/terminal-emulator-runtime.ts';import {darkTheme} from './packages/app/src/styles/theme.ts';import {toXtermTheme} from './packages/app/src/utils/to-xterm-theme.ts';window.Runtime=TerminalEmulatorRuntime;window.terminalTheme=toXtermTheme(darkTheme.colors.terminal);window.codeSize=${codeSize.initializer.text};`,
    resolveDir: root,
  },
  tsconfig: path.join(root, "packages/app/tsconfig.json"),
  bundle: true,
  format: "iife",
  platform: "browser",
  target: "es2022",
  outfile: path.join(output, "terminal-style-runtime.js"),
  plugins: [
    {
      name: "web-platform",
      setup(b) {
        b.onResolve({ filter: /^react-native$/ }, () => ({ path: "platform", namespace: "qa" }));
        b.onLoad({ filter: /.*/, namespace: "qa" }, () => ({
          contents: 'export const Platform={OS:"web",select:v=>v.web??v.default};',
        }));
      },
    },
  ],
});
