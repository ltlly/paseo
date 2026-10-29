# Bundled Android terminal fonts

Android registers `Paseo Terminal Mono` before React Native starts. It combines
JetBrains Mono (Latin, four styles) with Maple Mono Normal NF CN (Chinese and
Nerd Font symbols), followed by the system monospace fallback. Both supplied
fonts use a 0.6 em Latin cell; Maple's Chinese glyphs use 1.2 em.

The native terminal resolves its default and unavailable desktop fonts to this
family. No download, system font installation, or Expo font-loading race is
involved. `JetBrains Mono` and `Maple Mono Normal NF CN` are also registered as
explicit selections. The app requires Android API 29, where custom fallback
families are available.

Unmodified font sources:

- JetBrains Mono 2.304: <https://github.com/JetBrains/JetBrainsMono>
- Maple Mono Normal NF CN 7.900: <https://github.com/subframe7536/maple-font>

Both are distributed under the SIL Open Font License 1.1. Their licenses are
included beside the font assets in the APK.

`TerminalFontsTest` runs on Android and verifies the actual registered typeface
for all four styles: Latin and spaces occupy one cell, Chinese occupies two.
