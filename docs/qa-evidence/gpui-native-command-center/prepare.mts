import fs from "node:fs/promises";
const q = process.env.PASEO_NATIVE_QA;
if (!q) throw new Error("Set PASEO_NATIVE_QA to a fresh isolated fixture directory");
const m = JSON.parse(await fs.readFile(q + "/fixture.json", "utf8"));
await fs.mkdir(m.cwd + "/src", { recursive: true });
await fs.mkdir(m.cwd + "/many", { recursive: true });
await fs.writeFile(m.cwd + "/src/alpha.rs", "// COMMAND CENTER OPENED REAL FILE\nfn main() {}\n");
for (const file of ["src/alpine.ts", "README.md", "slow-one.txt", "fast-new.txt", "late-file.txt"])
  await fs.writeFile(m.cwd + "/" + file, "Isolated Command Center QA\n");
for (let index = 0; index < 120; index++)
  await fs.writeFile(m.cwd + `/many/entry-${String(index).padStart(3, "0")}.txt`, "QA\n");
await fs.writeFile(
  q + "/rules.json",
  JSON.stringify(
    {
      slow: { delay: 3000 },
      slowerror: { delay: 1800, error: "Injected late search error" },
      rpcerror: { error: "Injected search error" },
      "entry ": { delay: 2200 },
    },
    null,
    2,
  ),
);
