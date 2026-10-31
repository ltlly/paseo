import { isWorkspaceRootAgent } from "@/subagents/policies";
import type { Agent } from "@/stores/session-store";
import {
  collectAllTabs,
  findPaneById,
  type WorkspaceLayout,
} from "@/stores/workspace-layout-actions";
import { deriveSidebarStateBucket, type SidebarStateBucket } from "@/utils/sidebar-agent-state";

export type SidebarAgentSource = Pick<
  Agent,
  | "id"
  | "provider"
  | "workspaceId"
  | "title"
  | "archivedAt"
  | "parentAgentId"
  | "execution"
  | "status"
  | "turn"
  | "pendingPermissions"
  | "requiresAttention"
  | "attentionReason"
>;

export interface SidebarWorkspaceAgent {
  agentId: string;
  provider: string;
  icon: string;
  title: string;
  terminalId?: string;
  tabId?: string;
  focused: boolean;
  status: SidebarStateBucket;
}

// The directory is immutable. Index it once per host update, rather than scanning it for every row.
const indexes = new WeakMap<
  ReadonlyMap<string, SidebarAgentSource>,
  Map<string, SidebarAgentSource[]>
>();

function indexAgents(
  agents: ReadonlyMap<string, SidebarAgentSource>,
): Map<string, SidebarAgentSource[]> {
  const cached = indexes.get(agents);
  if (cached) return cached;
  const index = new Map<string, SidebarAgentSource[]>();
  for (const agent of agents.values()) {
    if (agent.archivedAt || !agent.workspaceId) continue;
    const group = index.get(agent.workspaceId) ?? [];
    group.push(agent);
    index.set(agent.workspaceId, group);
  }
  indexes.set(agents, index);
  return index;
}

export function selectSidebarWorkspaceAgents(input: {
  agents: ReadonlyMap<string, SidebarAgentSource>;
  workspaceId: string;
  layout?: WorkspaceLayout;
}): SidebarWorkspaceAgent[] {
  const tabs = input.layout ? collectAllTabs(input.layout.root) : [];
  const focusedTabId = input.layout
    ? findPaneById(input.layout.root, input.layout.focusedPaneId)?.focusedTabId
    : null;
  const result: SidebarWorkspaceAgent[] = [];
  for (const agent of indexAgents(input.agents).get(input.workspaceId) ?? []) {
    const matchingTabs = tabs.filter(({ target }) =>
      agent.execution
        ? target.kind === "terminal" && target.terminalId === agent.execution.terminalId
        : target.kind === "agent" && target.agentId === agent.id,
    );
    const tab =
      matchingTabs.find((candidate) => candidate.tabId === focusedTabId) ?? matchingTabs[0];
    const parent = agent.parentAgentId ? input.agents.get(agent.parentAgentId) : undefined;
    if (!tab && !isWorkspaceRootAgent(agent, parent)) continue;
    const running = agent.execution ? agent.status === "running" : agent.turn.phase === "open";
    const inactiveStatus = agent.status === "running" ? "idle" : agent.status;
    result.push({
      agentId: agent.id,
      provider: agent.provider,
      icon: agent.provider,
      title: agent.title?.trim() || agent.provider,
      terminalId: agent.execution?.terminalId,
      tabId: tab?.tabId,
      focused: Boolean(tab && tab.tabId === focusedTabId),
      status: deriveSidebarStateBucket({
        status: running ? "running" : inactiveStatus,
        pendingPermissionCount: agent.pendingPermissions.length,
        requiresAttention: agent.requiresAttention,
        attentionReason: agent.attentionReason,
      }),
    });
  }
  const order = new Map(tabs.map((tab, index) => [tab.tabId, index]));
  return result.sort(
    (a, b) => (order.get(a.tabId ?? "") ?? tabs.length) - (order.get(b.tabId ?? "") ?? tabs.length),
  );
}
