import { memo } from "react";
import { View } from "react-native";
import {
  StatusRingFrame,
  type StatusRingProps,
  rotatorStyles,
  styles,
} from "@/components/status-ring/frame";
import { useStatusRingAnimationRef } from "@/components/status-ring/clock.web";

/**
 * Web running indicator. A sidebar full of agents can hold dozens of these at once, so the browser
 * drives the rotation from one absolute document-timeline epoch rather than from mount time — see
 * `clock.web.ts`.
 */
export const StatusRing = memo(function StatusRing({ backdrop, size, children }: StatusRingProps) {
  const rotatorRef = useStatusRingAnimationRef();

  return (
    <StatusRingFrame backdrop={backdrop} size={size} content={children}>
      <View
        ref={rotatorRef}
        style={[rotatorStyles.rotator, size === undefined ? null : { width: size, height: size }]}
      >
        <View style={[styles.arc, size === undefined ? null : { width: size, height: size }]} />
      </View>
    </StatusRingFrame>
  );
});
