import fs from "node:fs/promises";
import { WebSocket, WebSocketServer } from "ws";
import { createTestPaseoDaemon } from "../../../packages/server/src/server/test-utils/paseo-daemon.ts";
import { createTestAgentClients } from "../../../packages/server/src/server/test-utils/fake-agent-client.ts";
import { DaemonClient } from "../../../packages/client/src/daemon-client.ts";
const dir = process.env.PASEO_NATIVE_QA;
if (!dir) throw new Error("Set PASEO_NATIVE_QA to a private QA directory");
await fs.mkdir(dir, { recursive: true });
const daemon = await createTestPaseoDaemon({
  pluginsEnabled: false,
  corsAllowedOrigins: ["*"],
  relayEnabled: false,
  agentClients: createTestAgentClients(),
});
const client = new DaemonClient({
  url: `ws://127.0.0.1:${daemon.port}/ws`,
  clientId: "native-keys-fixture",
  clientType: "cli",
  appVersion: "0.14.0",
});
await client.connect();
const cwd = daemon.paseoHome + "/native-keys-project";
await fs.mkdir(cwd, { recursive: true });
const project = await client.addProject(cwd);
const workspace = await client.createWorkspace({
  source: { kind: "directory", path: cwd, projectId: project.project.projectId },
});
await client.setWorkspaceTitle(workspace.workspace.id, "Terminal workspace header alignment");
const program = cwd + "/keyboard.cjs";
const input = cwd + "/input.jsonl";
const mode = cwd + "/mode.txt";
await fs.writeFile(input, "");
await fs.writeFile(mode, "");
const payload =
  "\x1b[?25l" +
  Array.from(
    { length: 240 },
    (_, i) => `ROW${i.toString().padStart(4, "0")} native-keyboard-qa\r\n`,
  ).join("") +
  "KEYS READY\r\n";
await fs.writeFile(
  program,
  `const fs=require('node:fs');process.stdin.setRawMode(true);process.stdin.resume();process.stdout.write(${JSON.stringify(payload)});process.stdin.on('data',bytes=>fs.appendFileSync(${JSON.stringify(input)},JSON.stringify({base64:bytes.toString('base64')})+'\\n'));fs.watchFile(${JSON.stringify(mode)},{interval:50},()=>process.stdout.write(fs.readFileSync(${JSON.stringify(mode)},'utf8')));`,
);
const terminal = await client.createTerminal(cwd, "Keyboard fixture", undefined, {
  workspaceId: workspace.workspace.id,
  command: process.execPath,
  args: [program],
  size: { cols: 80, rows: 24 },
});
if (!terminal.terminal) {
  await client.close();
  await daemon.close();
  throw new Error(JSON.stringify(terminal));
}
const terminalId = terminal.terminal.id;
const profiles = [
  {
    id: "pi-qa",
    name: "Pi fixture",
    command: process.execPath,
    args: [program, "{{{prompt}}}", "--prompt={{{prompt}}}"],
    icon: "pi",
  },
  { id: "grok-qa", name: "Grok fixture", command: process.execPath, args: [program], icon: "grok" },
  { id: "literal", name: "Unlabelled icon", command: process.execPath, args: [program], icon: "" },
];
await client.patchDaemonConfig({ terminalProfiles: profiles });
const secondCwd = daemon.paseoHome + "/second-project";
await fs.mkdir(secondCwd, { recursive: true });
const secondProject = await client.addProject(secondCwd);
const secondWorkspace = await client.createWorkspace({
  source: { kind: "directory", path: secondCwd, projectId: secondProject.project.projectId },
});
await client.setWorkspaceTitle(secondWorkspace.workspace.id, "Other isolated workspace");
const proxy = new WebSocketServer({ host: "127.0.0.1", port: 0 });
await new Promise((resolve) => proxy.on("listening", resolve));
const proxyPort = proxy.address().port;
await fs.writeFile(dir + "/proxy-control.json", JSON.stringify({ delayCreateMs: 0 }));
proxy.on("connection", (socket) => {
  const upstream = new WebSocket(`ws://127.0.0.1:${daemon.port}/ws`);
  const queue = [];
  socket.on("message", (data, binary) => {
    if (!binary) {
      try {
        const envelope = JSON.parse(data.toString());
        const msg = envelope.message;
        if (
          msg &&
          [
            "create_terminal_request",
            "kill_terminal_request",
            "get_daemon_config_request",
          ].includes(msg.type)
        )
          void fs.appendFile(
            dir + "/rpc.jsonl",
            JSON.stringify({ direction: "request", message: msg }) + "\n",
          );
      } catch {}
    }
    if (upstream.readyState === WebSocket.OPEN) upstream.send(data, { binary });
    else queue.push([data, binary]);
  });
  upstream.on("open", () => {
    for (const [data, binary] of queue) upstream.send(data, { binary });
  });
  upstream.on("message", async (data, binary) => {
    let delay = 0;
    if (!binary) {
      try {
        const envelope = JSON.parse(data.toString());
        const msg = envelope.message ?? envelope;
        if (["create_terminal_response", "kill_terminal_response"].includes(msg.type)) {
          await fs.appendFile(
            dir + "/rpc.jsonl",
            JSON.stringify({ direction: "response", message: msg }) + "\n",
          );
          if (msg.type === "create_terminal_response")
            delay =
              JSON.parse(await fs.readFile(dir + "/proxy-control.json", "utf8")).delayCreateMs ?? 0;
        }
      } catch {}
    }
    setTimeout(() => {
      if (socket.readyState === WebSocket.OPEN) socket.send(data, { binary });
    }, delay);
  });
  socket.on("close", () => upstream.close());
  upstream.on("close", () => socket.close());
  socket.on("error", () => {});
  upstream.on("error", () => socket.close());
});
await fs.writeFile(
  dir + "/extra.json",
  JSON.stringify({
    profiles,
    secondCwd,
    secondWorkspace: secondWorkspace.workspace.id,
    daemonPort: daemon.port,
  }),
);
let stopping = false;
const stop = async () => {
  if (stopping) return;
  stopping = true;
  for (const workspaceId of [workspace.workspace.id, secondWorkspace.workspace.id]) {
    const listed = await client
      .listTerminals(undefined, undefined, { workspaceId })
      .catch(() => ({ terminals: [] }));
    for (const item of listed.terminals ?? []) await client.killTerminal(item.id).catch(() => {});
  }
  for (const socket of proxy.clients) socket.terminate();
  proxy.close();
  await client.close();
  await daemon.close();
};
for (const signal of ["SIGTERM", "SIGINT"])
  process.on(signal, () => {
    void stop().then(() => process.exit(0));
  });
try {
  for (let attempt = 0; attempt < 100; attempt++) {
    const capture = await client.captureTerminal(terminalId, { stripAnsi: true });
    if (JSON.stringify(capture).includes("KEYS READY")) {
      await fs.writeFile(
        dir + "/fixture.json",
        JSON.stringify(
          {
            port: proxyPort,
            daemonPort: daemon.port,
            home: daemon.paseoHome,
            workspaceId: workspace.workspace.id,
            terminalId,
            cwd,
            input,
            mode,
          },
          null,
          2,
        ),
      );
      console.log("Ready");
      await new Promise(() => {});
    }
    await new Promise((resolve) => setTimeout(resolve, 50));
  }
  throw new Error("PTY output did not commit");
} finally {
  await stop();
}
