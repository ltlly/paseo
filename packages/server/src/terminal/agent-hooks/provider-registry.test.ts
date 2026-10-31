import { existsSync, mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { afterEach, describe, expect, it } from "vitest";
import { registerPiActivityHooks, type PiHookMetadata } from "./pi/pi.js";
import { installRegisteredAgentHooks, resolveHookReport } from "./provider-registry.js";

const temporaryDirs: string[] = [];

interface WarningLogEntry {
  bindings: Record<string, unknown>;
  message: string;
}

interface WarningLogger {
  entries: WarningLogEntry[];
  warn(bindings: Record<string, unknown>, message: string): void;
}

afterEach(() => {
  while (temporaryDirs.length > 0) {
    const dir = temporaryDirs.pop();
    if (dir) rmSync(dir, { recursive: true, force: true });
  }
});

function createTempDir(prefix: string): string {
  const dir = mkdtempSync(join(tmpdir(), prefix));
  temporaryDirs.push(dir);
  return dir;
}

function createWarningLogger(): WarningLogger {
  return {
    entries: [],
    warn(bindings, message) {
      this.entries.push({ bindings, message });
    },
  };
}

describe("terminal agent hook provider registry", () => {
  it("ignores inherited provider commands in the Grok hook runner", async () => {
    const input = { read: async () => JSON.stringify({ sessionId: "same-session" }) };
    const env = { GROK_HOOK_EVENT: "session_start" };
    expect(
      await resolveHookReport({ provider: "claude", event: "SessionStart", input, env }),
    ).toBeNull();
    expect(
      await resolveHookReport({ provider: "grok", event: "SessionStart", input, env }),
    ).toMatchObject({ provider: "grok", sessionId: "same-session" });
    // A Claude child launched by a Grok tool inherits GROK_SESSION_ID, but is not
    // running inside Grok's hook runner and still owns its own provider identity.
    expect(
      await resolveHookReport({
        provider: "claude",
        event: "SessionStart",
        input,
        env: { GROK_SESSION_ID: "parent-session" },
      }),
    ).toMatchObject({ provider: "claude" });
  });

  it("reports real Grok tool approval notifications even at info level", async () => {
    const report = await resolveHookReport({
      provider: "grok",
      event: "Notification",
      input: {
        read: async () =>
          JSON.stringify({
            notificationType: "permission_prompt",
            message: "Tool permission requested",
            level: "info",
          }),
      },
    });
    expect(report).toMatchObject({ state: "needs-input" });
  });

  it("does not let Grok subagent tool completion or exit finish the parent", async () => {
    for (const event of ["PreToolUse", "PostToolUse", "Stop", "SessionEnd"]) {
      expect(
        await resolveHookReport({
          provider: "grok",
          event,
          input: { read: async () => JSON.stringify({ subagentType: "explore" }) },
        }),
      ).toBeNull();
    }
  });

  it("preserves provider session identity and reads hook input once", async () => {
    let reads = 0;
    const report = await resolveHookReport({
      provider: "GROK",
      event: "PreToolUse",
      input: {
        read: async () => {
          reads++;
          return JSON.stringify({
            session_id: "grok-session",
            cwd: "/work",
            tool_name: "AskUserQuestion",
          });
        },
      },
    });
    expect(reads).toBe(1);
    expect(report).toMatchObject({
      provider: "grok",
      sessionId: "grok-session",
      cwd: "/work",
      state: "needs-input",
    });
  });
  it("recognizes Grok startup, completion and permission notifications", async () => {
    for (const event of ["SessionStart", "Stop", "StopCancelled", "StopFailure", "SessionEnd"]) {
      expect(
        await resolveHookReport({
          provider: "grok",
          event,
          input: { isTTY: true, read: async () => null },
        }),
      ).toMatchObject({ state: "idle", event });
    }
    expect(
      await resolveHookReport({
        provider: "grok",
        event: "Notification",
        input: { read: async () => JSON.stringify({ notification_type: "permission_prompt" }) },
      }),
    ).toMatchObject({ state: "needs-input" });
    expect(
      await resolveHookReport({
        provider: "constructor",
        event: "Stop",
        input: { read: async () => null },
      }),
    ).toBeNull();
  });
  it("continues installing provider hooks after one provider fails", () => {
    const root = createTempDir("paseo-agent-hook-registry-");
    const badClaudeConfigDir = join(root, "not-a-directory");
    const codexHome = join(root, "codex");
    const opencodeConfigDir = join(root, "opencode");
    const logger = createWarningLogger();
    writeFileSync(badClaudeConfigDir, "");

    const results = installRegisteredAgentHooks({
      env: {
        CLAUDE_CONFIG_DIR: badClaudeConfigDir,
        CODEX_HOME: codexHome,
        OPENCODE_CONFIG_DIR: opencodeConfigDir,
      },
      homeDir: join(root, "home"),
      logger,
    });

    expect(results.map((result) => result.configPath)).toEqual([
      join(codexHome, "hooks.json"),
      join(root, "home", ".grok", "hooks", "paseo-status.json"),
      join(opencodeConfigDir, "plugins", "paseo-terminal-activity.js"),
      join(root, "home", ".pi", "agent", "extensions", "paseo-terminal-activity.ts"),
    ]);
    expect(existsSync(join(codexHome, "hooks.json"))).toBe(true);
    expect(existsSync(join(opencodeConfigDir, "plugins", "paseo-terminal-activity.js"))).toBe(true);
    expect(logger.entries).toEqual([
      {
        bindings: expect.objectContaining({ err: expect.any(Error), provider: "claude" }),
        message: "Failed to install terminal activity hook provider",
      },
    ]);
  });
});

describe("Pi terminal extension", () => {
  it("reports preparation, final settlement, and new conversation identity", async () => {
    type HookHandler = Parameters<Parameters<typeof registerPiActivityHooks>[0]["on"]>[1];
    const handlers = new Map<string, HookHandler>();
    const reports: Array<{ event: string; metadata: PiHookMetadata }> = [];
    registerPiActivityHooks(
      {
        on: (event, handler) => {
          handlers.set(event, handler);
        },
      },
      (event, metadata) => {
        reports.push({ event, metadata });
      },
    );
    let sessionId = "pi-session-1";
    let idle = true;
    const context = {
      cwd: "/repo",
      isIdle: () => idle,
      sessionManager: {
        getSessionId: () => sessionId,
        getSessionName: () => undefined,
      },
    };
    await handlers.get("session_start")!({}, context);
    idle = false;
    await handlers.get("before_agent_start")!({ prompt: "Fix Pi status" }, context);
    await handlers.get("agent_start")!({}, context);
    expect(handlers.has("agent_end")).toBe(false);
    idle = true;
    await handlers.get("agent_settled")!({}, context);
    sessionId = "pi-session-2";
    await handlers.get("session_start")!({}, context);
    expect(
      reports.map(({ event, metadata }) => [event, metadata.sessionId, metadata.state]),
    ).toEqual([
      ["SessionStart", "pi-session-1", "idle"],
      ["UserPromptSubmit", "pi-session-1", "running"],
      ["UserPromptSubmit", "pi-session-1", "running"],
      ["Stop", "pi-session-1", "idle"],
      ["SessionStart", "pi-session-2", "idle"],
    ]);
    expect(
      await resolveHookReport({
        provider: "pi",
        event: "UserPromptSubmit",
        input: {
          read: async () => JSON.stringify(reports[1].metadata),
        },
      }),
    ).toMatchObject({
      provider: "pi",
      state: "running",
      sessionId: "pi-session-1",
      prompt: "Fix Pi status",
    });
  });
});

it.skipIf(process.platform !== "win32")(
  "Pi hooks invoke a Windows cmd launcher whose path contains spaces",
  async () => {
    const { piAgentHookProvider } = await import("./pi/pi.js");
    const { execFileSync } = await import("node:child_process");
    const { readFileSync } = await import("node:fs");
    const { pathToFileURL } = await import("node:url");
    const directory = createTempDir("paseo pi cmd ");
    const extension = join(directory, "extension.mjs"),
      probe = join(directory, "probe.mjs"),
      shim = join(directory, "paseo.cmd"),
      runner = join(directory, "runner.mjs"),
      result = join(directory, "result.json");
    writeFileSync(extension, piAgentHookProvider.install.source);
    writeFileSync(
      probe,
      `import fs from 'node:fs';let input='';for await(const chunk of process.stdin)input+=chunk;fs.writeFileSync(process.env.PASEO_QA_RESULT,JSON.stringify({args:process.argv.slice(2),input:JSON.parse(input)}));`,
    );
    writeFileSync(shim, `@echo off\r\n"${process.execPath}" "${probe}" %*\r\n`);
    writeFileSync(
      runner,
      `import register from ${JSON.stringify(pathToFileURL(extension).href)};const callbacks=new Map();register({on:(name,callback)=>callbacks.set(name,callback)});await callbacks.get('session_start')({}, {cwd:process.cwd(),isIdle:()=>true,sessionManager:{getSessionId:()=>'cmd-session',getSessionName:()=>undefined}});`,
    );
    execFileSync(process.execPath, [runner], {
      env: {
        ...process.env,
        PASEO_TERMINAL_ID: "cmd-terminal",
        PASEO_HOOK_CLI: shim,
        PASEO_QA_RESULT: result,
      },
      timeout: 10000,
    });
    expect(JSON.parse(readFileSync(result, "utf8"))).toMatchObject({
      args: ["hooks", "pi", "SessionStart"],
      input: { sessionId: "cmd-session", state: "idle" },
    });
  },
);
