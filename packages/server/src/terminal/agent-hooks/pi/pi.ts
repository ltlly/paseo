import type { AgentHookProvider } from "../agent-hook-installer.js";

interface PiHookEvent {
  prompt?: string;
}

interface PiHookContext {
  cwd: string;
  isIdle(): boolean;
  sessionManager: {
    getSessionId(): string;
    getSessionName(): string | undefined;
    getSessionFile?(): string | undefined;
  };
}

interface PiHookApi {
  on(
    event: string,
    handler: (event: PiHookEvent, context: PiHookContext) => void | Promise<void>,
  ): void;
}

export interface PiHookMetadata {
  sessionId: string;
  cwd: string;
  title?: string;
  prompt?: string;
  transcriptPath?: string;
  state: "idle" | "running";
}

// Keep this function self-contained: the installer serializes it into Pi's extension.
export function registerPiActivityHooks(
  pi: PiHookApi,
  report: (event: string, metadata: PiHookMetadata) => void | Promise<void>,
): void {
  for (const [nativeEvent, hookEvent] of [
    ["session_start", "SessionStart"],
    ["before_agent_start", "UserPromptSubmit"],
    ["agent_start", "UserPromptSubmit"],
    ["agent_settled", "Stop"],
    ["session_shutdown", "SessionEnd"],
    ["session_info_changed", "SessionInfo"],
  ]) {
    pi.on(nativeEvent, (event, context) => {
      return report(hookEvent, {
        sessionId: context.sessionManager.getSessionId(),
        cwd: context.cwd,
        title: context.sessionManager.getSessionName(),
        prompt: event.prompt,
        transcriptPath: context.sessionManager.getSessionFile?.(),
        state: context.isIdle() ? "idle" : "running",
      });
    });
  }
}

export const piAgentHookProvider: AgentHookProvider = {
  id: "pi",
  events: ["SessionStart", "UserPromptSubmit", "Stop", "SessionEnd", "SessionInfo"].map(
    (event) => ({ event }),
  ),
  install: {
    kind: "plugin-file",
    configDir: ".pi/agent",
    configFile: "extensions/paseo-terminal-activity.ts",
    configDirEnvOverride: "PI_CODING_AGENT_DIR",
    hookMarker: "paseo hooks pi",
    get source() {
      return `import { spawn } from "node:child_process";
${registerPiActivityHooks.toString()}
export default function (pi) {
  if (!process.env.PASEO_TERMINAL_ID) return;
  let pending = Promise.resolve();
  registerPiActivityHooks(pi, (event, metadata) => {
    const input = JSON.stringify(metadata);
    pending = pending.then(() => new Promise((resolve) => {
      try {
        const child = spawn(process.env.PASEO_HOOK_CLI || "paseo", ["hooks", "pi", event], {
          stdio: ["pipe", "ignore", "ignore"],
          windowsHide: true,
        });
        child.on("error", resolve);
        child.on("exit", resolve);
        child.stdin.on("error", () => {});
        child.stdin.end(input);
        const timeout = setTimeout(() => { child.kill(); resolve(); }, 2000);
        child.on("close", () => clearTimeout(timeout));
      } catch { resolve(); }
    })).catch(() => {});
    return pending;
  });
}
`;
    },
  },
  async resolveActivity({ event, input }) {
    if (event === "UserPromptSubmit") return "running";
    if (event === "SessionStart" || event === "Stop" || event === "SessionEnd") return "idle";
    if (event !== "SessionInfo") return null;
    try {
      const data = JSON.parse((await input.read()) ?? "{}");
      return data.state === "running" ? "running" : "idle";
    } catch {
      return null;
    }
  },
};
