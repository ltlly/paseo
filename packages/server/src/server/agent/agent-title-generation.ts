import type { AgentManager } from "./agent-manager.js";
import type { AgentStorage } from "./agent-storage.js";
import { CONVERSATION_TITLE_CONTEXT_CHARS } from "../worktree-branch-name-generator.js";
import { readTerminalConversation } from "../../terminal/terminal-agent-conversation.js";

type TitleGenerationManager = Pick<
  AgentManager,
  "getAgent" | "getTimeline" | "updateAgentMetadata"
>;
const pendingByManager = new WeakMap<TitleGenerationManager, Map<string, Promise<string>>>();

interface Dependencies {
  manager: TitleGenerationManager;
  storage: Pick<AgentStorage, "get">;
  generate(input: { cwd: string; provider: string; context: string }): Promise<string>;
}

/** One paid request per agent at a time, shared by all client sessions. */
export function regenerateAgentTitle(agentId: string, deps: Dependencies): Promise<string> {
  let pending = pendingByManager.get(deps.manager);
  if (!pending) {
    pending = new Map();
    pendingByManager.set(deps.manager, pending);
  }
  const existing = pending.get(agentId);
  if (existing) return existing;
  const request = generateAndApply(agentId, deps).finally(() => pending.delete(agentId));
  pending.set(agentId, request);
  return request;
}

async function generateAndApply(agentId: string, deps: Dependencies): Promise<string> {
  const record = await deps.storage.get(agentId);
  if (!record || record.archivedAt) throw new Error("This agent is no longer available.");
  let context = record.labels["paseo.terminal-last-prompt"] ?? "";
  if (record.execution || record.persistence) {
    const conversation = await readTerminalConversation({
      provider: record.provider,
      cwd: record.cwd,
      sessionId: record.execution?.providerSessionId ?? record.persistence?.sessionId,
      transcriptPath:
        record.labels["paseo.terminal-transcript"] ?? record.persistence?.nativeHandle ?? undefined,
    });
    context = conversation.context || context;
  }
  if (deps.manager.getAgent(agentId)) {
    const liveContext = deps.manager
      .getTimeline(agentId)
      .filter((item) => item.type === "user_message" || item.type === "assistant_message")
      .slice(-6)
      .map((item) => ("text" in item ? item.text.slice(-300) : ""))
      .join("\n")
      .slice(-CONVERSATION_TITLE_CONTEXT_CHARS);
    if (liveContext.trim()) context = liveContext;
  }
  if (!context.trim())
    throw new Error("Send a message in this conversation before requesting a new name.");
  const title = await deps.generate({ cwd: record.cwd, provider: record.provider, context });
  const current = await deps.storage.get(agentId);
  if (!current || current.archivedAt)
    throw new Error("The conversation was closed while generating its name.");
  if (current.title !== record.title)
    throw new Error("The name changed while generating. Your newer name was kept.");
  await deps.manager.updateAgentMetadata(agentId, { title });
  return title;
}
