import { afterEach, expect, it, vi } from "vitest";
import pino from "pino";
import { AgentStorage } from "./agent/agent-storage.js";
import { AgentDirectory } from "./agent/directory.js";
import { buildStoredAgentPayload } from "./agent/agent-projections.js";
import { isStoredAgentProviderAvailable } from "./persistence-hooks.js";
import { TerminalAgents } from "../terminal/terminal-agents.js";
import type express from "express";
import { existsSync, mkdtempSync, readFileSync, rmSync } from "node:fs";
import { join } from "node:path";
import { tmpdir } from "node:os";
import { createTerminalManager, type TerminalManager } from "../terminal/terminal-manager.js";
import { createTerminalActivityRouteHandler } from "./bootstrap.js";

interface MockResponse {
  statusCode: number;
  body: unknown;
  ended: boolean;
  status(code: number): MockResponse;
  json(body: unknown): MockResponse;
  end(): MockResponse;
}

async function waitForCondition(
  predicate: () => boolean,
  timeoutMs: number,
  intervalMs = 25,
): Promise<void> {
  const start = Date.now();
  while (Date.now() - start < timeoutMs) {
    if (predicate()) {
      return;
    }
    await new Promise((resolve) => setTimeout(resolve, intervalMs));
  }
  throw new Error(`Timed out after ${timeoutMs}ms waiting for condition`);
}

function createMockResponse(): MockResponse {
  return {
    statusCode: 200,
    body: undefined,
    ended: false,
    status(code: number): MockResponse {
      this.statusCode = code;
      return this;
    },
    json(body: unknown): MockResponse {
      this.body = body;
      this.ended = true;
      return this;
    },
    end(): MockResponse {
      this.ended = true;
      return this;
    },
  };
}

function createMockRequest(input: { body: unknown; remoteAddress?: string }): express.Request {
  return {
    body: input.body,
    socket: {
      remoteAddress: input.remoteAddress ?? "127.0.0.1",
    },
  } as express.Request;
}

let manager: TerminalManager | null = null;
const temporaryDirs: string[] = [];
let terminalAgents: TerminalAgents | undefined;

afterEach(async () => {
  await terminalAgents?.close();
  terminalAgents = undefined;
  if (manager) {
    const terminalsByCwd = await Promise.all(
      manager.listDirectories().map((cwd) => manager!.getTerminals(cwd)),
    );
    for (const terminal of terminalsByCwd.flat()) {
      await manager.killTerminalAndWait(terminal.id);
    }
    manager.killAll();
    manager = null;
  }
  while (temporaryDirs.length > 0) {
    const dir = temporaryDirs.pop();
    if (dir) {
      rmSync(dir, { recursive: true, force: true });
    }
  }
});

async function createTerminalAgentHarness() {
  const cwd = mkdtempSync(join(tmpdir(), "terminal-agents-"));
  temporaryDirs.push(cwd);
  const logger = pino({ level: "silent" });
  manager = createTerminalManager();
  const terminal = await manager.createTerminal({
    cwd,
    workspaceId: "workspace-1",
    name: "Grok Build",
    command: process.execPath,
    args: ["-e", "setInterval(() => {}, 1000)"],
  });
  const storage = new AgentStorage(join(cwd, "agents"), logger);
  const notifyStoredAgentState = vi.fn();
  const makeService = () =>
    new TerminalAgents({
      terminals: manager!,
      storage,
      agents: { notifyStoredAgentState },
      logger,
      profiles: () => [{ id: "grok-profile", name: "Grok Build", command: "grok", icon: "bot" }],
    });
  terminalAgents = makeService();
  await terminalAgents.initialize();
  let observedAt = 1000;
  const report = (
    event: string,
    state: "running" | "idle" | "needs-input",
    sessionId = "conversation-1",
  ) =>
    terminalAgents!.report(terminal.id, {
      provider: "grok",
      event,
      state,
      sessionId,
      observedAt: ++observedAt,
      cwd,
    });
  const directory = new AgentDirectory({
    manager: { listAgents: () => [] },
    storage,
    workspaces: { get: async () => null, list: async () => [] },
    projectLive: async () => {
      throw new Error("must not create an SDK runtime");
    },
    projectStored: (record) => buildStoredAgentPayload(record, []),
    isProviderVisible: () => true,
    isStoredProviderAvailable: (record) => isStoredAgentProviderAvailable(record, []),
  });
  return { terminal, storage, report, directory, makeService, notifyStoredAgentState };
}

it("promotes hook sessions into the agent directory with workspace, profile and attention", async () => {
  const h = await createTerminalAgentHarness();
  await h.report("SessionStart", "idle");
  const [first] = await h.directory.list();
  expect(first).toMatchObject({
    provider: "grok",
    workspaceId: "workspace-1",
    status: "idle",
    title: "Grok Build",
    execution: {
      kind: "terminal",
      terminalId: h.terminal.id,
      providerSessionId: "conversation-1",
      profileId: "grok-profile",
      icon: "bot",
    },
  });
  expect(first.providerUnavailable).toBeUndefined();
  await h.report("UserPromptSubmit", "running");
  await h.report("PreToolUse", "needs-input");
  expect((await h.directory.list())[0]).toMatchObject({
    id: first.id,
    status: "idle",
    requiresAttention: true,
    attentionReason: "permission",
  });
  await h.report("PostToolUse", "running");
  await h.report("Stop", "idle");
  expect((await h.directory.list())[0]).toMatchObject({
    id: first.id,
    requiresAttention: true,
    attentionReason: "finished",
  });
  await manager!.clearTerminalAttention(h.terminal.id);
  await vi.waitFor(async () => expect((await h.directory.list())[0].requiresAttention).toBe(false));
  expect(h.notifyStoredAgentState).toHaveBeenCalled();
  expect(await h.storage.list()).toHaveLength(1);
});

it("rejects stale sessions and retires terminal agents on SessionEnd and terminal exit", async () => {
  const h = await createTerminalAgentHarness();
  await h.report("UserPromptSubmit", "running");
  const [old] = await h.directory.list();
  await h.report("SessionStart", "idle", "conversation-2");
  await h.report("UserPromptSubmit", "running", "conversation-2");
  await h.report("Stop", "idle", "conversation-1");
  const [current] = await h.directory.list();
  expect(current.id).not.toBe(old.id);
  expect(current.status).toBe("running");
  expect((await h.storage.get(old.id))?.archivedAt).toBeTruthy();
  await terminalAgents!.report(h.terminal.id, {
    provider: "grok",
    event: "Stop",
    state: "idle",
    sessionId: "conversation-2",
    observedAt: 1,
  });
  expect((await h.directory.list())[0].status).toBe("running");
  await h.report("SessionEnd", "idle", "conversation-2");
  expect(await h.directory.list()).toEqual([]);
  await h.report("Stop", "idle", "conversation-2");
  expect(await h.directory.list()).toEqual([]);
  await h.report("SessionStart", "idle", "conversation-3");
  await manager!.killTerminalAndWait(h.terminal.id);
  await vi.waitFor(async () => expect(await h.directory.list()).toEqual([]));
});

it("restores a terminal agent from its surviving worker and closes it when the worker is gone", async () => {
  const h = await createTerminalAgentHarness();
  await h.report("UserPromptSubmit", "running");
  const [first] = await h.directory.list();
  await terminalAgents!.close();
  terminalAgents = h.makeService();
  await terminalAgents.initialize();
  expect((await h.directory.list())[0]).toMatchObject({ id: first.id, status: "running" });
  await terminalAgents.close();
  await manager!.killTerminalAndWait(h.terminal.id);
  terminalAgents = h.makeService();
  await terminalAgents.initialize();
  expect(await h.directory.list()).toEqual([]);
  expect((await h.storage.get(first.id))?.lastStatus).toBe("closed");
});

it("accepts terminalId and token reports through the route into the tracker", async () => {
  const cwd = mkdtempSync(join(tmpdir(), "terminal-activity-route-"));
  temporaryDirs.push(cwd);
  const envPath = join(cwd, "activity-env.json");
  manager = createTerminalManager({
    getTerminalActivityUrl: () => "http://127.0.0.1:6767/api/terminal-activity",
  });

  const session = await manager.createTerminal({
    cwd,
    command: process.execPath,
    args: [
      "-e",
      `require("node:fs").writeFileSync(${JSON.stringify(envPath)}, JSON.stringify({ terminalId: process.env.PASEO_TERMINAL_ID, token: process.env.PASEO_ACTIVITY_TOKEN, url: process.env.PASEO_TERMINAL_ACTIVITY_URL })); setInterval(() => {}, 1000);`,
    ],
  });
  await waitForCondition(() => existsSync(envPath), 10000);
  const env = JSON.parse(readFileSync(envPath, "utf8")) as {
    terminalId: string;
    token: string;
    url: string;
  };
  const response = createMockResponse();
  const handler = createTerminalActivityRouteHandler(manager);

  await handler(
    createMockRequest({ body: { terminalId: env.terminalId, token: env.token, state: "running" } }),
    response as unknown as express.Response,
    () => undefined,
  );

  expect(env.terminalId).toBe(session.id);
  expect(env.url).toBe("http://127.0.0.1:6767/api/terminal-activity");
  expect(response.statusCode).toBe(204);
  expect(session.getActivity()?.state).toBe("working");

  const storage = new AgentStorage(join(cwd, "agents"), pino({ level: "silent" }));
  terminalAgents = new TerminalAgents({
    terminals: manager,
    storage,
    agents: { notifyStoredAgentState: vi.fn() },
    profiles: () => [],
    logger: pino({ level: "silent" }),
  });
  await terminalAgents.initialize();
  const promotedResponse = createMockResponse();
  const promotedHandler = createTerminalActivityRouteHandler(manager, (id, report) =>
    terminalAgents!.report(id, report),
  );
  await promotedHandler(
    createMockRequest({
      body: {
        terminalId: env.terminalId,
        token: env.token,
        provider: "grok",
        event: "UserPromptSubmit",
        state: "running",
        sessionId: "route-session",
        observedAt: Date.now(),
      },
    }),
    promotedResponse as unknown as express.Response,
    () => undefined,
  );
  expect(promotedResponse.statusCode).toBe(204);
  expect(await storage.list()).toEqual([
    expect.objectContaining({
      provider: "grok",
      lastStatus: "running",
      execution: { kind: "terminal", terminalId: session.id, providerSessionId: "route-session" },
    }),
  ]);
});

it("rejects non-loopback activity reports before token handling", async () => {
  manager = createTerminalManager();
  const response = createMockResponse();
  const handler = createTerminalActivityRouteHandler(manager);

  await handler(
    createMockRequest({
      body: { terminalId: "terminal-1", token: "token", state: "running" },
      remoteAddress: "192.168.1.5",
    }),
    response as unknown as express.Response,
    () => undefined,
  );

  expect(response.statusCode).toBe(403);
});

it("uses one rejection for unknown terminals and wrong tokens", async () => {
  const cwd = mkdtempSync(join(tmpdir(), "terminal-activity-route-"));
  temporaryDirs.push(cwd);
  manager = createTerminalManager();
  const session = await manager.createTerminal({ cwd });
  const handler = createTerminalActivityRouteHandler(manager);
  const unknownResponse = createMockResponse();
  const invalidResponse = createMockResponse();

  await handler(
    createMockRequest({ body: { terminalId: "unknown", token: "bad", state: "running" } }),
    unknownResponse as unknown as express.Response,
    () => undefined,
  );
  await handler(
    createMockRequest({ body: { terminalId: session.id, token: "bad", state: "running" } }),
    invalidResponse as unknown as express.Response,
    () => undefined,
  );

  expect(unknownResponse.statusCode).toBe(403);
  expect(invalidResponse.statusCode).toBe(403);
  expect(unknownResponse.body).toEqual(invalidResponse.body);
});
