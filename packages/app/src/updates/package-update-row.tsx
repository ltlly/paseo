import { useCallback, useEffect, useState } from "react";
import { Text, View } from "react-native";
import { useTranslation } from "react-i18next";
import { Button } from "@/components/ui/button";
import { settingsStyles } from "@/styles/settings";
import { openExternalUrl } from "@/utils/open-external-url";
import type { PackageUpdate } from "./fork-package-update";

export function PackageUpdateRow({
  currentVersion,
  checkUpdate,
  kind,
}: {
  currentVersion: string | null;
  checkUpdate: (version: string, signal?: AbortSignal) => Promise<PackageUpdate | null>;
  kind: "androidUpdate" | "macUpdate";
}) {
  const { t } = useTranslation();
  const [update, setUpdate] = useState<PackageUpdate | null>(null);
  const [status, setStatus] = useState<"idle" | "checking" | "ready" | "error">("idle");
  const [attempt, setAttempt] = useState(0);
  useEffect(() => {
    if (!currentVersion) return;
    const controller = new AbortController();
    setStatus("checking");
    const timeout = setTimeout(() => controller.abort(), 20_000);
    let active = true;
    void checkUpdate(currentVersion, controller.signal)
      .then((result) => {
        if (active) {
          setUpdate(result);
          setStatus("ready");
        }
        return undefined;
      })
      .catch(() => {
        if (active) setStatus("error");
      })
      .finally(() => clearTimeout(timeout));
    return () => {
      active = false;
      clearTimeout(timeout);
      controller.abort();
    };
  }, [currentVersion, attempt, checkUpdate]);
  const check = useCallback(() => setAttempt((value) => value + 1), []);
  const download = useCallback(() => {
    if (update?.hasUpdate) void openExternalUrl(update.url).catch(() => setStatus("error"));
  }, [update]);
  return (
    <View style={[settingsStyles.row, settingsStyles.rowBorder]}>
      <View style={settingsStyles.rowContent}>
        <Text style={settingsStyles.rowTitle}>{t(`settings.about.${kind}.title`)}</Text>
        <Text style={settingsStyles.rowHint}>
          {t(status === "error" ? `settings.about.${kind}.error` : `settings.about.${kind}.hint`)}
        </Text>
        {update?.hasUpdate ? <Text style={settingsStyles.rowHint}>{update.version}</Text> : null}
        {status === "ready" && !update?.hasUpdate ? (
          <Text style={settingsStyles.rowHint}>{t(`settings.about.${kind}.current`)}</Text>
        ) : null}
      </View>
      <Button variant="outline" size="sm" disabled={status === "checking"} onPress={check}>
        {t(
          status === "checking"
            ? "settings.about.updates.checking"
            : "settings.about.updates.check",
        )}
      </Button>
      {update?.hasUpdate ? (
        <Button size="sm" onPress={download}>
          {t(`settings.about.${kind}.download`)}
        </Button>
      ) : null}
    </View>
  );
}
