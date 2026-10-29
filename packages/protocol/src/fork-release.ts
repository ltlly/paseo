/** First-party release metadata shared by clients and the standalone daemon. */
export const FORK_REPOSITORY = "ltlly/paseo";
export const FORK_RELEASES_URL = `https://github.com/${FORK_REPOSITORY}/releases`;
export const FORK_LATEST_RELEASE_API = `https://api.github.com/repos/${FORK_REPOSITORY}/releases/latest`;
export const FORK_UPDATE_FEED_URL = `${FORK_RELEASES_URL}/latest/download`;

export function parseForkVersion(version: string): number[] | null {
  const match = /^v?(\d+)\.(\d+)\.(\d+)-ltlly\.([1-9]\d*)$/.exec(version.trim());
  if (!match) return null;
  const parts = match.slice(1).map(Number);
  return parts.every(Number.isSafeInteger) ? parts : null;
}

export function isLegacyForkVersion(version: string): boolean {
  return /^v?0\.14\.[1-4]$/.test(version.trim()) || version === "0.14.0-mouse.1";
}

/** The original fork releases used 0.14.1–4 for patches on upstream 0.14.0. */
export function isNewerForkRelease(targetVersion: string, currentVersion: string): boolean {
  const target = parseForkVersion(targetVersion);
  if (!target) return false;
  const currentFork = parseForkVersion(currentVersion);
  const currentBase = /^v?(\d+)\.(\d+)\.(\d+)(?:-|$)/.exec(currentVersion.trim());
  let current = currentFork;
  if (!current && isLegacyForkVersion(currentVersion)) current = [0, 14, 0, 0];
  if (!current && currentBase) current = [...currentBase.slice(1).map(Number), 0];
  if (!current) return false;
  for (let i = 0; i < target.length; i++) {
    if (target[i] !== current[i]) return target[i] > current[i];
  }
  return false;
}
