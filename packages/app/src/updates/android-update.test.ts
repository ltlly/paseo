import { describe, expect, it } from "vitest";
import { parseAndroidUpdate } from "./android-update";
const version = "0.14.0-ltlly.1";
const name = `Paseo-${version}-arm64.apk`;
const url = `https://github.com/ltlly/paseo/releases/download/v${version}/${name}`;
const release = { tag_name: `v${version}`, assets: [{ name, browser_download_url: url }] };
describe("Android fork updates", () => {
  it("finds the full APK and migrates the old fork numbering", () => {
    expect(parseAndroidUpdate(release, "0.14.4")).toEqual({ version, url, hasUpdate: true });
    expect(parseAndroidUpdate(release, version)?.hasUpdate).toBe(false);
  });
  it("does not offer unpublished, unrelated or cross-repository assets", () => {
    expect(parseAndroidUpdate({ ...release, draft: true }, version)).toBeNull();
    expect(parseAndroidUpdate({ ...release, prerelease: true }, version)).toBeNull();
    expect(
      parseAndroidUpdate(
        { ...release, assets: [{ name, browser_download_url: url.replace("ltlly", "getpaseo") }] },
        version,
      ),
    ).toBeNull();
    expect(parseAndroidUpdate({ ...release, tag_name: "v0.14.4" }, version)).toBeNull();
  });
});
