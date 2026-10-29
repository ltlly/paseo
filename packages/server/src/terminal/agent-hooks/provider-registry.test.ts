import { existsSync, mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { afterEach, describe, expect, it } from "vitest";
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
