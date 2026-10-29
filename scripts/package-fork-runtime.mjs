import { execFileSync } from "node:child_process";
import {
  cpSync,
  existsSync,
  mkdirSync,
  mkdtempSync,
  readFileSync,
  renameSync,
  rmSync,
  writeFileSync,
} from "node:fs";
import { tmpdir } from "node:os";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { createHash } from "node:crypto";

const root = fileURLToPath(new URL("..", import.meta.url));
const tag = process.argv[2];
if (!tag || !/^[A-Za-z0-9][A-Za-z0-9._-]*$/.test(tag)) {
  throw new Error("Usage: npm run package:fork-runtime -- <release-tag> [output-directory]");
}
const output = path.resolve(root, process.argv[3] ?? "artifacts/fork-runtime");
const repository = "ltlly/paseo";
const packages = ["highlight", "relay", "protocol", "client", "plugin", "server", "cli"];
const internal = new Set(packages.map((name) => `@getpaseo/${name}`));
const baseUrl = `https://github.com/${repository}/releases/download/${tag}`;
const revision = execFileSync("git", ["rev-parse", "HEAD"], { cwd: root, encoding: "utf8" }).trim();
const npmCli = process.env.npm_execpath;
if (!npmCli) throw new Error("Run this script through npm run package:fork-runtime");
if (!existsSync(path.join(root, "packages/server/dist/server/web-ui/index.html"))) {
  throw new Error("Build the fork web client first: npm run build:daemon-web-ui");
}
const dirty =
  execFileSync("git", ["status", "--porcelain"], { cwd: root, encoding: "utf8" }).trim().length > 0;
const staging = mkdtempSync(path.join(tmpdir(), "paseo-fork-pack-"));
mkdirSync(output, { recursive: true });
function pinInternalDependencies(pkg) {
  for (const section of ["dependencies", "optionalDependencies", "peerDependencies"]) {
    for (const dependency of Object.keys(pkg[section] ?? {}).filter((name) => internal.has(name))) {
      pkg[section][dependency] = `${baseUrl}/getpaseo-${dependency.split("/")[1]}.tgz`;
    }
  }
}

const assets = [];
try {
  for (const name of packages) {
    const source = path.join(root, "packages", name);
    if (!existsSync(path.join(source, "dist")))
      throw new Error(`Build ${name} first: npm run build:server`);
    const stage = path.join(staging, name);
    mkdirSync(stage);
    cpSync(path.join(source, "dist"), path.join(stage, "dist"), { recursive: true });
    cpSync(path.join(root, "LICENSE"), path.join(stage, "LICENSE"));
    if (existsSync(path.join(source, "README.md")))
      cpSync(path.join(source, "README.md"), path.join(stage, "README.md"));
    const pkg = JSON.parse(readFileSync(path.join(source, "package.json"), "utf8"));
    for (const entry of pkg.files ?? []) {
      if (entry.startsWith("!") || entry.startsWith("dist") || entry.includes("*")) continue;
      if (existsSync(path.join(source, entry)))
        cpSync(path.join(source, entry), path.join(stage, entry), { recursive: true });
    }
    pinInternalDependencies(pkg);
    delete pkg.devDependencies;
    delete pkg.scripts;
    pkg.repository = { type: "git", url: `https://github.com/${repository}.git` };
    pkg.paseoFork = { repository, revision, tag, dirty };
    writeFileSync(path.join(stage, "package.json"), `${JSON.stringify(pkg, null, 2)}\n`);
    const packResult = JSON.parse(
      execFileSync(
        process.execPath,
        [npmCli, "pack", "--ignore-scripts", "--json", "--pack-destination", output],
        {
          cwd: stage,
          encoding: "utf8",
        },
      ),
    );
    // npm 11 emits an array; npm 12 keys the result by package name.
    const packed = Array.isArray(packResult) ? packResult[0] : packResult[pkg.name];
    if (!packed?.filename) throw new Error(`npm pack returned no filename for ${pkg.name}`);
    const filename = `getpaseo-${name}.tgz`;
    renameSync(path.join(output, packed.filename), path.join(output, filename));
    const sha256 = createHash("sha256")
      .update(readFileSync(path.join(output, filename)))
      .digest("hex");
    assets.push({ filename, sha256 });
  }
  writeFileSync(
    path.join(output, "runtime-manifest.json"),
    `${JSON.stringify({ repository, revision, tag, dirty, assets }, null, 2)}\n`,
  );
  writeFileSync(
    path.join(output, "SHA256SUMS"),
    assets.map((a) => `${a.sha256}  ${a.filename}\n`).join(""),
  );
  console.log(`Fork runtime prepared in ${output}. Nothing uploaded or published.`);
} finally {
  rmSync(staging, { recursive: true, force: true });
}
