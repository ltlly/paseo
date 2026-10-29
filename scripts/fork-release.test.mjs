import assert from "node:assert/strict";
import { mkdtemp, readFile, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import path from "node:path";
import test from "node:test";
import { load } from "js-yaml";
import {
  hashFile,
  validateReleaseInventory,
  validateUpdateManifest,
} from "./assemble-fork-release.mjs";

test("release builds are triggered only by fork version tags and published releases", async () => {
  const workflow = load(await readFile(".github/workflows/fork-release.yml", "utf8"));
  assert.deepEqual(Object.keys(workflow.on).sort(), ["push", "release"]);
  assert.deepEqual(workflow.on.push, { tags: ["v*-ltlly.*"] });
  assert.deepEqual(workflow.on.release.types, ["published"]);
  assert.deepEqual(workflow.jobs.publish.needs.sort(), [
    "android",
    "desktop",
    "prepare",
    "runtime",
    "verify",
  ]);
});

test("release inventory requires every platform before publication", () => {
  const version = "0.14.0-ltlly.2";
  const files = [
    ...["highlight", "relay", "protocol", "client", "plugin", "server", "cli"].map(
      (name) => `getpaseo-${name}.tgz`,
    ),
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
    "Paseo.AppImage",
    "Paseo.deb",
    "Paseo.rpm",
    "Paseo.tar.gz",
  ];
  validateReleaseInventory(version, files);
  for (const missing of files)
    assert.throws(
      () =>
        validateReleaseInventory(
          version,
          files.filter((name) => name !== missing),
        ),
      /Missing/,
    );
});

test("updater manifests must match actual bytes and stay inside the release", async () => {
  const directory = await mkdtemp(path.join(tmpdir(), "paseo-release-test-"));
  try {
    const file = path.join(directory, "Paseo.exe");
    await writeFile(file, "installer");
    const version = "0.14.0-ltlly.2";
    const manifest = {
      version,
      files: [{ url: "Paseo.exe", size: 9, sha512: await hashFile(file, "sha512", "base64") }],
    };
    await validateUpdateManifest(manifest, directory, version);
    await assert.rejects(validateUpdateManifest(manifest, directory, "0.14.0-ltlly.1"), /version/);
    await assert.rejects(
      validateUpdateManifest(
        { ...manifest, files: [{ ...manifest.files[0], url: "../Paseo.exe" }] },
        directory,
        version,
      ),
      /release-local/,
    );
    await writeFile(file, "corrupted");
    await assert.rejects(validateUpdateManifest(manifest, directory, version), /checksum/);
  } finally {
    await rm(directory, { recursive: true, force: true });
  }
});
