import { describe, expect, it } from "vitest";
import { BUNDLED_TERMINAL_FONT_FAMILY, resolveNativeTerminalFontFamily } from "./font.native";

describe("native terminal fonts", () => {
  it.each([
    undefined,
    "",
    "monospace",
    "CaskaydiaCove NFM",
    '"JetBrains Mono", "Maple Mono Normal NF CN"',
  ])("uses bundled Latin and Chinese for Android preference %s", (family) => {
    expect(resolveNativeTerminalFontFamily(family, "android")).toBe(BUNDLED_TERMINAL_FONT_FAMILY);
  });
  it("allows the bundled Maple family to be selected explicitly", () => {
    expect(resolveNativeTerminalFontFamily("Maple Mono Normal NF CN", "android")).toBe(
      "Maple Mono Normal NF CN",
    );
  });
  it("preserves iOS system font selection", () => {
    expect(resolveNativeTerminalFontFamily(undefined, "ios")).toBe("Menlo");
    expect(resolveNativeTerminalFontFamily("Courier", "ios")).toBe("Courier");
  });
});
