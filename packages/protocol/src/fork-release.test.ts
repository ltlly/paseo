import { describe, expect, it } from "vitest";
import { isNewerForkRelease, parseForkVersion } from "./fork-release";

describe("fork release versions", () => {
  it("orders fork revisions numerically and follows the upstream base", () => {
    expect(isNewerForkRelease("0.14.0-ltlly.10", "0.14.0-ltlly.9")).toBe(true);
    expect(isNewerForkRelease("0.14.1-ltlly.1", "0.14.0-ltlly.99")).toBe(true);
    expect(isNewerForkRelease("0.14.0-ltlly.2", "0.14.1-ltlly.1")).toBe(false);
    expect(isNewerForkRelease("0.14.0-ltlly.1", "0.14.0-ltlly.1")).toBe(false);
  });
  it.each(["0.14.1", "0.14.2", "0.14.3", "0.14.4", "0.14.0-mouse.1"])(
    "migrates legacy fork %s without treating the renamed version as older",
    (version) => {
      expect(isNewerForkRelease("0.14.0-ltlly.1", version)).toBe(true);
    },
  );
  it("rejects unrelated, malformed and older releases", () => {
    expect(isNewerForkRelease("0.14.4", "0.14.0-ltlly.1")).toBe(false);
    expect(isNewerForkRelease("0.14.0-ltlly.1", "0.15.0")).toBe(false);
    expect(parseForkVersion("0.14.0-ltlly.0")).toBeNull();
    expect(parseForkVersion("0.14.0-beta.1")).toBeNull();
  });
});
