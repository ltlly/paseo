const { app, BrowserWindow, Menu, nativeTheme } = require("electron");
const path = require("node:path");
const fs = require("node:fs");
const os = require("node:os");
const profile = fs.mkdtempSync(path.join(os.tmpdir(), "paseo-mouse-oracle-"));
app.setPath("userData", profile);
app.commandLine.appendSwitch("force-device-scale-factor", "1");
app.whenReady().then(() => {
  nativeTheme.themeSource = "dark";
  Menu.setApplicationMenu(null);
  return new BrowserWindow({
    width: Number(process.env.PASEO_QA_WIDTH || 1280),
    height: 800,
    useContentSize: true,
    frame: false,
    show: true,
    title: "Paseo UI reference",
    webPreferences: { nodeIntegration: false, contextIsolation: true },
  }).loadURL("about:blank");
});
app.on("window-all-closed", () => app.quit());
app.on("will-quit", () => fs.rmSync(profile, { recursive: true, force: true }));
