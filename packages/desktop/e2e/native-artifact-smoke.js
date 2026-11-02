// Smoke the opt-in native GPUI desktop artifacts produced by
// scripts/package-native.mjs. Run against the packaging output's artifacts/
// directory. Linux gets the full path: portable tar.gz layout, bundled Node
// runtime and CLI shim, an isolated-home daemon bootstrap on a non-default
// port (never touches a production daemon), app-launcher liveness when a
// display or Xvfb exists, and deb payload verification. macOS verifies the
// final .app structure and runs the bundled CLI; Windows verifies the
// portable layout and CLI shim. GUI interaction, signed installers, rpm,
// AppImage install behavior, dmg and NSIS remain explicit unpackaged gates.
// See docs/desktop-gpui.md for the acceptance bar.
const { execFileSync, spawn, spawnSync } = require("node:child_process");
const fs = require("node:fs");
const net = require("node:net");
const os = require("node:os");
const path = require("node:path");

const BUNDLE_LAYOUT = [
  "paseo-desktop",
  "paseo",
  "bin/paseo",
  "daemon/node/bin/node",
  "daemon/node_modules/@getpaseo/cli/dist/index.js",
  "daemon/node_modules/@getpaseo/server/dist/scripts/supervisor-entrypoint.js",
  "daemon/runtime.json",
  "daemon/external-dependencies.json",
  "daemon/native-addons.json",
  "daemon/native-libraries.txt",
  "share/applications/paseo-native.desktop",
  "share/icons/hicolor/256x256/apps/paseo-native.png",
];

const MAC_APP_LAYOUT = [
  "Contents/Info.plist",
  "Contents/MacOS/PaseoNative",
  "Contents/MacOS/paseo-desktop",
  "Contents/Resources/bin/paseo",
  "Contents/Resources/PaseoNative.icns",
  "Contents/Resources/daemon/node/bin/node",
  "Contents/Resources/daemon/node_modules/@getpaseo/cli/dist/index.js",
  "Contents/Resources/daemon/node_modules/@getpaseo/server/dist/scripts/supervisor-entrypoint.js",
  "Contents/Resources/daemon/runtime.json",
  "Contents/Resources/daemon/external-dependencies.json",
];

const WINDOWS_LAYOUT = [
  "paseo-desktop.exe",
  "PaseoNative.cmd",
  "bin/paseo.cmd",
  "PaseoNative.ico",
  "daemon/node/node.exe",
  "daemon/node_modules/@getpaseo/cli/dist/index.js",
  "daemon/node_modules/@getpaseo/server/dist/scripts/supervisor-entrypoint.js",
  "daemon/runtime.json",
  "daemon/external-dependencies.json",
];

function findArtifact(release, suffix) {
  const matches = fs.readdirSync(release).filter((file) => file.endsWith(suffix));
  if (matches.length !== 1) throw new Error(`Expected one ${suffix} in ${release}: ${matches}`);
  return path.join(release, matches[0]);
}

function assertBundledRuntime(daemonDir) {
  const runtime = JSON.parse(fs.readFileSync(path.join(daemonDir, "runtime.json"), "utf8"));
  if (runtime.nodeVersion !== "22.20.0") {
    throw new Error(`Unexpected bundled Node version: ${runtime.nodeVersion}`);
  }
  const external = JSON.parse(
    fs.readFileSync(path.join(daemonDir, "external-dependencies.json"), "utf8"),
  );
  if (!Array.isArray(external.packages) || external.packages.length === 0) {
    throw new Error("external-dependencies.json records no bundled packages");
  }
  const nodeRelative = process.platform === "win32" ? "node/node.exe" : "node/bin/node";
  const nodeVersion = execFileSync(path.join(daemonDir, nodeRelative), ["--version"], {
    encoding: "utf8",
  }).trim();
  if (nodeVersion !== `v${runtime.nodeVersion}`) {
    throw new Error(`Bundled node reports ${nodeVersion}, expected v${runtime.nodeVersion}`);
  }
}

function freeLoopbackListen() {
  return new Promise((resolve) => {
    const server = net.createServer();
    server.listen(0, "127.0.0.1", () => {
      const address = server.address();
      server.close(() => resolve(`127.0.0.1:${address.port}`));
    });
  });
}

function runIsolatedDaemon(cliShim, home, listen) {
  const env = { ...process.env, PASEO_HOME: home, PASEO_NODE_ENV: "production" };
  execFileSync(cliShim, ["daemon", "config", "set", "daemon.listen", listen, "--home", home], {
    env,
    stdio: ["ignore", "pipe", "inherit"],
  });
  execFileSync(cliShim, ["daemon", "start", "--home", home, "--timeout", "120"], {
    env,
    stdio: ["ignore", "pipe", "inherit"],
  });
  const pidState = JSON.parse(fs.readFileSync(path.join(home, "paseo.pid"), "utf8"));
  if (pidState.listen !== listen) {
    throw new Error(`Daemon listen ${pidState.listen} did not match requested ${listen}`);
  }
  process.kill(pidState.pid, 0);
  console.log(`native-smoke: isolated daemon up on ${listen} (pid ${pidState.pid})`);
}

function stopIsolatedDaemon(cliShim, home) {
  try {
    execFileSync(cliShim, ["daemon", "stop", "--home", home], {
      env: { ...process.env, PASEO_HOME: home },
      stdio: "ignore",
    });
  } catch (error) {
    console.error(`native-smoke: daemon stop failed: ${error.message}`);
  }
}

// App launcher liveness only — no interaction, no captures.
async function smokeGuiLiveness(bundle, home, diagnostics) {
  const guiLog = path.join(diagnostics, "gui-launch.log");
  const hasDisplay = Boolean(process.env.DISPLAY);
  const hasXvfb = spawnSync("sh", ["-c", "command -v xvfb-run"], { encoding: "utf8" }).status === 0;
  if (!hasDisplay && !hasXvfb) {
    console.log("native-smoke: GUI launcher liveness SKIPPED (no display backend)");
    return false;
  }
  const appCommand = [path.join(bundle, "paseo")];
  const command = hasDisplay ? appCommand : ["xvfb-run", "-a", ...appCommand];
  const logHandle = fs.openSync(guiLog, "a");
  const child = spawn(command[0], command.slice(1), {
    env: { ...process.env, PASEO_HOME: home, PASEO_NODE_ENV: "production", WAYLAND_DISPLAY: "" },
    stdio: ["ignore", logHandle, logHandle],
  });
  const alive = await new Promise((resolve) => {
    setTimeout(() => resolve(child.exitCode === null && child.signalCode === null), 8000);
  });
  child.kill("SIGTERM");
  if (!alive) {
    throw new Error(
      `Native launcher exited within 8s; see ${guiLog}. GUI liveness is a required gate.`,
    );
  }
  console.log("native-smoke: app launcher stayed alive for 8s (liveness only)");
  return true;
}

function smokeDeb(release, extracted) {
  const debPath = findArtifact(release, ".deb");
  const debDir = path.join(extracted, "deb");
  fs.mkdirSync(debDir);
  execFileSync("ar", ["x", debPath], { cwd: debDir });
  for (const member of ["debian-binary", "control.tar.gz"]) {
    fs.accessSync(path.join(debDir, member));
  }
  const dataMember = fs.existsSync(path.join(debDir, "data.tar.xz"))
    ? "data.tar.xz"
    : "data.tar.gz";
  const dataDir = path.join(debDir, "data");
  fs.mkdirSync(dataDir);
  execFileSync("tar", ["-xf", path.join(debDir, dataMember), "-C", dataDir]);
  const controlDir = path.join(debDir, "control");
  fs.mkdirSync(controlDir);
  execFileSync("tar", ["-xzf", path.join(debDir, "control.tar.gz"), "-C", controlDir]);
  for (const relative of [
    "opt/PaseoNative/paseo",
    "opt/PaseoNative/paseo-desktop",
    "usr/bin/paseo-native",
    "usr/bin/paseo-native-cli",
    "usr/share/applications/paseo-native.desktop",
  ]) {
    // /usr/bin entries are symlinks into /opt that only resolve after install.
    fs.lstatSync(path.join(dataDir, relative));
  }
  const control = fs.readFileSync(path.join(controlDir, "control"), "utf8");
  if (!/^Package: paseo-native$/m.test(control)) throw new Error("deb control is not paseo-native");
  if (/^Package: paseo$/m.test(control))
    throw new Error("deb must not claim the Electron package name");
  const depends = control.match(/^Depends: (.+)$/m)?.[1] ?? "";
  for (const required of ["libc6", "libxkbcommon0", "libxkbcommon-x11-0", "libxcb1"]) {
    if (!depends.includes(required)) throw new Error(`deb Depends missing ${required}: ${depends}`);
  }
  console.log(
    `native-smoke: deb payload verified (${path.basename(debPath)}; Depends: ${depends})`,
  );
}

function smokeRpm(release, extracted) {
  const rpmPath = findArtifact(release, ".rpm");
  // rpm2cpio needs no package database, unlike `rpm -qpl` on non-rpm hosts.
  const listing = execFileSync("sh", ["-c", 'rpm2cpio "$1" | cpio -it', "smoke", rpmPath], {
    encoding: "utf8",
    cwd: path.join(extracted, "rpm-payload"),
    maxBuffer: 64 * 1024 * 1024,
  });
  for (const expected of [
    "./opt/PaseoNative/paseo",
    "./opt/PaseoNative/paseo-desktop",
    "./usr/bin/paseo-native",
    "./usr/bin/paseo-native-cli",
    "./usr/share/applications/paseo-native.desktop",
  ]) {
    if (!listing.includes(expected)) throw new Error(`rpm payload missing ${expected}`);
  }
  console.log(`native-smoke: rpm payload verified (${path.basename(rpmPath)})`);
}

// AppImage: extract without executing the runtime (no FUSE needed), then apply
// the same bundle layout checks as the tar.gz.
function smokeAppImage(release, extracted) {
  const appImagePath = findArtifact(release, ".AppImage");
  const appImageDir = path.join(extracted, "appimage");
  fs.mkdirSync(appImageDir);
  execFileSync(appImagePath, ["--appimage-extract"], { cwd: appImageDir, stdio: "ignore" });
  const bundle = path.join(appImageDir, "squashfs-root", "usr");
  for (const relative of ["paseo", "paseo-desktop", "bin/paseo", "daemon/runtime.json"]) {
    fs.accessSync(path.join(bundle, relative));
  }
  fs.accessSync(path.join(appImageDir, "squashfs-root", "AppRun"));
  // Run the CLI shim from the extracted payload; the AppRun launcher itself is
  // not executed here because its daemon bootstrap uses the default home.
  const cliVersion = execFileSync(path.join(bundle, "bin", "paseo"), ["--version"], {
    encoding: "utf8",
  }).trim();
  if (!cliVersion) throw new Error("AppImage bundled CLI shim printed no version");
  console.log(
    `native-smoke: AppImage payload verified (${path.basename(appImagePath)}; CLI ${cliVersion.split("\n")[0]})`,
  );
}

async function smokeLinux(release, extracted, diagnostics) {
  execFileSync("tar", ["-xzf", findArtifact(release, ".tar.gz"), "-C", extracted]);
  const bundle = path.join(extracted, "PaseoNative");
  for (const relative of BUNDLE_LAYOUT) {
    fs.accessSync(path.join(bundle, relative));
  }
  fs.accessSync(path.join(bundle, "paseo-desktop"), fs.constants.X_OK);
  // The app launcher and CLI shim leave the packaging pipeline executable;
  // a fresh-stage chmod gap only surfaces as EACCES here.
  fs.accessSync(path.join(bundle, "paseo"), fs.constants.X_OK);
  fs.accessSync(path.join(bundle, "bin/paseo"), fs.constants.X_OK);
  assertBundledRuntime(path.join(bundle, "daemon"));
  const abiManifest = JSON.parse(
    fs.readFileSync(path.join(bundle, "daemon", "native-addons.json"), "utf8"),
  );
  if (!Array.isArray(abiManifest.files) || abiManifest.files.length === 0) {
    throw new Error("native-addons.json records no ABI assessment entries");
  }
  if (typeof abiManifest.maxGlibcVersion !== "string") {
    throw new Error("native-addons.json is missing the bundle's maximum glibc symbol version");
  }
  console.log(
    `native-smoke: ABI manifest covers ${abiManifest.files.length} ELF payloads (max glibc ${abiManifest.maxGlibcVersion}; enforced against the declared Linux baseline at packaging)`,
  );
  const cliVersion = execFileSync(path.join(bundle, "bin/paseo"), ["--version"], {
    encoding: "utf8",
  }).trim();
  if (!cliVersion) throw new Error("Bundled CLI shim printed no version");
  console.log(`native-smoke: bundled CLI ${cliVersion.split("\n")[0]}`);

  const home = fs.mkdtempSync(path.join(os.tmpdir(), "paseo-native-home-"));
  const listen = await freeLoopbackListen();
  let daemonStarted = false;
  let guiRan = false;
  try {
    runIsolatedDaemon(path.join(bundle, "bin/paseo"), home, listen);
    daemonStarted = true;
    guiRan = await smokeGuiLiveness(bundle, home, diagnostics);
    smokeDeb(release, extracted);
    fs.mkdirSync(path.join(extracted, "rpm-payload"), { recursive: true });
    smokeRpm(release, extracted);
    smokeAppImage(release, extracted);
  } finally {
    if (daemonStarted) stopIsolatedDaemon(path.join(bundle, "bin/paseo"), home);
    fs.rmSync(home, { recursive: true, force: true });
  }
  return guiRan;
}

// macOS: verify the final .app (post-rename layout, which is where the
// launcher resolves its daemon), run the bundled CLI, and bootstrap an
// isolated daemon through it. No GUI liveness here: interaction on macOS is a
// separate gate.
async function smokeDarwin(release, extracted) {
  execFileSync("unzip", ["-q", findArtifact(release, ".zip"), "-d", extracted]);
  const app = path.join(extracted, "Paseo Native.app");
  for (const relative of MAC_APP_LAYOUT) {
    fs.accessSync(path.join(app, relative));
  }
  fs.accessSync(path.join(app, "Contents/MacOS/paseo-desktop"), fs.constants.X_OK);
  const resources = path.join(app, "Contents", "Resources");
  assertBundledRuntime(path.join(resources, "daemon"));
  const cliVersion = execFileSync(path.join(resources, "bin", "paseo"), ["--version"], {
    encoding: "utf8",
  }).trim();
  if (!cliVersion) throw new Error("Bundled CLI shim printed no version");
  console.log(`native-smoke: .app bundled CLI ${cliVersion.split("\n")[0]}`);

  const dmgPath = fs.existsSync(release)
    ? fs.readdirSync(release).find((file) => file.endsWith(".dmg"))
    : undefined;
  if (!dmgPath) console.log("native-smoke: dmg SKIPPED (not produced; see packaging gate)");
  else console.log(`native-smoke: dmg present (${dmgPath}); mount/install behavior remains a gate`);

  const home = fs.mkdtempSync(path.join(os.tmpdir(), "paseo-native-home-"));
  const listen = await freeLoopbackListen();
  try {
    runIsolatedDaemon(path.join(resources, "bin", "paseo"), home, listen);
  } finally {
    stopIsolatedDaemon(path.join(resources, "bin", "paseo"), home);
    fs.rmSync(home, { recursive: true, force: true });
  }
  console.log("native-smoke: GUI liveness/interaction SKIPPED on macOS (explicit gate)");
  return false;
}

function escapePowerShellSingleQuoted(value) {
  return value.replace(/'/g, "''");
}

// Windows: verify the portable layout and run the bundled CLI shim through cmd.
// The daemon bootstrap and GUI are explicit gates here.
async function smokeWindows(release, extracted) {
  execFileSync("powershell.exe", [
    "-NoProfile",
    "-Command",
    `Expand-Archive -LiteralPath '${escapePowerShellSingleQuoted(findArtifact(release, ".zip"))}' -DestinationPath '${escapePowerShellSingleQuoted(extracted + "\\PaseoNative")}' -Force`,
  ]);
  const bundle = path.join(extracted, "PaseoNative");
  for (const relative of WINDOWS_LAYOUT) {
    fs.accessSync(path.join(bundle, relative));
  }
  const cliVersion = execFileSync(
    "cmd.exe",
    ["/c", path.join(bundle, "bin", "paseo.cmd"), "--version"],
    {
      encoding: "utf8",
    },
  ).trim();
  if (!cliVersion) throw new Error("Bundled CLI shim printed no version");
  console.log(`native-smoke: bundled CLI ${cliVersion.split("\n")[0]}`);
  const setup = fs.readdirSync(release).find((file) => file.endsWith("-setup.exe"));
  if (!setup) console.log("native-smoke: NSIS setup SKIPPED (not produced; see packaging gate)");
  else
    console.log(`native-smoke: NSIS setup present (${setup}); silent install check remains a gate`);
  console.log("native-smoke: daemon bootstrap and GUI SKIPPED on Windows (explicit gates)");
  return false;
}

async function main() {
  if (process.getuid && process.getuid() === 0) {
    throw new Error("Launch the smoke as an unprivileged user");
  }
  const release = path.resolve(process.argv[2] ?? "packages/desktop/release-native/artifacts");
  const diagnostics =
    process.env.PASEO_NATIVE_SMOKE_ARTIFACT_DIR ?? path.join(os.tmpdir(), "paseo-native-smoke");
  fs.mkdirSync(diagnostics, { recursive: true });
  const extracted = fs.mkdtempSync(path.join(os.tmpdir(), "paseo-native-artifacts-"));
  try {
    let guiRan = false;
    if (process.platform === "linux") guiRan = await smokeLinux(release, extracted, diagnostics);
    else if (process.platform === "darwin") guiRan = await smokeDarwin(release, extracted);
    else if (process.platform === "win32") guiRan = await smokeWindows(release, extracted);
    else throw new Error(`No native artifact smoke for ${process.platform}`);
    console.log(
      `native-smoke: PASS (${process.platform}: layout, runtime, artifacts${
        guiRan ? ", gui liveness" : ", gui gate outstanding"
      })`,
    );
  } finally {
    fs.rmSync(extracted, { recursive: true, force: true });
  }
}

main().catch((error) => {
  console.error(`native-smoke: FAIL: ${error?.message ?? error}`);
  process.exitCode = 1;
});
