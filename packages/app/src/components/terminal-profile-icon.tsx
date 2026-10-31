import type { ReactElement } from "react";
import { SquareTerminal } from "lucide-react-native";
import { withUnistyles } from "react-native-unistyles";
import { useProviderIcon } from "@/components/provider-icons";
import type { Theme } from "@/styles/theme";

const mutedColorMapping = (theme: Theme) => ({ color: theme.colors.foregroundMuted });

const ThemedSquareTerminal = withUnistyles(SquareTerminal);

function ProviderProfileIcon({
  iconKey,
  serverId,
  size,
  color = "",
}: {
  iconKey: string;
  serverId?: string;
  size: number;
  color?: string;
}) {
  const Icon = useProviderIcon(iconKey, serverId);
  return <Icon size={size} color={color} />;
}

const ThemedProviderProfileIcon = withUnistyles(ProviderProfileIcon);

export function TerminalProfileIcon({
  iconKey,
  serverId,
  size = 14,
}: {
  iconKey: string | undefined;
  serverId?: string;
  size?: number;
}): ReactElement {
  if (!iconKey) {
    return <ThemedSquareTerminal size={size} uniProps={mutedColorMapping} />;
  }
  return (
    <ThemedProviderProfileIcon
      iconKey={iconKey}
      serverId={serverId}
      size={size}
      uniProps={mutedColorMapping}
    />
  );
}
