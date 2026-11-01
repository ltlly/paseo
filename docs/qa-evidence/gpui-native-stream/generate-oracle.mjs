import fs from "node:fs/promises";
import { fileURLToPath } from "node:url";
import { createHash } from "node:crypto";
import { InMemoryAgentTimelineStore } from "../../../packages/server/src/server/agent/agent-timeline-store.ts";
const root = fileURLToPath(new URL("../../../", import.meta.url)).replace(/[\\/]$/, "");
function page(result) {
  return {
    requestId: "oracle",
    agentId: "a",
    epoch: result.epoch,
    reset: result.reset,
    direction: result.direction,
    window: result.window,
    entries: result.rows.map(({ seq: _seq, ...entry }) => entry),
    startCursor: result.startSeq === null ? null : { epoch: result.epoch, seq: result.startSeq },
    endCursor: result.endSeq === null ? null : { epoch: result.epoch, seq: result.endSeq },
    hasOlder: result.hasOlder,
    hasNewer: result.hasNewer,
    error: null,
  };
}
const tool = (status, detail = { type: "shell", command: "echo fixture" }, metadata = {}) => ({
  type: "tool_call",
  callId: "reused",
  name: "shell",
  status,
  detail,
  metadata,
});
const items = [
  ["t", tool("running", undefined, { first: 1 })],
  ["t", { type: "assistant_message", messageId: "m1", text: "A" }],
  ["t", { type: "reasoning", text: "r1" }],
  ["t", { type: "reasoning", text: "r2" }],
  ["t", tool("running", { type: "unknown", input: null, output: "partial" }, { second: 2 })],
  ["t", { type: "assistant_message", messageId: "m1", text: "B" }],
  ["t", { type: "assistant_message", text: "C" }],
  ["t", { type: "assistant_message", messageId: "m2", text: "D" }],
  [
    "t",
    { type: "plugin", pluginId: "p", id: "same", kind: "fixture", version: 1, data: { value: 1 } },
  ],
  ["t", { type: "assistant_message", text: "E" }],
  [
    "t",
    { type: "plugin", pluginId: "p", id: "same", kind: "fixture", version: 1, data: { value: 2 } },
  ],
  ["t", { type: "assistant_message", text: "F" }],
  ["t", { type: "user_message", clientMessageId: "client", text: "one" }],
  ["t", { type: "user_message", clientMessageId: "client", text: "two" }],
  ["new", tool("running")],
  ["t", tool("failed", undefined, { late: 1 })],
  ["t", tool("completed", { type: "unknown", input: null, output: "finished" }, { late: 2 })],
  ["new", tool("canceled")],
  ["new", { type: "reasoning", text: "new turn" }],
  ["other", { type: "reasoning", text: "other turn" }],
];
const store = new InMemoryAgentTimelineStore();
store.initialize("a", { epoch: "e" });
const initial = page(store.fetch("a", { limit: 0 }));
const snapshots = [];
for (const [turnId, item] of items) {
  const row = store.append("a", item, { turnId, timestamp: "time-" + (snapshots.length + 1) });
  snapshots.push({
    payload: {
      agentId: "a",
      epoch: "e",
      seq: row.seq,
      timestamp: row.timestamp,
      event: { type: "timeline", provider: "codex", turnId, item },
    },
    entries: page(store.fetch("a", { limit: 0 })).entries,
  });
}
const future = new InMemoryAgentTimelineStore();
future.initialize("a", { epoch: "e" });
let before;
for (let i = 1; i <= 12; i++) {
  future.append(
    "a",
    i === 1 || i === 5 || i === 12
      ? tool(i === 12 ? "completed" : "running")
      : { type: "assistant_message", messageId: "m" + i, text: "text-" + i },
    { turnId: "t", timestamp: "time-" + i },
  );
  if (i === 4) before = page(future.fetch("a", { limit: 0 }));
}
const partial = page(
  future.fetch("a", { direction: "after", cursor: { epoch: "e", seq: 4 }, limit: 1 }),
);
const rest = page(
  future.fetch("a", { direction: "after", cursor: { epoch: "e", seq: 5 }, limit: 0 }),
);
const final = page(future.fetch("a", { limit: 0 }));
const source = await fs.readFile(root + "/packages/server/src/server/agent/timeline-projection.ts");
await fs.mkdir(root + "/packages/desktop/native/src/testdata", { recursive: true });
await fs.writeFile(
  root + "/packages/desktop/native/src/testdata/timeline-live.json",
  JSON.stringify(
    {
      generatedFrom: "packages/server/src/server/agent/timeline-projection.ts",
      sourceSha256: createHash("sha256").update(source).digest("hex"),
      initial,
      snapshots,
      future: { before, partial, rest, final },
    },
    null,
    2,
  ) + "\n",
);
console.log(
  "Captured real timeline store projection after",
  snapshots.length,
  "live events; partial cursor",
  partial.endCursor?.seq,
  "window",
  partial.window.maxSeq,
);
