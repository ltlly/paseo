import {
  type AgentHookActivityInput,
  type AgentHookActivityState,
  type AgentHookInstallLogger,
  type AgentHookInstallOptions,
  type AgentHookInstallResult,
  type AgentHookProvider,
  agentHooksAreInstalled,
  installAgentHooks,
  uninstallAgentHooks,
} from "./agent-hook-installer.js";
import { claudeAgentHookProvider } from "./claude/claude.js";
import { codexAgentHookProvider } from "./codex/codex.js";
import { grokAgentHookProvider } from "./grok/grok.js";
import { opencodeAgentHookProvider } from "./opencode/opencode.js";

export type {
  AgentHookActivityInput,
  AgentHookActivityState,
  AgentHookProvider,
} from "./agent-hook-installer.js";

export const AGENT_HOOK_PROVIDERS = {
  [claudeAgentHookProvider.id]: claudeAgentHookProvider,
  [codexAgentHookProvider.id]: codexAgentHookProvider,
  [grokAgentHookProvider.id]: grokAgentHookProvider,
  [opencodeAgentHookProvider.id]: opencodeAgentHookProvider,
} satisfies Record<string, AgentHookProvider>;

export type AgentHookProviderId = keyof typeof AGENT_HOOK_PROVIDERS;

export interface AgentHookActivityRequest {
  provider: string;
  event: string;
  input: AgentHookActivityInput;
}

export interface RegisteredAgentHookInstallOptions extends AgentHookInstallOptions {
  logger?: AgentHookInstallLogger;
}

export function installRegisteredAgentHooks(
  options: RegisteredAgentHookInstallOptions = {},
): AgentHookInstallResult[] {
  const results: AgentHookInstallResult[] = [];
  for (const provider of Object.values(AGENT_HOOK_PROVIDERS)) {
    try {
      results.push(installAgentHooks(provider, options));
    } catch (error) {
      options.logger?.warn(
        { err: error, provider: provider.id },
        "Failed to install terminal activity hook provider",
      );
    }
  }
  return results;
}

export function uninstallRegisteredAgentHooks(options: AgentHookInstallOptions = {}): void {
  for (const provider of Object.values(AGENT_HOOK_PROVIDERS)) {
    uninstallAgentHooks(provider, options);
  }
}

export function registeredAgentHooksAreInstalled(options: AgentHookInstallOptions = {}): boolean {
  return Object.values(AGENT_HOOK_PROVIDERS).every((provider) =>
    agentHooksAreInstalled(provider, options),
  );
}

export async function resolveHookActivity(
  request: AgentHookActivityRequest,
): Promise<AgentHookActivityState | null> {
  const provider = Object.hasOwn(AGENT_HOOK_PROVIDERS, request.provider.toLowerCase())
    ? AGENT_HOOK_PROVIDERS[request.provider.toLowerCase()]
    : undefined;
  if (!provider) return null;

  return provider.resolveActivity({ event: request.event, input: request.input });
}

export interface AgentHookReport {
  provider: string;
  event: string;
  state: AgentHookActivityState;
  observedAt: number;
  sessionId?: string;
  cwd?: string;
}

/** Read stdin once: providers and identity extraction share the same bounded input. */
export async function resolveHookReport(
  request: AgentHookActivityRequest,
): Promise<AgentHookReport | null> {
  const observedAt = Date.now();
  let raw: Promise<string | null> | undefined;
  const read = () => (raw ??= request.input.isTTY ? Promise.resolve(null) : request.input.read());
  const state = await resolveHookActivity({ ...request, input: { ...request.input, read } });
  if (!state) return null;
  let payload: Record<string, unknown> = {};
  try {
    const value: unknown = JSON.parse((await read()) ?? "{}");
    if (value && typeof value === "object" && !Array.isArray(value))
      payload = value as Record<string, unknown>;
  } catch {
    /* Missing hook metadata does not prevent activity reporting. */
  }
  const sessionId = payload.session_id ?? payload.sessionId ?? payload.sessionID;
  return {
    provider: request.provider.toLowerCase(),
    event: request.event,
    state,
    observedAt,
    ...(typeof sessionId === "string" && sessionId.length > 0 && sessionId.length <= 512
      ? { sessionId }
      : {}),
    ...(typeof payload.cwd === "string" && payload.cwd.length <= 4096 ? { cwd: payload.cwd } : {}),
  };
}
