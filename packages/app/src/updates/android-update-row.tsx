import { Platform } from "react-native";
import { checkAndroidUpdate } from "./android-update";
import { PackageUpdateRow } from "./package-update-row";
export function AndroidUpdateRow({ currentVersion }: { currentVersion: string | null }) {
  if (Platform.OS !== "android") return null;
  return (
    <PackageUpdateRow
      currentVersion={currentVersion}
      checkUpdate={checkAndroidUpdate}
      kind="androidUpdate"
    />
  );
}
