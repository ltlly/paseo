const { withMainApplication } = require("expo/config-plugins");
const REGISTRATION = "sh.paseo.fonts.PaseoTerminalFonts.install(this)";
function configureTerminalFonts(contents) {
  if (contents.includes(REGISTRATION)) return contents;
  const onCreate = /override fun onCreate\(\)\s*\{\s*super\.onCreate\(\)/;
  if (!onCreate.test(contents))
    throw new Error("Could not register bundled terminal fonts before React Native starts");
  return contents.replace(onCreate, (match) => `${match}\n    ${REGISTRATION}`);
}
module.exports = (config) =>
  withMainApplication(config, (mod) => {
    mod.modResults.contents = configureTerminalFonts(mod.modResults.contents);
    return mod;
  });
module.exports.configureTerminalFonts = configureTerminalFonts;
