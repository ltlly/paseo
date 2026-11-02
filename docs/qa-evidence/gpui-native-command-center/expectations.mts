import fs from "node:fs/promises";
import { DaemonClient } from "../../../packages/client/src/daemon-client.ts";
const q = process.env.PASEO_NATIVE_QA!;
if (!q) throw new Error("Set PASEO_NATIVE_QA");
const m = JSON.parse(await fs.readFile(q + "/fixture.json", "utf8"));
const c = new DaemonClient({
  url: `ws://127.0.0.1:${m.backendPort}/ws`,
  clientId: "command-center-oracle",
  clientType: "cli",
  appVersion: "0.14.0",
});
await c.connect();
try {
  const result = await c.getDirectorySuggestions({
    cwd: m.cwd,
    query: "entry",
    includeFiles: true,
    includeDirectories: false,
    limit: 100,
  });
  if (result.error) throw new Error(result.error);
  await fs.writeFile(
    q + "/expected-entry-paths.json",
    JSON.stringify(
      result.entries.map((entry) => entry.path),
      null,
      2,
    ),
  );
} finally {
  await c.close();
}
