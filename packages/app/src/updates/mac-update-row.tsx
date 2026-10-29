import { getDesktopRuntimeInfo } from "@/desktop/updates/desktop-updates";
import { checkPackageUpdate } from "./fork-package-update";
import { PackageUpdateRow } from "./package-update-row";
async function checkMacUpdate(currentVersion: string, signal?: AbortSignal) {
  const runtime = await getDesktopRuntimeInfo();
  const arch =
    runtime.runningUnderARM64Translation || runtime.architecture === "arm64" ? "arm64" : "x64";
  return checkPackageUpdate(currentVersion, (version) => [`Paseo-${version}-${arch}.dmg`], signal);
}
export function MacUpdateRow({ currentVersion }: { currentVersion: string | null }) {
  return (
    <PackageUpdateRow
      currentVersion={currentVersion}
      checkUpdate={checkMacUpdate}
      kind="macUpdate"
    />
  );
}
