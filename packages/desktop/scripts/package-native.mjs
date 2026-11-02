// Opt-in packaging pipeline for the native GPUI desktop client.
//
// Produces, per native platform only (no cross bundling), into <out>/artifacts:
//   linux   → tar.gz + deb + rpm + AppImage (installers when their tools exist)
//   darwin  → unsigned "Paseo Native.app" zip + dmg (signing/notarization remain
//             explicit gates; never signed here)
//   win32   → portable zip + NSIS setup when makensis exists (signing remains a
//             gate; never signed here)
//
// The staged materialization stays in <out>/stage and is never part of an
// artifact. The Electron desktop package (`npm run build:desktop`) remains the
// default and is untouched.
//
// Every bundle carries its own pinned Node runtime plus the server/CLI daemon
// runtime copied from the repo's actually-installed dependency closure (no
// registry resolution at package time; conflicting versions are staged nested
// under their dependent, never first-wins). The generated app launcher
// bootstraps the daemon through `paseo daemon start --home` (never restarts a
// live daemon) and then execs the GPUI client, which discovers it through its
// existing PASEO_HOME / paseo.pid path.
import { createHash } from "node:crypto";
import { createRequire } from "node:module";
import fs from "node:fs";
import fsPromises from "node:fs/promises";
import { execFileSync, spawnSync } from "node:child_process";
import path from "node:path";
import { fileURLToPath } from "node:url";
import {
  APPIMAGE_RUNTIME_ENV_VAR,
  APPIMAGE_RUNTIME_SHA256,
  APPIMAGE_RUNTIME_URL,
  DEB_ARCH,
  NATIVE_NODE_SHASUMS256,
  NATIVE_NODE_SHASUMS_URL,
  NATIVE_NODE_VERSION,
  appLauncherScript,
  artifactBaseName,
  assertPosixExecutable,
  assertGlibcWithinBaseline,
  classifyNeededLibraries,
  cliShimScript,
  compareGlibcVersions,
  debControl,
  debDependsFromLibraries,
  debPackageVersion,
  distCopyFilter,
  escapeNsisString,
  escapePowerShellSingleQuoted,
  infoPlist,
  LINUX_GLIBC_BASELINE,
  linuxDesktopEntry,
  maxGlibcSymbolVersion,
  npmCommand,
  nodeBinaryRelativePath,
  nodeDistArchiveName,
  nodeDistUrl,
  nodeExtractedDirName,
  nodePlatformKey,
  nsisUninstallLines,
  planInstalledClosure,
  requiredBundlePaths,
  requiredMacAppPaths,
  resolveInstalledPackage,
  rpmRequiresFromDebPackages,
  workspaceRuntimePackageNames,
} from "./package-native-lib.mjs";

const require = createRequire(import.meta.url);
const { pruneClaudeAgentSdk, pruneNodePty, pruneSharpLibvips } = require("./after-pack.js");

const desktopRoot = path.dirname(path.dirname(fileURLToPath(import.meta.url)));
const repoRoot = path.resolve(desktopRoot, "..", "..");

// Pinned linuxdeploy release used to assemble the AppImage. Provenance:
// https://github.com/linuxdeploy/linuxdeploy/releases/tag/1-alpha-20251107-1
// — the SHA-256 values below were computed from that release's downloads.
const LINUXDEPLOY_RELEASE_TAG = "1-alpha-20251107-1";
const LINUXDEPLOY_SHA256 = {
  x64: "c20cd71e3a4e3b80c3483cef793cda3f4e990aca14014d23c544ca3ce1270b4d",
  arm64: "620095110d693282b8ebeb244a95b5e911cf8f65f76c88b4b47d16ae6346fcff",
};

const BUNDLE_NAME = "PaseoNative";

function fail(message) {
  console.error(`package-native: ${message}`);
  process.exit(1);
}

function parseArgs(argv) {
  const options = {
    platform: process.platform,
    arch: process.arch,
    out: path.join(repoRoot, "packages", "desktop", "release-native"),
    skipCargoBuild: false,
    skipDaemonDistCheck: false,
  };
  for (let index = 0; index < argv.length; index += 1) {
    const value = argv[index];
    if (value === "--platform") options.platform = argv[++index];
    else if (value === "--arch") options.arch = argv[++index];
    else if (value === "--out") options.out = path.resolve(argv[++index]);
    else if (value === "--skip-cargo-build") options.skipCargoBuild = true;
    else if (value === "--skip-daemon-dist-check") options.skipDaemonDistCheck = true;
    else fail(`Unknown argument: ${value}`);
  }
  return options;
}

function run(command, args, options = {}) {
  const { cwd = repoRoot, env = {}, stdio = "inherit" } = options;
  execFileSync(command, args, { cwd, env: { ...process.env, ...env }, stdio });
}

// npm must never be exec'd directly as `npm` on Windows (npm.cmd batch file,
// no shell here); go through the real CLI JS with the running Node instead.
function runNpm(args, options = {}) {
  const npm = npmCommand();
  if (npm.args.length > 0 && !fs.existsSync(npm.args[0])) {
    fail(`npm CLI JS not found next to the running Node: ${npm.args[0]}`);
  }
  run(npm.command, [...npm.args, ...args], options);
}

function commandExists(command) {
  // Windows has no `sh`; probe with where.exe so tool detection (makensis etc.)
  // works on windows runners.
  const probe =
    process.platform === "win32"
      ? spawnSync("where.exe", [command], { encoding: "utf8" })
      : spawnSync("sh", ["-c", `command -v ${JSON.stringify(command)}`], { encoding: "utf8" });
  return probe.status === 0;
}

function assertFile(filePath, hint) {
  if (!fs.existsSync(filePath)) fail(`Missing ${filePath}${hint ? ` (${hint})` : ""}`);
}

async function copyFiltered(sourceDir, targetDir, filter) {
  await fsPromises.mkdir(targetDir, { recursive: true });
  const entries = await fsPromises.readdir(sourceDir, { withFileTypes: true });
  for (const entry of entries) {
    const source = path.join(sourceDir, entry.name);
    const target = path.join(targetDir, entry.name);
    if (entry.isDirectory()) await copyFiltered(source, target, filter);
    else if (filter(entry.name)) await fsPromises.copyFile(source, target);
  }
}

async function copyFilteredTree(sourceDir, targetDir, filter) {
  await fsPromises.mkdir(path.dirname(targetDir), { recursive: true });
  await copyFiltered(sourceDir, targetDir, (relative) => filter(relative));
}

async function stageWorkspacePackage(sourcePackageDir, targetPackageDir) {
  const packageJson = JSON.parse(
    await fsPromises.readFile(path.join(sourcePackageDir, "package.json"), "utf8"),
  );
  await fsPromises.mkdir(targetPackageDir, { recursive: true });
  await fsPromises.writeFile(
    path.join(targetPackageDir, "package.json"),
    JSON.stringify(packageJson, null, 2),
  );
  if (fs.existsSync(path.join(sourcePackageDir, "dist"))) {
    await copyFilteredTree(
      path.join(sourcePackageDir, "dist"),
      path.join(targetPackageDir, "dist"),
      distCopyFilter,
    );
  }
  if (packageJson.bin && fs.existsSync(path.join(sourcePackageDir, "bin"))) {
    await copyFilteredTree(
      path.join(sourcePackageDir, "bin"),
      path.join(targetPackageDir, "bin"),
      () => true,
    );
  }
}

// Copy an installed external dependency directory, excluding its nested
// node_modules: those dependencies are staged explicitly by the closure plan so
// conflicting versions land under the dependent that requires them.
async function copyInstalledPackage(sourceDir, targetDir) {
  await fsPromises.mkdir(targetDir, { recursive: true });
  const entries = await fsPromises.readdir(sourceDir, { withFileTypes: true });
  for (const entry of entries) {
    if (entry.name === "node_modules") continue;
    const source = path.join(sourceDir, entry.name);
    const target = path.join(targetDir, entry.name);
    if (entry.isDirectory()) await fsPromises.cp(source, target, { recursive: true });
    else if (entry.isFile()) await fsPromises.copyFile(source, target);
    else if (entry.isSymbolicLink())
      await fsPromises.symlink(await fsPromises.readlink(source), target);
  }
}

async function verifyNodeArchiveChecksum(cacheDir, platform, arch, archivePath) {
  // Cross-check the pinned constant against the official SHASUMS256 manifest
  // fetched at package time; neither HTTPS nor the constant is trusted alone.
  const manifestPath = path.join(cacheDir, "SHASUMS256.txt");
  if (!fs.existsSync(manifestPath)) {
    run("curl", ["-fsSL", "-o", manifestPath, NATIVE_NODE_SHASUMS_URL]);
  }
  const manifest = await fsPromises.readFile(manifestPath, "utf8");
  const archiveName = nodeDistArchiveName(platform, arch);
  const manifestLine = manifest
    .split("\n")
    .find((line) => line.trim().endsWith(`  ${archiveName}`));
  if (!manifestLine) fail(`Official SHASUMS256 manifest has no entry for ${archiveName}`);
  const officialSha256 = manifestLine.trim().split(/\s+/)[0];
  if (officialSha256 !== NATIVE_NODE_SHASUMS256[nodePlatformKey(platform, arch)]) {
    fail(
      `Pinned Node checksum no longer matches the official manifest for ${archiveName}: manifest ${officialSha256}`,
    );
  }
  const actualSha256 = createHash("sha256")
    .update(await fsPromises.readFile(archivePath))
    .digest("hex");
  if (actualSha256 !== officialSha256) {
    fail(
      `Node archive checksum mismatch for ${archiveName}: expected ${officialSha256}, got ${actualSha256}`,
    );
  }
}

async function stageNodeRuntime(bundleDir, platform, arch, cacheDir) {
  const key = nodePlatformKey(platform, arch);
  if (!NATIVE_NODE_SHASUMS256[key]) fail(`No pinned Node checksum for ${key}`);
  const archiveName = nodeDistArchiveName(platform, arch);
  await fsPromises.mkdir(cacheDir, { recursive: true });
  const archivePath = path.join(cacheDir, archiveName);
  if (!fs.existsSync(archivePath)) {
    console.log(`package-native: downloading ${nodeDistUrl(platform, arch)}`);
    run("curl", ["-fL", "-o", archivePath, nodeDistUrl(platform, arch)]);
  }
  await verifyNodeArchiveChecksum(cacheDir, platform, arch, archivePath);

  const extractDir = path.join(cacheDir, "extracted", key, NATIVE_NODE_VERSION);
  await fsPromises.rm(extractDir, { recursive: true, force: true });
  await fsPromises.mkdir(extractDir, { recursive: true });
  if (platform === "win32") {
    run("powershell.exe", [
      "-NoProfile",
      "-Command",
      `Expand-Archive -LiteralPath '${escapePowerShellSingleQuoted(archivePath)}' -DestinationPath '${escapePowerShellSingleQuoted(extractDir)}' -Force`,
    ]);
  } else {
    run("tar", ["-xf", archivePath, "-C", extractDir]);
  }
  // Ship only the node executable: the official dist's bin/ symlinks (npm,
  // npx, corepack) point back into the extraction cache and the daemon runtime
  // never needs them.
  const extractedRoot = path.join(extractDir, nodeExtractedDirName(platform, arch));
  const nodeTarget = path.join(bundleDir, "daemon", nodeBinaryRelativePath(platform));
  await fsPromises.mkdir(path.dirname(nodeTarget), { recursive: true });
  await fsPromises.copyFile(
    path.join(extractedRoot, platform === "win32" ? "node.exe" : "bin/node"),
    nodeTarget,
  );
  if (platform !== "win32") fs.chmodSync(nodeTarget, 0o755);
}

async function stageDaemonRuntime(bundleDir, platform, arch, cacheDir) {
  console.log("package-native: staging bundled Node runtime");
  await stageNodeRuntime(bundleDir, platform, arch, cacheDir);

  const daemonDir = path.join(bundleDir, "daemon");

  console.log("package-native: staging daemon dependency closure from installed node_modules");
  const readWorkspacePackageJson = (name) =>
    JSON.parse(fs.readFileSync(path.join(repoRoot, "packages", name, "package.json"), "utf8"));
  const packageJsonsByName = Object.fromEntries(
    ["server", "cli", "client", "protocol", "relay", "highlight", "plugin"].map((name) => [
      name,
      readWorkspacePackageJson(name),
    ]),
  );
  const workspacePackages = workspaceRuntimePackageNames(packageJsonsByName, ["server", "cli"]);
  const readPackageJsonAt = (dir) =>
    JSON.parse(fs.readFileSync(path.join(dir, "package.json"), "utf8"));
  const plan = planInstalledClosure({
    entries: [
      {
        name: "@getpaseo/server",
        dir: path.join(repoRoot, "packages", "server"),
        stagingRel: "node_modules/@getpaseo/server",
      },
      {
        name: "@getpaseo/cli",
        dir: path.join(repoRoot, "packages", "cli"),
        stagingRel: "node_modules/@getpaseo/cli",
      },
    ],
    resolveInstalled: resolveInstalledPackage,
    readPackageJson: readPackageJsonAt,
  });

  const externalDependencies = [];
  for (const entry of plan) {
    const target = path.join(daemonDir, entry.stagingRel);
    if (entry.name.startsWith("@getpaseo/")) {
      await stageWorkspacePackage(entry.dir, target);
    } else {
      await copyInstalledPackage(entry.dir, target);
      externalDependencies.push({
        name: entry.name,
        version: entry.packageJson.version,
        stagedAs: entry.stagingRel,
        requiredBy: entry.dependentStagingRel ?? "closure-root",
      });
    }
  }

  console.log("package-native: pruning non-native platform binaries");
  const nodeModules = path.join(daemonDir, "node_modules");
  pruneClaudeAgentSdk(nodeModules, platform, arch);
  pruneNodePty(nodeModules, platform, arch);
  pruneSharpLibvips(nodeModules, platform, arch);

  // Reproducibility manifests: record exactly what was bundled and from where.
  await fsPromises.writeFile(
    path.join(daemonDir, "external-dependencies.json"),
    `${JSON.stringify(
      {
        source: "copied from the repository's installed node_modules at package time",
        packages: externalDependencies.sort((a, b) => a.name.localeCompare(b.name)),
      },
      null,
      2,
    )}\n`,
  );
  await fsPromises.writeFile(
    path.join(daemonDir, "runtime.json"),
    `${JSON.stringify(
      {
        nodeVersion: NATIVE_NODE_VERSION,
        nodeShasumsUrl: NATIVE_NODE_SHASUMS_URL,
        workspacePackages: Object.fromEntries(
          workspacePackages.map((name) => [name, packageJsonsByName[name].version]),
        ),
      },
      null,
      2,
    )}\n`,
  );
  console.log(`package-native: staged ${externalDependencies.length} external daemon dependencies`);
}

// Shared libraries the packaged GPUI binary links (NEEDED entries).
function neededSharedLibraries(binaryPath) {
  const objdump = spawnSync("objdump", ["-p", binaryPath], { encoding: "utf8" });
  if (objdump.status !== 0) fail(`objdump failed for ${binaryPath}: ${objdump.stderr}`);
  return [...objdump.stdout.matchAll(/NEEDED\s+(\S+)/g)].map((match) => match[1]);
}

// Versioned glibc symbol requirements of an ELF file (objdump -T).
function glibcSymbolVersion(binaryPath) {
  const objdump = spawnSync("objdump", ["-T", binaryPath], { encoding: "utf8" });
  if (objdump.status !== 0) fail(`objdump -T failed for ${binaryPath}: ${objdump.stderr}`);
  return maxGlibcSymbolVersion(objdump.stdout);
}

function isElfPayloadName(name) {
  return name.endsWith(".node") || name.endsWith(".so") || name.includes(".so.");
}

// Source-level Linux ABI assessment over every staged ELF payload: the GPUI
// binary, the bundled Node runtime, each native addon (.node) and each staged
// shared object. NEEDED entries resolving inside the bundle are excluded from
// Depends; system ones feed deb/rpm Depends. Musl-linked addon variants are
// recorded but skipped: they cannot load on the glibc targets we package for.
// Versioned glibc requirements are enforced against LINUX_GLIBC_BASELINE before
// artifacts are generated; the measured maximum also versions deb/rpm Depends.
// The result is recorded so each target distro baseline can be checked; a
// container smoke on one distro does not establish broader ABI compatibility.
async function assessBundleAbi(bundleDir) {
  const elfFiles = [];
  const walk = async (dir) => {
    for (const entry of await fsPromises.readdir(dir, { withFileTypes: true })) {
      const entryPath = path.join(dir, entry.name);
      if (entry.isDirectory()) await walk(entryPath);
      else if (entry.isFile() && isElfPayloadName(entry.name))
        elfFiles.push(path.relative(bundleDir, entryPath));
    }
  };
  await walk(bundleDir);
  elfFiles.push("paseo-desktop", `daemon/${nodeBinaryRelativePath("linux")}`);
  elfFiles.sort();

  const bundledNames = new Set(
    elfFiles.map((relative) => path.basename(relative)).filter((name) => name !== "node"),
  );
  const systemLibraries = new Set();
  const manifestEntries = [];
  const skippedMusl = [];
  let maxGlibcVersion = null;
  for (const relative of elfFiles) {
    const needed = neededSharedLibraries(path.join(bundleDir, relative));
    if (needed.some((library) => library.includes("musl"))) {
      skippedMusl.push(relative);
      continue;
    }
    const classification = classifyNeededLibraries(needed, [...bundledNames]);
    for (const library of classification.system) systemLibraries.add(library);
    const glibcVersion = glibcSymbolVersion(path.join(bundleDir, relative));
    if (
      glibcVersion &&
      (!maxGlibcVersion || compareGlibcVersions(glibcVersion, maxGlibcVersion) > 0)
    ) {
      maxGlibcVersion = glibcVersion;
    }
    manifestEntries.push({
      path: relative,
      systemLibraries: classification.system.sort(),
      bundledLibraries: classification.bundled.sort(),
      maxGlibcVersion: glibcVersion,
    });
  }
  return {
    systemLibraries: [...systemLibraries].sort(),
    maxGlibcVersion,
    manifest: {
      note:
        "Source-level ABI assessment of every staged ELF payload (GPUI binary, bundled Node, " +
        "native addons, staged .so files). System libraries drive deb/rpm Depends; bundled ones " +
        "resolve inside the payload. Versioned symbol requirements above the declared Linux " +
        "baseline fail packaging before artifacts; deb/rpm carry the measured glibc minimum.",
      maxGlibcVersion,
      skippedMuslPayload: skippedMusl.sort(),
      files: manifestEntries,
    },
  };
}

async function stageAppBundle(options) {
  const { platform, arch } = options;
  const stageDir = path.join(options.out, "stage");
  const bundleDir = path.join(stageDir, BUNDLE_NAME);
  await fsPromises.rm(stageDir, { recursive: true, force: true });
  await fsPromises.mkdir(bundleDir, { recursive: true });

  const cargoTargetDir =
    process.env.CARGO_TARGET_DIR ?? path.join(repoRoot, ".dev", "native-cargo-target");
  const binaryName = platform === "win32" ? "paseo-desktop.exe" : "paseo-desktop";
  const binarySource = path.join(cargoTargetDir, "release", binaryName);
  assertFile(
    binarySource,
    "build the GPUI client first or clear --skip-cargo-build so the pipeline runs cargo",
  );
  await fsPromises.copyFile(binarySource, path.join(bundleDir, binaryName));
  if (platform !== "win32") fs.chmodSync(path.join(bundleDir, binaryName), 0o755);

  await fsPromises.writeFile(
    path.join(bundleDir, platform === "win32" ? "PaseoNative.cmd" : "paseo"),
    appLauncherScript(platform),
  );
  await fsPromises.mkdir(path.join(bundleDir, "bin"), { recursive: true });
  await fsPromises.writeFile(
    path.join(bundleDir, platform === "win32" ? "bin/paseo.cmd" : "bin/paseo"),
    cliShimScript(platform),
  );
  if (platform !== "win32") {
    // Newly written files carry no execute bit by default; the desktop entry,
    // AppRun and everything that execs the launcher depend on this.
    fs.chmodSync(path.join(bundleDir, "paseo"), 0o755);
    fs.chmodSync(path.join(bundleDir, "bin/paseo"), 0o755);
    fs.chmodSync(path.join(bundleDir, "paseo-desktop"), 0o755);
  }

  const cacheDir =
    process.env.PASEO_NATIVE_NODE_CACHE ?? path.join(repoRoot, ".dev", "native-node-cache");
  await stageDaemonRuntime(bundleDir, platform, arch, cacheDir);

  let sharedLibraries = [];
  let maxGlibcVersion = null;
  if (platform === "linux") {
    const abiAssessment = await assessBundleAbi(bundleDir);
    assertGlibcWithinBaseline(abiAssessment.maxGlibcVersion);
    sharedLibraries = abiAssessment.systemLibraries;
    maxGlibcVersion = abiAssessment.maxGlibcVersion;
    await fsPromises.writeFile(
      path.join(bundleDir, "daemon", "native-libraries.txt"),
      `${sharedLibraries.join("\n")}\n`,
    );
    await fsPromises.writeFile(
      path.join(bundleDir, "daemon", "native-addons.json"),
      `${JSON.stringify(abiAssessment.manifest, null, 2)}\n`,
    );
    if (abiAssessment.maxGlibcVersion) {
      console.log(
        `package-native: linux ABI gate — bundle references glibc symbols up to ${abiAssessment.maxGlibcVersion}; ` +
          `within declared ${LINUX_GLIBC_BASELINE.distro} baseline (glibc ${LINUX_GLIBC_BASELINE.glibc})`,
      );
    }
    await fsPromises.mkdir(path.join(bundleDir, "share", "applications"), { recursive: true });
    await fsPromises.mkdir(path.join(bundleDir, "share", "icons", "hicolor", "256x256", "apps"), {
      recursive: true,
    });
    await fsPromises.writeFile(
      path.join(bundleDir, "share", "applications", "paseo-native.desktop"),
      linuxDesktopEntry(),
    );
    await fsPromises.copyFile(
      path.join(desktopRoot, "assets", "128x128@2x.png"),
      path.join(bundleDir, "share", "icons", "hicolor", "256x256", "apps", "paseo-native.png"),
    );
  }

  await verifyStagedBundle(bundleDir, platform);
  return {
    stageDir,
    bundleDir,
    sharedLibraries,
    maxGlibcVersion,
  };
}

async function verifyStagedBundle(bundleDir, platform) {
  for (const relative of requiredBundlePaths(platform)) {
    assertFile(path.join(bundleDir, relative));
  }
  if (platform === "win32") {
    console.log("package-native: structural verification only (batch scripts cannot run here)");
    return;
  }
  // Execute bits on the real staged assembly, not just existence: a missing
  // chmod here would only surface as EACCES after install.
  assertPosixExecutable(bundleDir, ["paseo-desktop", "paseo", "bin/paseo"]);
  const node = path.join(bundleDir, "daemon", nodeBinaryRelativePath(platform));
  const nodeVersion = spawnSync(node, ["--version"], { encoding: "utf8" });
  if (nodeVersion.error) fail(`Bundled Node runtime could not run: ${nodeVersion.error}`);
  if (nodeVersion.status !== 0 || nodeVersion.stdout?.trim() !== `v${NATIVE_NODE_VERSION}`) {
    fail(
      `Bundled Node runtime check failed: status=${nodeVersion.status} stdout=${nodeVersion.stdout} stderr=${nodeVersion.stderr}`,
    );
  }
  for (const script of ["paseo", "bin/paseo"]) {
    run("sh", ["-n", path.join(bundleDir, script)]);
  }
  const shim = spawnSync(path.join(bundleDir, "bin", "paseo"), ["--version"], { encoding: "utf8" });
  if (shim.status !== 0 || !shim.stdout.trim()) {
    fail(`Bundled CLI shim check failed: ${shim.status} ${shim.stderr}`);
  }
  console.log(`package-native: bundled CLI --version → ${shim.stdout.trim().split("\n")[0]}`);
}

function debArtifactPath(options, version) {
  return path.join(
    options.out,
    "artifacts",
    `paseo-native_${debPackageVersion(version)}_${DEB_ARCH[options.arch] ?? options.arch}.deb`,
  );
}

// dpkg-deb is not present everywhere (Arch dev machines); assemble the .deb by
// hand: debian-binary + control.tar.gz + data.tar.xz members, in that order.
async function assembleDeb(options, version, bundleDir, depends, maxGlibcVersion) {
  const debRoot = path.join(options.out, "stage", "deb");
  await fsPromises.rm(debRoot, { recursive: true, force: true });
  const controlDir = path.join(debRoot, "control");
  const dataDir = path.join(debRoot, "data");
  await fsPromises.mkdir(controlDir, { recursive: true });

  const prefix = path.join(dataDir, "opt", "PaseoNative");
  await fsPromises.mkdir(path.dirname(prefix), { recursive: true });
  await fsPromises.cp(bundleDir, prefix, { recursive: true });
  await fsPromises.mkdir(path.join(dataDir, "usr", "bin"), { recursive: true });
  await fsPromises.symlink(
    "/opt/PaseoNative/paseo",
    path.join(dataDir, "usr", "bin", "paseo-native"),
  );
  await fsPromises.symlink(
    "/opt/PaseoNative/bin/paseo",
    path.join(dataDir, "usr", "bin", "paseo-native-cli"),
  );
  await fsPromises.mkdir(path.join(dataDir, "usr", "share", "applications"), { recursive: true });
  await fsPromises.cp(
    path.join(bundleDir, "share", "applications", "paseo-native.desktop"),
    path.join(dataDir, "usr", "share", "applications", "paseo-native.desktop"),
  );
  await fsPromises.mkdir(
    path.join(dataDir, "usr", "share", "icons", "hicolor", "256x256", "apps"),
    {
      recursive: true,
    },
  );
  await fsPromises.cp(
    path.join(bundleDir, "share", "icons", "hicolor", "256x256", "apps", "paseo-native.png"),
    path.join(dataDir, "usr", "share", "icons", "hicolor", "256x256", "apps", "paseo-native.png"),
  );

  let installedBytes = 0;
  const md5Lines = [];
  const walk = async (dir) => {
    for (const entry of await fsPromises.readdir(dir, { withFileTypes: true })) {
      const entryPath = path.join(dir, entry.name);
      if (entry.isDirectory()) {
        await walk(entryPath);
        continue;
      }
      if (entry.isSymbolicLink()) continue;
      const contents = await fsPromises.readFile(entryPath);
      installedBytes += contents.byteLength;
      md5Lines.push(
        `${createHash("md5").update(contents).digest("hex")}  ${path.relative(dataDir, entryPath)}`,
      );
    }
  };
  await walk(dataDir);

  await fsPromises.writeFile(
    path.join(controlDir, "control"),
    debControl({
      version,
      arch: options.arch,
      depends,
      installedSizeKb: Math.ceil(installedBytes / 1024),
      maxGlibcVersion,
    }),
  );
  await fsPromises.writeFile(path.join(controlDir, "md5sums"), `${md5Lines.sort().join("\n")}\n`);

  const tarArgs = ["--sort=name", "--owner=root:0", "--group=root:0", "--mtime=@0", "-c"];
  run("tar", [...tarArgs, "-z", "-f", path.join(debRoot, "control.tar.gz"), "-C", controlDir, "."]);
  const dataMember = commandExists("xz") ? "data.tar.xz" : "data.tar.gz";
  run("tar", [
    ...tarArgs,
    ...(dataMember.endsWith(".xz") ? ["-J"] : ["-z"]),
    "-f",
    path.join(debRoot, dataMember),
    "-C",
    dataDir,
    ".",
  ]);
  const debPath = debArtifactPath(options, version);
  await fsPromises.writeFile(path.join(debRoot, "debian-binary"), "2.0\n");
  await fsPromises.rm(debPath, { force: true });
  run("ar", [
    "rc",
    debPath,
    path.join(debRoot, "debian-binary"),
    path.join(debRoot, "control.tar.gz"),
    path.join(debRoot, dataMember),
  ]);
  return debPath;
}

const RPM_ARCH = { x64: "x86_64", arm64: "aarch64" };

function rpmSpec({ version, depends, maxGlibcVersion }) {
  const requires = rpmRequiresFromDebPackages(depends, { maxGlibcVersion });
  return `Name:           paseo-native
Version:        ${debPackageVersion(version)}
Release:        1%{?dist}
Summary:        Paseo native (GPUI) desktop client (opt-in)
License:        Apache-2.0
URL:            https://paseo.sh
AutoReqProv:    no
Requires:       ${requires.join(", ")}

%description
Opt-in native desktop client packaged separately from the Electron Paseo
desktop app. Bundles its own Node runtime and daemon. Shared library
dependencies are derived from the packaged binary's NEEDED entries.

%install
rm -rf %{buildroot}
mkdir -p %{buildroot}/opt %{buildroot}%{_bindir} %{buildroot}%{_datadir}/applications
cp -a "%{paseo_stage}" %{buildroot}/opt/PaseoNative
ln -s /opt/PaseoNative/paseo %{buildroot}%{_bindir}/paseo-native
ln -s /opt/PaseoNative/bin/paseo %{buildroot}%{_bindir}/paseo-native-cli
mkdir -p %{buildroot}%{_datadir}/icons/hicolor/256x256/apps
cp "%{paseo_stage}/share/icons/hicolor/256x256/apps/paseo-native.png" \\
  %{buildroot}%{_datadir}/icons/hicolor/256x256/apps/paseo-native.png
cp "%{paseo_stage}/share/applications/paseo-native.desktop" \\
  %{buildroot}%{_datadir}/applications/paseo-native.desktop

%files
/opt/PaseoNative
%{_bindir}/paseo-native
%{_bindir}/paseo-native-cli
%{_datadir}/applications/paseo-native.desktop
%{_datadir}/icons/hicolor/256x256/apps/paseo-native.png

%changelog
* Sun Nov 02 2025 Paseo packaging <hello@moboudra.com> - ${debPackageVersion(version)}-1
- Opt-in native GPUI client package (automated spec).
`;
}

function assembleRpm(options, version, bundleDir, maxGlibcVersion) {
  if (!commandExists("rpmbuild")) {
    console.log("package-native: rpm SKIPPED (rpmbuild not installed; install the 'rpm' package)");
    return null;
  }
  const rpmTop = path.join(options.out, "stage", "rpm");
  fs.rmSync(rpmTop, { recursive: true, force: true });
  fs.mkdirSync(path.join(rpmTop, "SPECS"), { recursive: true });
  const depends = debDependsFromLibraries(sharedLibrariesFor(bundleDir));
  fs.writeFileSync(
    path.join(rpmTop, "SPECS", "paseo-native.spec"),
    rpmSpec({ version, depends, maxGlibcVersion }),
  );
  const rpmArch = RPM_ARCH[options.arch] ?? options.arch;
  run("rpmbuild", [
    "-bb",
    "--define",
    `_topdir ${rpmTop}`,
    "--define",
    `paseo_stage ${bundleDir}`,
    "--target",
    `${rpmArch}-linux`,
    path.join(rpmTop, "SPECS", "paseo-native.spec"),
  ]);
  const built = [];
  const collect = (dir) => {
    for (const entry of fs.readdirSync(dir, { withFileTypes: true })) {
      const entryPath = path.join(dir, entry.name);
      if (entry.isDirectory()) collect(entryPath);
      else if (entry.name.endsWith(".rpm")) built.push(entryPath);
    }
  };
  collect(rpmTop);
  if (built.length !== 1) fail(`Expected one built rpm, found: ${built.join(", ")}`);
  const rpmPath = path.join(
    options.out,
    "artifacts",
    `paseo-native-${debPackageVersion(version)}-1.${rpmArch}.rpm`,
  );
  fs.mkdirSync(path.dirname(rpmPath), { recursive: true });
  fs.renameSync(built[0], rpmPath);
  return rpmPath;
}

function sharedLibrariesFor(bundleDir) {
  return fs
    .readFileSync(path.join(bundleDir, "daemon", "native-libraries.txt"), "utf8")
    .split("\n")
    .filter(Boolean);
}

// Assemble the AppImage with the pinned linuxdeploy release. The staged bundle
// lives under AppDir/usr so the launcher's relative paths keep working; AppRun
// execs the launcher.
function assembleAppImage(options, version, bundleDir, cacheDir) {
  const archKey = options.arch;
  const toolName =
    archKey === "arm64" ? "linuxdeploy-aarch64.AppImage" : "linuxdeploy-x86_64.AppImage";
  const expectedSha256 = LINUXDEPLOY_SHA256[archKey];
  if (!expectedSha256) fail(`No pinned linuxdeploy checksum for ${archKey}`);
  fs.mkdirSync(cacheDir, { recursive: true });
  const toolPath = path.join(cacheDir, toolName);
  if (!fs.existsSync(toolPath)) {
    run("curl", [
      "-fsSL",
      "-o",
      toolPath,
      `https://github.com/linuxdeploy/linuxdeploy/releases/download/${LINUXDEPLOY_RELEASE_TAG}/${toolName}`,
    ]);
  }
  const actualSha256 = createHash("sha256").update(fs.readFileSync(toolPath)).digest("hex");
  if (actualSha256 !== expectedSha256) {
    fail(`linuxdeploy checksum mismatch: expected ${expectedSha256}, got ${actualSha256}`);
  }
  fs.chmodSync(toolPath, 0o755);

  const appDir = path.join(options.out, "stage", "appimage", "PaseoNative.AppDir");
  fs.rmSync(path.dirname(appDir), { recursive: true, force: true });
  const usrDir = path.join(appDir, "usr");
  fs.mkdirSync(usrDir, { recursive: true });
  fs.cpSync(bundleDir, usrDir, { recursive: true });
  fs.writeFileSync(
    path.join(appDir, "AppRun"),
    `#!/bin/sh
# Generated by packages/desktop/scripts/package-native.mjs. Do not edit.
exec "$APPDIR/usr/paseo" "$@"
`,
  );
  fs.chmodSync(path.join(appDir, "AppRun"), 0o755);
  fs.copyFileSync(
    path.join(usrDir, "share", "applications", "paseo-native.desktop"),
    path.join(appDir, "paseo-native.desktop"),
  );
  // Only the root desktop file may exist or linuxdeploy emits one AppImage
  // per desktop entry.
  fs.rmSync(path.join(usrDir, "share", "applications", "paseo-native.desktop"), { force: true });
  fs.copyFileSync(
    path.join(usrDir, "share", "icons", "hicolor", "256x256", "apps", "paseo-native.png"),
    path.join(appDir, "paseo-native.png"),
  );
  fs.copyFileSync(path.join(appDir, "paseo-native.png"), path.join(appDir, ".DirIcon"));

  // Explicitly hand the pinned runtime to appimagetool via the plugin's
  // LDAI_RUNTIME_FILE pass-through; without it appimagetool downloads the
  // mutable continuous runtime itself.
  const runtimePath = verifyAppImageRuntime(archKey, cacheDir);
  const artifactsDir = path.join(options.out, "artifacts");
  fs.mkdirSync(artifactsDir, { recursive: true });
  run(toolPath, ["--appimage-extract-and-run", "--appdir", appDir, "--output", "appimage"], {
    cwd: artifactsDir,
    env: {
      ARCH: options.arch === "arm64" ? "arm_aarch64" : "x86_64",
      [APPIMAGE_RUNTIME_ENV_VAR]: runtimePath,
    },
  });
  const candidates = fs.readdirSync(artifactsDir).filter((file) => file.endsWith(".AppImage"));
  if (candidates.length !== 1) fail(`Expected one AppImage output, found: ${candidates}`);
  const appImagePath = path.join(
    artifactsDir,
    `${artifactBaseName({ ...options, version })}.AppImage`,
  );
  fs.renameSync(path.join(artifactsDir, candidates[0]), appImagePath);
  return appImagePath;
}

// Download and hash-verify the pinned type-2 AppImage runtime. Without this
// explicit pass, appimagetool (bundled inside the pinned linuxdeploy release)
// downloads the mutable `continuous` runtime itself and embeds it — pinning
// linuxdeploy alone never pins the final AppImage.
function verifyAppImageRuntime(archKey, cacheDir) {
  const url = APPIMAGE_RUNTIME_URL[archKey];
  const expectedSha256 = APPIMAGE_RUNTIME_SHA256[archKey];
  if (!url || !expectedSha256) fail(`No pinned AppImage runtime for ${archKey}`);
  fs.mkdirSync(cacheDir, { recursive: true });
  const runtimePath = path.join(cacheDir, path.basename(url));
  if (!fs.existsSync(runtimePath)) {
    run("curl", ["-fsSL", "-o", runtimePath, url]);
  }
  const actualSha256 = createHash("sha256").update(fs.readFileSync(runtimePath)).digest("hex");
  if (actualSha256 !== expectedSha256) {
    fail(`AppImage runtime checksum mismatch: expected ${expectedSha256}, got ${actualSha256}`);
  }
  return runtimePath;
}

function verifyMacApp(appDir) {
  for (const relative of requiredMacAppPaths()) {
    assertFile(path.join(appDir, relative));
  }
  run("sh", ["-n", path.join(appDir, "Contents", "MacOS", "PaseoNative")]);
  run("sh", ["-n", path.join(appDir, "Contents", "Resources", "bin", "paseo")]);
  // The final .app launcher must resolve the daemon under Resources: prove it
  // with fake runtime binaries at the exact post-assembly paths.
  const node = path.join(appDir, "Contents", "Resources", "daemon", "node", "bin", "node");
  const nodeVersion = spawnSync(node, ["--version"], { encoding: "utf8" });
  if (nodeVersion.status !== 0 || nodeVersion.stdout?.trim() !== `v${NATIVE_NODE_VERSION}`) {
    fail(`Assembled .app bundled Node check failed: ${nodeVersion.status} ${nodeVersion.stderr}`);
  }
}

async function assembleMacApp(options, version, stageDir, bundleDir) {
  const artifactsDir = path.join(options.out, "artifacts");
  fs.mkdirSync(artifactsDir, { recursive: true });
  const appDir = path.join(stageDir, "Paseo Native.app");
  const contents = path.join(appDir, "Contents");
  await fsPromises.mkdir(path.join(contents, "MacOS"), { recursive: true });
  await fsPromises.mkdir(path.join(contents, "Resources"), { recursive: true });
  await fsPromises.rename(
    path.join(bundleDir, "paseo-desktop"),
    path.join(contents, "MacOS", "paseo-desktop"),
  );
  await fsPromises.rename(
    path.join(bundleDir, "paseo"),
    path.join(contents, "MacOS", "PaseoNative"),
  );
  await fsPromises.rename(path.join(bundleDir, "bin"), path.join(contents, "Resources", "bin"));
  await fsPromises.rename(
    path.join(bundleDir, "daemon"),
    path.join(contents, "Resources", "daemon"),
  );
  await fsPromises.copyFile(
    path.join(desktopRoot, "assets", "icon.icns"),
    path.join(contents, "Resources", "PaseoNative.icns"),
  );
  await fsPromises.writeFile(path.join(contents, "Info.plist"), infoPlist({ version }));
  fs.chmodSync(path.join(contents, "MacOS", "PaseoNative"), 0o755);
  fs.chmodSync(path.join(contents, "MacOS", "paseo-desktop"), 0o755);

  verifyMacApp(appDir);

  const zipPath = path.join(artifactsDir, `${artifactBaseName({ ...options, version })}.zip`);
  await fsPromises.rm(zipPath, { force: true });
  run("zip", ["-qry", zipPath, "Paseo Native.app"], { cwd: stageDir });

  // Unsigned dmg; signing and notarization require credentials and remain
  // explicit gates (never attempted here).
  let dmgPath = null;
  if (commandExists("hdiutil")) {
    dmgPath = path.join(artifactsDir, `${artifactBaseName({ ...options, version })}.dmg`);
    fs.rmSync(dmgPath, { force: true });
    run("hdiutil", [
      "create",
      "-volname",
      "Paseo Native",
      "-srcfolder",
      appDir,
      "-ov",
      "-format",
      "UDZO",
      dmgPath,
    ]);
  } else {
    console.log("package-native: dmg SKIPPED (hdiutil not available)");
  }
  return { zipPath, dmgPath };
}

function assembleWindowsZip(options, version, bundleDir) {
  const artifactsDir = path.join(options.out, "artifacts");
  fs.mkdirSync(artifactsDir, { recursive: true });
  fs.copyFileSync(
    path.join(desktopRoot, "assets", "icon.ico"),
    path.join(bundleDir, "PaseoNative.ico"),
  );
  const zipPath = path.join(artifactsDir, `${artifactBaseName({ ...options, version })}.zip`);
  fs.rmSync(zipPath, { force: true });
  run("powershell.exe", [
    "-NoProfile",
    "-Command",
    `Compress-Archive -Path '${escapePowerShellSingleQuoted(bundleDir)}\\*' -DestinationPath '${escapePowerShellSingleQuoted(zipPath)}' -Force`,
  ]);
  return zipPath;
}

// NSIS setup built when makensis exists (windows-latest runners ship it).
// Unsigned; code signing requires credentials and remains an explicit gate.
// Uninstall removes exactly the installed manifest files and then removes
// directories non-recursively, so a user who pointed the directory page at an
// existing folder never loses pre-existing files (the sentinel guarantee).
function assembleWindowsSetup(options, version, stageDir) {
  if (!commandExists("makensis")) {
    console.log("package-native: NSIS setup SKIPPED (makensis not available)");
    return null;
  }
  const artifactsDir = path.join(options.out, "artifacts");
  fs.mkdirSync(artifactsDir, { recursive: true });
  const setupPath = path.join(
    artifactsDir,
    `${artifactBaseName({ ...options, version })}-setup.exe`,
  );
  const nsiPath = path.join(options.out, "stage", "paseo-native.nsi");
  const staged = path.join(stageDir, BUNDLE_NAME);
  const { files, directories } = listStagedTree(staged);
  fs.writeFileSync(
    nsiPath,
    `; Generated by packages/desktop/scripts/package-native.mjs. Do not edit.
!define APPNAME "Paseo Native"
Name "\${APPNAME}"
OutFile "${escapeNsisString(setupPath)}"
InstallDir "$LOCALAPPDATA\\PaseoNative"
RequestExecutionLevel user
Page directory
Page instfiles
Section "install"
  SetOutPath "$INSTDIR"
  File /r "${escapeNsisString(staged)}\\*.*"
  CreateShortcut "$DESKTOP\\Paseo Native.lnk" "$INSTDIR\\PaseoNative.cmd"
  WriteUninstaller "$INSTDIR\\uninstall.exe"
SectionEnd
Section "uninstall"
  ; Leave $INSTDIR as the current directory would block deletion on Windows.
  SetOutPath "$TEMP"
${nsisUninstallLines({ files, directories }).join("\n")}
  RMDir "$INSTDIR"
  Delete "$DESKTOP\\Paseo Native.lnk"
SectionEnd
`,
  );
  run("makensis", [nsiPath]);
  return setupPath;
}

async function buildArtifacts(options, version, stageInfo) {
  const { stageDir, bundleDir, sharedLibraries, maxGlibcVersion } = stageInfo;
  const artifactsDir = path.join(options.out, "artifacts");
  fs.mkdirSync(artifactsDir, { recursive: true });
  const tarPath = path.join(artifactsDir, `${artifactBaseName({ ...options, version })}.tar.gz`);
  fs.rmSync(tarPath, { force: true });
  run("tar", ["-czf", tarPath, "-C", stageDir, BUNDLE_NAME]);
  const depends = debDependsFromLibraries(sharedLibraries);
  console.log(
    `package-native: deb/rpm Depends derived from NEEDED + dlopened libs: ${depends.join(", ")}`,
  );
  return (
    await Promise.all([
      tarPath,
      assembleDeb(options, version, bundleDir, depends, maxGlibcVersion),
      assembleRpm(options, version, bundleDir, maxGlibcVersion),
      assembleAppImage(options, version, bundleDir, path.join(repoRoot, ".dev", "tools")),
    ])
  ).filter(Boolean);
}

// Every file and directory the staged bundle would install, relative to the
// bundle root. Backbones the NSIS uninstall manifest: only these paths are
// ever deleted.
function listStagedTree(rootDir) {
  const files = [];
  const directories = [];
  const walk = (dir) => {
    for (const entry of fs.readdirSync(dir, { withFileTypes: true })) {
      const relative = path
        .relative(rootDir, path.join(dir, entry.name))
        .split(path.sep)
        .join("\\");
      if (entry.isDirectory()) {
        directories.push(relative);
        walk(path.join(dir, entry.name));
      } else if (entry.isFile()) {
        files.push(relative);
      }
    }
  };
  walk(rootDir);
  return { files, directories };
}

async function main() {
  const options = parseArgs(process.argv.slice(2));
  if (options.platform !== process.platform) {
    fail(
      `Requested --platform ${options.platform} differs from the host (${process.platform}); native packaging does not cross-bundle.`,
    );
  }
  if (!["linux", "darwin", "win32"].includes(options.platform))
    fail(`Unsupported platform ${options.platform}`);

  const version = JSON.parse(fs.readFileSync(path.join(repoRoot, "package.json"), "utf8")).version;
  console.log(
    `package-native: packaging paseo-native ${version} for ${options.platform}/${options.arch}`,
  );
  // Fresh artifact set each run: stale outputs must never be confused with
  // this build's artifacts, and stage/ is never part of the artifact set.
  await fsPromises.rm(path.join(options.out, "artifacts"), { recursive: true, force: true });
  await fsPromises.mkdir(path.join(options.out, "artifacts"), { recursive: true });
  await fsPromises.mkdir(path.join(options.out, "artifacts"), { recursive: true });

  if (!options.skipDaemonDistCheck) {
    assertFile(
      path.join(repoRoot, "packages", "server", "dist", "scripts", "supervisor-entrypoint.js"),
      "run npm run build:server first",
    );
    assertFile(
      path.join(repoRoot, "packages", "cli", "dist", "index.js"),
      "run npm run build:server first",
    );
  }

  console.log("package-native: refreshing generated native UI assets");
  runNpm(["run", "--silent", "sync:desktop:native-ui"]);

  if (!options.skipCargoBuild) {
    console.log(
      "package-native: building the GPUI client (isolated cargo target, CARGO_BUILD_JOBS=1 unless overridden)",
    );
    run(
      "cargo",
      ["build", "--locked", "--release", "-p", "paseo-desktop", "--bin", "paseo-desktop"],
      {
        env: {
          CARGO_TARGET_DIR:
            process.env.CARGO_TARGET_DIR ?? path.join(repoRoot, ".dev", "native-cargo-target"),
          CARGO_BUILD_JOBS: process.env.CARGO_BUILD_JOBS ?? "1",
        },
      },
    );
  }

  const stageInfo = await stageAppBundle(options);
  let artifacts = [];
  if (options.platform === "linux") {
    artifacts = await buildArtifacts(options, version, stageInfo);
  } else if (options.platform === "darwin") {
    const { zipPath, dmgPath } = await assembleMacApp(
      options,
      version,
      stageInfo.stageDir,
      stageInfo.bundleDir,
    );
    artifacts.push(zipPath);
    if (dmgPath) artifacts.push(dmgPath);
  } else {
    artifacts.push(assembleWindowsZip(options, version, stageInfo.bundleDir));
    const setup = assembleWindowsSetup(options, version, stageInfo.stageDir);
    if (setup) artifacts.push(setup);
  }

  console.log("package-native: artifacts");
  for (const artifact of artifacts) {
    const size = fs.statSync(artifact).size;
    console.log(`  ${artifact} (${(size / 1024 / 1024).toFixed(1)} MB)`);
  }
  console.log(
    "package-native: staged bundle kept at",
    stageInfo.stageDir,
    "(never part of artifacts)",
  );
  const gates = {
    linux:
      "Gate: GUI interaction under a real display remains unverified; code signing not attempted (no credentials).",
    darwin:
      "Gate: unsigned, unnotarized — codesign, notarization and GUI interaction remain explicit gates (no credentials).",
    win32:
      "Gate: unsigned — code signing and GUI interaction remain explicit gates (no credentials).",
  };
  console.log(`package-native: ${gates[options.platform]}`);
}

main().catch((error) => {
  fail(error?.stack ?? error?.message ?? String(error));
});
