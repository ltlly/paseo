import {
  FORK_LATEST_RELEASE_API,
  parseForkVersion,
  isNewerForkRelease,
} from "@getpaseo/protocol/fork-release";
import { getErrorMessage } from "@getpaseo/protocol/error-utils";
import {
  daemonInstallOriginRuntime,
  resolveNpmGlobalPrefix,
  resolveForkRuntimePrefix,
  validateDaemonInstallOrigin,
  type DaemonInstallOriginRuntime,
} from "./install-origin.js";
import { npmGlobalPaseoCli, type NpmGlobalPaseoCli } from "./npm-global-cli.js";

export type DaemonSelfUpdatePhase = "starting" | "downloading" | "installing" | "complete";

export interface DaemonSelfUpdateResult {
  success: boolean;
  error: string | null;
  newVersion: string | null;
}

export interface DaemonSelfUpdateInput {
  daemonVersion: string | null;
  desktopManaged: boolean;
  onProgress: (phase: DaemonSelfUpdatePhase) => void;
  logger: DaemonSelfUpdateLogger;
}

export interface DaemonSelfUpdateLogger {
  error(obj: object, msg?: string): void;
  warn(obj: object, msg?: string): void;
}

export interface DaemonSelfUpdateRuntime {
  npm: NpmGlobalPaseoCli;
  installOrigin: DaemonInstallOriginRuntime;
  resolveLatestForkTag?: () => Promise<string>;
}

export class DaemonSelfUpdateInProgressError extends Error {
  constructor() {
    super("An update is already in progress");
    this.name = "DaemonSelfUpdateInProgressError";
  }
}

const defaultRuntime: DaemonSelfUpdateRuntime = {
  npm: npmGlobalPaseoCli,
  installOrigin: daemonInstallOriginRuntime,
  resolveLatestForkTag,
};

async function resolveLatestForkTag(): Promise<string> {
  const response = await fetch(FORK_LATEST_RELEASE_API, { signal: AbortSignal.timeout(20_000) });
  if (!response.ok) throw new Error(`Could not check fork release: HTTP ${response.status}`);
  const release = await response.json();
  if (
    release.draft ||
    release.prerelease ||
    typeof release.tag_name !== "string" ||
    !parseForkVersion(release.tag_name)
  ) {
    throw new Error("The latest release is not a published fork revision");
  }
  const assets = new Set((release.assets ?? []).map((asset: { name: string }) => asset.name));
  if (!assets.has("getpaseo-cli.tgz") || !assets.has("getpaseo-server.tgz")) {
    throw new Error("The latest fork release is missing runtime packages");
  }
  return release.tag_name;
}

const DESKTOP_MANAGED_UPDATE_ERROR =
  "This daemon is managed by Paseo Desktop. Update Paseo Desktop on the host.";

export class DaemonSelfUpdater {
  private inProgress = false;

  constructor(private readonly runtime: DaemonSelfUpdateRuntime = defaultRuntime) {}

  async update(input: DaemonSelfUpdateInput): Promise<DaemonSelfUpdateResult> {
    if (input.desktopManaged) {
      return { success: false, error: DESKTOP_MANAGED_UPDATE_ERROR, newVersion: null };
    }

    if (this.inProgress) {
      throw new DaemonSelfUpdateInProgressError();
    }

    this.inProgress = true;
    try {
      input.onProgress("starting");
      const serverRoot = this.runtime.installOrigin.resolveCurrentServerPackageRoot();
      const forkPrefix = resolveForkRuntimePrefix(serverRoot);
      const releaseTag = forkPrefix
        ? await (this.runtime.resolveLatestForkTag ?? resolveLatestForkTag)()
        : undefined;
      const npmOptions = forkPrefix
        ? { prefix: forkPrefix, global: false, releaseTag }
        : { prefix: resolveNpmGlobalPrefix(serverRoot) };
      const install = await this.runtime.npm.inspect(npmOptions);
      const unsupportedReason = validateDaemonInstallOrigin(
        install,
        input.daemonVersion,
        this.runtime.installOrigin,
      );
      if (unsupportedReason) {
        return { success: false, error: unsupportedReason, newVersion: null };
      }

      if (releaseTag && !isNewerForkRelease(releaseTag, install.version)) {
        input.onProgress("complete");
        return { success: true, error: null, newVersion: install.version };
      }

      input.onProgress("downloading");
      input.onProgress("installing");

      const result = await this.runtime.npm.installLatest(npmOptions);
      if (result.exitCode !== 0) {
        const error =
          result.stderr.trim() || result.stdout.trim() || `npm exited with code ${result.exitCode}`;
        input.logger.error(
          { exitCode: result.exitCode, stderr: result.stderr },
          "Daemon self-update failed",
        );
        return { success: false, error, newVersion: null };
      }

      const updatedInstall = await this.runtime.npm.inspect(npmOptions).catch((error: unknown) => {
        input.logger.warn({ err: error }, "Unable to read updated npm package version");
        return null;
      });

      input.onProgress("complete");
      return { success: true, error: null, newVersion: updatedInstall?.version ?? null };
    } catch (error) {
      input.logger.error({ err: error }, "Daemon self-update failed with exception");
      return { success: false, error: getErrorMessage(error), newVersion: null };
    } finally {
      this.inProgress = false;
    }
  }
}

export const daemonSelfUpdater = new DaemonSelfUpdater();
