import fs from "node:fs/promises";
import { DaemonClient } from "../../../packages/client/src/daemon-client.ts";
const q = process.env.PASEO_NATIVE_QA;
if (!q) throw new Error("Set PASEO_NATIVE_QA");
const m = JSON.parse(await fs.readFile(q + "/fixture.json", "utf8"));
const client = new DaemonClient({
  url: `ws://127.0.0.1:${m.backendPort}/ws`,
  clientId: "command-center-close-check",
  clientType: "cli",
  appVersion: "0.14.0",
});
await client.connect();
try {
  const { terminals } = await client.listTerminals(m.cwd, undefined, {
    workspaceId: m.workspaceId,
  });
  if (!terminals.some((terminal) => terminal.id === m.terminalId))
    throw new Error("Original PTY lost");
  const capture = await client.captureTerminal(m.terminalId, { stripAnsi: true });
  if (!JSON.stringify(capture).includes("KEYS READY")) throw new Error("Original output lost");
  await fs.writeFile(
    q + "/client-close.json",
    JSON.stringify(
      {
        terminalId: m.terminalId,
        originalPTYRetained: true,
        originalOutputRetained: true,
        terminalCount: terminals.length,
      },
      null,
      2,
    ),
  );
} finally {
  await client.close();
}
