// Shared, testable pieces for the opt-in native GPUI desktop packaging pipeline
// (`package-native.mjs`). Keep every function pure or injectable so the desktop
// vitest suite can exercise them without building the real bundle.
import fs from "node:fs";
import path from "node:path";

// Pinned Node runtime for the bundled daemon. Must match the `nodejs` entry in
// the repo's `.tool-versions`. Upgrades are manual: change both places, then
// refresh NATIVE_NODE_SHASUMS256 from the official release manifest at
// https://nodejs.org/dist/v<version>/SHASUMS256.txt (see NATIVE_NODE_SHASUMS_URL).
// Provenance: the constants below were copied from
// https://nodejs.org/dist/v22.20.0/SHASUMS256.txt (official Node.js dist);
// the packaging script re-fetches that manifest over HTTPS on every build and
// cross-checks each pinned constant before trusting the local one.
export const NATIVE_NODE_VERSION = "22.20.0";
export const NATIVE_NODE_SHASUMS_URL = `https://nodejs.org/dist/v${NATIVE_NODE_VERSION}/SHASUMS256.txt`;

// SHA-256 per platform key, copied from the official Node.js SHASUMS256.txt for
// the pinned version. HTTPS alone only proves transport integrity; these pinned
// constants are the authenticity check for cached downloads, and the fetched
// manifest is the cross-check for the constants themselves.
export const NATIVE_NODE_SHASUMS256 = {
  "darwin-arm64": "f2ed1fbdfb79afb9343dfbb609efde657b41e7efcd5215264ce78f8906356b4d",
  "darwin-x64": "2a291f0a9555f5d6685d96ce9429da3d0ea3cd896c012702c6ee0015f818684e",
  "linux-arm64": "06907b9c088ce62305bc1530e5c1ae1510245114645768f7750c349c5b6fe667",
  "linux-x64": "00bbd05e306ea68b6e13e17360d0e2f680b493ef95f2fea1c4296ff7437530bc",
  "win32-arm64": "b12919e609b4fa1176ba8a155b49f761419a0c7cc97b42e6be09874a3f760ab6",
  "win32-x64": "bb819d6eb8f5bfda294bbc83a7e4ec6539da67c4233d54b0d655b9248b15e29d",
};

const NODE_PLATFORM_DIR = { darwin: "darwin", linux: "linux", win32: "win" };
const NODE_ARCHIVE_EXT = { darwin: "tar.xz", linux: "tar.xz", win32: "zip" };

// Pinned type-2 AppImage runtime. Without `LDAI_RUNTIME_FILE` the appimage
// output plugin vendored into linuxdeploy lets appimagetool download the
// mutable `continuous` runtime itself, so pinning linuxdeploy alone never pins
// what gets embedded in the final AppImage. Hashes computed from the
// authoritative release assets; packaging re-fetches and re-hashes both files
// (mirroring the Node/linuxdeploy double check).
export const APPIMAGE_RUNTIME_URL = {
  x64: "https://github.com/AppImage/type2-runtime/releases/download/continuous/runtime-x86_64",
  arm64: "https://github.com/AppImage/type2-runtime/releases/download/continuous/runtime-aarch64",
};
export const APPIMAGE_RUNTIME_SHA256 = {
  x64: "617aa77314d57c8b8e4fae9c3f0b35af7b30d5529cb69e98f64f92510ed98a46",
  arm64: "9a72a744d221576c9347ec074243acc5c3c7568896b12714e0e82e9f0bf47900",
};
// Consumed by the appimage output plugin vendored into the pinned linuxdeploy
// release, which forwards it to its bundled appimagetool as `--runtime-file`.
export const APPIMAGE_RUNTIME_ENV_VAR = "LDAI_RUNTIME_FILE";

// Launch npm without a shell. On Windows npm is a batch wrapper (npm.cmd) that
// execFileSync cannot start directly (same constraint scripts/npm-retry.mjs
// documents), so resolve the real npm CLI JS next to the running Node and
// launch it through process.execPath.
export function npmCommand({ platform = process.platform, execPath = process.execPath } = {}) {
  if (platform !== "win32") return { command: "npm", args: [] };
  // Built without path.join so the Windows result is identical on any host.
  const installDir = execPath.slice(
    0,
    Math.max(execPath.lastIndexOf("\\"), execPath.lastIndexOf("/")),
  );
  const npmCliJs = `${installDir}\\node_modules\\npm\\bin\\npm-cli.js`;
  return { command: execPath, args: [npmCliJs] };
}

// PowerShell single-quoted strings escape a literal quote by doubling it, so
// paths like `O'Brien` survive insertion into -Command snippets.
export function escapePowerShellSingleQuoted(value) {
  return value.replace(/'/g, "''");
}

// NSIS string literals: a literal `$` must be doubled (`$$`) and backslashes
// are path separators that must be doubled inside string escapes.
export function escapeNsisString(value) {
  // `$$` in a replace() replacement means a literal `$`, so doubling requires
  // four dollars here.
  return value.replace(/\\/g, "\\\\").replace(/\$/g, "$$$$");
}

// Fail unless every listed bundle file carries the execute bit. Applied to the
// real staged assembly (not test fixtures) so a missing chmod on the app
// launcher fails packaging instead of surfacing as EACCES after install.
export function assertPosixExecutable(
  bundleDir,
  relativePaths,
  { accessSync = fs.accessSync } = {},
) {
  for (const relative of relativePaths) {
    try {
      accessSync(path.join(bundleDir, relative), fs.constants.X_OK);
    } catch {
      throw new Error(`Staged bundle file is missing its execute permission: ${relative}`);
    }
  }
}

// Uninstall body for the generated NSIS script: delete exactly the installed
// manifest files, then remove directories non-recursively (deepest first, and
// RMDir only removes empty directories), so pre-existing files in a
// user-chosen $INSTDIR survive uninstall. Paths must already be relative and
// are NSIS-escaped here.
export function nsisUninstallLines({ files, directories }) {
  const lines = [];
  for (const relative of [...files].sort()) {
    lines.push(`  Delete "$INSTDIR\\${escapeNsisString(relative)}"`);
  }
  // WriteUninstaller runs after the staged manifest is captured, so the
  // generated uninstaller is not in `files`. Delete it explicitly or the final
  // non-recursive RMDir leaves a non-empty installation directory.
  lines.push('  Delete "$INSTDIR\\uninstall.exe"');
  const depth = (relative) => relative.split("\\").length;
  for (const relative of [...directories].sort(
    (a, b) => depth(b) - depth(a) || a.localeCompare(b),
  )) {
    lines.push(`  RMDir "$INSTDIR\\${escapeNsisString(relative)}"`);
  }
  return lines;
}

// Classify an ELF file's NEEDED entries: libraries shipped inside the bundle
// (resolved at runtime next to the file, never a system requirement) versus
// system libraries that deb/rpm Depends must cover. vdso is a kernel-provided
// mapping, not a dependency.
export function classifyNeededLibraries(neededLibraries, bundledLibraryNames) {
  const bundled = new Set(bundledLibraryNames);
  const system = [];
  const inBundle = [];
  for (const library of neededLibraries) {
    if (library === "linux-vdso.so.1") continue;
    if (bundled.has(library)) inBundle.push(library);
    else system.push(library);
  }
  return { system, bundled: inBundle };
}

// Declared Linux baseline for native artifacts. The native-linux CI job builds
// on ubuntu-24.04 and installs the .deb in an ubuntu:24.04 container; Ubuntu
// 24.04 LTS ships glibc 2.39. A staged ELF payload referencing newer GLIBC_*
// symbols cannot load on the distro those artifacts claim to support.
export const LINUX_GLIBC_BASELINE = {
  distro: "Ubuntu 24.04",
  glibc: "2.39",
};

// Highest GLIBC_<major>.<minor> symbol version referenced by `objdump -T`
// output, or null when the binary references no versioned glibc symbols.
// A container smoke on one distro exercises only that baseline and never
// proves the bundle meets another distro's glibc, so the value is enforced
// against LINUX_GLIBC_BASELINE before artifacts are built.
export function maxGlibcSymbolVersion(objdumpOutput) {
  let max = null;
  for (const match of objdumpOutput.matchAll(/GLIBC_([0-9]+\.[0-9]+)/g)) {
    const candidate = match[1].split(".").map(Number);
    if (
      max === null ||
      candidate[0] > max[0] ||
      (candidate[0] === max[0] && candidate[1] > max[1])
    ) {
      max = candidate;
    }
  }
  return max ? max.join(".") : null;
}

export function compareGlibcVersions(a, b) {
  const [aMajor, aMinor] = a.split(".").map(Number);
  const [bMajor, bMinor] = b.split(".").map(Number);
  if (aMajor !== bMajor) return aMajor < bMajor ? -1 : 1;
  if (aMinor !== bMinor) return aMinor < bMinor ? -1 : 1;
  return 0;
}

// Fail packaging before any artifact is written when a staged payload needs a
// newer glibc than the declared distro baseline. There is no opt-out: building
// on a newer host produces binaries that cannot load on the baseline.
export function assertGlibcWithinBaseline(maxGlibcVersion, baseline = LINUX_GLIBC_BASELINE) {
  if (maxGlibcVersion === null) return;
  if (compareGlibcVersions(maxGlibcVersion, baseline.glibc) <= 0) return;
  throw new Error(
    `Staged Linux payloads reference glibc ${maxGlibcVersion}, newer than the declared ` +
      `${baseline.distro} baseline (glibc ${baseline.glibc}). Build the GPUI client and ` +
      `bundle on ${baseline.distro}; a newer build host produces artifacts that cannot ` +
      `load on the baseline.`,
  );
}

export function nodePlatformKey(platform, arch) {
  return `${platform}-${arch}`;
}

export function nodeDistArchiveName(platform, arch) {
  return `node-v${NATIVE_NODE_VERSION}-${NODE_PLATFORM_DIR[platform]}-${arch}.${NODE_ARCHIVE_EXT[platform]}`;
}

export function nodeDistUrl(platform, arch) {
  return `https://nodejs.org/dist/v${NATIVE_NODE_VERSION}/${nodeDistArchiveName(platform, arch)}`;
}

export function nodeExtractedDirName(platform, arch) {
  return `node-v${NATIVE_NODE_VERSION}-${NODE_PLATFORM_DIR[platform]}-${arch}`;
}

// Path of the node executable relative to the bundle's daemon/ directory.
export function nodeBinaryRelativePath(platform) {
  return platform === "win32" ? "node/node.exe" : "node/bin/node";
}

export const DEB_ARCH = { x64: "amd64", arm64: "arm64" };

// Debian revisions use `-`; npm prerelease uses it as a separator. `~` sorts
// before the final release, mirroring how the fork prerelease should rank.
export function debPackageVersion(version) {
  const separator = version.indexOf("-");
  if (separator === -1) return version;
  return `${version.slice(0, separator)}~${version.slice(separator + 1)}`;
}

export function artifactBaseName({ version, platform, arch }) {
  const platformLabel = { linux: "linux", darwin: "macos", win32: "windows" }[platform];
  return `paseo-native-${version}-${platformLabel}-${arch}`;
}

// Runtime dependencies in dependency order of strictness: regular
// dependencies must exist in the installed tree, optionalDependencies are
// skipped when absent (matching npm's install semantics).
export function directRuntimeDependencies(packageJson) {
  return [
    ...Object.keys(packageJson.dependencies ?? {}).map((name) => ({ name, optional: false })),
    ...Object.keys(packageJson.optionalDependencies ?? {}).map((name) => ({
      name,
      optional: true,
    })),
  ];
}

// Workspace packages the bundled daemon needs copied into daemon/node_modules,
// derived from the @getpaseo/* dependency closure of the entry packages. Returns
// short workspace names (e.g. "server" for @getpaseo/server).
export function workspaceRuntimePackageNames(packageJsonsByName, entryNames) {
  const resolved = [];
  const seen = new Set();
  const queue = [...entryNames];
  while (queue.length > 0) {
    const name = queue.shift();
    if (seen.has(name)) continue;
    seen.add(name);
    const pkg = packageJsonsByName[name];
    if (!pkg) throw new Error(`Unknown workspace runtime package: ${name}`);
    resolved.push(name);
    for (const dependency of Object.keys(pkg.dependencies ?? {})) {
      if (!dependency.startsWith("@getpaseo/")) continue;
      const shortName = dependency.slice("@getpaseo/".length);
      if (!seen.has(shortName)) queue.push(shortName);
    }
  }
  return resolved;
}

// Resolve an installed package directory by walking node_modules the way Node
// does from `fromDir`. Done manually instead of require.resolve so package
// "exports" restrictions cannot block locating the package root.
export function resolveInstalledPackage(name, fromDir) {
  const segments = name.split("/");
  let dir = path.resolve(fromDir);
  for (;;) {
    const candidate = path.join(dir, "node_modules", ...segments);
    if (fs.existsSync(path.join(candidate, "package.json"))) {
      const pkg = JSON.parse(fs.readFileSync(path.join(candidate, "package.json"), "utf8"));
      if (pkg.name !== name) {
        throw new Error(`Resolved ${candidate} to ${pkg.name}, expected ${name}`);
      }
      return candidate;
    }
    const parent = path.dirname(dir);
    if (parent === dir) throw new Error(`Installed package ${name} not found from ${fromDir}`);
    dir = parent;
  }
}

// Walk the real installed dependency closure for the daemon runtime. Every
// external dependency is the copy the repo actually installed — no registry
// resolution at package time. Version conflicts are never resolved first-wins:
// the conflicting copy is staged nested under its dependent's own node_modules,
// mirroring npm's nested layout, so Node resolves each dependent to its own
// installed version.
export function planInstalledClosure({ entries, resolveInstalled, readPackageJson }) {
  const plan = [];
  const seen = new Map();
  const queue = entries.map((entry) => ({
    name: entry.name,
    dir: entry.dir,
    stagingRel: entry.stagingRel,
  }));
  while (queue.length > 0) {
    const item = queue.shift();
    const realDir = fs.realpathSync(item.dir);
    const previous = seen.get(item.stagingRel);
    if (previous) {
      if (previous === realDir) continue;
      if (!item.dependentStagingRel) {
        throw new Error(
          `Conflicting installed versions of ${item.name} without a dependent to nest under`,
        );
      }
      // Never first-wins: stage the conflicting copy nested under the
      // dependent that actually requires it, mirroring npm's nested layout.
      item.stagingRel = path.join(item.dependentStagingRel, "node_modules", item.name);
      item.dependentStagingRel = undefined;
      const nestedSeen = seen.get(item.stagingRel);
      if (nestedSeen) {
        if (nestedSeen === realDir) continue;
        throw new Error(
          `Unresolvable nested conflict at ${item.stagingRel}: ${nestedSeen} vs ${realDir}`,
        );
      }
    }
    seen.set(item.stagingRel, realDir);
    const packageJson = readPackageJson(item.dir);
    if (packageJson.name === undefined) {
      throw new Error(`Installed package at ${item.dir} has no name`);
    }
    plan.push({ ...item, packageJson });
    for (const { name, optional } of directRuntimeDependencies(packageJson)) {
      let resolvedDir;
      try {
        resolvedDir = resolveInstalled(name, item.dir);
      } catch (error) {
        if (optional) continue;
        throw error;
      }
      queue.push({
        name,
        dir: resolvedDir,
        stagingRel: path.join("node_modules", name),
        dependentStagingRel: item.stagingRel,
      });
    }
  }
  return plan;
}

// Generated launcher for the packaged native app: bootstrap the bundled daemon
// through the bundled CLI (safe against an already-running instance — `daemon
// start` never restarts), then exec the GPUI client, which discovers the daemon
// through its existing PASEO_HOME/paseo.pid path. This launcher is the only
// production entrypoint that launches the daemon; the Rust client stays
// discovery-only.
//
// Layout: linux/win32 keep the bundle flat (launcher sits next to the binary
// and daemon/). macOS moves the launcher into Contents/MacOS and the daemon
// into Contents/Resources, so the darwin template resolves those paths.
export function appLauncherScript(platform) {
  if (platform === "win32") {
    return [
      "@echo off",
      "setlocal",
      "rem Generated by packages/desktop/scripts/package-native.mjs. Do not edit.",
      'set "BUNDLE_DIR=%~dp0"',
      'set "PASEO_NODE_ENV=production"',
      'set "DAEMON_HOME=%PASEO_HOME%"',
      'if "%DAEMON_HOME%"=="" set "DAEMON_HOME=%USERPROFILE%\\.paseo"',
      'if exist "%BUNDLE_DIR%daemon\\node\\node.exe" (',
      `  "%BUNDLE_DIR%daemon\\node\\node.exe" "%BUNDLE_DIR%daemon\\node_modules\\@getpaseo\\cli\\dist\\index.js" daemon start --home "%DAEMON_HOME%"`,
      ")",
      '"%BUNDLE_DIR%paseo-desktop.exe" %*',
      "",
    ].join("\r\n");
  }
  if (platform === "darwin") {
    return `#!/bin/sh
# Generated by packages/desktop/scripts/package-native.mjs. Do not edit.
# This launcher lives at Paseo Native.app/Contents/MacOS/PaseoNative; the
# daemon runtime and CLI shim live under Contents/Resources.
set -eu

SCRIPT_PATH=$0
case "$SCRIPT_PATH" in
  /*) ;;
  *) SCRIPT_PATH="$(pwd)/$SCRIPT_PATH" ;;
esac
while [ -L "$SCRIPT_PATH" ]; do
  SCRIPT_DIR=$(CDPATH= cd -- "$(dirname -- "$SCRIPT_PATH")" && pwd)
  LINK_TARGET=$(readlink "$SCRIPT_PATH")
  case "$LINK_TARGET" in
    /*) SCRIPT_PATH="$LINK_TARGET" ;;
    *) SCRIPT_PATH="\${SCRIPT_DIR}/\${LINK_TARGET}" ;;
  esac
done
MACOS_DIR=$(CDPATH= cd -- "$(dirname -- "$SCRIPT_PATH")" && pwd)
CONTENTS_DIR=$(CDPATH= cd -- "$MACOS_DIR/.." && pwd)
APP="$MACOS_DIR/paseo-desktop"
NODE="$CONTENTS_DIR/Resources/daemon/node/bin/node"
CLI="$CONTENTS_DIR/Resources/daemon/node_modules/@getpaseo/cli/dist/index.js"

DAEMON_HOME=\${PASEO_HOME:-"$HOME/.paseo"}
# daemon start never restarts a live daemon; failures must not block the app.
PASEO_NODE_ENV=production "$NODE" "$CLI" daemon start --home "$DAEMON_HOME" 1>&2 ||
  echo "paseo-native: daemon bootstrap failed; continuing with discovery" 1>&2

exec "$APP" "$@"
`;
  }
  return `#!/bin/sh
# Generated by packages/desktop/scripts/package-native.mjs. Do not edit.
set -eu

SCRIPT_PATH=$0
case "$SCRIPT_PATH" in
  /*) ;;
  *) SCRIPT_PATH="$(pwd)/$SCRIPT_PATH" ;;
esac
while [ -L "$SCRIPT_PATH" ]; do
  SCRIPT_DIR=$(CDPATH= cd -- "$(dirname -- "$SCRIPT_PATH")" && pwd)
  LINK_TARGET=$(readlink "$SCRIPT_PATH")
  case "$LINK_TARGET" in
    /*) SCRIPT_PATH="$LINK_TARGET" ;;
    *) SCRIPT_PATH="\${SCRIPT_DIR}/\${LINK_TARGET}" ;;
  esac
done
BUNDLE_DIR=$(CDPATH= cd -- "$(dirname -- "$SCRIPT_PATH")" && pwd)

NODE="$BUNDLE_DIR/daemon/node/bin/node"
CLI="$BUNDLE_DIR/daemon/node_modules/@getpaseo/cli/dist/index.js"
APP="$BUNDLE_DIR/paseo-desktop"

DAEMON_HOME=\${PASEO_HOME:-"$HOME/.paseo"}
# daemon start never restarts a live daemon; failures must not block the app.
PASEO_NODE_ENV=production "$NODE" "$CLI" daemon start --home "$DAEMON_HOME" 1>&2 ||
  echo "paseo-native: daemon bootstrap failed; continuing with discovery" 1>&2

exec "$APP" "$@"
`;
}

// Generated CLI shim: runs the bundled CLI with the bundled Node runtime,
// mirroring the packaged Electron `bin/paseo` shim but without Electron.
export function cliShimScript(platform) {
  if (platform === "win32") {
    return [
      "@echo off",
      "setlocal",
      "rem Generated by packages/desktop/scripts/package-native.mjs. Do not edit.",
      'set "PASEO_NODE_ENV=production"',
      `"%~dp0..\\daemon\\node\\node.exe" "%~dp0..\\daemon\\node_modules\\@getpaseo\\cli\\dist\\index.js" %*`,
      "",
    ].join("\r\n");
  }
  return `#!/bin/sh
# Generated by packages/desktop/scripts/package-native.mjs. Do not edit.
set -eu
BUNDLE_DIR=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
exec env PASEO_NODE_ENV=production "$BUNDLE_DIR/daemon/node/bin/node" \\
  "$BUNDLE_DIR/daemon/node_modules/@getpaseo/cli/dist/index.js" "$@"
`;
}

export function linuxDesktopEntry() {
  return `[Desktop Entry]
Type=Application
Name=Paseo Native
Comment=Paseo native (GPUI) desktop client
Exec=/opt/PaseoNative/paseo
Icon=paseo-native
Terminal=false
Categories=Development;
`;
}

// Shared libraries the packaged Linux binary needs. Map from ldd/NEEDED names
// to Debian packages; anything unmapped must fail packaging so the Depends
// list can never silently ship incomplete. Package ownership comes from
// `dpkg -S <soname>` on the declared Ubuntu 24.04 baseline.
export const SHARED_LIBRARY_DEB_PACKAGES = {
  "ld-linux-x86-64.so.2": "libc6",
  "libc.so.6": "libc6",
  "libm.so.6": "libc6",
  // Merged into libc in glibc >= 2.34 but still recorded as NEEDED by the
  // official Node dist build and common native addons; libc6 ships them on
  // every supported Debian/Ubuntu baseline.
  "libdl.so.2": "libc6",
  "libpthread.so.0": "libc6",
  "librt.so.1": "libc6",
  "libutil.so.1": "libc6",
  "libresolv.so.2": "libc6",
  "libgcc_s.so.1": "libgcc-s1",
  "libxkbcommon.so.0": "libxkbcommon0",
  "libxkbcommon-x11.so.0": "libxkbcommon-x11-0",
  "libxcb.so.1": "libxcb1",
  "libXau.so.6": "libxau6",
  "libXdmcp.so.6": "libxdmcp6",
  "libfontconfig.so.1": "libfontconfig1",
  "libwayland-client.so.0": "libwayland-client0",
  "libstdc++.so.6": "libstdc++6",
  "libX11.so.6": "libx11-6",
  // Embedded browser stack the GPUI client links through wry (WebKitGTK 4.1).
  // Ubuntu renamed GTK3 and GLib for the 64-bit time_t transition; the t64
  // packages own these sonames on 24.04, and they Provide the pre-transition
  // package names.
  "libglib-2.0.so.0": "libglib2.0-0t64",
  "libgobject-2.0.so.0": "libglib2.0-0t64",
  "libgio-2.0.so.0": "libglib2.0-0t64",
  "libgtk-3.so.0": "libgtk-3-0t64",
  "libgdk-3.so.0": "libgtk-3-0t64",
  "libsoup-3.0.so.0": "libsoup-3.0-0",
  "libjavascriptcoregtk-4.1.so.0": "libjavascriptcoregtk-4.1-0",
  "libwebkit2gtk-4.1.so.0": "libwebkit2gtk-4.1-0",
};

// Fedora-family package for each Debian runtime package. Fedora splits the
// same runtime differently: WebKitGTK 4.1 and JavaScriptCore are separate
// packages, gtk3 owns both libgtk-3 and libgdk-3, and glib2 owns the whole
// glib/gobject/gio trio. Ownership verified with
// `dnf repoquery --whatprovides '<soname>()(64bit)'` on Fedora 40.
export const SHARED_LIBRARY_RPM_PACKAGES = {
  libc6: "glibc",
  "libgcc-s1": "libgcc",
  "libstdc++6": "libstdc++",
  libxkbcommon0: "libxkbcommon",
  "libxkbcommon-x11-0": "libxkbcommon-x11",
  libxcb1: "libxcb",
  libxau6: "libXau",
  libxdmcp6: "libXdmcp",
  libfontconfig1: "fontconfig",
  "libwayland-client0": "libwayland-client",
  "libx11-6": "libX11",
  "libglib2.0-0t64": "glib2",
  "libgtk-3-0t64": "gtk3",
  "libsoup-3.0-0": "libsoup3",
  "libjavascriptcoregtk-4.1-0": "javascriptcoregtk4.1",
  "libwebkit2gtk-4.1-0": "webkit2gtk4.1",
};

// Libraries loaded at runtime through dlopen rather than linked NEEDED entries
// (GPUI loads the Wayland client and fontconfig configuration lazily).
export const EXTRA_RUNTIME_DEB_PACKAGES = ["libwayland-client0", "libfontconfig1"];

export function debDependsFromLibraries(neededLibraries) {
  const depends = new Set(EXTRA_RUNTIME_DEB_PACKAGES);
  const unmapped = [];
  for (const library of neededLibraries) {
    if (library === "linux-vdso.so.1") continue;
    const pkg = SHARED_LIBRARY_DEB_PACKAGES[library];
    if (pkg) depends.add(pkg);
    else unmapped.push(library);
  }
  if (unmapped.length > 0) {
    throw new Error(
      `Unmapped shared library dependencies need Debian package names: ${unmapped.join(", ")}`,
    );
  }
  return [...depends].sort();
}

// RPM Requires for the dependency set deb Depends was derived from. A single
// Fedora package can cover several Debian packages, so the list is deduped.
// Unknown Debian names fail packaging so an RPM can never ship missing
// Requires.
export function rpmRequiresFromDebPackages(debPackages, { maxGlibcVersion = null } = {}) {
  const unmapped = debPackages.filter((name) => !SHARED_LIBRARY_RPM_PACKAGES[name]);
  if (unmapped.length > 0) {
    throw new Error(`Unmapped Debian packages need RPM package names: ${unmapped.join(", ")}`);
  }
  const requires = new Set();
  for (const debPackage of debPackages) {
    const rpmPackage = SHARED_LIBRARY_RPM_PACKAGES[debPackage];
    requires.add(
      rpmPackage === "glibc" && maxGlibcVersion ? `glibc >= ${maxGlibcVersion}` : rpmPackage,
    );
  }
  return [...requires];
}

export function debControl({ version, arch, depends, installedSizeKb, maxGlibcVersion }) {
  const versionedDepends = maxGlibcVersion
    ? depends.map((dep) => (dep === "libc6" ? `libc6 (>= ${maxGlibcVersion})` : dep))
    : depends;
  return `Package: paseo-native
Version: ${debPackageVersion(version)}
Section: devel
Priority: optional
Architecture: ${DEB_ARCH[arch] ?? arch}
Installed-Size: ${installedSizeKb}
Maintainer: Mohamed Boudra <hello@moboudra.com>
Depends: ${versionedDepends.join(", ")}
Description: Paseo native (GPUI) desktop client (opt-in)
 Opt-in native desktop client packaged separately from the Electron
 Paseo desktop app. Bundles its own Node runtime and daemon. Shared
 library dependencies are derived from the packaged binary's NEEDED
 entries plus known dlopened libraries.
`;
}

// macOS bundle metadata. Bundle identifier is distinct from the Electron app
// (sh.paseo.desktop) so both clients can coexist during the migration.
export function infoPlist({ version }) {
  return `<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
	<key>CFBundleName</key>
	<string>Paseo Native</string>
	<key>CFBundleDisplayName</key>
	<string>Paseo Native</string>
	<key>CFBundleIdentifier</key>
	<string>sh.paseo.desktop.native</string>
	<key>CFBundleVersion</key>
	<string>${version}</string>
	<key>CFBundleShortVersionString</key>
	<string>${version}</string>
	<key>CFBundlePackageType</key>
	<string>APPL</string>
	<key>CFBundleExecutable</key>
	<string>PaseoNative</string>
	<key>CFBundleIconFile</key>
	<string>PaseoNative</string>
	<key>LSMinimumSystemVersion</key>
	<string>13.0.0</string>
	<key>NSHighResolutionCapable</key>
	<true/>
</dict>
</plist>
`;
}

// Relative paths that must exist inside a staged bundle before archiving.
export function requiredBundlePaths(platform) {
  const binary = platform === "win32" ? "paseo-desktop.exe" : "paseo-desktop";
  const launcher = platform === "win32" ? "PaseoNative.cmd" : "paseo";
  const shim = platform === "win32" ? "bin/paseo.cmd" : "bin/paseo";
  const node = `daemon/${nodeBinaryRelativePath(platform)}`;
  const paths = [
    binary,
    launcher,
    shim,
    node,
    "daemon/node_modules/@getpaseo/cli/dist/index.js",
    "daemon/node_modules/@getpaseo/server/dist/scripts/supervisor-entrypoint.js",
    "daemon/runtime.json",
    "daemon/external-dependencies.json",
  ];
  if (platform === "linux") paths.push("daemon/native-addons.json", "daemon/native-libraries.txt");
  return paths;
}

// Final on-disk .app contract, checked after assembleMacApp renames/moves the
// staged layout — the pre-rename check cannot catch post-assembly breakage.
export function requiredMacAppPaths() {
  return [
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
}

export function distCopyFilter(relativePath) {
  if (relativePath.endsWith(".map")) return false;
  if (relativePath.endsWith(".test.js") || relativePath.endsWith(".test.mjs")) return false;
  if (relativePath.endsWith(".spec.js") || relativePath.endsWith(".spec.mjs")) return false;
  if (relativePath.endsWith(".d.ts.tsbuildinfo")) return false;
  // The bundled daemon web UI duplicates ~10 MiB of renderer assets the native
  // client never serves; the Electron package excludes it for the same reason.
  const segments = relativePath.split(path.sep);
  if (segments.includes("web-ui")) return false;
  return true;
}
