import { memo } from "react";
import { View } from "react-native";
import Animated, { useAnimatedStyle } from "react-native-reanimated";
import {
  StatusRingFrame,
  type StatusRingProps,
  rotatorStyles,
  styles,
} from "@/components/status-ring/frame";
import { getStatusRingRotation } from "@/components/status-ring/geometry";
import { useSmallWidgetClock } from "@/hooks/use-small-widget-clock";

/**
 * Native running indicator. The rotation is published by one shared UI-thread clock rather than
 * per-instance timing, so a ring that mounts mid-flight is already in phase.
 *
 * The rotated view carries no theme-tracked style; the coloured arc is nested inside it. Putting
 * a Unistyles style on a Reanimated view crashes on theme change (docs/unistyles.md).
 */
export const StatusRing = memo(function StatusRing({ backdrop, size, children }: StatusRingProps) {
  const time = useSmallWidgetClock();
  const rotatorStyle = useAnimatedStyle(() => ({
    transform: [{ rotate: `${getStatusRingRotation(time.value)}deg` }],
  }));

  return (
    <StatusRingFrame backdrop={backdrop} size={size} content={children}>
      <Animated.View
        style={[
          rotatorStyles.rotator,
          size === undefined ? null : { width: size, height: size },
          rotatorStyle,
        ]}
      >
        <View style={[styles.arc, size === undefined ? null : { width: size, height: size }]} />
      </Animated.View>
    </StatusRingFrame>
  );
});
