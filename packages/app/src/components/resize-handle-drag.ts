import { computeResizeHandleSizes } from "@/components/resize-handle-sizes";

interface StartResizeHandleDragInput {
  sizes: number[];
  index: number;
  preview: (sizes: number[]) => void;
  commit: (sizes: number[]) => void;
  scheduleFrame?: (callback: () => void) => number;
  cancelFrame?: (handle: number) => void;
}

export interface ResizeHandleDrag {
  move: (deltaRatio: number) => void;
  finish: () => void;
}

export function startResizeHandleDrag({
  sizes,
  index,
  preview,
  commit,
  scheduleFrame,
  cancelFrame,
}: StartResizeHandleDragInput): ResizeHandleDrag {
  let pendingSizes: number[] | null = null;
  let frameHandle: number | null = null;

  return {
    move(deltaRatio) {
      pendingSizes = computeResizeHandleSizes({ sizes, index, deltaRatio });
      if (!scheduleFrame) {
        preview(pendingSizes);
        return;
      }
      if (frameHandle !== null) {
        return;
      }
      frameHandle = scheduleFrame(() => {
        frameHandle = null;
        if (pendingSizes) {
          preview(pendingSizes);
        }
      });
    },
    finish() {
      if (frameHandle !== null) {
        cancelFrame?.(frameHandle);
        frameHandle = null;
      }
      if (pendingSizes) {
        commit(pendingSizes);
        pendingSizes = null;
      }
    },
  };
}
