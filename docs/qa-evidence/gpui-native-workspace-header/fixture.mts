import fs from "node:fs/promises";
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
let stopping = false;
const stop = async () => {
  if (stopping) return;
  stopping = true;
  await client.killTerminal(terminalId).catch(() => {});
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
            port: daemon.port,
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
