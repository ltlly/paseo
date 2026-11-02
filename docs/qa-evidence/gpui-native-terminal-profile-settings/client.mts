import fs from "node:fs/promises";
import { DaemonClient } from "../../../packages/client/src/daemon-client.ts";
const q = "/home/ltlly/.cache/paseo-native-qa/profiles-20261102";
const m = JSON.parse(await fs.readFile(q + "/fixture.json", "utf8"));
if (m.daemonPort === 6767) throw Error("Use private fixture");
const client = new DaemonClient({
  url: `ws://127.0.0.1:${m.daemonPort}/ws`,
  clientId: "profile-proof",
  clientType: "cli",
  appVersion: "0.14.0",
});
await client.connect();
try {
  const action = process.argv[2];
  let result;
  if (action === "config") {
    const { config } = await client.getDaemonConfig();
    result = { profiles: config.terminalProfiles, hooks: config.enableTerminalAgentHooks };
  }
  if (action === "seed") {
    const { config } = await client.getDaemonConfig();
    const profiles = config.terminalProfiles;
    profiles[1].args.push("a b");
    profiles[1].opaque = { keep: 17 };
    await client.patchDaemonConfig({ terminalProfiles: profiles });
    result = profiles;
  }
  if (action === "list") result = await client.listTerminals();
  if (action === "killAll") {
    for (const t of (await client.listTerminals()).terminals) await client.killTerminal(t.id);
    result = await client.listTerminals();
  }
  if (action === "patch")
    result = (
      await client.patchDaemonConfig(JSON.parse(await fs.readFile(process.argv[3], "utf8")))
    ).config.terminalProfiles;
  console.log(JSON.stringify(result));
} finally {
  await client.close();
}
