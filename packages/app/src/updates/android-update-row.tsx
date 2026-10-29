import { useCallback, useEffect, useState } from "react";
import { Linking, Platform, Text, View } from "react-native";
import { useTranslation } from "react-i18next";
import { Button } from "@/components/ui/button";
import { settingsStyles } from "@/styles/settings";
import { checkAndroidUpdate, type AndroidUpdate } from "./android-update";

export function AndroidUpdateRow({ currentVersion }: { currentVersion: string | null }) {
  const { t } = useTranslation();
  const [update, setUpdate] = useState<AndroidUpdate | null>(null);
  const [status, setStatus] = useState<"idle" | "checking" | "ready" | "error">("idle");
  const [attempt, setAttempt] = useState(0);
  useEffect(() => {
    if (Platform.OS !== "android" || !currentVersion) return;
    const controller = new AbortController();
    setStatus("checking");
    const timeout = setTimeout(() => controller.abort(), 20_000);
    let active = true;
    void checkAndroidUpdate(currentVersion, controller.signal)
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
  }, [currentVersion, attempt]);
  const check = useCallback(() => setAttempt((value) => value + 1), []);
  const download = useCallback(() => {
    if (update?.hasUpdate) void Linking.openURL(update.url).catch(() => setStatus("error"));
  }, [update]);
  if (Platform.OS !== "android") return null;
  return (
    <View style={[settingsStyles.row, settingsStyles.rowBorder]}>
      <View style={settingsStyles.rowContent}>
        <Text style={settingsStyles.rowTitle}>{t("settings.about.androidUpdate.title")}</Text>
        <Text style={settingsStyles.rowHint}>
          {t(
            status === "error"
              ? "settings.about.androidUpdate.error"
              : "settings.about.androidUpdate.hint",
          )}
        </Text>
        {update?.hasUpdate ? <Text style={settingsStyles.rowHint}>{update.version}</Text> : null}
        {status === "ready" && !update?.hasUpdate ? (
          <Text style={settingsStyles.rowHint}>{t("settings.about.androidUpdate.current")}</Text>
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
          {t("settings.about.androidUpdate.download")}
        </Button>
      ) : null}
    </View>
  );
}
