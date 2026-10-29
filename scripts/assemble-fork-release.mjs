import { createHash } from "node:crypto";
import { createReadStream } from "node:fs";
import { cp, mkdir, readdir, readFile, stat, writeFile } from "node:fs/promises";
import path from "node:path";
import { execFileSync } from "node:child_process";
import { load, dump } from "js-yaml";
import { mergeMacManifest } from "./merge-mac-manifest.mjs";
import { isMainModule } from "./is-main-module.mjs";

const runtimePackages = ["highlight", "relay", "protocol", "client", "plugin", "server", "cli"];
const repository = "ltlly/paseo";

export function validateReleaseInventory(version, names) {
  if (!/^\d+\.\d+\.\d+-ltlly\.[1-9]\d*$/.test(version)) throw new Error("Invalid fork version");
  const required = [
    ...runtimePackages.map((name) => `getpaseo-${name}.tgz`),
    "runtime-manifest.json",
    "latest.yml",
    "latest-mac.yml",
    "latest-linux.yml",
    ...["arm64", "universal"].map((arch) => `Paseo-${version}-${arch}.apk`),
    ...["arm64", "x64"].flatMap((arch) => [
      `Paseo-${version}-${arch}.dmg`,
      `Paseo-${version}-${arch}.zip`,
      `Paseo-Setup-${version}-${arch}.exe`,
    ]),
  ];
  for (const name of required)
    if (!names.includes(name)) throw new Error(`Missing release artifact: ${name}`);
  for (const extension of [".AppImage", ".deb", ".rpm", ".tar.gz"]) {
    if (!names.some((name) => name.endsWith(extension)))
      throw new Error(`Missing Linux ${extension} artifact`);
  }
}

export async function hashFile(file, algorithm, encoding) {
  const hash = createHash(algorithm);
  for await (const chunk of createReadStream(file)) hash.update(chunk);
  return hash.digest(encoding);
}

export async function validateUpdateManifest(manifest, directory, version) {
  if (manifest.version !== version || !Array.isArray(manifest.files) || manifest.files.length === 0)
    throw new Error("Invalid updater manifest version/files");
  for (const item of manifest.files) {
    if (
      typeof item.url !== "string" ||
      path.basename(item.url) !== item.url ||
      item.url.includes("\\")
    )
      throw new Error("Updater asset must be a release-local filename");
    const file = path.join(directory, item.url);
    if ((await stat(file)).size !== item.size)
      throw new Error(`Updater size mismatch: ${item.url}`);
    if ((await hashFile(file, "sha512", "base64")) !== item.sha512)
      throw new Error(`Updater checksum mismatch: ${item.url}`);
  }
}

async function validateRuntime(output, version, tag, commit) {
  const runtime = JSON.parse(await readFile(path.join(output, "runtime-manifest.json"), "utf8"));
  if (
    runtime.repository !== repository ||
    runtime.revision !== commit ||
    runtime.tag !== tag ||
    runtime.dirty
  )
    throw new Error("Runtime source does not match the release tag");
  for (const name of runtimePackages) {
    const file = path.join(output, `getpaseo-${name}.tgz`);
    const pkg = JSON.parse(
      execFileSync("tar", ["-xOf", file, "package/package.json"], { encoding: "utf8" }),
    );
    if (
      pkg.version !== version ||
      pkg.paseoFork?.revision !== commit ||
      pkg.paseoFork?.repository !== repository
    )
      throw new Error(`Wrong runtime provenance: ${name}`);
    for (const section of ["dependencies", "optionalDependencies", "peerDependencies"]) {
      for (const [dependency, target] of Object.entries(pkg[section] ?? {})) {
        if (!runtimePackages.some((item) => dependency === `@getpaseo/${item}`)) continue;
        const expected = `https://github.com/${repository}/releases/download/${tag}/getpaseo-${dependency.split("/")[1]}.tgz`;
        if (target !== expected)
          throw new Error(`Runtime dependency is not pinned to this fork release: ${dependency}`);
      }
    }
  }
}

export async function assembleForkRelease(input, output, tag, commit) {
  const version = tag.replace(/^v/, "");
  if (tag !== `v${version}` || !/^[a-f0-9]{40}$/.test(commit))
    throw new Error("Invalid release source");
  await mkdir(output, { recursive: true });
  for (const group of await readdir(input, { withFileTypes: true })) {
    if (!group.isDirectory() || !/^(runtime|desktop-|android-)/.test(group.name)) continue;
    const directory = path.join(input, group.name);
    for (const entry of await readdir(directory, { withFileTypes: true })) {
      if (!entry.isFile() || entry.name === "SHA256SUMS" || entry.name === "latest-mac.yml")
        continue;
      if (!/\.(tgz|json|yml|apk|dmg|zip|exe|AppImage|deb|rpm|tar\.gz|blockmap)$/.test(entry.name))
        continue;
      await cp(path.join(directory, entry.name), path.join(output, entry.name), {
        force: false,
        errorOnExist: true,
      });
    }
  }
  mergeMacManifest(
    path.join(input, "desktop-macos-arm64/latest-mac.yml"),
    path.join(input, "desktop-macos-x64/latest-mac.yml"),
    path.join(output, "latest-mac.yml"),
  );
  const names = await readdir(output);
  validateReleaseInventory(version, names);
  await validateRuntime(output, version, tag, commit);
  const releaseDate = new Date().toISOString();
  for (const name of ["latest.yml", "latest-linux.yml", "latest-mac.yml"]) {
    const file = path.join(output, name);
    const manifest = load(await readFile(file, "utf8"));
    await validateUpdateManifest(manifest, output, version);
    await writeFile(file, dump({ ...manifest, rolloutHours: 0, releaseDate }, { lineWidth: -1 }));
  }
  const assets = [];
  for (const name of names.sort()) {
    assets.push({
      name,
      size: (await stat(path.join(output, name))).size,
      sha256: await hashFile(path.join(output, name), "sha256", "hex"),
    });
  }
  await writeFile(
    path.join(output, "release-manifest.json"),
    JSON.stringify({ repository, version, tag, commit, releaseDate, assets }, null, 2) + "\n",
  );
  assets.push({
    name: "release-manifest.json",
    sha256: await hashFile(path.join(output, "release-manifest.json"), "sha256", "hex"),
  });
  await writeFile(
    path.join(output, "SHA256SUMS"),
    assets.map((asset) => `${asset.sha256}  ${asset.name}\n`).join(""),
  );
  const changelog = await readFile("CHANGELOG.md", "utf8");
  const heading = `## ${version}`;
  const start = changelog.indexOf(heading);
  if (start < 0) throw new Error("Missing release changelog");
  const end = changelog.indexOf("\n## ", start + heading.length);
  const body = changelog.slice(start, end < 0 ? undefined : end).trim();
  await writeFile(
    path.join(output, "../release-notes.md"),
    `${body}\n\nBuilt by GitHub Actions from ${commit}. Android APKs use the existing fork signing key and include both terminal fonts. macOS DMGs use ad hoc signing and are installed manually from Settings → About; Windows and Linux AppImage update manifests are included.\n`,
  );
  console.log(`Verified ${assets.length} release files for ${tag}.`);
}

if (isMainModule(import.meta.url)) {
  const [, , input, output, tag, commit] = process.argv;
  if (!input || !output || !tag || !commit)
    throw new Error("Usage: assemble-fork-release.mjs <artifact-dir> <output-dir> <tag> <commit>");
  await assembleForkRelease(input, output, tag, commit);
}
