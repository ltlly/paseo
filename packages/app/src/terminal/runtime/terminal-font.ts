const DEFAULT_TERMINAL_FONT_SIZE = 13;

export const DEFAULT_TERMINAL_FONT_FAMILY = [
  // Prefer common developer fonts, with Nerd Font variants for prompt/TUI glyphs.
  "JetBrains Mono",
  "JetBrainsMono Nerd Font",
  "JetBrainsMono NF",
  "MesloLGM Nerd Font",
  "MesloLGM NF",
  "Hack Nerd Font",
  "FiraCode Nerd Font",
  // PUA-only fallback (many Nerd glyphs live here on some systems).
  "Symbols Nerd Font",
  // System fallbacks.
  "SF Mono",
  "Menlo",
  "Monaco",
  "Consolas",
  "'Liberation Mono'",
  "monospace",
].join(", ");

export function resolveTerminalFontFamily(
  fontFamily: string | undefined,
  measureText?: (family: string, text: string) => number,
): string {
  const trimmed = fontFamily?.trim();
  const requested = trimmed && trimmed.length > 0 ? trimmed : DEFAULT_TERMINAL_FONT_FAMILY;
  if (!measureText) return requested;

  // Font preferences can travel between machines. A missing developer font
  // followed by a CJK UI font silently gives xterm proportional Latin glyphs.
  // Check the resolved stack, not document.fonts.check(), which also succeeds
  // for absent system fonts. Only Latin cells must be equal; CJK can be wide.
  for (const family of [requested, DEFAULT_TERMINAL_FONT_FAMILY, "monospace"]) {
    const widths = ["W", "i", "0", " "].map((text) => measureText(family, text));
    if (
      widths.every((width) => Number.isFinite(width) && width > 0) &&
      Math.max(...widths) - Math.min(...widths) < 0.1
    ) {
      return family;
    }
  }
  return "monospace";
}

export function resolveBrowserTerminalFontFamily(
  fontFamily: string | undefined,
  fontSize: number,
): string {
  const context =
    typeof document === "undefined" ? null : document.createElement("canvas").getContext("2d");
  return resolveTerminalFontFamily(
    fontFamily,
    context
      ? (family, text) => {
          context.font = `${fontSize}px ${family}`;
          return context.measureText(text).width;
        }
      : undefined,
  );
}

export function resolveTerminalFontSize(fontSize: number | undefined): number {
  return typeof fontSize === "number" && Number.isFinite(fontSize) && fontSize > 0
    ? fontSize
    : DEFAULT_TERMINAL_FONT_SIZE;
}
