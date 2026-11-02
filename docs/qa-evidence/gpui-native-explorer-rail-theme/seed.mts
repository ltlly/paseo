import fs from "node:fs/promises";
import { DaemonClient } from "../../../packages/client/src/daemon-client.ts";
const q = process.env.PASEO_NATIVE_QA!;
const m = JSON.parse(await fs.readFile(q + "/fixture.json", "utf8"));
if (m.daemonPort === 6767) throw Error("private only");
const client = new DaemonClient({
  url: `ws://127.0.0.1:${m.daemonPort}/ws`,
  clientId: "rail-seed",
  clientType: "cli",
  appVersion: "0.14.0",
});
await client.connect();
try {
  const ids = { a: m.workspaceId, main: m.terminalId };
  for (const [key, tag] of [
    ["pi", "pi-qa"],
    ["grokA", "grok-qa"],
  ]) {
    const t = await client.createTerminal(m.cwd, tag, undefined, {
      workspaceId: m.workspaceId,
      command: process.execPath,
      args: [m.cwd + "/keyboard.cjs", tag],
      size: { cols: 80, rows: 24 },
    });
    if (!t.terminal) throw Error(JSON.stringify(t));
    ids[key] = t.terminal.id;
  }
  await fs.writeFile(q + "/terminal-ids.json", JSON.stringify(ids, null, 2));
  const status = await (await fetch(`http://127.0.0.1:${m.daemonPort}/api/status`)).json();
  const layout = {
    root: { kind: "pane", pane: { id: "main", tabs: [ids.main], focusedTab: ids.main } },
    focusedPaneId: "main",
    explorer: {
      visible: true,
      width: 480,
      pane: {
        id: "explorer",
        tabs: ["files", "changes_tree", ids.pi, ids.grokA],
        focusedTab: ids.pi,
      },
    },
  };
  await fs.writeFile(
    m.home + "/native-workspace-layouts.json",
    JSON.stringify({
      version: 1,
      layouts: [{ hostId: status.serverId, workspaceId: m.workspaceId, layout }],
    }),
  );
  await fs.writeFile(q + "/layout-seed.json", JSON.stringify(layout, null, 2));
} finally {
  await client.close();
}
