import { readTerminalState } from "./test-utils/terminal-state.js";
import type { TerminalState } from "@getpaseo/protocol/messages";
// POSIX-only: node-pty + POSIX shell assertions
/* eslint-disable max-nested-callbacks */
import { describe, it, expect, afterEach } from "vitest";
import { isPlatform } from "../test-utils/platform.js";
import {
  buildTerminalEnvironment,
  createTerminal,
  resolveZshShellIntegrationDir,
  type LocalTerminalSession as TerminalSession,
} from "./terminal.js";
import {
  chmodSync,
  cpSync,
  existsSync,
  mkdtempSync,
  mkdirSync,
  realpathSync,
  readFileSync,
  rmSync,
  writeFileSync,
} from "node:fs";
import { spawnSync } from "node:child_process";
import { join } from "node:path";
import { createTerminalWorkerProcessInspector } from "./terminal-worker-processes.js";
import { tmpdir, userInfo } from "node:os";

const hasZsh = existsSync("/bin/zsh");

if (isPlatform("win32") && !process.env.ComSpec && !process.env.COMSPEC) {
  process.env.ComSpec = "C:\\Windows\\System32\\cmd.exe";
}

type TerminalRow = TerminalState["grid"][number];

function rowToText(row: TerminalRow): string {
  return row
    .map((cell) => cell.char)
    .join("")
    .trimEnd();
}

// Extract text from a single row
function getRowText(state: TerminalState, rowIndex: number): string {
  return rowToText(state.grid[rowIndex]);
}

// Extract all visible lines as array (trimmed, empty lines included)
function getLines(state: TerminalState): string[] {
  return state.grid.map(rowToText);
}

// Wait for terminal state to match expected lines
async function waitForLines(
  session: TerminalSession,
  expectedLines: string[],
  timeoutMs = 5000,
): Promise<void> {
  const start = Date.now();
  while (Date.now() - start < timeoutMs) {
    const lines = getLines(await readTerminalState(session));
    let matches = true;
    for (let i = 0; i < expectedLines.length; i++) {
      if (lines[i] !== expectedLines[i]) {
        matches = false;
        break;
      }
    }
    if (matches) {
      return;
    }
    await new Promise((resolve) => setTimeout(resolve, 50));
  }
  const actual = getLines(await readTerminalState(session)).slice(0, expectedLines.length);
  throw new Error(
    `Timeout waiting for expected lines.\nExpected:\n${JSON.stringify(expectedLines, null, 2)}\nActual:\n${JSON.stringify(actual, null, 2)}`,
  );
}

async function waitForState(
  session: TerminalSession,
  predicate: (state: TerminalState) => boolean,
  timeoutMs = 5000,
): Promise<TerminalState> {
  const start = Date.now();
  while (Date.now() - start < timeoutMs) {
    const state = await readTerminalState(session);
    if (predicate(state)) {
      return state;
    }
    await new Promise((resolve) => setTimeout(resolve, 50));
  }

  throw new Error("Timeout waiting for terminal state predicate to match");
}

async function waitForActivity(
  session: TerminalSession,
  predicate: (activity: ReturnType<TerminalSession["getActivity"]>) => boolean,
  timeoutMs = 5000,
): Promise<void> {
  const start = Date.now();
  while (Date.now() - start < timeoutMs) {
    if (predicate(session.getActivity())) {
      return;
    }
    await new Promise((resolve) => setTimeout(resolve, 50));
  }
  throw new Error("Timeout waiting for terminal activity predicate to match");
}

function killBackgroundProcess(pid: number): void {
  try {
    process.kill(pid, "SIGTERM");
  } catch (error) {
    if (error instanceof Error && "code" in error && error.code === "ESRCH") return;
    throw error;
  }
}

async function waitForTitle(
  session: TerminalSession,
  predicate: (title: string | undefined) => boolean,
  timeoutMs = 5000,
): Promise<string | undefined> {
  const start = Date.now();
  while (Date.now() - start < timeoutMs) {
    const title = session.getTitle();
    if (predicate(title)) {
      return title;
    }
    await new Promise((resolve) => setTimeout(resolve, 25));
  }

  throw new Error("Timeout waiting for terminal title predicate to match");
}

const sessions: TerminalSession[] = [];

const temporaryDirs: string[] = [];

afterEach(async () => {
  for (const session of sessions) {
    await session.killAndWait();
  }
  sessions.length = 0;
  while (temporaryDirs.length > 0) {
    const dir = temporaryDirs.pop();
    if (dir) {
      rmSync(dir, { recursive: true, force: true });
    }
  }
});

function trackSession(session: TerminalSession): TerminalSession {
  sessions.push(session);
  return session;
}

const DA_HELPER_SCRIPT = `process.stdin.setRawMode(true);
process.stdin.resume();
let buf = "";
const timer = setTimeout(() => {
  process.stdout.write("DA_TIMEOUT\\n");
  process.exit(2);
}, 2500);
process.stdin.on("data", (chunk) => {
  buf += chunk.toString("binary");
  const m = buf.match(/\\x1b\\[\\?[\\d;]+c/);
  if (m) {
    clearTimeout(timer);
    process.stdout.write("DA_OK:" + m[0].slice(1) + "\\n");
    process.exit(0);
  }
});
process.stdout.write("\\x1b[c");
`;

const DSR_HELPER_SCRIPT = `process.stdin.setRawMode(true);
process.stdin.resume();
const mode = process.argv[2] || "cursor";
const query = mode === "private-cursor" ? "\\x1b[?6n" : mode === "status" ? "\\x1b[5n" : "\\x1b[6n";
const pattern = mode === "private-cursor" ? /\\x1b\\[\\?(\\d+);(\\d+)R/ : mode === "status" ? /\\x1b\\[0n/ : /\\x1b\\[(\\d+);(\\d+)R/;
let buf = "";
const timer = setTimeout(() => {
  process.stdout.write("DSR_TIMEOUT\\n");
  process.exit(2);
}, 2500);
process.stdin.on("data", (chunk) => {
  buf += chunk.toString("binary");
  const match = buf.match(pattern);
  if (!match) {
    return;
  }
  clearTimeout(timer);
  if (mode === "status") {
    process.stdout.write("DSR_OK:status\\n");
  } else {
    process.stdout.write("DSR_OK:" + match[1] + ":" + match[2] + "\\n");
  }
  process.exit(0);
});
process.stdout.write(query);
`;

const OSC11_HELPER_SCRIPT = `process.stdin.setRawMode(true);
process.stdin.resume();
let buf = "";
const timer = setTimeout(() => {
  process.stdout.write("OSC11_TIMEOUT\\n");
  process.exit(2);
}, 2500);
process.stdin.on("data", (chunk) => {
  buf += chunk.toString("binary");
  const match = buf.match(/\\x1b\\]11;rgb:[0-9a-f/]+\\x1b\\\\/);
  if (!match) {
    return;
  }
  clearTimeout(timer);
  process.stdout.write("OSC11_OK:" + match[0].replace(/\\x1b/g, "ESC") + "\\n");
  process.exit(0);
});
process.stdout.write("\\x1b]11;?\\x07");
`;

function writeDaHelper(prefix: string): string {
  const dir = mkdtempSync(join(tmpdir(), prefix));
  temporaryDirs.push(dir);
  const path = join(dir, "helper.cjs");
  writeFileSync(path, DA_HELPER_SCRIPT);
  return path;
}

function writeDsrHelper(prefix: string): string {
  const dir = mkdtempSync(join(tmpdir(), prefix));
  temporaryDirs.push(dir);
  const path = join(dir, "helper.cjs");
  writeFileSync(path, DSR_HELPER_SCRIPT);
  return path;
}

function writeOsc11Helper(prefix: string): string {
  const dir = mkdtempSync(join(tmpdir(), prefix));
  temporaryDirs.push(dir);
  const path = join(dir, "helper.cjs");
  writeFileSync(path, OSC11_HELPER_SCRIPT);
  return path;
}

const OSC7501_HELPER_SCRIPT = `process.stdin.setRawMode(true);
process.stdin.resume();
let buf = "";
const timer = setTimeout(() => {
  process.stdout.write("OSC7501_TIMEOUT\\n");
  process.exit(2);
}, 2500);
process.stdin.on("data", (chunk) => {
  buf += chunk.toString("binary");
  const match = buf.match(/\\x1b\\]7501;\\?\\x1b\\\\/);
  if (!match) {
    return;
  }
  clearTimeout(timer);
  process.stdout.write("OSC7501_OK:" + match[0].replace(/\\x1b/g, "ESC") + "\\n");
  process.exit(0);
});
process.stdout.write("\\x1b]7501;?\\x07");
`;

function writeOsc7501Helper(prefix: string): string {
  const dir = mkdtempSync(join(tmpdir(), prefix));
  temporaryDirs.push(dir);
  const path = join(dir, "helper.cjs");
  writeFileSync(path, OSC7501_HELPER_SCRIPT);
  return path;
}

function isDaOkLine(line: string): boolean {
  return line.startsWith("DA_OK:");
}

function isDsrOkLine(line: string): boolean {
  return line.startsWith("DSR_OK:");
}

function isOsc11OkLine(line: string): boolean {
  return line.startsWith("OSC11_OK:");
}

function isOsc7501OkLine(line: string): boolean {
  return line.startsWith("OSC7501_OK:");
}

function hasDaOkLine(state: TerminalState): boolean {
  return getLines(state).some(isDaOkLine);
}

function hasDsrOkLine(state: TerminalState): boolean {
  return getLines(state).some(isDsrOkLine);
}

function hasOsc11OkLine(state: TerminalState): boolean {
  return getLines(state).some(isOsc11OkLine);
}

function hasOsc7501OkLine(state: TerminalState): boolean {
  return getLines(state).some(isOsc7501OkLine);
}

function lastNonEmptyLineIsPrompt(state: TerminalState): boolean {
  const last =
    getLines(state)
      .toReversed()
      .find((line) => line.length > 0) ?? "";
  return last === "$";
}

function removeZshShellIntegrationRuntimeDir(): void {
  rmSync(join(tmpdir(), `${userInfo().username || "unknown"}-paseo-zsh-${process.pid}`), {
    recursive: true,
    force: true,
  });
}

describe.skipIf(isPlatform("win32"))("terminal POSIX-only", () => {
  it("sets zsh wrapper env when spawning zsh", () => {
    const resolvedEnv = buildTerminalEnvironment({
      shell: "/bin/zsh",
      env: {
        HOME: "/tmp/paseo-home",
        ZDOTDIR: "/tmp/paseo-zdotdir",
      },
    });

    expect(resolvedEnv.TERM).toBe("xterm-256color");
    expect(resolvedEnv.TERM_PROGRAM).toBe("kitty");
    expect(resolvedEnv.PASEO_ZSH_ZDOTDIR).toBe("/tmp/paseo-zdotdir");
    expect(resolvedEnv.ZDOTDIR).toBe(
      join(tmpdir(), `${userInfo().username || "unknown"}-paseo-zsh-${process.pid}`),
    );
    expect(existsSync(join(resolvedEnv.ZDOTDIR, ".zshenv"))).toBe(true);
    expect(existsSync(join(resolvedEnv.ZDOTDIR, "paseo-integration.zsh"))).toBe(true);
  });

  it("reuses zsh shell integration copied from read-only source files", () => {
    const integrationSourceDir = mkdtempSync(join(tmpdir(), "paseo-zsh-readonly-source-"));
    const tmpHome = mkdtempSync(join(tmpdir(), "paseo-zsh-readonly-home-"));
    temporaryDirs.push(integrationSourceDir, tmpHome);
    cpSync(resolveZshShellIntegrationDir(), integrationSourceDir, { recursive: true });
    chmodSync(join(integrationSourceDir, ".zshenv"), 0o444);
    chmodSync(join(integrationSourceDir, "paseo-integration.zsh"), 0o444);
    removeZshShellIntegrationRuntimeDir();

    const buildEnvironment = () =>
      buildTerminalEnvironment({
        shell: "/bin/zsh",
        env: { HOME: tmpHome },
        zshShellIntegrationDir: integrationSourceDir,
      });

    buildEnvironment();

    expect(buildEnvironment).not.toThrow();
  });

  describe("send input", () => {
    it("executes a simple echo command", async () => {
      const session = trackSession(
        await createTerminal({
          workspaceId: "ws-test",
          cwd: "/tmp",
          shell: "/bin/sh",
          env: { PS1: "$ " },
        }),
      );

      // Wait for initial prompt, then send command
      await waitForLines(session, ["$"]);

      session.send({ type: "input", data: "echo hello\r" });

      // After running "echo hello", terminal should show:
      // Line 0: "$ echo hello"
      // Line 1: "hello"
      // Line 2: "$"
      await waitForLines(session, ["$ echo hello", "hello", "$"]);

      const state = await readTerminalState(session);
      expect(getRowText(state, 0)).toBe("$ echo hello");
      expect(getRowText(state, 1)).toBe("hello");
      expect(getRowText(state, 2)).toBe("$");
    });

    it("executes multiple commands sequentially", async () => {
      const session = trackSession(
        await createTerminal({
          workspaceId: "ws-test",
          cwd: "/tmp",
          shell: "/bin/sh",
          env: { PS1: "$ " },
        }),
      );

      await waitForLines(session, ["$"]);

      session.send({ type: "input", data: "echo first\r" });
      await waitForLines(session, ["$ echo first", "first", "$"]);

      session.send({ type: "input", data: "echo second\r" });
      await waitForLines(session, ["$ echo first", "first", "$ echo second", "second", "$"]);

      const state = await readTerminalState(session);
      expect(getRowText(state, 0)).toBe("$ echo first");
      expect(getRowText(state, 1)).toBe("first");
      expect(getRowText(state, 2)).toBe("$ echo second");
      expect(getRowText(state, 3)).toBe("second");
      expect(getRowText(state, 4)).toBe("$");
    });

    it("captures output from pwd in specified cwd", async () => {
      const session = trackSession(
        await createTerminal({
          workspaceId: "ws-test",
          cwd: "/tmp",
          shell: "/bin/sh",
          env: { PS1: "$ " },
        }),
      );

      await waitForLines(session, ["$"]);

      session.send({ type: "input", data: "pwd\r" });

      await waitForLines(session, ["$ pwd", "/tmp", "$"]);

      const state = await readTerminalState(session);
      expect(getRowText(state, 0)).toBe("$ pwd");
      expect(getRowText(state, 1)).toBe("/tmp");
      expect(getRowText(state, 2)).toBe("$");
    });
  });

  describe("terminal title", () => {
    it.skipIf(!hasZsh)("restores the user's ZDOTDIR through the zsh wrapper", async () => {
      const homeDir = mkdtempSync(join(tmpdir(), "terminal-zsh-home-"));
      temporaryDirs.push(homeDir);
      const realZdotdir = join(homeDir, ".config", "zsh");
      mkdirSync(realZdotdir, { recursive: true });
      writeFileSync(join(realZdotdir, ".zshenv"), "export PASEO_TEST_REAL_ZDOTDIR=1\n");

      const session = trackSession(
        await createTerminal({
          workspaceId: "ws-test",
          cwd: homeDir,
          command: "/bin/zsh",
          args: ["-c", 'printf \'%s\\n%s\\n\' "${ZDOTDIR-}" "${PASEO_TEST_REAL_ZDOTDIR-}"'],
          env: {
            HOME: homeDir,
            ZDOTDIR: realZdotdir,
          },
        }),
      );

      const exitInfo = await new Promise<NonNullable<ReturnType<TerminalSession["getExitInfo"]>>>(
        (resolve) => {
          session.onExit((info) => resolve(info));
        },
      );

      expect(exitInfo.lastOutputLines).toEqual([realZdotdir, "1"]);
    });

    it("emits the initial title from command args to title listeners", async () => {
      const packageRoot = mkdtempSync(join(tmpdir(), "terminal-title-script-"));
      temporaryDirs.push(packageRoot);
      const scriptPath = join(packageRoot, "npm-cli.js");
      writeFileSync(scriptPath, "setTimeout(() => process.exit(0), 1000);\n");

      const session = trackSession(
        await createTerminal({
          workspaceId: "ws-test",
          cwd: packageRoot,
          command: process.execPath,
          args: [scriptPath, "run", "dev"],
        }),
      );
      const seenTitles: Array<string | undefined> = [];
      const unsubscribeTitle = session.onTitleChange((title) => {
        seenTitles.push(title);
      });

      await waitForTitle(session, (title) => title === "npm run dev");
      await waitForState(session, (state) => state.title === "npm run dev");

      expect(seenTitles).toContain("npm run dev");
      expect(session.getTitle()).toBe("npm run dev");
      expect(session.getTitle()).toBe("npm run dev");

      unsubscribeTitle();
    });

    it("emits OSC title updates to title listeners", async () => {
      const session = trackSession(
        await createTerminal({
          workspaceId: "ws-test",
          cwd: "/tmp",
          shell: "/bin/sh",
          env: { PS1: "$ " },
        }),
      );
      const seenTitles: Array<string | undefined> = [];
      const unsubscribeTitle = session.onTitleChange((title) => {
        seenTitles.push(title);
      });

      await waitForLines(session, ["$"]);
      session.send({ type: "input", data: "printf '\\033]0;Build Log\\007'\r" });

      await waitForTitle(session, (title) => title === "Build Log");

      expect(seenTitles).toContain("Build Log");
      expect(session.getTitle()).toBe("Build Log");
      expect(session.getTitle()).toBe("Build Log");

      unsubscribeTitle();
    });

    it("keeps preset titles instead of applying OSC title updates", async () => {
      const session = trackSession(
        await createTerminal({
          workspaceId: "ws-test",
          cwd: "/tmp",
          shell: "/bin/sh",
          env: { PS1: "$ " },
          title: "typecheck",
        }),
      );

      await waitForLines(session, ["$"]);
      session.send({ type: "input", data: "printf '\\033]0;Build Log\\007'\r" });
      await new Promise((resolve) => setTimeout(resolve, 150));

      expect(session.getTitle()).toBe("typecheck");
      expect(session.getTitle()).toBe("typecheck");
    });

    it("emits command completion from VS Code OSC 633 without visible output", async () => {
      const packageRoot = mkdtempSync(join(tmpdir(), "terminal-command-finished-"));
      temporaryDirs.push(packageRoot);
      const scriptPath = join(packageRoot, "emit-command-finished.sh");
      writeFileSync(scriptPath, "#!/bin/sh\nprintf '\\033]633;D;7\\007'\n");
      chmodSync(scriptPath, 0o755);

      const session = trackSession(
        await createTerminal({
          workspaceId: "ws-test",
          cwd: packageRoot,
          shell: "/bin/sh",
          env: { PS1: "$ " },
        }),
      );
      const commandCompletions: Array<number | null> = [];
      const unsubscribeCommandFinished = session.onCommandFinished((info) => {
        commandCompletions.push(info.exitCode);
      });

      await waitForLines(session, ["$"]);
      session.send({ type: "input", data: "./emit-command-finished.sh\r" });

      await waitForState(session, () => commandCompletions.length === 1);

      expect(commandCompletions).toEqual([7]);
      expect(getLines(await readTerminalState(session)).join("\n")).not.toContain("633;D;7");

      unsubscribeCommandFinished();
    });

    it("ignores malformed VS Code OSC 633 command completion payloads", async () => {
      const packageRoot = mkdtempSync(join(tmpdir(), "terminal-command-finished-malformed-"));
      temporaryDirs.push(packageRoot);
      const scriptPath = join(packageRoot, "emit-malformed-command-finished.sh");
      writeFileSync(
        scriptPath,
        "#!/bin/sh\nprintf '\\033]633;D;garbage\\007\\033]633;D;8;extra\\007\\033]633;D;3\\007'\n",
      );
      chmodSync(scriptPath, 0o755);

      const session = trackSession(
        await createTerminal({
          workspaceId: "ws-test",
          cwd: packageRoot,
          shell: "/bin/sh",
          env: { PS1: "$ " },
        }),
      );
      const commandCompletions: Array<number | null> = [];
      const unsubscribeCommandFinished = session.onCommandFinished((info) => {
        commandCompletions.push(info.exitCode);
      });

      await waitForLines(session, ["$"]);
      session.send({ type: "input", data: "./emit-malformed-command-finished.sh\r" });

      await waitForState(session, () => commandCompletions.length === 1);

      expect(commandCompletions).toEqual([3]);
      expect(getLines(await readTerminalState(session)).join("\n")).not.toContain("633;D;garbage");

      unsubscribeCommandFinished();
    });

    it("debounces rapid title changes and emits only the final title", async () => {
      const session = trackSession(
        await createTerminal({
          workspaceId: "ws-test",
          cwd: "/tmp",
          shell: "/bin/sh",
          env: { PS1: "$ " },
        }),
      );
      const seenTitles: Array<string | undefined> = [];
      const seenMessages: Array<string | undefined> = [];
      const unsubscribeTitle = session.onTitleChange((title) => {
        seenTitles.push(title);
      });
      const unsubscribeMessages = session.subscribe((message) => {
        if (message.type === "titleChange") {
          seenMessages.push(message.title);
        }
      });

      await waitForLines(session, ["$"]);
      session.send({
        type: "input",
        data: "printf '\\033]0;First\\007\\033]0;Second\\007\\033]0;Final\\007'\r",
      });

      await waitForTitle(session, (title) => title === "Final");

      expect(seenTitles).toEqual(["Final"]);
      expect(seenMessages).toEqual(["Final"]);

      unsubscribeMessages();
      unsubscribeTitle();
    });

    it.skipIf(!hasZsh)("emits zsh shell integration titles for commands and prompts", async () => {
      const homeDir = mkdtempSync(join(tmpdir(), "terminal-zsh-integration-home-"));
      temporaryDirs.push(homeDir);
      const realZdotdir = join(homeDir, ".config", "zsh");
      const workingDir = join(homeDir, "dev", "faro");
      mkdirSync(realZdotdir, { recursive: true });
      mkdirSync(workingDir, { recursive: true });
      writeFileSync(join(realZdotdir, ".zshenv"), "");
      writeFileSync(join(realZdotdir, ".zshrc"), "PS1='$ '\n");

      const session = trackSession(
        await createTerminal({
          workspaceId: "ws-test",
          cwd: workingDir,
          shell: "/bin/zsh",
          env: {
            HOME: homeDir,
            ZDOTDIR: realZdotdir,
          },
        }),
      );

      await waitForLines(session, ["$"]);
      await waitForTitle(session, (title) => title === "~/dev/faro");

      session.send({ type: "input", data: "sleep 1\r" });

      await waitForTitle(session, (title) => title === "sleep 1");
      await waitForTitle(session, (title) => title === "~/dev/faro", 4000);
    });

    it.skipIf(!hasZsh)("loads the user's zsh prompt when the integration dir is packaged", () => {
      const homeDir = mkdtempSync(join(tmpdir(), "terminal-zsh-packaged-home-"));
      temporaryDirs.push(homeDir);
      writeFileSync(join(homeDir, ".zshrc"), "PS1='PASEO_CUSTOM_PROMPT> '\n");

      const fakeAppRoot = join(homeDir, "Paseo.app", "Contents", "Resources");
      const inaccessiblePackagedIntegrationDir = join(
        fakeAppRoot,
        "app.asar",
        "node_modules",
        "@getpaseo",
        "server",
        "dist",
        "server",
        "terminal",
        "shell-integration",
        "zsh",
      );
      const unpackedIntegrationDir = join(
        fakeAppRoot,
        "app.asar.unpacked",
        "node_modules",
        "@getpaseo",
        "server",
        "dist",
        "server",
        "terminal",
        "shell-integration",
        "zsh",
      );
      mkdirSync(unpackedIntegrationDir, { recursive: true });
      cpSync(resolveZshShellIntegrationDir(), unpackedIntegrationDir, { recursive: true });
      writeFileSync(join(fakeAppRoot, "app.asar"), "asar archive placeholder");

      const env = buildTerminalEnvironment({
        shell: "/bin/zsh",
        env: {
          HOME: homeDir,
        },
        zshShellIntegrationDir: inaccessiblePackagedIntegrationDir,
      });

      const result = spawnSync("/bin/zsh", ["-i", "-c", "print -r -- ${PROMPT}"], {
        cwd: homeDir,
        env,
        encoding: "utf8",
      });

      expect(result.status).toBe(0);
      expect(result.stdout.split(/\r?\n/)).toContain("PASEO_CUSTOM_PROMPT> ");
    });

    it.skipIf(!hasZsh)("emits zsh shell integration command completion", async () => {
      const homeDir = mkdtempSync(join(tmpdir(), "terminal-zsh-command-finished-home-"));
      temporaryDirs.push(homeDir);
      const realZdotdir = join(homeDir, ".config", "zsh");
      const workingDir = join(homeDir, "dev", "faro");
      mkdirSync(realZdotdir, { recursive: true });
      mkdirSync(workingDir, { recursive: true });
      writeFileSync(join(realZdotdir, ".zshenv"), "");
      writeFileSync(join(realZdotdir, ".zshrc"), "PS1='$ '\n");

      const session = trackSession(
        await createTerminal({
          workspaceId: "ws-test",
          cwd: workingDir,
          shell: "/bin/zsh",
          env: {
            HOME: homeDir,
            ZDOTDIR: realZdotdir,
          },
        }),
      );
      const commandCompletions: Array<number | null> = [];
      const unsubscribeCommandFinished = session.onCommandFinished((info) => {
        commandCompletions.push(info.exitCode);
      });

      await waitForLines(session, ["$"]);
      session.send({ type: "input", data: "false\r" });

      await waitForState(session, () => commandCompletions.includes(1));

      expect(commandCompletions).toEqual([1]);
      expect(getLines(await readTerminalState(session)).join("\n")).not.toContain("633;D;1");

      unsubscribeCommandFinished();
    });
  });

  describe("program status", () => {
    it.skipIf(process.env.PASEO_REAL_PI_ACCEPTANCE !== "1")(
      "accepts real Pi OSC negotiation, permission, hook fallback and crash recovery",
      async () => {
        const { default: express } = await import("express");
        const { createTerminalActivityRouteHandler } = await import("../server/bootstrap.js");
        const { createTerminalManager } = await import("./terminal-manager.js");
        const packageRoot = mkdtempSync(join(tmpdir(), "terminal-real-pi-"));
        temporaryDirs.push(packageRoot);
        const extensionPath = join(packageRoot, "permission.ts");
        writeFileSync(
          extensionPath,
          'export default function(pi) { pi.on("tool_call", async (event, ctx) => {\n' +
            'if (event.toolName === "bash" && !(await ctx.ui.confirm("PASEO_QA_PERMISSION", "Allow printf?")))\n' +
            'return { block: true, reason: "Permission rejected" }; }); }\n',
        );
        const app = express();
        app.use(express.json());
        const manager = createTerminalManager();
        const reports: Array<{ terminalId: string; event: string }> = [];
        app.post(
          "/api/terminal-activity",
          (req, _res, next) => {
            reports.push({ terminalId: req.body.terminalId, event: req.body.event });
            next();
          },
          createTerminalActivityRouteHandler(manager),
        );
        const server = app.listen(0, "127.0.0.1");
        await new Promise<void>((resolve) => server.once("listening", resolve));
        const address = server.address();
        if (!address || typeof address === "string") throw new Error("Expected TCP server");
        try {
          for (const osc of [true, false]) {
            const session = trackSession(
              await manager.createTerminal({
                cwd: packageRoot,
                workspaceId: "real-pi",
                command: "/bin/bash",
                args: ["--noprofile", "--norc"],
                activityUrl: `http://127.0.0.1:${address.port}/api/terminal-activity`,
                paseoCliPath: join(__dirname, "../../../cli/bin/paseo"),
                env: { PS1: "$ ", PI_OFFLINE: "1", ...(osc ? {} : { PI_PROGRAM_STATUS: "0" }) },
                rows: 40,
                cols: 120,
              }),
            );
            let output = "";
            session.subscribe((message) => {
              if (message.type === "output") output += message.data;
            });
            session.send({
              type: "input",
              data: `pi --offline --no-mcp --no-context-files --no-session --extension ${extensionPath} --tools bash 'Call bash once with printf PASEO_PERMISSION_OK. Then reply PASEO_FINISHED_OK.'\r`,
            });
            await waitForActivity(session, (activity) => activity?.state === "working", 60_000);
            if (osc) {
              await waitForActivity(
                session,
                (activity) => activity?.attentionReason === "needs_input",
                60_000,
              );
              expect(output).toContain("\x1b]7501;?\x1b\\");
              expect(output).toContain("state=blocked:app=pi:kind=permission");
              session.send({ type: "input", data: "\r" });
              await waitForActivity(
                session,
                (activity) => activity?.attentionReason === "finished",
                60_000,
              );
              expect(output).toContain("state=done:app=pi");
              session.clearActivityAttention();
              session.send({ type: "input", data: "Use bash to sleep 30.\r" });
              await waitForActivity(session, (activity) => activity?.state === "working", 60_000);
            }
            await new Promise((resolve) => setTimeout(resolve, 700));
            const processRows = spawnSync("ps", ["-eo", "pid=,ppid=,args="], { encoding: "utf8" });
            const child = processRows.stdout.split("\n").find((row) => {
              const match = row.match(/^\s*(\d+)\s+(\d+)\s+(.+)$/);
              return match && Number(match[2]) === session.pid && match[3].includes("pi");
            });
            expect(child).toBeDefined();
            process.kill(Number(child!.trim().split(/\s+/)[0]), "SIGKILL");
            await waitForActivity(session, (activity) => activity === null, 10_000);
            session.send({
              type: "input",
              data: "pi --offline --no-mcp --no-context-files --no-session --no-tools 'Reply PASEO_RECOVERY_OK.'\r",
            });
            await waitForActivity(session, (activity) => activity?.state === "working", 60_000);
            await waitForActivity(
              session,
              (activity) => activity?.attentionReason === "finished",
              60_000,
            );
            expect(reports).toContainEqual({ terminalId: session.id, event: "Stop" });
            if (!osc) expect(output).not.toContain("\x1b]7501;");
            await manager.killTerminalAndWait(session.id);
          }
        } finally {
          for (const cwd of manager.listDirectories()) {
            for (const terminal of await manager.getTerminals(cwd)) {
              await manager.killTerminalAndWait(terminal.id);
            }
          }
          await new Promise<void>((resolve) => server.close(() => resolve()));
        }
      },
      240_000,
    );

    it.each([
      {
        platform: "linux" as const,
        output:
          "100 1 100 200 Thu Nov  5 12:00:00 2026\n200 100 100 200 Thu Nov  5 12:00:01 2026\n300 1 300 -1 Thu Nov  5 12:00:02 2026\n",
        sessionId: 100,
        detachedSessionId: 300,
      },
      {
        platform: "darwin" as const,
        output:
          "100 1 200 Thu Nov  5 12:00:00 2026\n200 100 200 Thu Nov  5 12:00:01 2026\n300 1 -1 Thu Nov  5 12:00:02 2026\n",
        sessionId: null,
        detachedSessionId: null,
      },
    ])(
      "reads foreground groups and birth times from the $platform process table",
      async ({ platform, output, sessionId, detachedSessionId }) => {
        const inspector = createTerminalWorkerProcessInspector({
          platform,
          commandRunner: {
            exec: async () => ({ stdout: output, stderr: "" }),
          },
        });
        expect(await inspector.listProcesses()).toEqual([
          {
            pid: 100,
            ppid: 1,
            sessionId,
            foregroundProcessGroupId: 200,
            startedAt: "Thu Nov  5 12:00:00 2026",
          },
          {
            pid: 200,
            ppid: 100,
            sessionId,
            foregroundProcessGroupId: 200,
            startedAt: "Thu Nov  5 12:00:01 2026",
          },
          {
            pid: 300,
            ppid: 1,
            sessionId: detachedSessionId,
            foregroundProcessGroupId: -1,
            startedAt: "Thu Nov  5 12:00:02 2026",
          },
        ]);
      },
    );

    it("keeps foreground activity when an earlier background job exits", async () => {
      const cwd = mkdtempSync(join(tmpdir(), "terminal-foreground-activity-"));
      temporaryDirs.push(cwd);
      const backgroundPidPath = join(cwd, "background.pid");
      const foregroundPidPath = join(cwd, "foreground.pid");
      const reporter = join(cwd, "reporter.cjs");
      writeFileSync(
        reporter,
        `
        require("node:fs").writeFileSync(${JSON.stringify(foregroundPidPath)}, String(process.pid));
        process.stdout.write("\\x1b]7501;state=working:app=grok\\x07");
        setInterval(() => {}, 1000);
      `,
      );
      const session = trackSession(
        await createTerminal({
          workspaceId: "ws-test",
          cwd,
          shell: "/bin/sh",
          env: { PS1: "$ " },
        }),
      );
      await waitForLines(session, ["$"]);
      session.send({
        type: "input",
        data: `sleep 60 & echo $! > ${JSON.stringify(backgroundPidPath)}; echo BACKGROUND_READY\r`,
      });
      await waitForState(session, (state) => getLines(state).includes("BACKGROUND_READY"));
      const backgroundPid = Number(readFileSync(backgroundPidPath, "utf8"));
      const completions: Array<number | null> = [];
      session.onCommandFinished(({ exitCode }) => completions.push(exitCode));
      try {
        session.send({
          type: "input",
          data: `${JSON.stringify(process.execPath)} ${JSON.stringify(reporter)}\r`,
        });
        await waitForActivity(session, (activity) => activity?.state === "working");
        await new Promise((resolve) => setTimeout(resolve, 1000));
        killBackgroundProcess(backgroundPid);
        await new Promise((resolve) => setTimeout(resolve, 1000));
        expect(session.getActivity()).toMatchObject({ state: "working" });
        expect(completions).toEqual([]);
        process.kill(Number(readFileSync(foregroundPidPath, "utf8")), "SIGKILL");
        await waitForActivity(session, (activity) => activity === null);
        expect(completions).toEqual([null]);
      } finally {
        killBackgroundProcess(backgroundPid);
      }
    });

    it.each(["working", "blocked"])(
      "clears %s after an externally killed foreground process without shell prompt marks",
      async (state) => {
        const packageRoot = mkdtempSync(join(tmpdir(), "terminal-program-status-crash-"));
        temporaryDirs.push(packageRoot);
        const pidPath = join(packageRoot, "reporter.pid");
        const scriptPath = join(packageRoot, "reporter.cjs");
        writeFileSync(
          scriptPath,
          `require("node:fs").writeFileSync(${JSON.stringify(pidPath)}, String(process.pid));\n` +
            `process.stdout.write("\\x1b]7501;state=${state}:app=pi\\x07");\n` +
            "setInterval(() => {}, 1000);\n",
        );
        const session = trackSession(
          await createTerminal({
            workspaceId: "ws-test",
            cwd: packageRoot,
            shell: "/bin/sh",
            env: { PS1: "$ " },
          }),
        );
        await waitForLines(session, ["$"]);
        session.send({ type: "input", data: `${process.execPath} ${scriptPath}\r` });
        await waitForActivity(session, (activity) =>
          state === "working"
            ? activity?.state === "working"
            : activity?.attentionReason === "needs_input",
        );
        const reporterPid = Number(readFileSync(pidPath, "utf8"));
        await new Promise((resolve) => setTimeout(resolve, 600));
        process.kill(reporterPid, "SIGKILL");
        await waitForActivity(session, (activity) => activity === null);
        session.send({ type: "input", data: "echo RECOVERED\r" });
        await waitForState(session, (snapshot) => getLines(snapshot).includes("RECOVERED"));
        session.setActivity("working");
        expect(session.getActivity()).toMatchObject({ state: "working" });
        session.setActivity("attention");
        expect(session.getActivity()).toMatchObject({ attentionReason: "needs_input" });
        session.setActivity("idle");
        expect(session.getActivity()).toMatchObject({ state: "idle" });
      },
    );

    it("maps OSC 7501 reports onto terminal activity and answers feature queries", async () => {
      const packageRoot = mkdtempSync(join(tmpdir(), "terminal-program-status-"));
      temporaryDirs.push(packageRoot);
      const scriptPath = join(packageRoot, "emit-program-status.sh");
      // The sleep keeps working observable while the test polls for it.
      writeFileSync(
        scriptPath,
        "#!/bin/sh\nprintf '\\033]7501;state=working:app=omp\\007'\nsleep 2\nprintf '\\033]7501;state=done:app=omp\\007'\n",
      );
      chmodSync(scriptPath, 0o755);
      const helperPath = writeOsc7501Helper("terminal-osc7501-helper-");

      const session = trackSession(
        await createTerminal({
          workspaceId: "ws-test",
          cwd: packageRoot,
          shell: "/bin/sh",
          env: { PS1: "$ " },
        }),
      );

      await waitForLines(session, ["$"]);
      session.send({ type: "input", data: "./emit-program-status.sh\r" });

      await waitForActivity(session, (activity) => activity?.state === "working");
      await waitForActivity(
        session,
        (activity) => activity?.state === "idle" && activity?.attentionReason === "finished",
      );

      expect(session.getActivity()).toMatchObject({ state: "idle", attentionReason: "finished" });
      expect(getLines(await readTerminalState(session)).join("\n")).not.toContain("7501");

      session.send({ type: "input", data: `${process.execPath} ${helperPath}\r` });
      await waitForState(session, hasOsc7501OkLine);

      const ack = getLines(await readTerminalState(session)).find(isOsc7501OkLine) ?? "";
      expect(ack).toBe("OSC7501_OK:ESC]7501;?ESC\\");
    });

    it.skipIf(!hasZsh)(
      "deviation: ignores OSC 133 prompt zones a running program draws, keeping its working status",
      async () => {
        const packageRoot = mkdtempSync(join(tmpdir(), "terminal-program-status-"));
        temporaryDirs.push(packageRoot);
        const scriptPath = join(packageRoot, "tui-program-status.sh");
        // Mimics OMP: report working, then draw an OSC 133 prompt zone in its own UI.
        writeFileSync(
          scriptPath,
          "#!/bin/sh\nprintf '\\033]7501;state=working:app=omp\\007'\nprintf '\\033]133;A\\007> \\033]133;B\\007'\necho READY\nsleep 3\n",
        );
        chmodSync(scriptPath, 0o755);
        writeFileSync(join(packageRoot, ".zshrc"), "PS1='$ '\n");

        const session = trackSession(
          await createTerminal({
            workspaceId: "ws-test",
            cwd: packageRoot,
            shell: "/bin/zsh",
            env: { HOME: packageRoot, ZDOTDIR: packageRoot },
          }),
        );

        await waitForLines(session, ["$"]);
        session.send({ type: "input", data: "./tui-program-status.sh\r" });
        await waitForState(session, (state) =>
          getLines(state).some((line) => line.includes("READY")),
        );

        session.send({ type: "input", data: "\x1b" });

        // Escape alone would end a hook-driven run; a program with records
        // reports its own interrupts.
        expect(session.getActivity()).toMatchObject({ state: "working" });
      },
    );

    it.skipIf(!hasZsh)(
      "ignores hook reports while a program run reports OSC 7501, then applies them after the prompt",
      async () => {
        const packageRoot = mkdtempSync(join(tmpdir(), "terminal-program-status-"));
        temporaryDirs.push(packageRoot);
        const scriptPath = join(packageRoot, "reporting-harness.sh");
        writeFileSync(
          scriptPath,
          "#!/bin/sh\nprintf '\\033]7501;state=working:app=claude\\007'\necho REPORTING\nsleep 1\nprintf '\\033]7501;state=done:app=claude\\007'\n",
        );
        chmodSync(scriptPath, 0o755);
        writeFileSync(join(packageRoot, ".zshrc"), "PS1='$ '\n");
        const session = trackSession(
          await createTerminal({
            workspaceId: "ws-test",
            cwd: packageRoot,
            shell: "/bin/zsh",
            env: { HOME: packageRoot, ZDOTDIR: packageRoot },
          }),
        );
        await waitForLines(session, ["$"]);

        session.send({ type: "input", data: "./reporting-harness.sh\r" });
        await waitForState(session, (state) =>
          getLines(state).some((line) => line.includes("REPORTING")),
        );
        session.setActivity("attention");
        expect(session.getActivity()).toMatchObject({ state: "working" });

        // The next command's output proves the shell printed the prompt that ends the run.
        await waitForActivity(session, (activity) => activity?.attentionReason === "finished");
        session.send({ type: "input", data: "echo NEXT\r" });
        await waitForState(session, (state) => getLines(state).some((line) => line === "NEXT"));
        session.setActivity("working");
        expect(session.getActivity()).toMatchObject({ state: "working" });
        session.send({ type: "input", data: "\x1b" });
        expect(session.getActivity()).toBeNull();
      },
    );

    it("applies hook reports again once a program clears its records, without prompt marks", async () => {
      const session = trackSession(
        await createTerminal({
          workspaceId: "ws-test",
          cwd: "/tmp",
          shell: "/bin/sh",
          env: { PS1: "$ " },
        }),
      );
      await waitForLines(session, ["$"]);

      session.send({ type: "input", data: "printf '\\033]7501;state=working\\007'; echo RUN\r" });
      await waitForState(session, (state) => getLines(state).some((line) => line === "RUN"));
      session.setActivity("attention");
      expect(session.getActivity()).toMatchObject({ state: "working" });

      session.send({ type: "input", data: "printf '\\033]7501;state=clear\\007'; echo END\r" });
      await waitForState(session, (state) => getLines(state).some((line) => line === "END"));
      session.setActivity("working");
      expect(session.getActivity()).toMatchObject({ state: "working" });
    });

    it("drops working and blocked on an OSC 133 prompt when no command is known to run", async () => {
      const session = trackSession(
        await createTerminal({
          workspaceId: "ws-test",
          cwd: "/tmp",
          shell: "/bin/sh",
          env: { PS1: "$ " },
        }),
      );
      await waitForLines(session, ["$"]);

      session.send({
        type: "input",
        data: "printf '\\033]7501;state=working\\007\\033]7501;state=done:id=x\\007'\r",
      });
      await waitForActivity(session, (activity) => activity?.state === "working");

      session.send({ type: "input", data: "printf '\\033]133;A\\007'\r" });
      await waitForActivity(
        session,
        (activity) => activity?.state === "idle" && activity?.attentionReason === "finished",
      );
    });

    it("marks a turn finished when it reaches done in either order with idle, without re-raising cleared attention", async () => {
      const session = trackSession(
        await createTerminal({
          workspaceId: "ws-test",
          cwd: "/tmp",
          shell: "/bin/sh",
          env: { PS1: "$ " },
        }),
      );
      await waitForLines(session, ["$"]);

      session.send({
        type: "input",
        data: "printf '\\033]7501;state=idle\\007\\033]7501;state=done\\007'; echo DONE\r",
      });
      await waitForState(session, (state) => getLines(state).some((line) => line === "DONE"));
      expect(session.getActivity()).toMatchObject({ state: "idle", attentionReason: "finished" });

      session.clearActivityAttention();
      session.send({ type: "input", data: "printf '\\033]7501;state=done\\007'; echo AGAIN\r" });
      await waitForState(session, (state) => getLines(state).some((line) => line === "AGAIN"));
      expect(session.getActivity()).toMatchObject({ state: "idle" });
      expect(session.getActivity()).not.toHaveProperty("attentionReason");

      session.send({
        type: "input",
        data: "printf '\\033]7501;state=working\\007\\033]7501;state=done\\007\\033]7501;state=idle\\007'; echo REST\r",
      });
      await waitForState(session, (state) => getLines(state).some((line) => line === "REST"));
      expect(session.getActivity()).toMatchObject({ state: "idle", attentionReason: "finished" });
    });

    it("removes every record on a full reset but not on a soft reset", async () => {
      const session = trackSession(
        await createTerminal({
          workspaceId: "ws-test",
          cwd: "/tmp",
          shell: "/bin/sh",
          env: { PS1: "$ " },
        }),
      );
      await waitForLines(session, ["$"]);

      session.send({
        type: "input",
        data: "printf '\\033]7501;state=done\\007\\033[!p'; echo SOFT\r",
      });
      await waitForState(session, (state) => getLines(state).some((line) => line === "SOFT"));
      expect(session.getActivity()).toMatchObject({ state: "idle", attentionReason: "finished" });

      session.send({ type: "input", data: "printf '\\033c'; echo HARD\r" });
      await waitForState(session, (state) => getLines(state).some((line) => line === "HARD"));
      expect(session.getActivity()).toBeNull();
    });

    it("keeps records across alternate screen switches", async () => {
      const session = trackSession(
        await createTerminal({
          workspaceId: "ws-test",
          cwd: "/tmp",
          shell: "/bin/sh",
          env: { PS1: "$ " },
        }),
      );
      await waitForLines(session, ["$"]);

      session.send({
        type: "input",
        data: "printf '\\033]7501;state=done\\007\\033[?1049h\\033]7501;state=error:id=alt\\007\\033[?1049l'; echo SWITCHED\r",
      });
      await waitForState(session, (state) => getLines(state).some((line) => line === "SWITCHED"));

      expect(session.getActivity()).toMatchObject({ state: "idle", attentionReason: "finished" });
    });
  });

  describe("colors", () => {
    it("captures ANSI 16 color codes (mode 1)", async () => {
      const session = trackSession(
        await createTerminal({
          workspaceId: "ws-test",
          cwd: "/tmp",
          shell: "/bin/sh",
          env: { PS1: "$ ", TERM: "xterm-256color" },
        }),
      );

      await waitForLines(session, ["$"]);

      // \033[31m = ANSI red (color 1)
      session.send({ type: "input", data: "printf '\\033[31mRED\\033[0m'\r" });

      await waitForLines(session, ["$ printf '\\033[31mRED\\033[0m'", "RED$"]);

      const state = await readTerminalState(session);
      const outputRow = state.grid[1];

      expect(outputRow[0].char).toBe("R");
      expect(outputRow[0].fg).toBe(1); // ANSI red = 1
      expect(outputRow[0].fgMode).toBe(1); // Mode 1 = 16 ANSI colors

      // The "$" after RED should have default color
      expect(outputRow[3].char).toBe("$");
      expect(outputRow[3].fg).toBe(undefined);
      expect(outputRow[3].fgMode).toBe(undefined);
    });

    it("captures 256 color codes (mode 2)", async () => {
      const session = trackSession(
        await createTerminal({
          workspaceId: "ws-test",
          cwd: "/tmp",
          shell: "/bin/sh",
          env: { PS1: "$ ", TERM: "xterm-256color" },
        }),
      );

      await waitForLines(session, ["$"]);

      // \033[38;5;208m = 256-color orange (color 208)
      session.send({ type: "input", data: "printf '\\033[38;5;208mORG\\033[0m'\r" });

      await waitForLines(session, ["$ printf '\\033[38;5;208mORG\\033[0m'", "ORG$"]);

      const state = await readTerminalState(session);
      const outputRow = state.grid[1];

      // Check O cell
      expect(outputRow[0].char).toBe("O");
      expect(outputRow[0].fg).toBe(208); // 256-color index
      expect(outputRow[0].fgMode).toBe(2); // Mode 2 = 256 colors
    });

    it("captures true color RGB (mode 3)", async () => {
      const session = trackSession(
        await createTerminal({
          workspaceId: "ws-test",
          cwd: "/tmp",
          shell: "/bin/sh",
          env: { PS1: "$ ", TERM: "xterm-256color" },
        }),
      );

      await waitForLines(session, ["$"]);

      // \033[38;2;255;128;64m = true color RGB(255, 128, 64)
      session.send({ type: "input", data: "printf '\\033[38;2;255;128;64mRGB\\033[0m'\r" });

      await waitForLines(session, ["$ printf '\\033[38;2;255;128;64mRGB\\033[0m'", "RGB$"]);

      const state = await readTerminalState(session);
      const outputRow = state.grid[1];

      // Check R cell
      expect(outputRow[0].char).toBe("R");
      expect(outputRow[0].fgMode).toBe(3); // Mode 3 = true color

      // The color value should be packed RGB: (255 << 16) | (128 << 8) | 64
      const expectedPacked = (255 << 16) | (128 << 8) | 64;
      expect(outputRow[0].fg).toBe(expectedPacked);
    });

    it("captures background colors", async () => {
      const session = trackSession(
        await createTerminal({
          workspaceId: "ws-test",
          cwd: "/tmp",
          shell: "/bin/sh",
          env: { PS1: "$ ", TERM: "xterm-256color" },
        }),
      );

      await waitForLines(session, ["$"]);

      // \033[41m = ANSI red background
      session.send({ type: "input", data: "printf '\\033[41mBG\\033[0m'\r" });

      await waitForLines(session, ["$ printf '\\033[41mBG\\033[0m'", "BG$"]);

      const state = await readTerminalState(session);
      const outputRow = state.grid[1];

      expect(outputRow[0].char).toBe("B");
      expect(outputRow[0].bg).toBe(1); // ANSI red = 1
      expect(outputRow[0].bgMode).toBe(1); // Mode 1 = 16 ANSI colors
    });
  });

  describe("subscribe", () => {
    it("receives readiness on initial subscription", async () => {
      const session = trackSession(
        await createTerminal({
          workspaceId: "ws-test",
          cwd: realpathSync(tmpdir()),
        }),
      );

      const messages: Array<{ type: string }> = [];
      const unsubscribe = session.subscribe((msg) => {
        messages.push(msg);
      });

      await new Promise((resolve) => setTimeout(resolve, 100));

      expect(messages.length).toBeGreaterThan(0);
      expect(messages[0].type).toBe("snapshotReady");

      unsubscribe();
    });

    it("receives output messages on updates without replay snapshots", async () => {
      const session = trackSession(
        await createTerminal({
          workspaceId: "ws-test",
          cwd: "/tmp",
          shell: "/bin/sh",
          env: { PS1: "$ " },
        }),
      );

      await waitForLines(session, ["$"]);

      const messages: Array<{ type: string }> = [];
      const unsubscribe = session.subscribe((msg) => {
        messages.push(msg);
      });

      await new Promise((resolve) => setTimeout(resolve, 100));
      messages.length = 0;

      session.send({ type: "input", data: "echo test\r" });

      await waitForLines(session, ["$ echo test", "test", "$"]);

      expect(messages.some((message) => message.type === "output")).toBe(true);
      expect(
        messages.every((message) => message.type === "output" || message.type === "titleChange"),
      ).toBe(true);

      unsubscribe();
    });

    it("keeps resize updates in the live observation lifecycle", async () => {
      const session = trackSession(
        await createTerminal({
          workspaceId: "ws-test",
          cwd: "/tmp",
          shell: "/bin/sh",
          env: { PS1: "$ " },
          rows: 24,
          cols: 80,
        }),
      );

      const messages: Array<{ type: string }> = [];
      const unsubscribe = session.subscribe((msg) => {
        messages.push(msg);
      });

      await new Promise((resolve) => setTimeout(resolve, 100));
      messages.length = 0;

      session.send({ type: "resize", rows: 30, cols: 100 });
      await new Promise((resolve) => setTimeout(resolve, 200));

      expect(
        messages.every((message) => message.type === "output" || message.type === "titleChange"),
      ).toBe(true);
      expect(session.getSize()).toEqual({ rows: 30, cols: 100 });

      unsubscribe();
    });

    it("emits output only after text capture reflects the new data", async () => {
      const session = trackSession(
        await createTerminal({
          workspaceId: "ws-test",
          cwd: "/tmp",
          shell: "/bin/sh",
          env: { PS1: "$ " },
        }),
      );

      await waitForLines(session, ["$"]);
      const outputSeenInState = new Promise<boolean>((resolve) => {
        const unsubscribe = session.subscribe((message) => {
          if (message.type !== "output" || !message.data.includes("state-after-output")) {
            return;
          }
          unsubscribe();
          const stateText = session.captureLines().lines.join("\n");
          resolve(stateText.includes("state-after-output"));
        });
      });

      session.send({ type: "input", data: "echo state-after-output\r" });
      expect(await outputSeenInState).toBe(true);
    });

    it("unsubscribe stops receiving messages", async () => {
      const session = trackSession(
        await createTerminal({
          workspaceId: "ws-test",
          cwd: "/tmp",
          shell: "/bin/sh",
          env: { PS1: "$ " },
        }),
      );

      await waitForLines(session, ["$"]);

      const messages: Array<{ type: string }> = [];
      const unsubscribe = session.subscribe((msg) => {
        messages.push(msg);
      });

      unsubscribe();
      messages.length = 0;

      session.send({ type: "input", data: "echo after\r" });
      await new Promise((resolve) => setTimeout(resolve, 200));

      expect(messages.length).toBe(0);
    });
  });

  describe("terminal protocol queries", () => {
    it("delivers a DA1 reply to a foreground app on stdin", async () => {
      const helperPath = writeDaHelper("terminal-da-helper-");

      const session = trackSession(
        await createTerminal({
          workspaceId: "ws-test",
          cwd: "/tmp",
          shell: "/bin/sh",
          env: { PS1: "$ " },
        }),
      );
      await waitForLines(session, ["$"]);

      session.send({ type: "input", data: `${process.execPath} ${helperPath}\r` });
      await waitForState(session, hasDaOkLine);

      const ack = getLines(await readTerminalState(session)).find(isDaOkLine) ?? "";
      expect(ack).toMatch(/^DA_OK:\[\?[\d;]+c$/);
    });

    it("does not echo DA1 replies onto the prompt after the foreground app exits", async () => {
      const helperPath = writeDaHelper("terminal-da-cleanup-");

      const session = trackSession(
        await createTerminal({
          workspaceId: "ws-test",
          cwd: "/tmp",
          shell: "/bin/sh",
          env: { PS1: "$ " },
        }),
      );
      await waitForLines(session, ["$"]);

      session.send({ type: "input", data: `${process.execPath} ${helperPath}\r` });
      await waitForState(session, hasDaOkLine);
      await waitForState(session, lastNonEmptyLineIsPrompt);
    });

    it("delivers public DSR cursor-position replies to a foreground app on stdin", async () => {
      const helperPath = writeDsrHelper("terminal-dsr-helper-");

      const session = trackSession(
        await createTerminal({
          workspaceId: "ws-test",
          cwd: "/tmp",
          shell: "/bin/sh",
          env: { PS1: "$ " },
        }),
      );
      await waitForLines(session, ["$"]);

      session.send({ type: "input", data: `${process.execPath} ${helperPath} cursor\r` });
      await waitForState(session, hasDsrOkLine);

      const ack = getLines(await readTerminalState(session)).find(isDsrOkLine) ?? "";
      expect(ack).toMatch(/^DSR_OK:\d+:\d+$/);
    });

    it("delivers private DSR cursor-position replies to a foreground app on stdin", async () => {
      const helperPath = writeDsrHelper("terminal-dsr-private-helper-");

      const session = trackSession(
        await createTerminal({
          workspaceId: "ws-test",
          cwd: "/tmp",
          shell: "/bin/sh",
          env: { PS1: "$ " },
        }),
      );
      await waitForLines(session, ["$"]);

      session.send({ type: "input", data: `${process.execPath} ${helperPath} private-cursor\r` });
      await waitForState(session, hasDsrOkLine);

      const ack = getLines(await readTerminalState(session)).find(isDsrOkLine) ?? "";
      expect(ack).toMatch(/^DSR_OK:\d+:\d+$/);
    });

    it("delivers DSR terminal-status replies to a foreground app on stdin", async () => {
      const helperPath = writeDsrHelper("terminal-dsr-status-helper-");

      const session = trackSession(
        await createTerminal({
          workspaceId: "ws-test",
          cwd: "/tmp",
          shell: "/bin/sh",
          env: { PS1: "$ " },
        }),
      );
      await waitForLines(session, ["$"]);

      session.send({ type: "input", data: `${process.execPath} ${helperPath} status\r` });
      await waitForState(session, hasDsrOkLine);

      const ack = getLines(await readTerminalState(session)).find(isDsrOkLine) ?? "";
      expect(ack).toBe("DSR_OK:status");
    });

    it("delivers OSC 11 background-color replies to a foreground app on stdin", async () => {
      const helperPath = writeOsc11Helper("terminal-osc11-helper-");

      const session = trackSession(
        await createTerminal({
          workspaceId: "ws-test",
          cwd: "/tmp",
          shell: "/bin/sh",
          env: { PS1: "$ " },
        }),
      );
      await waitForLines(session, ["$"]);

      session.send({ type: "input", data: `${process.execPath} ${helperPath}\r` });
      await waitForState(session, hasOsc11OkLine);

      const ack = getLines(await readTerminalState(session)).find(isOsc11OkLine) ?? "";
      expect(ack).toBe("OSC11_OK:ESC]11;rgb:0b0b/0b0b/0b0bESC\\");
    });
  });

  describe("stream snapshots", () => {
    it("streams raw output messages without replay metadata", async () => {
      const session = trackSession(
        await createTerminal({
          workspaceId: "ws-test",
          cwd: "/tmp",
          shell: "/bin/sh",
          env: { PS1: "$ " },
        }),
      );

      await waitForLines(session, ["$"]);

      const outputMessages: string[] = [];
      const unsubscribe = session.subscribe((message) => {
        if (message.type !== "output") {
          return;
        }
        outputMessages.push(message.data);
      });

      session.send({ type: "input", data: "echo raw-stream\r" });
      await waitForLines(session, ["$ echo raw-stream", "raw-stream", "$"]);

      expect(outputMessages.join("")).toContain("raw-stream");

      unsubscribe();
    });

    it("announces readiness for a fresh replay instead of replaying raw live output", async () => {
      const session = trackSession(
        await createTerminal({
          workspaceId: "ws-test",
          cwd: "/tmp",
          shell: "/bin/sh",
          env: { PS1: "$ " },
        }),
      );

      await waitForLines(session, ["$"]);

      session.send({ type: "input", data: "echo before-detach\r" });
      await waitForLines(session, ["$ echo before-detach", "before-detach", "$"]);

      session.send({ type: "input", data: "echo after-detach\r" });
      await waitForLines(session, [
        "$ echo before-detach",
        "before-detach",
        "$ echo after-detach",
        "after-detach",
        "$",
      ]);

      let ready!: () => void;
      const readiness = new Promise<void>((resolve) => {
        ready = resolve;
      });
      const unsubscribe = session.subscribe((message) => {
        if (message.type === "snapshotReady") ready();
      });
      await readiness;
      const state = await readTerminalState(session);
      const text = [...state.scrollback, ...state.grid].map(rowToText).join("\n");
      expect(text).toContain("before-detach");
      expect(text).toContain("after-detach");
      unsubscribe();
    });
  });

  describe("ANSI replay observation", () => {
    it("reconstructs current terminal state from ANSI", async () => {
      const session = trackSession(
        await createTerminal({
          workspaceId: "ws-test",
          cwd: "/tmp",
          shell: "/bin/sh",
          env: { PS1: "$ " },
          rows: 24,
          cols: 80,
        }),
      );

      const state = await readTerminalState(session);

      expect(state.rows).toBe(24);
      expect(state.cols).toBe(80);
      expect(state.grid).toBeDefined();
      expect(state.grid.length).toBe(24);
      expect(state.grid[0].length).toBe(80);
      expect(state.cursor).toBeDefined();
      expect(typeof state.cursor.row).toBe("number");
      expect(typeof state.cursor.col).toBe("number");
    });

    it("captures cursor presentation modes emitted by terminal apps", async () => {
      const session = trackSession(
        await createTerminal({
          workspaceId: "ws-test",
          cwd: "/tmp",
          shell: "/bin/sh",
          env: { PS1: "$ " },
          rows: 24,
          cols: 80,
        }),
      );

      await waitForLines(session, ["$"]);
      session.send({ type: "input", data: "printf '\\033[2 q\\033[?25l'\r" });

      const deadline = Date.now() + 5000;
      while (
        !new TextDecoder().decode(session.serializeReplay().data).includes("\x1b[?25l\x1b[2 q")
      ) {
        if (Date.now() > deadline) throw new Error("cursor presentation did not reach replay");
        await new Promise((resolve) => setTimeout(resolve, 20));
      }
      expect(new TextDecoder().decode(session.serializeReplay().data)).toContain(
        "\x1b[?25l\x1b[2 q",
      );
    });

    it("grid cells have char and color attributes", async () => {
      const session = trackSession(
        await createTerminal({
          workspaceId: "ws-test",
          cwd: "/tmp",
          shell: "/bin/sh",
          env: { PS1: "$ " },
        }),
      );

      await waitForLines(session, ["$"]);

      const state = await readTerminalState(session);
      // First cell should be "$"
      expect(state.grid[0][0].char).toBe("$");
      expect(state.grid[0][0]).toHaveProperty("fg");
      expect(state.grid[0][0]).toHaveProperty("bg");
    });
  });

  describe("scrollback", () => {
    it("preserves scrollback buffer", async () => {
      const session = trackSession(
        await createTerminal({
          workspaceId: "ws-test",
          cwd: "/tmp",
          shell: "/bin/sh",
          env: { PS1: "$ " },
          rows: 5,
          cols: 80,
        }),
      );

      await waitForLines(session, ["$"]);

      // seq 1 20 produces 20 lines of output
      // With 5 rows, we expect lines to scroll into scrollback
      session.send({ type: "input", data: "seq 1 20\r" });

      // Wait for command to finish - final prompt appears after "20"
      // In a 5-row terminal, we'll see the last lines plus prompt
      // The visible area will show something like: 17, 18, 19, 20, $
      await waitForLines(session, ["17", "18", "19", "20", "$"]);

      const state = await readTerminalState(session);

      // Scrollback should contain the earlier output
      expect(state.scrollback.length).toBeGreaterThan(0);

      const scrollbackText = state.scrollback.map(rowToText).filter((line) => line.length > 0);

      // The scrollback should contain the command and early numbers
      expect(scrollbackText).toContain("$ seq 1 20");
      expect(scrollbackText).toContain("1");
      expect(scrollbackText).toContain("2");
      expect(scrollbackText).toContain("3");
    });
  });

  describe("kill", () => {
    it("terminates the shell process", async () => {
      const session = trackSession(
        await createTerminal({
          workspaceId: "ws-test",
          cwd: "/tmp",
          shell: "/bin/sh",
          env: { PS1: "$ " },
        }),
      );

      await waitForLines(session, ["$"]);

      session.kill();

      // Should not throw when trying to get state after kill
      const state = await readTerminalState(session);
      expect(state).toBeDefined();
    });

    it("send after kill is a no-op", async () => {
      const session = trackSession(
        await createTerminal({
          workspaceId: "ws-test",
          cwd: "/tmp",
          shell: "/bin/sh",
          env: { PS1: "$ " },
        }),
      );

      session.kill();

      // Should not throw
      session.send({ type: "input", data: "echo test\r" });
    });
  });
});
