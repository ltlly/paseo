import {
  createDefaultLayout,
  createTabInLayout,
  FOCUSED_PANE_PLACEMENT,
} from "@/stores/workspace-layout-actions";
import { selectSidebarWorkspaceAgents, type SidebarAgentSource } from "./sidebar-workspace-agents";
import { i18n } from "@/i18n/i18next";
import { describe, expect, it } from "vitest";
import {
  resolveSidebarWorkspaceAccessibilityLabel,
  resolveSidebarWorkspacePrimaryLabel,
} from "@/components/sidebar/sidebar-workspace-title";

describe("resolveSidebarWorkspacePrimaryLabel", () => {
  it("uses the workspace name in title mode", () => {
    const label = resolveSidebarWorkspacePrimaryLabel({
      workspace: { name: "Investigate search", currentBranch: "fix/search" },
      workspaceTitleSource: "title",
    });

    expect(label).toBe("Investigate search");
  });

  it("uses the branch name in branch mode", () => {
    const label = resolveSidebarWorkspacePrimaryLabel({
      workspace: { name: "Investigate search", currentBranch: "fix/search" },
      workspaceTitleSource: "branch",
    });

    expect(label).toBe("fix/search");
  });

  it("falls back to the workspace name in branch mode without a branch", () => {
    const label = resolveSidebarWorkspacePrimaryLabel({
      workspace: { name: "Local folder", currentBranch: null },
      workspaceTitleSource: "branch",
    });

    expect(label).toBe("Local folder");
  });
});

describe("resolveSidebarWorkspaceAccessibilityLabel", () => {
  it("includes the visible host badge with the workspace title", () => {
    const label = resolveSidebarWorkspaceAccessibilityLabel({
      workspace: { name: "Investigate search", currentBranch: "fix/search", statusBucket: "done" },
      workspaceTitleSource: "title",
      hostBadgeLabel: "Build host",
      t: i18n.t,
    });

    expect(label).toBe("Investigate search, Build host");
  });

  it("owns every visual row contributor in one accessible label", () => {
    const label = resolveSidebarWorkspaceAccessibilityLabel({
      workspace: {
        name: "Investigate search",
        currentBranch: "fix/search",
        statusBucket: "running",
      },
      workspaceTitleSource: "branch",
      leadingProjectName: "Search project",
      hostBadgeLabel: "Build host",
      pullRequestLabel: "Pull request 42",
      serviceLabel: "Service web running",
      t: i18n.t,
    });

    expect(label).toBe(
      "Search project, fix/search, Build host, Pull request 42, Service web running, Working",
    );
  });

  it("omits the idle status from the workspace label", () => {
    const label = resolveSidebarWorkspaceAccessibilityLabel({
      workspace: { name: "Investigate search", currentBranch: "fix/search", statusBucket: "done" },
      workspaceTitleSource: "title",
      leadingProjectName: "Search project",
      hostBadgeLabel: "Build host",
      t: i18n.t,
    });

    expect(label).toBe("Search project, Investigate search, Build host");
  });
});

it("uses the focused agent title while preserving the explicit branch display setting", () => {
  const workspace = { name: "Old workspace task", currentBranch: "main" };
  expect(
    resolveSidebarWorkspacePrimaryLabel({
      workspace,
      workspaceTitleSource: "title",
      focusedAgentTitle: "Current conversation",
    }),
  ).toBe("Current conversation");
  expect(
    resolveSidebarWorkspacePrimaryLabel({
      workspace,
      workspaceTitleSource: "branch",
      focusedAgentTitle: "Current conversation",
    }),
  ).toBe("main");
});

function sidebarAgent(id: string, overrides: Partial<SidebarAgentSource> = {}): SidebarAgentSource {
  return {
    id,
    provider: "pi",
    workspaceId: "workspace-a",
    title: id,
    archivedAt: null,
    parentAgentId: null,
    status: "idle",
    turn: { phase: "idle", cancellationRequestId: null },
    pendingPermissions: [],
    requiresAttention: false,
    attentionReason: null,
    ...overrides,
  };
}

it("keeps idle agents visible and reports terminal work without a managed turn", () => {
  const agents = new Map([
    ["idle", sidebarAgent("idle")],
    [
      "working",
      sidebarAgent("working", {
        status: "running",
        execution: { kind: "terminal", terminalId: "terminal-pi", icon: "bot" },
      }),
    ],
    ["other", sidebarAgent("other", { workspaceId: "workspace-b" })],
    ["archived", sidebarAgent("archived", { archivedAt: new Date("2026-10-31") })],
    ["child", sidebarAgent("child", { parentAgentId: "idle" })],
  ]);
  expect(
    selectSidebarWorkspaceAgents({ agents, workspaceId: "workspace-a" }).map(
      ({ agentId, status, terminalId }) => ({ agentId, status, terminalId }),
    ),
  ).toEqual([
    { agentId: "idle", status: "done", terminalId: undefined },
    { agentId: "working", status: "running", terminalId: "terminal-pi" },
  ]);
});

it("keeps provider identity on terminal icons even with a generic profile icon", () => {
  const terminal = sidebarAgent("terminal", {
    execution: { kind: "terminal", terminalId: "pi", icon: "bot" },
  });
  expect(
    selectSidebarWorkspaceAgents({
      agents: new Map([[terminal.id, terminal]]),
      workspaceId: "workspace-a",
    })[0].icon,
  ).toBe("pi");
});

it("recognizes the focused tab when two tabs show the same agent", () => {
  let layout = createDefaultLayout();
  for (const tabId of ["first", "second"]) {
    const next = createTabInLayout({
      layout,
      target: { kind: "agent", agentId: "agent" },
      now: 0,
      placement: FOCUSED_PANE_PLACEMENT,
      explorerSidebarPaneId: null,
      createTabId: () => tabId,
    });
    if (!next) throw new Error("Could not create fixture tab");
    layout = next.layout;
  }
  expect(
    selectSidebarWorkspaceAgents({
      agents: new Map([["agent", sidebarAgent("agent")]]),
      workspaceId: "workspace-a",
      layout,
    }),
  ).toMatchObject([{ agentId: "agent", tabId: "second", focused: true }]);
});

it("includes an agent assigned to a different workspace than its parent", () => {
  const parent = sidebarAgent("parent", { workspaceId: "workspace-b" });
  const child = sidebarAgent("child", { parentAgentId: "parent" });
  expect(
    selectSidebarWorkspaceAgents({
      agents: new Map([
        [parent.id, parent],
        [child.id, child],
      ]),
      workspaceId: "workspace-a",
    }).map(({ agentId }) => agentId),
  ).toEqual(["child"]);
});
