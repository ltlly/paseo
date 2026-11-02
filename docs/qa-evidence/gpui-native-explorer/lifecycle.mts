import fs from "node:fs/promises";
import { DaemonClient } from "../../../packages/client/src/daemon-client.ts";
const q = process.env.PASEO_NATIVE_QA!;
if (!q) throw new Error("Set PASEO_NATIVE_QA");
const m = JSON.parse(await fs.readFile(q + "/fixture.json", "utf8"));
const c = new DaemonClient({
  url: `ws://127.0.0.1:${m.port}/ws`,
  clientId: "native-explorer-lifecycle",
  clientType: "cli",
  appVersion: "0.14.0",
});
await c.connect();
try {
  if (process.argv[2] === "create") {
    const path = m.cwd + "/second";
    await fs.mkdir(path, { recursive: true });
    const p = await c.addProject(m.cwd);
    const w = await c.createWorkspace({
      source: { kind: "directory", path, projectId: p.project.projectId },
      title: "Second Explorer QA",
    });
    await fs.writeFile(
      q + "/second-workspace.json",
      JSON.stringify({ workspaceId: w.workspace.id, path }, null, 2),
    );
  } else {
    const inventory = await c.listTerminals(m.cwd, undefined, { workspaceId: m.workspaceId });
    if (!inventory.terminals.some((t) => t.id === m.terminalId))
      throw new Error("Original PTY missing");
    console.log(
      JSON.stringify({
        terminalId: m.terminalId,
        retained: true,
        terminalCount: inventory.terminals.length,
      }),
    );
  }
} finally {
  await c.close();
}
