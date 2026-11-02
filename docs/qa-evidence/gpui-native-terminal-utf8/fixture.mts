import fs from "node:fs/promises";
import { createTestPaseoDaemon } from "../../../packages/server/src/server/test-utils/paseo-daemon.ts";
import { createTestAgentClients } from "../../../packages/server/src/server/test-utils/fake-agent-client.ts";
import { DaemonClient } from "../../../packages/client/src/daemon-client.ts";
const dir = process.env.PASEO_NATIVE_QA;
if (!dir) throw new Error("Set PASEO_NATIVE_QA to a private QA directory");
await fs.mkdir(dir, { recursive: true });
const daemon = await createTestPaseoDaemon({
  pluginsEnabled: false,
  relayEnabled: false,
  agentClients: createTestAgentClients(),
});
const client = new DaemonClient({
  url: `ws://127.0.0.1:${daemon.port}/ws`,
  clientId: "native-utf8-fixture",
  clientType: "cli",
  appVersion: "0.14.0",
});
await client.connect();
const cwd = daemon.paseoHome + "/utf8-replay";
await fs.mkdir(cwd, { recursive: true });
const project = await client.addProject(cwd);
const workspace = await client.createWorkspace({
  source: { kind: "directory", path: cwd, projectId: project.project.projectId },
});
const count = 4000;
const payload =
  Array.from(
    { length: count },
    (_, index) => `${index.toString().padStart(5, "0")} e\u0301 中 box─ end\r\n`,
  ).join("") + "WIRE-END\r\n";
const program = cwd + "/output.cjs";
await fs.writeFile(
  program,
  `process.stdin.resume();process.stdout.write(${JSON.stringify(payload)});`,
);
const terminal = await client.createTerminal(cwd, "UTF8 replay QA", undefined, {
  command: process.execPath,
  args: [program],
  size: { cols: 80, rows: 24 },
  workspaceId: workspace.workspace.id,
});
if (!terminal.terminal) {
  await client.close();
  await daemon.close();
  throw new Error(`Terminal creation failed: ${JSON.stringify(terminal)}`);
}
const terminalId = terminal.terminal.id;
let stopping = false;
const stop = async () => {
  if (stopping) return;
  stopping = true;
  await client.killTerminal(terminalId).catch(() => {});
  await client.close();
  await daemon.close();
};
for (const signal of ["SIGTERM", "SIGINT"]) {
  process.on(signal, () => {
    void stop().then(() => process.exit(0));
  });
}
try {
  for (let attempt = 0; attempt < 100; attempt++) {
    const captured = await client.captureTerminal(terminalId, {
      stripAnsi: true,
    });
    if (JSON.stringify(captured).includes("WIRE-END")) {
      await fs.writeFile(
        dir + "/fixture.json",
        JSON.stringify({ port: daemon.port, home: daemon.paseoHome, terminalId, count }, null, 2),
      );
      console.log("Ready with committed PTY output");
      await new Promise(() => {});
    }
    await new Promise((resolve) => setTimeout(resolve, 50));
  }
  throw new Error("Terminal output did not reach the worker");
} finally {
  await stop();
}
