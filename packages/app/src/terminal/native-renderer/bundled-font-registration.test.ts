import { describe, expect, it } from "vitest";
const { configureTerminalFonts } = require("../../../modules/paseo-terminal-fonts/app.plugin");

describe("bundled terminal font registration", () => {
  it("registers fonts before React Native starts, once across repeated prebuilds", () => {
    const source = "override fun onCreate() {\n    super.onCreate()\n    loadReactNative(this)\n}";
    const result = configureTerminalFonts(source);
    expect(result.indexOf("PaseoTerminalFonts.install(this)")).toBeLessThan(
      result.indexOf("loadReactNative(this)"),
    );
    expect(configureTerminalFonts(result)).toBe(result);
  });
  it("fails prebuild when the application template has changed", () => {
    expect(() => configureTerminalFonts("different template")).toThrow(
      "Could not register bundled terminal fonts",
    );
  });
});
