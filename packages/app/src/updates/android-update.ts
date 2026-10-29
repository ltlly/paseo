import { checkPackageUpdate, parsePackageUpdate } from "./fork-package-update";
export type { PackageUpdate as AndroidUpdate } from "./fork-package-update";
const androidAssets = (version: string) => [
  `Paseo-${version}-universal.apk`,
  `Paseo-${version}-arm64.apk`,
];
export const parseAndroidUpdate = (value: unknown, currentVersion: string) =>
  parsePackageUpdate(value, currentVersion, androidAssets);
export const checkAndroidUpdate = (currentVersion: string, signal?: AbortSignal) =>
  checkPackageUpdate(currentVersion, androidAssets, signal);
