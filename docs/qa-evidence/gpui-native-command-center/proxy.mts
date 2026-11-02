// Test-only delay/failure injection in front of the isolated real daemon.
import fs from "node:fs/promises";
import { createServer } from "node:http";
import { WebSocket, WebSocketServer } from "ws";
const q = process.env.PASEO_NATIVE_QA!;
if (!q) throw new Error("Set PASEO_NATIVE_QA");
const original = JSON.parse(await fs.readFile(q + "/fixture.json", "utf8"));
await fs.writeFile(q + "/backend-fixture.json", JSON.stringify(original, null, 2));
const server = createServer(async (req, res) => {
  try {
    const result = await fetch(`http://127.0.0.1:${original.port}${req.url}`);
    res.writeHead(result.status, Object.fromEntries(result.headers));
    res.end(Buffer.from(await result.arrayBuffer()));
  } catch {
    res.statusCode = 502;
    res.end();
  }
});
const wss = new WebSocketServer({ server });
wss.on("connection", (client) => {
  const upstream = new WebSocket(`ws://127.0.0.1:${original.port}/ws`);
  const buffered: Array<[Buffer, boolean]> = [];
  const queries = new Map<string, { query: string; delay: number }>();
  const timers = new Set<ReturnType<typeof setTimeout>>();
  const send = (data: Buffer, binary: boolean, delay = 0) => {
    const deliver = () => {
      if (client.readyState === WebSocket.OPEN) client.send(data, { binary });
    };
    if (delay) {
      const timer = setTimeout(() => {
        timers.delete(timer);
        deliver();
      }, delay);
      timers.add(timer);
    } else deliver();
  };
  upstream.on("open", () => {
    for (const [data, binary] of buffered) upstream.send(data, { binary });
    buffered.length = 0;
  });
  client.on("message", async (raw, binary) => {
    const data = Buffer.from(raw as Buffer);
    let message;
    try {
      message = JSON.parse(data.toString()).message;
    } catch {}
    if (message?.type === "directory_suggestions_request") {
      let rules: Record<string, { delay?: number; error?: string }> = {};
      try {
        rules = JSON.parse(await fs.readFile(q + "/rules.json", "utf8"));
      } catch {}
      const rule = rules[message.query] ?? {};
      queries.set(message.requestId, { query: message.query, delay: rule.delay ?? 0 });
      await fs.appendFile(
        q + "/requests.jsonl",
        JSON.stringify({
          at: Date.now(),
          requestId: message.requestId,
          query: message.query,
          cwd: message.cwd,
          includeFiles: message.includeFiles,
          includeDirectories: message.includeDirectories,
          limit: message.limit,
        }) + "\n",
      );
      if (rule.error) {
        queries.delete(message.requestId);
        send(
          Buffer.from(
            JSON.stringify({
              type: "session",
              message: {
                type: "rpc_error",
                payload: {
                  requestId: message.requestId,
                  requestType: message.type,
                  error: rule.error,
                },
              },
            }),
          ),
          false,
          rule.delay ?? 0,
        );
        return;
      }
    }
    if (upstream.readyState === WebSocket.OPEN) upstream.send(data, { binary });
    else buffered.push([data, binary]);
  });
  upstream.on("message", (raw, binary) => {
    const data = Buffer.from(raw as Buffer);
    let id;
    try {
      id = JSON.parse(data.toString()).message?.payload?.requestId;
    } catch {}
    const query = queries.get(id);
    if (query) queries.delete(id);
    send(data, binary, query?.delay ?? 0);
  });
  client.on("close", () => {
    for (const timer of timers) clearTimeout(timer);
    upstream.close();
  });
  upstream.on("close", () => client.close());
  upstream.on("error", () => client.close());
});
await new Promise<void>((resolve) => server.listen(0, "127.0.0.1", resolve));
const address = server.address();
if (!address || typeof address === "string") throw new Error("Expected private listener");
await fs.writeFile(
  q + "/fixture.json",
  JSON.stringify({ ...original, backendPort: original.port, port: address.port }, null, 2),
);
console.log(`Proxy ready ${address.port}`);
for (const signal of ["SIGTERM", "SIGINT"])
  process.on(signal, () => {
    for (const client of wss.clients) client.close();
    wss.close();
    server.close(() => process.exit(0));
  });
