import { describe, expect, it } from "vitest";
import { DEFAULT_TERMINAL_FONT_FAMILY, resolveTerminalFontFamily } from "./terminal-font";

describe("terminal font fallback", () => {
  it("rejects a missing custom font falling through to proportional CJK UI text", () => {
    const requested = "CaskaydiaCove NFM, Microsoft YaHei UI, monospace";
    const measure = (family: string, text: string) =>
      family === requested ? ({ W: 14.246, i: 3.726, "0": 8.21, " ": 4 }[text] ?? 14) : 7.697;
    expect(resolveTerminalFontFamily(requested, measure)).toBe(DEFAULT_TERMINAL_FONT_FAMILY);
  });

  it("retains a working custom font and tolerates subpixel measurement rounding", () => {
    expect(
      resolveTerminalFontFamily("  Custom Mono, monospace  ", (_family, text) =>
        text === "W" ? 8.001 : 8,
      ),
    ).toBe("Custom Mono, monospace");
  });

  it("rechecks the requested stack after a custom font becomes available", () => {
    let loaded = false;
    const measure = (family: string, text: string) =>
      family === "Delayed Mono, sans-serif" && !loaded && text === "i" ? 3 : 8;
    expect(resolveTerminalFontFamily("Delayed Mono, sans-serif", measure)).toBe(
      DEFAULT_TERMINAL_FONT_FAMILY,
    );
    loaded = true;
    expect(resolveTerminalFontFamily("Delayed Mono, sans-serif", measure)).toBe(
      "Delayed Mono, sans-serif",
    );
  });

  it("uses generic monospace if the developer-font fallback also has invalid metrics", () => {
    expect(
      resolveTerminalFontFamily("Broken", (family) => (family === "monospace" ? 8 : NaN)),
    ).toBe("monospace");
  });
});
