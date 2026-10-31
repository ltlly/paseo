import { test, expect } from "../support/fixtures";
import { expectComposerVisible, submitMessage } from "../support/helpers/composer";
import { delayCreatedAgentInitialTailResponse } from "../support/helpers/agent-timeline-gate";
import { delayBrowserAgentCreatedStatus } from "../support/helpers/new-workspace";
import { seedWorkspace, type SeedDaemonClient } from "../support/helpers/seed-client";
import {
  createAgentTabFromMenu,
  waitForWorkspaceTabsVisible,
} from "../support/helpers/workspace-tabs";
import { getServerId } from "../support/helpers/server-id";
import { openMobileAgentSidebar, expectMobileAgentSidebarHidden } from "../support/helpers/sidebar";
import { buildHostWorkspaceRoute } from "@/utils/host-routes";

async function waitForCreatedAgentId(
  client: SeedDaemonClient,
  input: { cwd: string; workspaceId: string },
): Promise<string> {
  await expect
    .poll(
      async () => {
        const result = await client.fetchAgents({ scope: "active" });
        return result.entries
          .filter(
            (entry) =>
              entry.agent.cwd === input.cwd && entry.agent.workspaceId === input.workspaceId,
          )
          .map((entry) => entry.agent.id);
      },
      { timeout: 30_000 },
    )
    .toHaveLength(1);
  const result = await client.fetchAgents({ scope: "active" });
  const agent = result.entries.find(
    (entry) => entry.agent.cwd === input.cwd && entry.agent.workspaceId === input.workspaceId,
  );
  if (!agent) {
    throw new Error(`Expected one created agent in ${input.cwd}`);
  }
  return agent.agent.id;
}

async function fetchActiveAgentTitle(
  client: SeedDaemonClient,
  agentId: string,
): Promise<string | null> {
  const result = await client.fetchAgents({ scope: "active" });
  return result.entries.find((entry) => entry.agent.id === agentId)?.agent.title ?? null;
}

test.describe("Workspace agent title handoff", () => {
  test("does not cover the agent pane while the optimistic create becomes authoritative", async ({
    page,
  }) => {
    test.setTimeout(120_000);
    await page.setViewportSize({ width: 1440, height: 900 });

    const timelineGate = await delayCreatedAgentInitialTailResponse(page);
    const workspace = await seedWorkspace({ repoPrefix: "workspace-create-handoff-flash-" });

    try {
      await page.goto(buildHostWorkspaceRoute(getServerId(), workspace.workspaceId));
      await waitForWorkspaceTabsVisible(page);
      await createAgentTabFromMenu(page);
      await expectComposerVisible(page);

      const prompt = "Keep the optimistic agent pane visible during handoff";
      await submitMessage(page, prompt);
      const agentId = await timelineGate.waitForCreatedAgent();
      await timelineGate.waitForDelayedResponse();

      await expect(page.getByTestId(`workspace-tab-agent_${agentId}`).first()).toBeVisible({
        timeout: 15_000,
      });
      await expect(page.getByText(prompt, { exact: true }).first()).toBeVisible();
      await expect(page.getByTestId("agent-history-overlay")).toHaveCount(0);

      const overlayAppeared = page
        .getByTestId("agent-history-overlay")
        .waitFor({ state: "attached", timeout: 2_000 })
        .then(
          () => true,
          () => false,
        );
      timelineGate.release();
      await timelineGate.waitForForwardedResponse();

      expect(await overlayAppeared).toBe(false);
    } finally {
      timelineGate.release();
      await workspace.cleanup();
    }
  });

  test("shows the prompt tab title and replaces it when the daemon title updates", async ({
    page,
  }) => {
    test.setTimeout(120_000);
    await page.setViewportSize({ width: 1440, height: 900 });

    const agentCreatedDelay = await delayBrowserAgentCreatedStatus(page);
    const workspace = await seedWorkspace({ repoPrefix: "workspace-title-handoff-" });

    try {
      await page.goto(buildHostWorkspaceRoute(getServerId(), workspace.workspaceId));
      await waitForWorkspaceTabsVisible(page);
      await createAgentTabFromMenu(page);
      await expectComposerVisible(page);

      const promptTitle = "Investigate optimistic tab title handoff";
      const generatedTitle = "Generated Handoff Title";
      await submitMessage(page, `${promptTitle}\n\nMake the UI state deterministic.`);
      await agentCreatedDelay.waitForCreateRequest();
      await agentCreatedDelay.waitForDelayedCreatedStatus();

      await expect(page.getByRole("button", { name: promptTitle }).first()).toBeVisible({
        timeout: 15_000,
      });
      await expect(
        page.getByText(/Loading agent title|Loading\.\.\./).filter({ visible: true }),
      ).toHaveCount(0);

      const agentId = await waitForCreatedAgentId(workspace.client, {
        cwd: workspace.repoPath,
        workspaceId: workspace.workspaceId,
      });

      await expect(page.getByTestId(`workspace-tab-agent_${agentId}`)).toHaveCount(0);
      agentCreatedDelay.release();

      const agentTab = page.getByTestId(`workspace-tab-agent_${agentId}`).first();
      await expect(agentTab).toBeVisible({ timeout: 15_000 });
      await expect
        .poll(() => fetchActiveAgentTitle(workspace.client, agentId), { timeout: 10_000 })
        .toBe(promptTitle);
      await expect(agentTab).toContainText(promptTitle, { timeout: 15_000 });
      await agentTab.click({ button: "right" });
      await expect(page.getByRole("menuitem", { name: "Rename", exact: true })).toBeVisible({
        timeout: 10_000,
      });
      await page.keyboard.press("Escape");
      await expect(
        page.getByText(/Loading agent title|Loading\.\.\./).filter({ visible: true }),
      ).toHaveCount(0);

      await workspace.client.updateAgent(agentId, { name: generatedTitle });
      await expect
        .poll(() => fetchActiveAgentTitle(workspace.client, agentId), { timeout: 10_000 })
        .toBe(generatedTitle);
      await expect(page.getByRole("button", { name: generatedTitle }).first()).toBeVisible({
        timeout: 15_000,
      });
    } finally {
      agentCreatedDelay.release();
      await workspace.cleanup();
    }
  });
});

test("sidebar agent icons retain idle agents and follow the focused conversation title", async ({
  page,
}) => {
  test.setTimeout(120_000);
  await page.setViewportSize({ width: 1440, height: 900 });
  const nestedButtonErrors: string[] = [];
  page.on("console", (message) => {
    if (message.type() === "error" && /descendant|nested.*button|hydration/i.test(message.text()))
      nestedButtonErrors.push(message.text());
  });
  const workspace = await seedWorkspace({ repoPrefix: "sidebar-agent-presence-" });
  try {
    const first = await workspace.client.createAgent({
      provider: "mock",
      cwd: workspace.repoPath,
      workspaceId: workspace.workspaceId,
      title: "Repair Pi activity",
      modeId: "load-test",
      model: "e2e-fast-stream",
    });
    const second = await workspace.client.createAgent({
      provider: "mock",
      cwd: workspace.repoPath,
      workspaceId: workspace.workspaceId,
      title: "Investigate another task",
      modeId: "load-test",
      model: "ten-second-stream",
    });
    await page.goto(buildHostWorkspaceRoute(getServerId(), workspace.workspaceId));
    const row = page.getByTestId(`sidebar-workspace-row-${getServerId()}:${workspace.workspaceId}`);
    const firstIcon = page.getByTestId(`sidebar-agent-${first.id}`);
    const secondIcon = page.getByTestId(`sidebar-agent-${second.id}`);
    await expect(firstIcon).toBeVisible();
    await expect(secondIcon).toBeVisible();
    await firstIcon.click();
    await expect(row).toContainText("Repair Pi activity");
    await page.mouse.move(1000, 200);
    await firstIcon.hover();
    await expect(page.getByRole("tooltip")).toHaveText("Repair Pi activity");
    await firstIcon.click({ button: "right" });
    await expect(page.getByTestId(`sidebar-agent-regenerate-${first.id}`)).toBeVisible();
    await page.getByTestId(`sidebar-agent-rename-action-${first.id}`).click();
    await page.getByTestId(`sidebar-agent-rename-${first.id}-input`).fill("Manually named Pi task");
    await page.getByTestId(`sidebar-agent-rename-${first.id}-submit`).click();
    await expect(row).toContainText("Manually named Pi task");
    await expect(page.getByTestId(`workspace-tab-agent_${first.id}`).first()).toBeVisible();
    await secondIcon.click();
    await expect(row).toContainText("Investigate another task");
    await expect(page.getByTestId(`workspace-tab-agent_${second.id}`).first()).toBeVisible();
    await workspace.client.sendAgentMessage(second.id, "Show the running indicator while working");
    await expect(page.getByTestId(`sidebar-agent-status-${second.id}-running`)).toBeVisible();
    await expect(firstIcon).toBeVisible();
    await workspace.client.cancelAgent(second.id);
    await expect(page.getByTestId(`sidebar-agent-status-${second.id}-running`)).toHaveCount(0);
    await expect(secondIcon).toBeVisible();
    await firstIcon.click();
    await expect(row).toContainText("Manually named Pi task");
    await workspace.client.updateAgent(first.id, { name: "Updated conversation name" });
    await expect(row).toContainText("Updated conversation name");
    await page.reload();
    await expect(row).toContainText("Updated conversation name");
    await expect(firstIcon).toBeVisible();
    await expect(secondIcon).toBeVisible();
    await page.mouse.move(1000, 200);
    await page.screenshot({ path: "/tmp/paseo-sidebar-agent-presence-desktop.png" });
    expect(nestedButtonErrors).toEqual([]);
  } finally {
    await workspace.cleanup();
  }
});

test("right-click naming keeps failures retryable and applies a bounded generated name", async ({
  page,
}) => {
  test.setTimeout(120_000);
  await page.setViewportSize({ width: 1440, height: 900 });
  const workspace = await seedWorkspace({ repoPrefix: "sidebar-regenerate-name-" });
  const original = await workspace.client.getDaemonConfig();
  try {
    await workspace.client.patchDaemonConfig({
      metadataGeneration: { providers: [{ provider: "mock", model: "e2e-fast-stream" }] },
    });
    const agent = await workspace.client.createAgent({
      provider: "mock",
      cwd: workspace.repoPath,
      workspaceId: workspace.workspaceId,
      title: "An old topic",
      modeId: "load-test",
      model: "e2e-fast-stream",
    });
    await page.goto(buildHostWorkspaceRoute(getServerId(), workspace.workspaceId));
    await page.getByTestId(`sidebar-agent-${agent.id}`).click();
    const row = page.getByTestId(`sidebar-workspace-row-${getServerId()}:${workspace.workspaceId}`);
    await row.click({ button: "right" });
    await page
      .getByTestId(
        `sidebar-workspace-menu-regenerate-title-${getServerId()}:${workspace.workspaceId}`,
      )
      .click();
    await expect(page.getByTestId("regenerate-agent-title-error")).toContainText("Send a message");
    await expect(row).toContainText("An old topic");
    await workspace.client.sendAgentMessage(agent.id, "Repair the Pi status indicator");
    await workspace.client.waitForFinish(agent.id);
    await page.getByTestId("regenerate-agent-title-retry").click();
    await expect(page.getByTestId("regenerate-agent-title-dialog")).toHaveCount(0, {
      timeout: 30_000,
    });
    await expect(row).toContainText("Repair the Pi status indicator");
    await page.mouse.move(1000, 200);
    await page.screenshot({ path: "/tmp/paseo-regenerate-name-success.png" });
  } finally {
    await workspace.client.patchDaemonConfig({
      metadataGeneration: original.config.metadataGeneration,
    });
    await workspace.cleanup();
  }
});

test("compact sidebar agent icons navigate and close the drawer", async ({ page }) => {
  test.setTimeout(90_000);
  await page.setViewportSize({ width: 390, height: 844 });
  const workspace = await seedWorkspace({ repoPrefix: "sidebar-agent-compact-" });
  try {
    const agent = await workspace.client.createAgent({
      provider: "mock",
      cwd: workspace.repoPath,
      workspaceId: workspace.workspaceId,
      title: "Focused phone conversation",
      modeId: "load-test",
      model: "e2e-fast-stream",
    });
    await page.goto(buildHostWorkspaceRoute(getServerId(), workspace.workspaceId));
    await openMobileAgentSidebar(page);
    await page.getByTestId(`sidebar-agent-${agent.id}`).click();
    await expectMobileAgentSidebarHidden(page);
    await expectComposerVisible(page);
    await page.screenshot({ path: "/tmp/paseo-sidebar-agent-presence-compact.png" });
  } finally {
    await workspace.cleanup();
  }
});
