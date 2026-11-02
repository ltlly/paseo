import { spawnSync } from "node:child_process";
import {
  chmodSync,
  copyFileSync,
  mkdirSync,
  mkdtempSync,
  readFileSync,
  rmSync,
  writeFileSync,
} from "node:fs";
import { tmpdir } from "node:os";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { describe, expect, it } from "vitest";

const packageRoot = join(dirname(fileURLToPath(import.meta.url)), "..", "..");

function writeExecutable(filePath: string, contents: string): void {
  writeFileSync(filePath, contents, "utf8");
  chmodSync(filePath, 0o755);
}

function createFakeMacBundle(options: { includeHelper: boolean }): {
  root: string;
  shimPath: string;
} {
  const root = mkdtempSync(join(tmpdir(), "paseo-cli-shim-test-"));
  const appPath = join(root, "Paseo.app");
  const contentsPath = join(appPath, "Contents");
  const resourcesPath = join(contentsPath, "Resources");
  const shimPath = join(resourcesPath, "bin", "paseo");
  const mainPath = join(contentsPath, "MacOS", "Paseo");
  const helperPath = join(
    contentsPath,
    "Frameworks",
    "Paseo Helper.app",
    "Contents",
    "MacOS",
    "Paseo Helper",
  );

  mkdirSync(dirname(shimPath), { recursive: true });
  mkdirSync(dirname(mainPath), { recursive: true });
  copyFileSync(join(packageRoot, "bin", "paseo"), shimPath);
  chmodSync(shimPath, 0o755);

  writeExecutable(mainPath, "#!/bin/sh\necho main-executable\n");

  if (options.includeHelper) {
    mkdirSync(dirname(helperPath), { recursive: true });
    writeExecutable(
      helperPath,
      [
        "#!/bin/sh",
        'printf "helper env=%s/%s cli=%s\\n" "$ELECTRON_RUN_AS_NODE" "$PASEO_NODE_ENV" "$PASEO_CLI"',
        'printf "args=%s\\n" "$*"',
        "",
      ].join("\n"),
    );
  }

  return { root, shimPath };
}

describe("desktop packaging", () => {
  it("uses an Electron runtime whose Squirrel handoff explicitly wakes ShipIt", () => {
    const pkg = JSON.parse(readFileSync(join(packageRoot, "package.json"), "utf8")) as {
      devDependencies?: Record<string, string>;
    };
    const electronVersion = pkg.devDependencies?.electron ?? "0.0.0";
    const electronMajor = Number(electronVersion.split(".")[0]);

    expect(electronMajor).toBeGreaterThanOrEqual(44);
  });

  it("requires macOS 13 or newer in the packaged application", () => {
    const config = readFileSync(join(packageRoot, "electron-builder.yml"), "utf8");

    expect(config).toContain('minimumSystemVersion: "13.0.0"');
  });

  it("unpacks server zsh shell integration files for external shells", () => {
    const config = readFileSync(join(packageRoot, "electron-builder.yml"), "utf8");

    expect(config).toContain(
      "node_modules/@getpaseo/server/dist/server/terminal/shell-integration/**/*",
    );
    expect(config).not.toContain(
      "node_modules/@getpaseo/server/dist/src/terminal/shell-integration/**/*",
    );
  });

  it("excludes package debug/source files from the packaged app", () => {
    const config = readFileSync(join(packageRoot, "electron-builder.yml"), "utf8");

    expect(config).toContain("!**/*.map");
    expect(config).toContain("!node_modules/@getpaseo/*/src/**");
    expect(config).toContain("!node_modules/@getpaseo/**/*.test.*");
    expect(config).toContain("!node_modules/@getpaseo/**/*.spec.*");
  });

  it("excludes the bundled daemon web UI from the packaged app", () => {
    const config = readFileSync(join(packageRoot, "electron-builder.yml"), "utf8");

    expect(config).toContain("!node_modules/@getpaseo/server/dist/server/web-ui/**");
  });

  it("uses the server skill catalog without a duplicate desktop resource", () => {
    const config = readFileSync(join(packageRoot, "electron-builder.yml"), "utf8");
    const serverPackage = readFileSync(join(packageRoot, "..", "server", "package.json"), "utf8");
    const runtimeTrace = readFileSync(
      join(packageRoot, "..", "..", "scripts", "trace-daemon.mjs"),
      "utf8",
    );

    expect(config).not.toContain("from: ../../skills");
    expect(serverPackage).toContain("fs.rmSync('dist/server/skills',{recursive:true,force:true})");
    expect(serverPackage).toContain("fs.cpSync('../../skills','dist/server/skills'");
    expect(runtimeTrace).toContain('"packages/server/dist/server/skills/**"');
  });

  it("registers Paseo agent links with the operating system", () => {
    const config = readFileSync(join(packageRoot, "electron-builder.yml"), "utf8");

    expect(config).toContain("name: Paseo agent link");
    expect(config).toContain("- paseo");
  });

  // electron-builder packs production dependencies declared in package.json into
  // app.asar. Runtime code in runtime-paths.ts and bin/paseo dynamically resolves
  // these workspace packages by string, so static analysis (TypeScript, Knip) cannot
  // see the link. If a runtime-required workspace dep is dropped from
  // dependencies, the build still succeeds but ships a broken bundle. This
  // assertion is the safety net.
  it("declares all workspace packages required at runtime", () => {
    const pkg = JSON.parse(readFileSync(join(packageRoot, "package.json"), "utf8")) as {
      dependencies?: Record<string, string>;
    };
    const deps = pkg.dependencies ?? {};

    for (const required of ["@getpaseo/cli", "@getpaseo/server"]) {
      expect(deps[required], `${required} must be declared in dependencies`).toBe("*");
    }
  });

  it("launches the packaged macOS CLI through Helper instead of the main app executable", () => {
    if (process.platform === "win32") return;

    const bundle = createFakeMacBundle({ includeHelper: true });
    try {
      const result = spawnSync(bundle.shimPath, ["--version"], { encoding: "utf8" });

      expect(result.status).toBe(0);
      expect(result.stdout).toContain(`helper env=1/production cli=${bundle.shimPath}`);
      expect(result.stdout).toContain("node-entrypoint-runner.js");
      expect(result.stdout).toContain("node-script");
      expect(result.stdout).toContain("@getpaseo/cli/dist/index.js");
      expect(result.stdout).toContain("--version");
      expect(result.stdout).not.toContain("main-executable");
    } finally {
      rmSync(bundle.root, { recursive: true, force: true });
    }
  });

  it("fails packaged macOS CLI startup when Helper is missing", () => {
    if (process.platform === "win32") return;

    const bundle = createFakeMacBundle({ includeHelper: false });
    try {
      const result = spawnSync(bundle.shimPath, ["--version"], { encoding: "utf8" });

      expect(result.status).toBe(1);
      expect(result.stderr).toContain("Bundled Paseo Helper executable not found");
      expect(result.stdout).not.toContain("main-executable");
    } finally {
      rmSync(bundle.root, { recursive: true, force: true });
    }
  });
});

it("installs the Linux helper as root-owned 4755 regardless of root's namespace access", () => {
  const config = readFileSync(join(packageRoot, "electron-builder.yml"), "utf8");
  expect(config).toContain("afterInstall: scripts/linux-sandbox/after-install.tpl");
  const installer = readFileSync(
    join(packageRoot, "scripts/linux-sandbox/after-install.tpl"),
    "utf8",
  );
  expect(installer).not.toContain("unshare");
  expect(installer).toContain("chown root:root '/opt/${sanitizedProductName}/chrome-sandbox'");
  expect(installer).toContain("chmod 4755 '/opt/${sanitizedProductName}/chrome-sandbox'");
  expect(installer).not.toContain("chmod 0755");
});

describe("native GPUI packaging (opt-in)", () => {
  const workflowPath = join(
    packageRoot,
    "..",
    "..",
    ".github",
    "workflows",
    "desktop-packages.yml",
  );
  const workflow = () => readFileSync(workflowPath, "utf8");

  it("stays dispatch-only and defaults the native packaging to off", () => {
    const text = workflow();
    expect(text).toContain("workflow_dispatch");
    expect(text).not.toMatch(/^\s*on:\s*\n\s*push/m);
    expect(text).toMatch(/native:\s*\n\s+description:.*[Nn]ative/);
    expect(text).toMatch(/default:\s*"false"/);
  });

  it("gates every native packaging job behind the opt-in input", () => {
    const text = workflow();
    for (const job of ["native-linux", "native-macos", "native-windows"]) {
      expect(text).toContain(`  ${job}:`);
    }
    expect(text).toMatch(/inputs\.native == 'true'/);
  });

  it("never publishes or uploads native artifacts to a release", () => {
    const text = workflow();
    const nativeSection = text.slice(text.indexOf("  native-linux:"));
    expect(nativeSection).not.toContain("--publish");
    expect(nativeSection).not.toContain("gh release");
    expect(nativeSection).toContain("retention-days");
    // Upload the curated artifact set only; the materialized stage/ bundle is
    // not part of the artifact.
    expect(nativeSection).toContain("path: packages/desktop/release-native/artifacts");
    expect(nativeSection).not.toContain("path: packages/desktop/release-native\n");
  });

  it("installs the deb in a disposable container, not on the runner host", () => {
    const text = workflow();
    const nativeSection = text.slice(text.indexOf("  native-linux:"));
    expect(nativeSection).toContain("docker run --rm");
    expect(nativeSection).toContain("apt-get install -y /artifacts/*.deb");
  });

  it("isolates the native cargo build in its own target directory", () => {
    const script = readFileSync(join(packageRoot, "scripts", "package-native.mjs"), "utf8");
    expect(script).toContain("native-cargo-target");
    expect(script).toContain("CARGO_BUILD_JOBS");
    expect(script).toContain("--locked");
  });

  it("packages the daemon runtime with a pinned Node release", () => {
    const library = readFileSync(join(packageRoot, "scripts", "package-native-lib.mjs"), "utf8");
    expect(library).toContain("nodejs.org/dist");
    expect(library).toContain("SHASUMS256");
  });
});
