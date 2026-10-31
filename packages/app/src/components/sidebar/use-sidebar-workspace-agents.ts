import { useCallback, useEffect, useRef } from "react";
import { useStoreWithEqualityFn } from "zustand/traditional";
import { useSessionStore, type Agent } from "@/stores/session-store";
import { useWorkspaceLayoutStore } from "@/stores/workspace-layout-store";
import { workspaceEqualityFns } from "@/stores/session-store-hooks/selectors";
import { selectSidebarWorkspaceAgents } from "./sidebar-workspace-agents";

const EMPTY_AGENTS: ReadonlyMap<string, Agent> = new Map();

export function useSidebarWorkspaceAgents(serverId: string, workspaceId: string) {
  const workspaceKey = `${serverId}:${workspaceId}`;
  const layout = useWorkspaceLayoutStore((state) => state.layoutByWorkspace[workspaceKey]);
  const select = useCallback(
    (state: ReturnType<typeof useSessionStore.getState>) =>
      selectSidebarWorkspaceAgents({
        agents: state.sessions[serverId]?.agents ?? EMPTY_AGENTS,
        workspaceId,
        layout,
      }),
    [serverId, workspaceId, layout],
  );
  const agents = useStoreWithEqualityFn(useSessionStore, select, workspaceEqualityFns.deep);
  const focused = agents.find((agent) => agent.focused);
  const lastFocused = useRef<{ workspaceKey: string; agentId: string } | null>(null);
  useEffect(() => {
    if (focused) lastFocused.current = { workspaceKey, agentId: focused.agentId };
  }, [focused, workspaceKey]);
  const remembered =
    lastFocused.current?.workspaceKey === workspaceKey
      ? agents.find((agent) => agent.agentId === lastFocused.current?.agentId)
      : undefined;
  const activeAgent = focused ?? remembered ?? agents[0];
  return { agents, activeAgent, title: activeAgent?.title ?? null };
}
