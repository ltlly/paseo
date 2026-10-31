import type { Agent, WorkspaceDescriptor } from "@/stores/session-store";
import { isWorkspaceRootAgent } from "@/subagents/policies";
import { deriveSidebarStateBucket, aggregateSidebarStateBuckets } from "./sidebar-agent-state";

export interface WorkspaceAgentActivity {
  agentId: string;
  status: WorkspaceDescriptor["status"];
  enteredAt: Date | null;
}

function workspaceAgentStatus(agent: Agent): Agent["status"] {
  if (agent.execution) return agent.status;
  if (agent.turn.phase === "open") return "running";
  return agent.status === "running" ? "idle" : agent.status;
}

export function buildWorkspaceAgentActivityIndex(
  agents: ReadonlyMap<string, Agent>,
  previous?: ReadonlyMap<string, WorkspaceAgentActivity>,
): Map<string, WorkspaceAgentActivity> {
  const activityByWorkspaceId = new Map<string, WorkspaceAgentActivity>();

  for (const agent of agents.values()) {
    const parentAgent = agent.parentAgentId ? agents.get(agent.parentAgentId) : undefined;
    if (agent.archivedAt || !agent.workspaceId || !isWorkspaceRootAgent(agent, parentAgent)) {
      continue;
    }

    const enteredAt = agent.attentionTimestamp ?? agent.updatedAt;
    const status = deriveSidebarStateBucket({
      status: workspaceAgentStatus(agent),
      pendingPermissionCount: agent.pendingPermissions.length,
      requiresAttention: agent.requiresAttention,
      attentionReason: agent.attentionReason,
    });
    const current = activityByWorkspaceId.get(agent.workspaceId);
    if (!shouldReplaceWorkspaceActivity(current, status, enteredAt)) continue;
    activityByWorkspaceId.set(agent.workspaceId, {
      agentId: agent.id,
      status,
      enteredAt,
    });
  }

  for (const [workspaceId, activity] of activityByWorkspaceId) {
    const previousActivity = previous?.get(workspaceId);
    if (
      previousActivity?.agentId === activity.agentId &&
      previousActivity.status === activity.status
    ) {
      activityByWorkspaceId.set(workspaceId, previousActivity);
    }
  }

  if (previous && areWorkspaceAgentActivityIndexesIdentical(previous, activityByWorkspaceId)) {
    return previous instanceof Map ? previous : new Map(previous);
  }
  return activityByWorkspaceId;
}

function areWorkspaceAgentActivityIndexesIdentical(
  previous: ReadonlyMap<string, WorkspaceAgentActivity>,
  next: ReadonlyMap<string, WorkspaceAgentActivity>,
): boolean {
  if (previous.size !== next.size) {
    return false;
  }
  for (const [workspaceId, activity] of next) {
    if (previous.get(workspaceId) !== activity) {
      return false;
    }
  }
  return true;
}

function shouldReplaceWorkspaceActivity(
  current: WorkspaceAgentActivity | undefined,
  status: WorkspaceAgentActivity["status"],
  enteredAt: Date,
): boolean {
  if (!current) return true;
  if (current.status !== status)
    return aggregateSidebarStateBuckets([current.status, status]) === status;
  return !current.enteredAt || enteredAt > current.enteredAt;
}
