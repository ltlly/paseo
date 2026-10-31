import { useCallback, useLayoutEffect, useRef, type RefCallback } from "react";
import type { View as NativeView } from "react-native";
import { STATUS_RING_PERIOD_MS } from "@/components/status-ring/geometry";
import { useRetainedPanelActive } from "@/components/retained-panel";
import { useAppVisible } from "@/hooks/use-app-visible";

const STATUS_RING_KEYFRAMES: PropertyIndexedKeyframes = {
  transform: ["rotate(0deg)", "rotate(360deg)"],
};
const STATUS_RING_TIMING: KeyframeAnimationOptions = {
  duration: STATUS_RING_PERIOD_MS,
  easing: "linear",
  iterations: Number.POSITIVE_INFINITY,
};
const STATUS_RING_TIMELINE_START_MS = 0;

export function useStatusRingAnimationRef(): RefCallback<NativeView> {
  const panelActive = useRetainedPanelActive();
  const appVisible = useAppVisible();
  const arcElement = useRef<HTMLElement | null>(null);
  const setArcElement = useCallback((instance: NativeView | null) => {
    arcElement.current = instance instanceof HTMLElement ? instance : null;
  }, []);

  useLayoutEffect(() => {
    const element = arcElement.current;
    if (!element || !panelActive || !appVisible) {
      return;
    }

    let animation: Animation | null = null;
    const observer = new IntersectionObserver((entries) => {
      for (const entry of entries) {
        if (entry.isIntersecting) {
          if (animation) continue;
          animation = element.animate(STATUS_RING_KEYFRAMES, STATUS_RING_TIMING);
          animation.startTime = STATUS_RING_TIMELINE_START_MS;
        } else {
          animation?.cancel();
          animation = null;
        }
      }
    });
    observer.observe(element);
    return () => {
      observer.disconnect();
      animation?.cancel();
    };
  }, [panelActive, appVisible]);

  return setArcElement;
}
