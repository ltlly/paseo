import {
  buildAgentHookShellCommand,
  buildAgentHookWindowsCommand,
  type AgentHookActivityState,
  type AgentHookProvider,
} from "../agent-hook-installer.js";

const STATES: Record<string, AgentHookActivityState> = {
  SessionStart: "idle",
  UserPromptSubmit: "running",
  PreToolUse: "running",
  PostToolUse: "running",
  PostToolUseFailure: "running",
  Stop: "idle",
  StopCancelled: "idle",
  StopFailure: "idle",
  SessionEnd: "idle",
};

export const grokAgentHookProvider: AgentHookProvider = {
  id: "grok",
  events: [...Object.keys(STATES), "Notification"].map((event) => ({ event })),
  install: {
    kind: "plugin-file",
    configDir: ".grok",
    configFile: "hooks/paseo-status.json",
    configDirEnvOverride: "GROK_HOME",
    hookMarker: "hooks grok",
    get source() {
      const hooks = Object.fromEntries(
        grokAgentHookProvider.events.map((event) => [
          event.event,
          [
            {
              ...(event.event.includes("ToolUse") ? { matcher: ".*" } : {}),
              hooks: [
                {
                  type: "command",
                  command:
                    process.platform === "win32"
                      ? buildAgentHookWindowsCommand(grokAgentHookProvider, event)
                      : buildAgentHookShellCommand(grokAgentHookProvider, event),
                  timeout: 10,
                },
              ],
            },
          ],
        ]),
      );
      return `${JSON.stringify({ hooks }, null, 2)}\n`;
    },
  },
  async resolveActivity({ event, input }) {
    let data: Record<string, unknown> = {};
    try {
      const value: unknown = JSON.parse(input.isTTY ? "{}" : ((await input.read()) ?? "{}"));
      if (value && typeof value === "object" && !Array.isArray(value))
        data = value as Record<string, unknown>;
    } catch {
      /* Activity-only hooks still work without metadata. */
    }
    if (data.subagentType || data.subagent_type) return null;
    if (
      event === "PreToolUse" &&
      ["AskUserQuestion", "ask_user", "ask_user_question"].includes(
        String(data.toolName ?? data.tool_name),
      )
    )
      return "needs-input";
    if (event === "Notification") {
      const kind = data.notificationType ?? data.notification_type;
      return ["permission_prompt", "idle_prompt", "elicitation_prompt"].includes(String(kind))
        ? "needs-input"
        : null;
    }
    return STATES[event] ?? null;
  },
};
