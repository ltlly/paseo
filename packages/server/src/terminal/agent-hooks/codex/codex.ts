import type { AgentHookActivityState, AgentHookProvider } from "../agent-hook-installer.js";
import { type CodexHooksFile, codexHooksFormat } from "./codex-settings.js";

const CODEX_EVENT_STATES: Record<string, AgentHookActivityState> = {
  SessionStart: "idle",
  UserPromptSubmit: "running",
  PreToolUse: "running",
  PostToolUse: "running",
  PermissionRequest: "needs-input",
  Stop: "idle",
  SessionEnd: "idle",
};

export const codexAgentHookProvider: AgentHookProvider<CodexHooksFile> = {
  id: "codex",
  events: [
    { event: "UserPromptSubmit" },
    { event: "PreToolUse" },
    { event: "PostToolUse" },
    { event: "PermissionRequest" },
    { event: "Stop" },
    { event: "SessionEnd" },
    { event: "SessionStart" },
  ],
  install: {
    kind: "config-file",
    configDir: ".codex",
    configFile: "hooks.json",
    configDirEnvOverride: "CODEX_HOME",
    hookMarker: "hooks codex",
    format: codexHooksFormat,
  },
  async resolveActivity({ event }) {
    return CODEX_EVENT_STATES[event] ?? null;
  },
};
