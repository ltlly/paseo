import { afterEach, describe, expect, it, vi } from "vitest";
import {
  DEFAULT_TERMINAL_FONT_FAMILY,
  WINDOWS_TERMINAL_FONT_FAMILY,
  resolveBrowserTerminalFontFamily,
  resolveTerminalFontFamily,
} from "./terminal-font";

describe("terminal font fallback", () => {
  const originalDocument = (globalThis as { document?: unknown }).document;

  afterEach(() => {
    (globalThis as { document?: unknown }).document = originalDocument;
    vi.restoreAllMocks();
  });

  it("prefers installed Windows programming fonts when the configured stack is proportional", () => {
    expect(
      resolveTerminalFontFamily(
        "Missing Font, Microsoft YaHei UI",
        (family, text) => {
          if (family === WINDOWS_TERMINAL_FONT_FAMILY) return 8.2;
          return text === "i" ? 3 : 14;
        },
        WINDOWS_TERMINAL_FONT_FAMILY,
      ),
    ).toBe(WINDOWS_TERMINAL_FONT_FAMILY);
  });
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

  it("reuses the browser measurement canvas and caches resolved monospace stacks", () => {
    let customFontLoaded = false;
    const measureCalls: Array<{ font: string; text: string }> = [];
    const context = {
      font: "",
      measureText(text: string) {
        measureCalls.push({ font: this.font, text });
        if (this.font.includes("DelayedBrowserMono") && !customFontLoaded && text === "i") {
          return { width: 3 };
        }
        return { width: 8 };
      },
    };
    const getContext = vi.fn().mockReturnValue(context);
    const createElement = vi.fn().mockReturnValue({ getContext });
    (globalThis as { document?: unknown }).document = { createElement };

    expect(resolveBrowserTerminalFontFamily("CachedBrowserMono, monospace", 14)).toBe(
      "CachedBrowserMono, monospace",
    );
    const firstCallMeasures = measureCalls.length;
    expect(firstCallMeasures).toBeGreaterThan(0);

    expect(resolveBrowserTerminalFontFamily("CachedBrowserMono, monospace", 14)).toBe(
      "CachedBrowserMono, monospace",
    );
    expect(measureCalls.length).toBe(firstCallMeasures);
    expect(createElement).toHaveBeenCalledTimes(1);

    expect(resolveBrowserTerminalFontFamily("DelayedBrowserMono, sans-serif", 14)).toBe(
      DEFAULT_TERMINAL_FONT_FAMILY,
    );
    customFontLoaded = true;
    expect(resolveBrowserTerminalFontFamily("DelayedBrowserMono, sans-serif", 14)).toBe(
      "DelayedBrowserMono, sans-serif",
    );
  });
});
