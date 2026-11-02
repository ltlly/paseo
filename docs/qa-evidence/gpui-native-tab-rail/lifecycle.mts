import fs from "node:fs/promises";
import { DaemonClient } from "../../../packages/client/src/daemon-client.ts";
const q = process.env.PASEO_NATIVE_QA;
if (!q) throw new Error("Set PASEO_NATIVE_QA");
const m = JSON.parse(await fs.readFile(q + "/fixture.json", "utf8"));
const extra = JSON.parse(await fs.readFile(q + "/extra.json", "utf8"));
const c = new DaemonClient({
  url: `ws://127.0.0.1:${m.port}/ws`,
  clientId: "tab-rail-close-check",
  clientType: "cli",
  appVersion: "0.14.0",
});
await c.connect();
try {
  const { terminals } = await c.listTerminals(m.cwd, undefined, { workspaceId: m.workspaceId });
  const retained = [];
  for (const [id, marker] of [
    [m.terminalId, "KEYS READY"],
    [extra.terminalId, "SECOND READY"],
  ]) {
    if (!terminals.some((t) => t.id === id)) throw new Error("Retained PTY missing");
    if (!JSON.stringify(await c.captureTerminal(id, { stripAnsi: true })).includes(marker))
      throw new Error("Original PTY output missing");
    retained.push({ terminalId: id, outputMarker: marker, retained: true });
  }
  await fs.writeFile(
    q + "/client-close.json",
    JSON.stringify({ terminalCount: terminals.length, retained }, null, 2),
  );
} finally {
  await c.close();
}
