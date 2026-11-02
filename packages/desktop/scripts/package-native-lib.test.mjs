import { spawnSync } from "node:child_process";
import { chmodSync, mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { describe, expect, it } from "vitest";
import {
  APPIMAGE_RUNTIME_ENV_VAR,
  APPIMAGE_RUNTIME_SHA256,
  APPIMAGE_RUNTIME_URL,
  LINUX_GLIBC_BASELINE,
  NATIVE_NODE_SHASUMS256,
  NATIVE_NODE_SHASUMS_URL,
  NATIVE_NODE_VERSION,
  appLauncherScript,
  artifactBaseName,
  assertGlibcWithinBaseline,
  assertPosixExecutable,
  classifyNeededLibraries,
  cliShimScript,
  debControl,
  debDependsFromLibraries,
  distCopyFilter,
  escapeNsisString,
  escapePowerShellSingleQuoted,
  infoPlist,
  linuxDesktopEntry,
  maxGlibcSymbolVersion,
  npmCommand,
  nodeDistArchiveName,
  nodeDistUrl,
  nsisUninstallLines,
  planInstalledClosure,
  requiredBundlePaths,
  requiredMacAppPaths,
  resolveInstalledPackage,
  rpmRequiresFromDebPackages,
  workspaceRuntimePackageNames,
} from "./package-native-lib.mjs";

const repoRoot = join(dirname(fileURLToPath(import.meta.url)), "..", "..", "..");

function fakeNodeRecordScript() {
  return `#!/bin/sh
{
  echo "ENV:PASEO_NODE_ENV=\${PASEO_NODE_ENV}"
  printf 'ARG:%s\\n' "$@"
} >> "$PASEO_NATIVE_FAKE_NODE_RECORD"
`;
}

// Fake bundle pieces at the exact layout each launcher template resolves.
function createPosixBundle(root, platform) {
  const recordPath = join(root, "record.log");
  const daemonDir = join(
    root,
    ...(platform === "darwin" ? ["Contents", "Resources", "daemon"] : ["daemon"]),
  );
  const macosDir = platform === "darwin" ? join(root, "Contents", "MacOS") : root;
  mkdirSync(macosDir, { recursive: true });
  mkdirSync(join(daemonDir, "node", "bin"), { recursive: true });
  mkdirSync(join(daemonDir, "node_modules", "@getpaseo", "cli", "dist"), { recursive: true });
  writeFileSync(join(daemonDir, "node_modules", "@getpaseo", "cli", "dist", "index.js"), "");
  const nodeBinary = join(daemonDir, "node", "bin", "node");
  writeFileSync(nodeBinary, fakeNodeRecordScript());
  chmodSync(nodeBinary, 0o755);
  const appBinary = join(macosDir, "paseo-desktop");
  writeFileSync(
    appBinary,
    `#!/bin/sh
{
  echo "APP_EXEC:$*"
} >> "$PASEO_NATIVE_FAKE_NODE_RECORD"
`,
  );
  chmodSync(appBinary, 0o755);
  const launcherName = platform === "darwin" ? "PaseoNative" : "paseo";
  const launcher = join(macosDir, launcherName);
  writeFileSync(launcher, appLauncherScript(platform));
  chmodSync(launcher, 0o755);
  return { recordPath, launcher, appBinary, daemonDir };
}

describe("native packaging runtime pinning", () => {
  it("pins the Node runtime to the repo's .tool-versions nodejs entry", () => {
    const toolVersions = readFileSync(join(repoRoot, ".tool-versions"), "utf8");
    const pinned = toolVersions
      .split("\n")
      .find((line) => line.startsWith("nodejs"))
      ?.split(/\s+/)[1];

    expect(pinned).toBeTruthy();
    expect(NATIVE_NODE_VERSION).toBe(pinned);
  });

  it("covers every desktop platform/arch pair with a pinned checksum and official manifest provenance", () => {
    for (const platform of ["darwin", "linux", "win32"]) {
      for (const arch of ["x64", "arm64"]) {
        const checksum = NATIVE_NODE_SHASUMS256[`${platform}-${arch}`];
        expect(checksum, `${platform}-${arch}`).toMatch(/^[0-9a-f]{64}$/);
        expect(nodeDistArchiveName(platform, arch)).toContain(`node-v${NATIVE_NODE_VERSION}`);
        expect(nodeDistUrl(platform, arch)).toMatch(/^https:\/\/nodejs\.org\/dist\//);
      }
    }
    expect(NATIVE_NODE_SHASUMS_URL).toBe(
      `https://nodejs.org/dist/v${NATIVE_NODE_VERSION}/SHASUMS256.txt`,
    );
  });
});

describe("native packaging bundle metadata", () => {
  it("keeps the native deb package distinct from the Electron paseo package", () => {
    const control = debControl({
      version: "0.14.0-ltlly.3",
      arch: "x64",
      depends: ["libc6", "libxkbcommon0"],
      installedSizeKb: 1,
      maxGlibcVersion: "2.39",
    });
    expect(control).toContain("Package: paseo-native");
    expect(control).toContain("Version: 0.14.0~ltlly.3");
    expect(control).toContain("Architecture: amd64");
    // The declared baseline allows at most glibc 2.39; the measured maximum
    // versions the dependency instead of leaving an unversioned libc6 that
    // installs on distros the binary cannot load on.
    expect(control).toContain("Depends: libc6 (>= 2.39), libxkbcommon0");
  });

  it("derives deb Depends from the binary's NEEDED libraries and never ships unmapped ones", () => {
    const depends = debDependsFromLibraries([
      "libc.so.6",
      "libgcc_s.so.1",
      "libxkbcommon.so.0",
      "libxkbcommon-x11.so.0",
      "libxcb.so.1",
    ]);
    for (const required of [
      "libc6",
      "libgcc-s1",
      "libxkbcommon0",
      "libxkbcommon-x11-0",
      "libxcb1",
      "libwayland-client0",
      "libfontconfig1",
    ]) {
      expect(depends).toContain(required);
    }
    expect(() => debDependsFromLibraries(["libmystery.so.9"])).toThrow(/libmystery\.so\.9/);
  });

  it("uses a distinct bundle identifier so native coexists with the Electron app", () => {
    const plist = infoPlist({ version: "0.14.0-ltlly.3" });
    expect(plist).toContain("<string>sh.paseo.desktop.native</string>");
    expect(plist).toContain("<string>0.14.0-ltlly.3</string>");
  });

  it("points the Linux desktop entry at the installed launcher, not the binary", () => {
    expect(linuxDesktopEntry()).toContain("Exec=/opt/PaseoNative/paseo");
    expect(linuxDesktopEntry()).not.toContain("MimeType");
  });

  it("names artifacts per platform", () => {
    expect(artifactBaseName({ version: "0.1.0", platform: "linux", arch: "x64" })).toBe(
      "paseo-native-0.1.0-linux-x64",
    );
    expect(artifactBaseName({ version: "0.1.0", platform: "darwin", arch: "arm64" })).toBe(
      "paseo-native-0.1.0-macos-arm64",
    );
    expect(artifactBaseName({ version: "0.1.0", platform: "win32", arch: "x64" })).toBe(
      "paseo-native-0.1.0-windows-x64",
    );
  });

  it("declares the same bundle contract on every platform", () => {
    for (const platform of ["linux", "darwin", "win32"]) {
      const paths = requiredBundlePaths(platform);
      expect(paths).toContain("daemon/node_modules/@getpaseo/cli/dist/index.js");
      expect(paths).toContain(
        "daemon/node_modules/@getpaseo/server/dist/scripts/supervisor-entrypoint.js",
      );
      expect(paths).toContain("daemon/runtime.json");
      expect(paths).toContain("daemon/external-dependencies.json");
      expect(paths.some((entry) => entry.startsWith("daemon/node/"))).toBe(true);
    }
    expect(requiredBundlePaths("win32")).toContain("paseo-desktop.exe");
    expect(requiredBundlePaths("linux")).toContain("paseo-desktop");
    // Linux-only ABI assessment artifacts are part of the bundle contract.
    expect(requiredBundlePaths("linux")).toContain("daemon/native-addons.json");
    expect(requiredBundlePaths("linux")).toContain("daemon/native-libraries.txt");
    expect(requiredBundlePaths("win32")).not.toContain("daemon/native-addons.json");
  });

  it("declares the final macOS .app contract including the Resources daemon", () => {
    const paths = requiredMacAppPaths();
    expect(paths).toContain("Contents/MacOS/PaseoNative");
    expect(paths).toContain("Contents/Resources/daemon/node/bin/node");
    expect(paths).toContain("Contents/Resources/daemon/runtime.json");
    expect(paths).toContain("Contents/Resources/bin/paseo");
  });
});

describe("native packaging installed dependency closure", () => {
  function writePackage(dir, manifest) {
    mkdirSync(dir, { recursive: true });
    writeFileSync(join(dir, "package.json"), JSON.stringify(manifest));
  }

  it("resolves installed packages through node_modules nesting", () => {
    const root = mkdtempSync(join(tmpdir(), "paseo-native-resolve-"));
    try {
      writePackage(join(root, "node_modules", "left-pad"), {
        name: "left-pad",
        version: "1.0.0",
      });
      writePackage(join(root, "packages", "app"), { name: "app" });
      writePackage(join(root, "packages", "app", "node_modules", "left-pad"), {
        name: "left-pad",
        version: "2.0.0",
      });

      expect(resolveInstalledPackage("left-pad", join(root, "packages", "app"))).toBe(
        join(root, "packages", "app", "node_modules", "left-pad"),
      );
      expect(resolveInstalledPackage("left-pad", root)).toBe(
        join(root, "node_modules", "left-pad"),
      );
    } finally {
      rmSync(root, { recursive: true, force: true });
    }
  });

  it("stages the installed closure without registry resolution and never resolves conflicts first-wins", () => {
    const root = mkdtempSync(join(tmpdir(), "paseo-native-closure-"));
    try {
      writePackage(join(root, "node_modules", "zod"), {
        name: "zod",
        version: "3.0.0",
        dependencies: { "left-pad": "^1" },
      });
      writePackage(join(root, "node_modules", "left-pad"), {
        name: "left-pad",
        version: "1.0.0",
      });
      writePackage(join(root, "node_modules", "tslib"), {
        name: "tslib",
        version: "9.9.9",
      });
      writePackage(join(root, "packages", "server"), {
        name: "@getpaseo/server",
        version: "0.1.0",
        dependencies: { zod: "^3", "left-pad": "^2", tslib: "^9" },
      });
      writePackage(join(root, "packages", "server", "node_modules", "left-pad"), {
        name: "left-pad",
        version: "2.0.0",
      });

      const plan = planInstalledClosure({
        entries: [
          {
            name: "@getpaseo/server",
            dir: join(root, "packages", "server"),
            stagingRel: "node_modules/@getpaseo/server",
          },
        ],
        resolveInstalled: resolveInstalledPackage,
        readPackageJson: (dir) => JSON.parse(readFileSync(join(dir, "package.json"), "utf8")),
      });
      const stagedAs = Object.fromEntries(plan.map((entry) => [entry.name, entry.stagingRel]));
      expect(stagedAs["@getpaseo/server"]).toBe("node_modules/@getpaseo/server");
      expect(stagedAs["zod"]).toBe("node_modules/zod");
      expect(stagedAs["tslib"]).toBe("node_modules/tslib");
      // Both installed left-pad versions are staged: the first dependent takes
      // the root slot and every later conflicting version is nested under the
      // dependent that requires it — never dropped, never overwritten.
      const leftPadCopies = plan.filter((entry) => entry.name === "left-pad");
      expect(leftPadCopies).toHaveLength(2);
      expect(new Set(leftPadCopies.map((entry) => entry.stagingRel))).toEqual(
        new Set(["node_modules/left-pad", "node_modules/zod/node_modules/left-pad"]),
      );
      expect(leftPadCopies.map((entry) => entry.packageJson.version).sort()).toEqual([
        "1.0.0",
        "2.0.0",
      ]);
      expect(
        plan.find((entry) => entry.stagingRel === "node_modules/@getpaseo/server").packageJson.name,
      ).toBe("@getpaseo/server");
    } finally {
      rmSync(root, { recursive: true, force: true });
    }
  });

  it("copies dist without maps, tests, and the bundled web UI", () => {
    expect(distCopyFilter("server/server.js")).toBe(true);
    expect(distCopyFilter("server/server.js.map")).toBe(false);
    expect(distCopyFilter("server/agent.test.js")).toBe(false);
    expect(distCopyFilter("server/web-ui/index.html")).toBe(false);
  });

  it("resolves the full workspace runtime closure from the server and CLI entries", () => {
    const packages = {
      server: { dependencies: { "@getpaseo/client": "*", "@getpaseo/protocol": "*" } },
      cli: { dependencies: { "@getpaseo/server": "*" } },
      client: { dependencies: { "@getpaseo/protocol": "*" } },
      protocol: { dependencies: {} },
    };
    expect(workspaceRuntimePackageNames(packages, ["cli", "server"])).toEqual([
      "cli",
      "server",
      "client",
      "protocol",
    ]);
  });
});

describe("native packaging launchers", () => {
  it("bootstraps the daemon with the bundled CLI and never restarts it", () => {
    for (const platform of ["linux", "darwin", "win32"]) {
      const launcher = appLauncherScript(platform);
      expect(launcher).toContain(
        platform === "win32"
          ? 'daemon start --home "%DAEMON_HOME%"'
          : 'daemon start --home "$DAEMON_HOME"',
      );
      expect(launcher).not.toContain("daemon restart");
      expect(launcher).not.toContain("ELECTRON_RUN_AS_NODE");
    }
  });

  it("runs the flat Linux launcher against fake node and app binaries", () => {
    if (process.platform === "win32") return;
    const root = mkdtempSync(join(tmpdir(), "paseo-native-launcher-"));
    const { recordPath, launcher, daemonDir } = createPosixBundle(join(root, "bundle"), "linux");
    const home = join(root, "paseo-home");
    mkdirSync(home, { recursive: true });
    try {
      const result = spawnSync(launcher, ["--flag", "value"], {
        encoding: "utf8",
        env: { ...process.env, PASEO_NATIVE_FAKE_NODE_RECORD: recordPath, PASEO_HOME: home },
      });
      expect(result.status).toBe(0);
      const record = readFileSync(recordPath, "utf8").split("\n").filter(Boolean);
      const daemonStartIndex = record.indexOf("ARG:daemon");
      expect(record[daemonStartIndex + 1]).toBe("ARG:start");
      expect(record[daemonStartIndex + 3]).toBe(`ARG:${home}`);
      expect(record).toContain("ENV:PASEO_NODE_ENV=production");
      expect(record[record.length - 1]).toBe("APP_EXEC:--flag value");
      expect(daemonDir).toContain(join("bundle", "daemon"));
    } finally {
      rmSync(root, { recursive: true, force: true });
    }
  });

  it("runs the assembled macOS .app launcher against the post-assembly Resources layout", () => {
    if (process.platform === "win32") return;
    const root = mkdtempSync(join(tmpdir(), "paseo-native-macapp-"));
    const { recordPath, launcher } = createPosixBundle(join(root, "Paseo Native.app"), "darwin");
    const home = join(root, "paseo-home");
    mkdirSync(home, { recursive: true });
    try {
      const result = spawnSync(launcher, ["--flag", "value"], {
        encoding: "utf8",
        env: { ...process.env, PASEO_NATIVE_FAKE_NODE_RECORD: recordPath, PASEO_HOME: home },
      });
      expect(result.status).toBe(0);
      const record = readFileSync(recordPath, "utf8").split("\n").filter(Boolean);
      const daemonStartIndex = record.indexOf("ARG:daemon");
      expect(daemonStartIndex).toBeGreaterThan(-1);
      expect(record[daemonStartIndex + 1]).toBe("ARG:start");
      expect(record[daemonStartIndex + 3]).toBe(`ARG:${home}`);
      expect(record).toContain("ENV:PASEO_NODE_ENV=production");
      expect(record[record.length - 1]).toBe("APP_EXEC:--flag value");
    } finally {
      rmSync(root, { recursive: true, force: true });
    }
  });

  it("runs the CLI shim through the bundled node with production env", () => {
    if (process.platform === "win32") return;
    const root = mkdtempSync(join(tmpdir(), "paseo-native-shim-"));
    const bundle = join(root, "bundle");
    const recordPath = join(root, "record.log");
    const daemonDir = join(bundle, "daemon");
    mkdirSync(join(daemonDir, "node", "bin"), { recursive: true });
    mkdirSync(join(daemonDir, "node_modules", "@getpaseo", "cli", "dist"), { recursive: true });
    writeFileSync(join(daemonDir, "node_modules", "@getpaseo", "cli", "dist", "index.js"), "");
    writeFileSync(join(daemonDir, "node", "bin", "node"), fakeNodeRecordScript());
    chmodSync(join(daemonDir, "node", "bin", "node"), 0o755);
    mkdirSync(join(bundle, "bin"), { recursive: true });
    writeFileSync(join(bundle, "bin", "paseo"), cliShimScript("linux"));
    chmodSync(join(bundle, "bin", "paseo"), 0o755);
    try {
      const result = spawnSync(join(bundle, "bin", "paseo"), ["--version"], {
        encoding: "utf8",
        env: { ...process.env, PASEO_NATIVE_FAKE_NODE_RECORD: recordPath },
      });
      expect(result.status).toBe(0);
      const record = readFileSync(recordPath, "utf8").split("\n").filter(Boolean);
      expect(record).toContain("ENV:PASEO_NODE_ENV=production");
      expect(record).toContain("ARG:--version");
      expect(record.join("\n")).not.toContain("ARG:daemon");
    } finally {
      rmSync(root, { recursive: true, force: true });
    }
  });

  it("generates CRLF Windows launchers with the same daemon bootstrap contract", () => {
    const launcher = appLauncherScript("win32");
    expect(launcher).toContain("\r\n");
    expect(launcher).toContain('daemon start --home "%DAEMON_HOME%"');
    expect(launcher).not.toContain("daemon restart");
    expect(cliShimScript("win32")).toContain("\\daemon\\node\\node.exe");
  });
});

describe("native packaging tool invocation and escaping", () => {
  it("launches npm through the real CLI JS on Windows, not the batch wrapper", () => {
    const windows = {
      platform: "win32",
      execPath: "C:\\Program Files\\nodejs\\node.exe",
    };
    const npm = npmCommand(windows);
    expect(npm.command).toBe(windows.execPath);
    expect(npm.args).toEqual(["C:\\Program Files\\nodejs\\node_modules\\npm\\bin\\npm-cli.js"]);
    expect(npmCommand({ platform: "linux", execPath: "/usr/bin/node" })).toEqual({
      command: "npm",
      args: [],
    });
  });

  it("doubles single quotes for PowerShell single-quoted strings", () => {
    expect(escapePowerShellSingleQuoted("O'Brien's dir")).toBe("O''Brien''s dir");
    expect(escapePowerShellSingleQuoted("plain-path")).toBe("plain-path");
  });

  it("escapes NSIS `$` and backslashes in path literals", () => {
    expect(escapeNsisString("C:\\Users\\a$b\\stage")).toBe("C:\\\\Users\\\\a$$b\\\\stage");
  });
});

describe("native packaging staged permissions", () => {
  it("fails when a staged launcher lost its execute bit and passes a real assembly", () => {
    const root = mkdtempSync(join(tmpdir(), "paseo-native-exec-"));
    try {
      mkdirSync(join(root, "bin"), { recursive: true });
      writeFileSync(join(root, "paseo"), "#!/bin/sh\n");
      writeFileSync(join(root, "bin", "paseo"), "#!/bin/sh\n");
      chmodSync(join(root, "paseo"), 0o644);
      chmodSync(join(root, "bin", "paseo"), 0o755);
      expect(() => assertPosixExecutable(root, ["paseo", "bin/paseo"])).toThrow(
        /missing its execute permission: paseo/,
      );
      chmodSync(join(root, "paseo"), 0o755);
      expect(() => assertPosixExecutable(root, ["paseo", "bin/paseo"])).not.toThrow();
    } finally {
      rmSync(root, { recursive: true, force: true });
    }
  });
});

describe("native packaging NSIS uninstall manifest", () => {
  it("deletes only installed files, never recurses, and leaves pre-existing files alone", () => {
    const lines = nsisUninstallLines({
      files: ["PaseoNative.cmd", "daemon\\runtime.json", "bin\\paseo.cmd"],
      directories: ["bin", "daemon\\node\\bin", "daemon\\node", "daemon"],
    });
    const section = lines.join("\n");
    // Every installed file is deleted by exact path.
    expect(section).toContain('Delete "$INSTDIR\\PaseoNative.cmd"');
    expect(section).toContain('Delete "$INSTDIR\\daemon\\\\runtime.json"');
    expect(section).toContain('Delete "$INSTDIR\\bin\\\\paseo.cmd"');
    // Non-recursive RMDir only: a sentinel dropped in $INSTDIR (or any other
    // pre-existing file) is never matched.
    expect(section).not.toMatch(/RMDir \/r/);
    expect(section).not.toContain("sentinel.txt");
    // Directories go deepest-first so they can empty out before their parents.
    const rmdirPaths = lines
      .filter((line) => line.includes("RMDir"))
      .map((line) => line.match(/"(.+)"/)[1]);
    expect(rmdirPaths.indexOf("$INSTDIR\\daemon\\\\node\\\\bin")).toBeLessThan(
      rmdirPaths.indexOf("$INSTDIR\\daemon\\\\node"),
    );
    expect(rmdirPaths.indexOf("$INSTDIR\\daemon\\\\node")).toBeLessThan(
      rmdirPaths.indexOf("$INSTDIR\\daemon"),
    );
  });

  it("escapes `$` in installed relative paths", () => {
    const lines = nsisUninstallLines({ files: ["weird$name.txt"], directories: [] });
    expect(lines).toContain('  Delete "$INSTDIR\\weird$$name.txt"');
  });

  it("deletes the uninstaller generated after the staged manifest, never recursively", () => {
    const lines = nsisUninstallLines({ files: ["paseo-desktop.exe"], directories: ["daemon"] });
    const section = lines.join("\n");
    // WriteUninstaller creates $INSTDIR\uninstall.exe after listStagedTree ran,
    // so the uninstall section must remove it explicitly before RMDir.
    expect(lines).toContain('  Delete "$INSTDIR\\uninstall.exe"');
    expect(section).not.toMatch(/RMDir \/r/);
    expect(lines.indexOf('  Delete "$INSTDIR\\uninstall.exe"')).toBeLessThan(
      lines.findIndex((line) => line.includes("RMDir")),
    );
  });
});

describe("native packaging AppImage runtime pinning", () => {
  it("pins the type-2 runtime per architecture from the authoritative release URL", () => {
    for (const arch of ["x64", "arm64"]) {
      expect(APPIMAGE_RUNTIME_URL[arch]).toMatch(
        /^https:\/\/github\.com\/AppImage\/type2-runtime\/releases\/download\/continuous\/runtime-(x86_64|aarch64)$/,
      );
      expect(APPIMAGE_RUNTIME_SHA256[arch]).toMatch(/^[0-9a-f]{64}$/);
    }
    // The plugin vendored into the pinned linuxdeploy release forwards this
    // variable to appimagetool as --runtime-file; anything else re-downloads
    // the mutable continuous runtime implicitly.
    expect(APPIMAGE_RUNTIME_ENV_VAR).toBe("LDAI_RUNTIME_FILE");
  });
});

describe("native packaging Linux ABI assessment", () => {
  it("splits NEEDED entries into system dependencies and bundled payload libraries", () => {
    const { system, bundled } = classifyNeededLibraries(
      [
        "linux-vdso.so.1",
        "libc.so.6",
        "libstdc++.so.6",
        "libonnxruntime.so",
        "libsherpa-onnx-c-api.so",
      ],
      ["libonnxruntime.so", "libsherpa-onnx-c-api.so", "libvips.so.42"],
    );
    expect(system.sort()).toEqual(["libc.so.6", "libstdc++.so.6"]);
    expect(bundled.sort()).toEqual(["libonnxruntime.so", "libsherpa-onnx-c-api.so"]);
  });

  it("extracts the highest versioned glibc symbol reference numerically", () => {
    expect(maxGlibcSymbolVersion("readelf GLIBC_2.9 foo\nreadelf GLIBC_2.44 bar")).toBe("2.44");
    expect(maxGlibcSymbolVersion("no versions here")).toBeNull();
    expect(maxGlibcSymbolVersion("GLIBC_2.27 a\nGLIBC_2.28 b\nGLIBC_2.4 c")).toBe("2.28");
  });

  it("fails packaging when a fixture payload needs newer glibc than the declared baseline", () => {
    expect(LINUX_GLIBC_BASELINE).toEqual({ distro: "Ubuntu 24.04", glibc: "2.39" });
    // Pure fixture symbol-version output: no staged bundle, no artifact writes.
    const baselinePayload = ["GLIBC_2.28 node", "GLIBC_2.39 paseo-desktop"].join("\n");
    expect(() => assertGlibcWithinBaseline(maxGlibcSymbolVersion(baselinePayload))).not.toThrow();
    const newerHostPayload = ["GLIBC_2.28 node", "GLIBC_2.44 paseo-desktop"].join("\n");
    expect(() => assertGlibcWithinBaseline(maxGlibcSymbolVersion(newerHostPayload))).toThrow(
      /glibc 2\.44.*Ubuntu 24\.04.*glibc 2\.39/,
    );
    expect(() => assertGlibcWithinBaseline(null)).not.toThrow();
  });

  it("covers the merged glibc-internal NEEDED entries shipped by Node and native addons", () => {
    const depends = debDependsFromLibraries([
      "libdl.so.2",
      "libpthread.so.0",
      "librt.so.1",
      "libutil.so.1",
    ]);
    // All four map to libc6 (plus the fixed dlopened extras).
    expect(depends).toEqual(["libc6", "libfontconfig1", "libwayland-client0"]);
  });

  it("maps the wry browser stack sonames to packages on the declared baseline", () => {
    // The 15 NEEDED entries `readelf -d` reports for the wry-linked
    // paseo-desktop binary built on Ubuntu 24.04. dpkg -S on that baseline
    // names the owning package for each soname; unknown sonames still fail.
    const depends = debDependsFromLibraries([
      "libxkbcommon.so.0",
      "libxkbcommon-x11.so.0",
      "libxcb.so.1",
      "libwebkit2gtk-4.1.so.0",
      "libgtk-3.so.0",
      "libgdk-3.so.0",
      "libsoup-3.0.so.0",
      "libgio-2.0.so.0",
      "libjavascriptcoregtk-4.1.so.0",
      "libgobject-2.0.so.0",
      "libglib-2.0.so.0",
      "libgcc_s.so.1",
      "libm.so.6",
      "libc.so.6",
      "ld-linux-x86-64.so.2",
    ]);
    expect(depends).toEqual([
      "libc6",
      "libfontconfig1",
      "libgcc-s1",
      "libglib2.0-0t64",
      "libgtk-3-0t64",
      "libjavascriptcoregtk-4.1-0",
      "libsoup-3.0-0",
      "libwayland-client0",
      "libwebkit2gtk-4.1-0",
      "libxcb1",
      "libxkbcommon-x11-0",
      "libxkbcommon0",
    ]);
    expect(() => debDependsFromLibraries(["libwebextension.so.1"])).toThrow(
      /libwebextension\.so\.1/,
    );
  });

  it("derives versioned rpm Requires from the same dependency set", () => {
    const requires = rpmRequiresFromDebPackages(
      ["libc6", "libglib2.0-0t64", "libgtk-3-0t64", "libwebkit2gtk-4.1-0"],
      { maxGlibcVersion: "2.39" },
    );
    expect(requires).toEqual(["glibc >= 2.39", "glib2", "gtk3", "webkit2gtk4.1"]);
    // Fedora folds several sonames into one package; a repeated Debian entry
    // must not repeat the Requires line.
    expect(
      rpmRequiresFromDebPackages(["libgtk-3-0t64", "libgtk-3-0t64"], { maxGlibcVersion: null }),
    ).toEqual(["gtk3"]);
    expect(() => rpmRequiresFromDebPackages(["libmystery0"])).toThrow(/libmystery0/);
  });
});
