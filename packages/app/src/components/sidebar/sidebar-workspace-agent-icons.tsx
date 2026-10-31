import { memo, useCallback } from "react";
import { View, type GestureResponderEvent } from "react-native";
import { StyleSheet } from "react-native-unistyles";
import { useTranslation } from "react-i18next";
import { TerminalProfileIcon } from "@/components/terminal-profile-icon";
import { ToolbarButton } from "@/components/ui/pane-content-toolbar";
import { StatusRing } from "@/components/status-ring";
import { navigateToAgent } from "@/utils/navigate-to-agent";
import { useWorkspaceLayoutStore } from "@/stores/workspace-layout-store";
import type { SidebarWorkspaceAgent } from "./sidebar-workspace-agents";
import {
  ContextMenu,
  ContextMenuContent,
  ContextMenuItem,
  ContextMenuTrigger,
} from "@/components/ui/context-menu";
import { useRegenerateAgentTitle } from "./use-regenerate-agent-title";

export const SidebarWorkspaceAgentIcons = memo(function SidebarWorkspaceAgentIcons({
  agents,
  serverId,
  workspaceId,
}: {
  agents: readonly SidebarWorkspaceAgent[];
  serverId: string;
  workspaceId: string;
}) {
  if (!agents.length) return null;
  return (
    <View style={styles.row} testID={`sidebar-workspace-agents-${serverId}:${workspaceId}`}>
      {agents.map((agent) => (
        <AgentIconButton
          key={agent.agentId}
          agent={agent}
          serverId={serverId}
          workspaceId={workspaceId}
        />
      ))}
    </View>
  );
});

function AgentIconButton({
  agent,
  serverId,
  workspaceId,
}: {
  agent: SidebarWorkspaceAgent;
  serverId: string;
  workspaceId: string;
}) {
  const { t } = useTranslation();
  const naming = useRegenerateAgentTitle(serverId, agent.agentId, agent.title);
  const handlePress = useCallback(
    (event: GestureResponderEvent) => {
      event.stopPropagation();
      if (agent.tabId)
        useWorkspaceLayoutStore.getState().focusTab(`${serverId}:${workspaceId}`, agent.tabId);
      navigateToAgent({
        serverId,
        workspaceId,
        agentId: agent.agentId,
        terminalId: agent.terminalId,
      });
    },
    [agent, serverId, workspaceId],
  );
  const icon = <TerminalProfileIcon iconKey={agent.icon} serverId={serverId} size={16} />;
  return (
    <>
      <ContextMenu>
        <ContextMenuTrigger contextOnly enabledOnMobile={false}>
          <ToolbarButton
            label={agent.title}
            selected={agent.focused}
            round
            style={styles.button}
            testID={`sidebar-agent-${agent.agentId}`}
            onPress={handlePress}
          >
            <View
              style={styles.icon}
              testID={`sidebar-agent-status-${agent.agentId}-${agent.status}`}
            >
              {agent.status === "running" ? <StatusRing size={24}>{icon}</StatusRing> : icon}
              {agent.status === "needs_input" ||
              agent.status === "attention" ||
              agent.status === "failed" ? (
                <View
                  style={[styles.dot, agent.status === "failed" ? styles.failed : styles.attention]}
                />
              ) : null}
            </View>
          </ToolbarButton>
        </ContextMenuTrigger>
        <ContextMenuContent width={260} testID={`sidebar-agent-menu-${agent.agentId}`}>
          <ContextMenuItem
            testID={`sidebar-agent-rename-action-${agent.agentId}`}
            onSelect={naming.onRename}
          >
            {t("workspace.tabs.menu.renameAgent")}
          </ContextMenuItem>
          {naming.onRegenerate ? (
            <ContextMenuItem
              testID={`sidebar-agent-regenerate-${agent.agentId}`}
              onSelect={naming.onRegenerate}
              status={naming.pending ? "pending" : "idle"}
              pendingLabel={t("conversationNaming.pending")}
            >
              {t("conversationNaming.action")}
            </ContextMenuItem>
          ) : null}
        </ContextMenuContent>
      </ContextMenu>
      {naming.dialog}
    </>
  );
}

const styles = StyleSheet.create((theme) => ({
  row: {
    flexDirection: "row",
    flexWrap: "wrap",
    gap: theme.spacing[1],
    marginTop: theme.spacing[1],
  },
  button: { width: 30, height: 30, padding: 0 },
  icon: { width: 26, height: 26, alignItems: "center", justifyContent: "center" },
  dot: {
    position: "absolute",
    width: 6,
    height: 6,
    borderRadius: theme.borderRadius.full,
    right: 0,
    bottom: 0,
  },
  failed: { backgroundColor: theme.colors.statusDanger },
  attention: { backgroundColor: theme.colors.statusWarning },
}));
