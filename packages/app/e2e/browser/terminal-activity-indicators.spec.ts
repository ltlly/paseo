import { mkdir, writeFile } from "node:fs/promises";
import { join } from "node:path";
import { test, expect, type Page } from "../support/fixtures";
import { TerminalE2EHarness, type TerminalInstance } from "../support/helpers/terminal-dsl";
import { getServerId } from "../support/helpers/server-id";

type HookActivityState = "running" | "idle" | "needs-input";
type TabStatusBucket = "running" | "needs_input" | "attention" | "none";

const TERMINAL_ACTIVITY_REPORTER_SCRIPT = `
const fs = require("node:fs");
const path = require("node:path");

const triggerDir = process.argv[1];
const provider = process.argv[2];
const states = ["running", "idle", "needs-input"];

function sleep(ms) {
  return new Promise((resolve) => setTimeout(resolve, ms));
}

async function waitForTrigger(state) {
  const triggerPath = path.join(triggerDir, state);
  while (!fs.existsSync(triggerPath)) {
    await sleep(50);
  }
}

async function reportActivity(state) {
  const response = await fetch(process.env.PASEO_TERMINAL_ACTIVITY_URL, {
    method: "POST",
    headers: { "content-type": "application/json" },
    body: JSON.stringify({
      terminalId: process.env.PASEO_TERMINAL_ID,
      token: process.env.PASEO_ACTIVITY_TOKEN,
      state,
      ...(provider ? { provider, event: state === "running" ? "UserPromptSubmit" : "Stop", sessionId: provider + "-conversation", title: provider + " terminal conversation", prompt: state === "running" ? "Investigate " + provider + " status" : undefined } : {}),
    }),
  });
  if (!response.ok) {
    throw new Error("Activity report failed: " + response.status);
  }
  process.stdout.write("PASEO_ACTIVITY_REPORTED:" + state + "\\n");
}

(async () => {
  for (const state of states) {
    await waitForTrigger(state);
    await reportActivity(state);
  }
  setInterval(() => {}, 1000);
})().catch((error) => {
  console.error(error && error.stack ? error.stack : error);
  setInterval(() => {}, 1000);
});
`;

function internalActivityState(state: HookActivityState): "working" | "idle" {
  if (state === "running") return "working";
  return "idle";
}

function tabStatusBucket(state: HookActivityState): TabStatusBucket {
  if (state === "running") return "running";
  if (state === "needs-input") return "needs_input";
  return "attention";
}

function expectedAttentionReason(state: HookActivityState): "finished" | "needs_input" | null {
  if (state === "idle") return "finished";
  if (state === "needs-input") return "needs_input";
  return null;
}

function terminalTab(page: Page, terminalId: string) {
  return page.getByTestId(`workspace-tab-terminal_${terminalId}`).first();
}

async function expectTerminalTabStatus(
  page: Page,
  terminalId: string,
  status: TabStatusBucket,
): Promise<void> {
  await expect(
    terminalTab(page, terminalId).locator(`[data-status-bucket="${status}"]`),
  ).toBeVisible({
    timeout: 15_000,
  });
}

async function focusTerminalTab(page: Page, terminalId: string): Promise<void> {
  await terminalTab(page, terminalId).click();
}

class ControlledActivityTerminal {
  constructor(
    readonly terminal: TerminalInstance,
    private readonly harness: TerminalE2EHarness,
    private readonly triggerDir: string,
  ) {}

  async report(state: HookActivityState): Promise<void> {
    await writeFile(join(this.triggerDir, state), "");
    await this.harness.waitForTerminalActivity({
      terminalId: this.terminal.id,
      state: internalActivityState(state),
      attentionReason: expectedAttentionReason(state),
      timeoutMs: 15_000,
    });
  }
}

async function createControlledActivityTerminal(
  harness: TerminalE2EHarness,
  provider?: string,
): Promise<ControlledActivityTerminal> {
  const triggerDir = join(harness.tempRepo.path, ".activity-triggers" + (provider ?? ""));
  await mkdir(triggerDir, { recursive: true });
  const terminal = await harness.createTerminal({
    name: "activity-source",
    command: process.execPath,
    args: ["-e", TERMINAL_ACTIVITY_REPORTER_SCRIPT, triggerDir, ...(provider ? [provider] : [])],
  });
  return new ControlledActivityTerminal(terminal, harness, triggerDir);
}

async function withTerminalActivityFixture(
  harness: TerminalE2EHarness,
  fn: (input: {
    activityTerminal: ControlledActivityTerminal;
    focusTerminal: TerminalInstance;
  }) => Promise<void>,
): Promise<void> {
  const activityTerminal = await createControlledActivityTerminal(harness);
  const focusTerminal = await harness.createTerminal({ name: "focus-sink" });
  try {
    await fn({ activityTerminal, focusTerminal });
  } finally {
    await harness.killTerminal(activityTerminal.terminal.id);
    await harness.killTerminal(focusTerminal.id);
  }
}

test.describe("Terminal activity indicators", () => {
  let harness: TerminalE2EHarness;

  test.beforeAll(async () => {
    harness = await TerminalE2EHarness.create({ tempPrefix: "terminal-activity-indicators-" });
  });

  test.afterAll(async () => {
    await harness?.cleanup();
  });

  test("terminal activity follows the tab and clears when the terminal is focused", async ({
    page,
  }) => {
    await withTerminalActivityFixture(harness, async ({ activityTerminal, focusTerminal }) => {
      await harness.openTerminal(page, { terminalId: activityTerminal.terminal.id });
      await harness.openTerminal(page, { terminalId: focusTerminal.id });

      await activityTerminal.report("running");
      await expectTerminalTabStatus(page, activityTerminal.terminal.id, tabStatusBucket("running"));

      await activityTerminal.report("idle");
      await expectTerminalTabStatus(page, activityTerminal.terminal.id, tabStatusBucket("idle"));

      await activityTerminal.report("needs-input");
      await expectTerminalTabStatus(
        page,
        activityTerminal.terminal.id,
        tabStatusBucket("needs-input"),
      );

      await focusTerminalTab(page, activityTerminal.terminal.id);
      await harness.waitForTerminalActivity({
        terminalId: activityTerminal.terminal.id,
        state: "idle",
        attentionReason: null,
        timeoutMs: 15_000,
      });
      await expectTerminalTabStatus(page, activityTerminal.terminal.id, "none");
    });
  });
});

test("sidebar icons include both Pi and Grok terminal sessions and navigate to their terminals", async ({
  page,
}) => {
  test.setTimeout(120_000);
  await page.emulateMedia({ colorScheme: "dark" });
  const terminalHarness = await TerminalE2EHarness.create({
    tempPrefix: "sidebar-terminal-agents-",
  });
  const original = await terminalHarness.client.getDaemonConfig();
  try {
    await terminalHarness.client.patchDaemonConfig({
      metadataGeneration: { providers: [{ provider: "mock", model: "e2e-fast-stream" }] },
    });
    const pi = await createControlledActivityTerminal(terminalHarness, "pi");
    const grok = await createControlledActivityTerminal(terminalHarness, "grok");
    await terminalHarness.openTerminal(page, { terminalId: grok.terminal.id });
    await pi.report("running");
    await grok.report("running");
    const entries = (await terminalHarness.client.fetchAgents({ scope: "active" })).entries;
    const piAgent = entries.find(
      (entry) =>
        entry.agent.provider === "pi" && entry.agent.workspaceId === terminalHarness.workspaceId,
    )?.agent;
    const grokAgent = entries.find(
      (entry) =>
        entry.agent.provider === "grok" && entry.agent.workspaceId === terminalHarness.workspaceId,
    )?.agent;
    if (!piAgent || !grokAgent) throw new Error("Expected both terminal agents in the directory");
    await expect(page.getByTestId(`sidebar-agent-status-${piAgent.id}-running`)).toBeVisible();
    await expect(page.getByTestId(`sidebar-agent-status-${grokAgent.id}-running`)).toBeVisible();
    await page.getByTestId(`sidebar-agent-${piAgent.id}`).click();
    await expect(terminalTab(page, pi.terminal.id)).toHaveAttribute("aria-selected", "true");
    await page.getByTestId(`sidebar-agent-${grokAgent.id}`).click();
    await pi.report("idle");
    await expect(page.getByTestId(`sidebar-agent-${piAgent.id}`)).toBeVisible();
    await expect(page.getByTestId(`sidebar-agent-status-${piAgent.id}-running`)).toHaveCount(0);
    await expect(page.getByTestId(`sidebar-agent-status-${grokAgent.id}-running`)).toBeVisible();
    await page.getByTestId(`sidebar-agent-${grokAgent.id}`).click();
    await expect(terminalTab(page, grok.terminal.id)).toHaveAttribute("aria-selected", "true");
    const row = page.getByTestId(
      `sidebar-workspace-row-${getServerId()}:${terminalHarness.workspaceId}`,
    );
    await row.click({ button: "right" });
    await page
      .getByTestId(
        `sidebar-workspace-menu-regenerate-title-${getServerId()}:${terminalHarness.workspaceId}`,
      )
      .click();
    await expect(page.getByTestId("regenerate-agent-title-dialog")).toHaveCount(0, {
      timeout: 30_000,
    });
    await expect(row).toContainText("Investigate grok status");
    await page.mouse.move(1000, 200);
    await expect(page.getByTestId("workspace-hover-card")).toHaveCount(0);
    await page.screenshot({ path: "/tmp/paseo-sidebar-terminal-agents.png" });
  } finally {
    await terminalHarness.client.patchDaemonConfig({
      metadataGeneration: original.config.metadataGeneration,
    });
    await terminalHarness.cleanup();
  }
});
