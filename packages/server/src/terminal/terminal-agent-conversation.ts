import { open, readdir } from "node:fs/promises";
import { homedir } from "node:os";
import path from "node:path";

const READ_BYTES = 256 * 1024;
const CONTEXT_CHARS = 2000;

interface ConversationInput {
  provider: string;
  cwd: string;
  sessionId?: string;
  transcriptPath?: string;
  homeDir?: string;
  env?: NodeJS.ProcessEnv;
}

export interface TerminalConversation {
  title?: string;
  context: string;
}

function object(value: unknown): Record<string, unknown> | null {
  return value && typeof value === "object" && !Array.isArray(value)
    ? (value as Record<string, unknown>)
    : null;
}

async function readBounded(file: string, tail = true): Promise<string> {
  const handle = await open(file, "r");
  try {
    const { size } = await handle.stat();
    const start = tail ? Math.max(0, size - READ_BYTES) : 0;
    const buffer = Buffer.alloc(Math.min(size, READ_BYTES));
    const { bytesRead } = await handle.read(buffer, 0, buffer.length, start);
    const text = buffer.subarray(0, bytesRead).toString("utf8");
    return start ? text.slice(text.indexOf("\n") + 1) : text;
  } finally {
    await handle.close();
  }
}

function textContent(value: unknown): string {
  if (typeof value === "string") return value;
  if (!Array.isArray(value)) return "";
  return value
    .flatMap((part) => {
      const item = object(part);
      return item &&
        ["text", "input_text", "output_text"].includes(String(item.type)) &&
        typeof item.text === "string"
        ? [item.text]
        : [];
    })
    .join("\n");
}

function parseConversation(text: string): TerminalConversation {
  const messages: string[] = [];
  let title: string | undefined;
  for (const line of text.split("\n")) {
    let entry: Record<string, unknown> | null;
    try {
      entry = object(JSON.parse(line));
    } catch {
      continue;
    }
    if (!entry) continue;
    if (entry.type === "session_info" && typeof entry.name === "string")
      title = entry.name.trim().slice(0, 80) || undefined;
    const message = object(entry.message) ?? object(entry.payload) ?? entry;
    const role = message.role ?? message.type;
    if (role !== "user" && role !== "assistant") continue;
    if (message.synthetic_reason) continue;
    const content = textContent(message.content).trim();
    if (content) messages.push(`${role}: ${content.slice(-300)}`);
  }
  return { title, context: messages.slice(-6).join("\n").slice(-CONTEXT_CHARS) };
}

async function findPiTranscript(input: ConversationInput, home: string): Promise<string | null> {
  const env = input.env ?? process.env;
  const root =
    env.PI_CODING_AGENT_SESSION_DIR ??
    path.join(env.PI_CODING_AGENT_DIR ?? path.join(home, ".pi", "agent"), "sessions");
  const directory = path.join(
    root,
    `--${input.cwd.replace(/^[/\\]|[/\\]$/g, "").replace(/[/\\:]/g, "-")}--`,
  );
  const files = await readdir(directory);
  const file = files.find((name) => name.endsWith(`_${input.sessionId}.jsonl`));
  return file ? path.join(directory, file) : null;
}

async function resolveTranscript(input: ConversationInput, home: string): Promise<string | null> {
  if (input.transcriptPath && path.isAbsolute(input.transcriptPath)) return input.transcriptPath;
  if (!input.sessionId || !/^[a-zA-Z0-9-]+$/.test(input.sessionId)) return null;
  if (input.provider === "pi") return findPiTranscript(input, home);
  if (input.provider === "grok") {
    const root = (input.env ?? process.env).GROK_HOME ?? path.join(home, ".grok");
    return path.join(
      root,
      "sessions",
      encodeURIComponent(input.cwd),
      input.sessionId,
      "chat_history.jsonl",
    );
  }
  if (input.provider !== "codex") return null;
  const timestamp = Number.parseInt(input.sessionId.replaceAll("-", "").slice(0, 12), 16);
  const date = new Date(timestamp);
  if (!Number.isFinite(date.getTime())) return null;
  const root = (input.env ?? process.env).CODEX_HOME ?? path.join(home, ".codex");
  const directory = path.join(root, "sessions", ...date.toISOString().slice(0, 10).split("-"));
  const file = (await readdir(directory)).find((name) => name.endsWith(`${input.sessionId}.jsonl`));
  return file ? path.join(directory, file) : null;
}

/** Read native conversation data locally; never opens a provider runtime or calls a model. */
export async function readTerminalConversation(
  input: ConversationInput,
): Promise<TerminalConversation> {
  try {
    const transcript = await resolveTranscript(input, input.homeDir ?? homedir());
    if (!transcript) return { context: "" };
    const result = parseConversation(await readBounded(transcript));
    if (input.provider === "grok") {
      try {
        const summary = object(
          JSON.parse(await readBounded(path.join(path.dirname(transcript), "summary.json"), false)),
        );
        if (typeof summary?.generated_title === "string" && summary.generated_title.trim())
          result.title = summary.generated_title.trim().slice(0, 80);
      } catch {
        /* A new conversation can have history before its summary is written. */
      }
    }
    return result;
  } catch {
    return { context: "" };
  }
}
