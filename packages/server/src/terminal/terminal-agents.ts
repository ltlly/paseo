import { randomUUID } from "node:crypto";
import { isAbsolute } from "node:path";
import type { Logger } from "pino";
import type { TerminalProfile } from "@getpaseo/protocol/terminal-profile";
import type { AgentStorage, StoredAgentRecord } from "../server/agent/agent-storage.js";
import type { AgentManager } from "../server/agent/agent-manager.js";
import type { TerminalManager } from "./terminal-manager.js";
import type { TerminalSession } from "./terminal.js";
import { AGENT_HOOK_PROVIDERS, type AgentHookReport } from "./agent-hooks/provider-registry.js";
import { resolveCreateAgentTitles } from "../server/agent/create-agent-title.js";
import { readTerminalConversation } from "./terminal-agent-conversation.js";

const GENERATED_TITLE_LABEL = "paseo.terminal-generated-title";

interface Dependencies {
  terminals: TerminalManager;
  storage: AgentStorage;
  agents: Pick<AgentManager, "notifyStoredAgentState">;
  profiles(): readonly TerminalProfile[];
  logger: Logger;
  generateTitle?: (input: {
    cwd: string;
    prompt: string;
    provider: string;
  }) => Promise<string | null>;
}

/** Durable directory records for externally driven agents. Never creates an SDK runtime. */
export class TerminalAgents {
  private readonly active = new Map<string, string>();
  private readonly tails = new Map<string, Promise<unknown>>();
  private readonly watches = new Map<string, () => void>();
  private readonly retiredSessions = new Set<string>();
  private unsubscribe: (() => void) | undefined;
  private closed = false;

  constructor(private readonly deps: Dependencies) {}

  async initialize(): Promise<void> {
    this.closed = false;
    for (const record of await this.deps.storage.list()) {
      if (!record.execution) continue;
      if (record.archivedAt) {
        this.retire(record);
        continue;
      }
      const terminal = this.deps.terminals.getTerminal(record.execution.terminalId);
      if (!terminal || terminal.getExitInfo()) {
        await this.finish(record);
        continue;
      }
      this.active.set(terminal.id, record.id);
      this.watch(terminal);
      // The surviving worker owns live activity. A saved 'running' bit is not evidence.
      await this.synchronize(record, terminal);
      this.refreshConversation(record);
    }
    this.unsubscribe = this.deps.terminals.subscribeTerminalsChanged(({ cwd }) => {
      for (const [terminalId, agentId] of this.active) {
        this.background(terminalId, async () => {
          const record = await this.deps.storage.get(agentId);
          if (!record || record.archivedAt) return;
          const terminal = this.deps.terminals.getTerminal(terminalId);
          if (!terminal || terminal.getExitInfo()) await this.finish(record);
          else if (terminal.cwd === cwd) await this.synchronize(record, terminal);
        });
      }
    });
  }

  report(terminalId: string, report: AgentHookReport): Promise<boolean> {
    return this.enqueue(terminalId, async () => {
      const terminal = this.deps.terminals.getTerminal(terminalId);
      if (
        !terminal ||
        terminal.getExitInfo() ||
        !Object.hasOwn(AGENT_HOOK_PROVIDERS, report.provider)
      )
        return false;
      await this.applyReport(terminal, report);
      return true;
    });
  }

  private async applyReport(terminal: TerminalSession, report: AgentHookReport): Promise<void> {
    const currentId = this.active.get(terminal.id);
    let record = currentId ? await this.deps.storage.get(currentId) : null;
    if (record && report.observedAt < (record.lastHookAt ?? 0)) return;
    const startsSession = report.event === "SessionStart" || report.event === "UserPromptSubmit";
    const key = this.sessionKey(terminal.id, report.provider, report.sessionId);
    if (key && this.retiredSessions.has(key) && !startsSession) return;
    if (record && this.isReplacement(record, report)) {
      // A late Stop from an earlier CLI must not take over the current pane.
      if (!startsSession) return;
      await this.finish(record);
      record = null;
    }
    if (record?.archivedAt) {
      if (!startsSession) return;
      record = null;
    }
    if (report.event === "SessionEnd") {
      if (record) await this.finish({ ...record, lastHookAt: report.observedAt });
      return;
    }
    record ??= this.createRecord(terminal, report.provider);
    if (key) this.retiredSessions.delete(key);
    const firstPrompt = firstConversationPrompt(record, report);
    record = this.withReportMetadata(record, report);
    const activityStates = {
      running: "working",
      "needs-input": "attention",
      idle: "idle",
    } as const;
    await this.deps.terminals.setTerminalActivity(terminal.id, activityStates[report.state]);
    await this.synchronize(record, terminal);
    this.scheduleTitle(record, firstPrompt, report.title);
    if (["SessionStart", "Stop", "SessionInfo"].includes(report.event))
      this.refreshConversation(record);
  }

  private isReplacement(record: StoredAgentRecord, report: AgentHookReport): boolean {
    const sessionId = record.execution?.providerSessionId;
    return (
      record.provider !== report.provider ||
      Boolean(sessionId && report.sessionId && sessionId !== report.sessionId)
    );
  }

  private createRecord(terminal: TerminalSession, provider: string): StoredAgentRecord {
    const profile = this.deps.profiles().find((item) => item.name === terminal.name);
    const now = new Date().toISOString();
    const record: StoredAgentRecord = {
      id: randomUUID(),
      provider,
      cwd: terminal.cwd,
      workspaceId: terminal.workspaceId,
      createdAt: now,
      updatedAt: now,
      title: profile?.name ?? provider,
      labels: { [GENERATED_TITLE_LABEL]: profile?.name ?? provider },
      lastStatus: "idle",
      execution: {
        kind: "terminal",
        terminalId: terminal.id,
        ...(profile ? { profileId: profile.id, icon: profile.icon } : {}),
      },
    };
    this.active.set(terminal.id, record.id);
    this.watch(terminal);
    return record;
  }

  private withReportMetadata(
    record: StoredAgentRecord,
    report: AgentHookReport,
  ): StoredAgentRecord {
    const firstPrompt = report.event === "UserPromptSubmit" && !record.lastUserMessageAt;
    const provisionalTitle =
      firstPrompt && report.prompt
        ? resolveCreateAgentTitles({ initialPrompt: report.prompt }).provisionalTitle
        : null;
    const automaticTitle = report.title || provisionalTitle;
    const canRename = this.canUpdateAutomaticTitle(record);
    const labels = {
      ...record.labels,
      ...(report.prompt ? { "paseo.terminal-last-prompt": report.prompt.slice(-2000) } : {}),
      ...(report.transcriptPath && isAbsolute(report.transcriptPath)
        ? { "paseo.terminal-transcript": report.transcriptPath }
        : {}),
    };
    return {
      ...record,
      labels,
      ...(automaticTitle && canRename
        ? {
            title: automaticTitle,
            labels: {
              ...labels,
              [GENERATED_TITLE_LABEL]: automaticTitle,
            },
          }
        : {}),
      cwd: report.cwd && isAbsolute(report.cwd) ? report.cwd : record.cwd,
      execution: {
        ...record.execution!,
        ...(report.sessionId ? { providerSessionId: report.sessionId } : {}),
      },
      lastHookAt: report.observedAt,
      ...(report.event === "UserPromptSubmit"
        ? { lastUserMessageAt: new Date(report.observedAt).toISOString() }
        : {}),
    };
  }

  private canUpdateAutomaticTitle(record: StoredAgentRecord): boolean {
    if (record.title === record.labels[GENERATED_TITLE_LABEL]) return true;
    if (record.labels[GENERATED_TITLE_LABEL]) return false;
    return (
      record.title === record.provider ||
      this.deps
        .profiles()
        .some(
          (profile) => profile.id === record.execution?.profileId && profile.name === record.title,
        )
    );
  }

  private refreshConversation(record: StoredAgentRecord): void {
    this.background(record.execution!.terminalId, async () => {
      if (this.closed) return;
      const conversation = await readTerminalConversation({
        provider: record.provider,
        cwd: record.cwd,
        sessionId: record.execution?.providerSessionId,
        transcriptPath: record.labels["paseo.terminal-transcript"],
      });
      const current = await this.deps.storage.get(record.id);
      if (this.closed || !current || current.archivedAt) return;
      const title =
        conversation.title && this.canUpdateAutomaticTitle(current)
          ? conversation.title
          : undefined;
      if (!title && !conversation.context) return;
      await this.publish({
        ...current,
        ...(title ? { title } : {}),
        labels: {
          ...current.labels,
          ...(title ? { [GENERATED_TITLE_LABEL]: title } : {}),
          ...(conversation.context ? { "paseo.terminal-last-prompt": conversation.context } : {}),
        },
      });
    });
  }

  private scheduleTitle(
    record: StoredAgentRecord,
    prompt: string | null,
    nativeTitle?: string,
  ): void {
    if (!prompt || nativeTitle || !this.deps.generateTitle) return;
    void this.deps
      .generateTitle({ cwd: record.cwd, prompt, provider: record.provider })
      .then((title) => {
        if (!title || this.closed) return;
        this.background(record.execution!.terminalId, async () => {
          if (this.closed) return;
          const current = await this.deps.storage.get(record.id);
          if (!current || current.archivedAt || current.title !== record.title) return;
          await this.publish({
            ...current,
            title,
            labels: { ...current.labels, [GENERATED_TITLE_LABEL]: title },
          });
        });
        return undefined;
      })
      .catch((err) =>
        this.deps.logger.warn({ err, agentId: record.id }, "Failed to name terminal agent"),
      );
  }

  private watch(terminal: TerminalSession): void {
    if (this.watches.has(terminal.id)) return;
    const finish = () =>
      this.background(terminal.id, async () => {
        const id = this.active.get(terminal.id);
        const record = id ? await this.deps.storage.get(id) : null;
        if (record && !record.archivedAt) await this.finish(record);
      });
    const offExit = terminal.onExit(finish);
    const offCommand = terminal.onCommandFinished(finish);
    this.watches.set(terminal.id, () => {
      offExit();
      offCommand();
    });
  }

  private async synchronize(record: StoredAgentRecord, terminal: TerminalSession): Promise<void> {
    const activity = terminal.getActivity();
    let attentionReason: StoredAgentRecord["attentionReason"] = null;
    if (activity?.attentionReason === "needs_input") attentionReason = "permission";
    if (activity?.attentionReason === "finished") attentionReason = "finished";
    const next: StoredAgentRecord = {
      ...record,
      lastStatus: activity?.state === "working" ? "running" : "idle",
      requiresAttention: attentionReason !== null,
      attentionReason,
      attentionTimestamp:
        attentionReason && activity ? new Date(activity.changedAt).toISOString() : null,
      resumeOnStartup: false,
    };
    const previous = await this.deps.storage.get(record.id);
    if (previous && JSON.stringify(previous) === JSON.stringify(next)) return;
    await this.publish(next);
  }

  private async finish(record: StoredAgentRecord): Promise<void> {
    const terminalId = record.execution!.terminalId;
    await this.publish({
      ...record,
      lastStatus: "closed",
      archivedAt: new Date().toISOString(),
      requiresAttention: false,
      attentionReason: null,
      attentionTimestamp: null,
      resumeOnStartup: false,
    });
    this.retire(record);
    if (this.active.get(terminalId) === record.id) this.active.delete(terminalId);
    this.watches.get(terminalId)?.();
    this.watches.delete(terminalId);
    // A SessionEnd is not a completed turn; leave the shell without unread attention.
    if (this.deps.terminals.getTerminal(terminalId)?.getActivity()?.state === "working") {
      await this.deps.terminals.setTerminalActivity(terminalId, "idle");
    }
    await this.deps.terminals.clearTerminalAttention(terminalId);
  }

  private retire(record: StoredAgentRecord): void {
    const execution = record.execution!;
    const key = this.sessionKey(execution.terminalId, record.provider, execution.providerSessionId);
    if (key) this.retiredSessions.add(key);
  }

  private sessionKey(terminalId: string, provider: string, sessionId?: string): string | null {
    return sessionId ? JSON.stringify([terminalId, provider, sessionId]) : null;
  }

  private async publish(record: StoredAgentRecord): Promise<void> {
    const next = {
      ...record,
      updatedAt: new Date(Math.max(Date.now(), Date.parse(record.updatedAt) + 1)).toISOString(),
    };
    await this.deps.storage.upsert(next);
    this.deps.agents.notifyStoredAgentState(next);
  }

  private enqueue<T>(terminalId: string, run: () => Promise<T>): Promise<T> {
    const next = (this.tails.get(terminalId) ?? Promise.resolve()).catch(() => undefined).then(run);
    this.tails.set(terminalId, next);
    void next
      .finally(() => {
        if (this.tails.get(terminalId) === next) this.tails.delete(terminalId);
      })
      .catch(() => undefined);
    return next;
  }

  private background(terminalId: string, run: () => Promise<void>): void {
    void this.enqueue(terminalId, run).catch((err) =>
      this.deps.logger.error({ err, terminalId }, "Failed to synchronize terminal agent"),
    );
  }

  async close(): Promise<void> {
    this.closed = true;
    this.unsubscribe?.();
    for (const off of this.watches.values()) off();
    this.watches.clear();
    await Promise.allSettled(this.tails.values());
  }
}

function firstConversationPrompt(
  record: StoredAgentRecord,
  report: AgentHookReport,
): string | null {
  if (report.event !== "UserPromptSubmit" || record.lastUserMessageAt) return null;
  const prompt = report.prompt?.trim();
  return prompt && !prompt.startsWith("/") ? prompt : null;
}
