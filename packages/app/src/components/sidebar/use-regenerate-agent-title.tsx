import { useCallback, useMemo, useState } from "react";
import { Text, View } from "react-native";
import { StyleSheet } from "react-native-unistyles";
import { useMutation } from "@tanstack/react-query";
import { useTranslation } from "react-i18next";
import { AdaptiveModalSheet } from "@/components/adaptive-modal-sheet";
import { Button } from "@/components/ui/button";
import { StatusRing } from "@/components/status-ring";
import { getHostRuntimeStore } from "@/runtime/host-runtime";
import { useHostFeature } from "@/runtime/host-features";
import { useToast } from "@/contexts/toast-context";

export function useRegenerateAgentTitle(serverId?: string, agentId?: string) {
  const { t } = useTranslation();
  const toast = useToast();
  const [visible, setVisible] = useState(false);
  const supported = useHostFeature(serverId, "agentTitleRegeneration");
  const mutation = useMutation({
    retry: false,
    mutationFn: async (targetAgentId: string) => {
      const client = getHostRuntimeStore().getClient(serverId ?? "");
      if (!client) throw new Error(t("sidebar.workspace.toasts.hostDisconnected"));
      return client.regenerateAgentTitle(targetAgentId);
    },
    onSuccess: () => {
      setVisible(false);
      toast.show(t("conversationNaming.updated"));
    },
  });
  const mutate = mutation.mutate;
  const start = useCallback(() => {
    if (!agentId || mutation.isPending) return;
    setVisible(true);
    mutate(agentId);
  }, [agentId, mutate, mutation.isPending]);
  const retry = useCallback(() => {
    if (mutation.variables) mutate(mutation.variables);
  }, [mutate, mutation.variables]);
  const close = useCallback(() => setVisible(false), []);
  const header = useMemo(() => ({ title: t("conversationNaming.action") }), [t]);
  const dialog = visible ? (
    <AdaptiveModalSheet
      visible={visible}
      header={header}
      onClose={close}
      testID="regenerate-agent-title-dialog"
    >
      <View style={styles.body}>
        <Text style={styles.description}>{t("conversationNaming.budget")}</Text>
        {mutation.isPending ? <StatusRing /> : null}
        {mutation.isError ? (
          <Text style={styles.error} testID="regenerate-agent-title-error">
            {mutation.error.message}
          </Text>
        ) : null}
        {mutation.isError ? (
          <Button variant="outline" onPress={retry} testID="regenerate-agent-title-retry">
            {t("conversationNaming.retry")}
          </Button>
        ) : null}
      </View>
    </AdaptiveModalSheet>
  ) : null;
  return {
    onRegenerate: supported && agentId ? start : undefined,
    pending: mutation.isPending,
    dialog,
  };
}

const styles = StyleSheet.create((theme) => ({
  body: { padding: theme.spacing[4], gap: theme.spacing[3] },
  description: { color: theme.colors.foregroundMuted, fontSize: theme.fontSize.sm },
  error: { color: theme.colors.statusDanger, fontSize: theme.fontSize.sm },
}));
