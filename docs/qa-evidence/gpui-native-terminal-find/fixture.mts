import fs from "node:fs/promises";
import { createTestPaseoDaemon } from "../../../packages/server/src/server/test-utils/paseo-daemon.ts";
import { createTestAgentClients } from "../../../packages/server/src/server/test-utils/fake-agent-client.ts";
import { DaemonClient } from "../../../packages/client/src/daemon-client.ts";
const dir = process.env.PASEO_NATIVE_QA;
if (!dir) throw new Error("Set PASEO_NATIVE_QA to an empty QA evidence directory");
await fs.mkdir(dir, { recursive: true });
const daemon = await createTestPaseoDaemon({
  pluginsEnabled: false,
  relayEnabled: false,
  agentClients: createTestAgentClients(),
});
const client = new DaemonClient({
  url: `ws://127.0.0.1:${daemon.port}/ws`,
  clientId: "native-find-qa",
  clientType: "cli",
  appVersion: "0.14.0",
});
await client.connect();
const projectDir = daemon.paseoHome + "/native-find-project";
await fs.mkdir(projectDir, { recursive: true });
const project = await client.addProject(projectDir);
const workspace = await client.createWorkspace({
  source: { kind: "directory", path: projectDir, projectId: project.project.projectId },
});
await client.setWorkspaceTitle(workspace.workspace.id, "Terminal find QA");
const program = projectDir + "/fixture.cjs";
await fs.writeFile(
  program,
  `const fs=require('node:fs');process.stdin.setRawMode(true);process.stdin.resume();function draw(){let cols=process.stdout.columns||80;process.stdout.write('\\x1b[?25l\\x1b[2J\\x1b[H'+'z'.repeat(cols-6)+'d/path/to/project\\r\\ncafe\\u0301 cafe\\u0301\\r\\naaaaaaa\\r\\nfind-qa-ready\\r\\n');}draw();process.stdout.on('resize',draw);process.stdin.on('data',bytes=>{fs.appendFileSync(__dirname+'/input.jsonl',JSON.stringify({base64:bytes.toString('base64')})+'\\n');});`,
);
const terminal = await client.createTerminal(projectDir, "Find fixture", undefined, {
  workspaceId: workspace.workspace.id,
  command: process.execPath,
  args: [program],
  size: { cols: 80, rows: 24 },
});
await fs.writeFile(
  dir + "/fixture.json",
  JSON.stringify(
    {
      port: daemon.port,
      home: daemon.paseoHome,
      projectDir,
      workspaceId: workspace.workspace.id,
      terminalId: terminal.terminal.id,
    },
    null,
    2,
  ),
);
console.log("ready " + daemon.port);
const stop = async () => {
  await client.killTerminal(terminal.terminal.id).catch(() => {});
  await client.close();
  await daemon.close();
  process.exit(0);
};
process.on("SIGTERM", stop);
process.on("SIGINT", stop);
