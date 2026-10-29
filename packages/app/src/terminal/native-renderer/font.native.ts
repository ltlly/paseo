import { Platform } from "react-native";
import { resolveTerminalFontFamily } from "../runtime/terminal-font";

const IOS_LOADED_MONO_FAMILIES: ReadonlySet<string> = new Set(["courier", "courier new", "menlo"]);

const ANDROID_LOADED_MONO_FAMILIES: ReadonlySet<string> = new Set([
  "monospace",
  "sans-serif-monospace",
]);

export const BUNDLED_TERMINAL_FONT_FAMILY = "Paseo Terminal Mono";

const IOS_MONO_ALIASES = new Map<string, string>([
  ["sf mono", "Menlo"],
  ["sfmono-regular", "Menlo"],
  ["ui-monospace", "Menlo"],
]);

function stripFontQuotes(family: string): string {
  const trimmed = family.trim();
  if (
    (trimmed.startsWith("'") && trimmed.endsWith("'")) ||
    (trimmed.startsWith('"') && trimmed.endsWith('"'))
  ) {
    return trimmed.slice(1, -1).trim();
  }
  return trimmed;
}

function splitFontStack(fontFamily: string): string[] {
  return fontFamily
    .split(",")
    .map(stripFontQuotes)
    .filter((family) => family.length > 0);
}

function loadedNativeFontFamily(family: string, platform: string): string | null {
  const normalized = family.toLowerCase();
  if (platform === "ios") {
    const alias = IOS_MONO_ALIASES.get(normalized);
    if (alias) return alias;
    return IOS_LOADED_MONO_FAMILIES.has(normalized) ? family : null;
  }
  if (platform === "android") {
    if (normalized === "maple mono normal nf cn") return "Maple Mono Normal NF CN";
    if (
      normalized === "jetbrains mono" ||
      normalized === BUNDLED_TERMINAL_FONT_FAMILY.toLowerCase() ||
      ANDROID_LOADED_MONO_FAMILIES.has(normalized)
    )
      return BUNDLED_TERMINAL_FONT_FAMILY;
    return null;
  }
  return normalized === "monospace" ? family : null;
}

export function resolveNativeTerminalFontFamily(
  fontFamily: string | undefined,
  platform: string = Platform.OS,
): string {
  const stack = resolveTerminalFontFamily(fontFamily);
  const loadedFamily = splitFontStack(stack)
    .map((family) => loadedNativeFontFamily(family, platform))
    .find((family): family is string => family !== null);
  if (loadedFamily) return loadedFamily;
  if (platform === "android") return BUNDLED_TERMINAL_FONT_FAMILY;
  return platform === "ios" ? "Menlo" : "monospace";
}
