import fs from "node:fs/promises";
import { DaemonClient } from "../../../packages/client/src/daemon-client.ts";
const q = process.env.PASEO_NATIVE_QA;
if (!q) throw new Error("Set PASEO_NATIVE_QA");
const m = JSON.parse(await fs.readFile(q + "/fixture.json", "utf8"));
const client = new DaemonClient({
  url: `ws://127.0.0.1:${m.port}/ws`,
  clientId: "tab-rail-fixture",
  clientType: "cli",
  appVersion: "0.14.0",
});
await client.connect();
try {
  if (process.argv[2] === "cleanup") {
    const extra = JSON.parse(await fs.readFile(q + "/extra.json", "utf8"));
    await client.killTerminal(extra.terminalId);
  } else {
    await fs.mkdir(m.cwd + "/src", { recursive: true });
    await fs.writeFile(m.cwd + "/src/alpha.rs", "// PANE ALPHA MARKER\nfn main() {}\n");
    await fs.writeFile(m.cwd + "/notes.md", "PANE NOTES MARKER\n");
    await fs.writeFile(m.cwd + "/dirty.txt", "PANE DIRTY MARKER\n");
    for (let i = 0; i < 9; i++)
      await fs.writeFile(m.cwd + `/long-file-${i}-for-overflow.txt`, `OVERFLOW ${i}\n`);
    const input = m.cwd + "/second-input.jsonl";
    await fs.writeFile(input, "");
    const program = m.cwd + "/second.cjs";
    await fs.writeFile(
      program,
      `const fs=require('node:fs');process.stdin.setRawMode(true);process.stdin.resume();process.stdout.write('SECOND READY\\r\\n');process.stdin.on('data',b=>fs.appendFileSync(${JSON.stringify(input)},JSON.stringify({base64:b.toString('base64')})+'\\n'));`,
    );
    const terminal = await client.createTerminal(m.cwd, "Second fixture", undefined, {
      workspaceId: m.workspaceId,
      command: process.execPath,
      args: [program],
      size: { cols: 80, rows: 24 },
    });
    if (!terminal.terminal) throw new Error(JSON.stringify(terminal));
    const secondPath = m.cwd + "/second-workspace";
    await fs.mkdir(secondPath, { recursive: true });
    await fs.writeFile(secondPath + "/other.txt", "OTHER WORKSPACE MARKER\n");
    const project = await client.addProject(m.cwd);
    const workspace = await client.createWorkspace({
      source: { kind: "directory", path: secondPath, projectId: project.project.projectId },
      title: "Second pane QA",
    });
    await fs.writeFile(
      q + "/extra.json",
      JSON.stringify(
        {
          terminalId: terminal.terminal.id,
          input,
          secondWorkspaceId: workspace.workspace.id,
          secondPath,
        },
        null,
        2,
      ),
    );
  }
} finally {
  await client.close();
}
