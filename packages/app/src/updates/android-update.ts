import {
  FORK_LATEST_RELEASE_API,
  FORK_RELEASES_URL,
  isNewerForkRelease,
  parseForkVersion,
} from "@getpaseo/protocol/fork-release";

export interface AndroidUpdate {
  version: string;
  url: string;
  hasUpdate: boolean;
}

export function parseAndroidUpdate(value: unknown, currentVersion: string): AndroidUpdate | null {
  if (!value || typeof value !== "object") return null;
  const release = value as Record<string, unknown>;
  if (release.draft || release.prerelease || typeof release.tag_name !== "string") return null;
  const version = release.tag_name.replace(/^v/, "");
  if (!parseForkVersion(version) || !Array.isArray(release.assets)) return null;
  const names = [`Paseo-${version}-arm64.apk`, `Paseo-${version}-universal.apk`];
  for (const name of names) {
    const asset = release.assets.find((item) => item?.name === name);
    const expectedUrl = `${FORK_RELEASES_URL}/download/v${version}/${name}`;
    if (asset?.browser_download_url === expectedUrl) {
      return { version, url: expectedUrl, hasUpdate: isNewerForkRelease(version, currentVersion) };
    }
  }
  return null;
}

export async function checkAndroidUpdate(
  currentVersion: string,
  signal?: AbortSignal,
): Promise<AndroidUpdate | null> {
  const response = await fetch(FORK_LATEST_RELEASE_API, {
    signal,
    headers: { Accept: "application/vnd.github+json" },
  });
  if (!response.ok) throw new Error(`GitHub HTTP ${response.status}`);
  return parseAndroidUpdate(await response.json(), currentVersion);
}
