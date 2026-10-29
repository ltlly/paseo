import { describe, expect, it } from "vitest";
import { parsePackageUpdate } from "./fork-package-update";
const version = "0.14.0-ltlly.2";
const names = ["arm64", "x64"].map((arch) => `Paseo-${version}-${arch}.dmg`);
const release = {
  tag_name: `v${version}`,
  assets: names.map((name) => ({
    name,
    browser_download_url: `https://github.com/ltlly/paseo/releases/download/v${version}/${name}`,
  })),
};
describe("fork package downloads", () => {
  it("selects the exact macOS architecture from the published fork release", () => {
    for (const arch of ["arm64", "x64"]) {
      const update = parsePackageUpdate(release, "0.14.0-ltlly.1", (v) => [
        `Paseo-${v}-${arch}.dmg`,
      ]);
      expect(update?.hasUpdate).toBe(true);
      expect(update?.url).toContain(`-${arch}.dmg`);
    }
  });
  it("does not substitute a missing architecture or follow another repository", () => {
    expect(
      parsePackageUpdate({ ...release, assets: [release.assets[0]] }, "0.14.0-ltlly.1", (v) => [
        `Paseo-${v}-x64.dmg`,
      ]),
    ).toBeNull();
    expect(
      parsePackageUpdate(
        {
          ...release,
          assets: [
            { ...release.assets[0], browser_download_url: "https://example.com/update.dmg" },
          ],
        },
        "0.14.0-ltlly.1",
        (v) => [`Paseo-${v}-arm64.dmg`],
      ),
    ).toBeNull();
  });
});
