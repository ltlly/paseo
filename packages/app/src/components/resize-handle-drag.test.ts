import { describe, expect, it, vi } from "vitest";
import { startResizeHandleDrag } from "@/components/resize-handle-drag";

describe("startResizeHandleDrag", () => {
  it("previews pointer frames and commits only the final size", () => {
    const preview = vi.fn();
    const commit = vi.fn();
    const drag = startResizeHandleDrag({
      sizes: [0.5, 0.5],
      index: 0,
      preview,
      commit,
    });

    drag.move(0.05);
    drag.move(0.1);

    expect(preview).toHaveBeenCalledTimes(2);
    expect(preview.mock.calls[0]?.[0][0]).toBeCloseTo(0.55, 10);
    expect(preview.mock.calls[0]?.[0][1]).toBeCloseTo(0.45, 10);
    expect(preview.mock.calls[1]?.[0][0]).toBeCloseTo(0.6, 10);
    expect(preview.mock.calls[1]?.[0][1]).toBeCloseTo(0.4, 10);
    expect(commit).not.toHaveBeenCalled();

    drag.finish();

    expect(commit).toHaveBeenCalledOnce();
    expect(commit.mock.calls[0]?.[0][0]).toBeCloseTo(0.6, 10);
    expect(commit.mock.calls[0]?.[0][1]).toBeCloseTo(0.4, 10);
  });

  it("coalesces rapid pointer moves into one preview per animation frame when scheduled", () => {
    const preview = vi.fn();
    const commit = vi.fn();
    const scheduledCallbacks: Array<() => void> = [];
    const scheduleFrame = vi.fn((cb: () => void) => {
      scheduledCallbacks.push(cb);
      return 1;
    });
    const cancelFrame = vi.fn();

    const drag = startResizeHandleDrag({
      sizes: [0.5, 0.5],
      index: 0,
      preview,
      commit,
      scheduleFrame,
      cancelFrame,
    });

    drag.move(0.05);
    drag.move(0.1);
    drag.move(0.15);

    expect(scheduleFrame).toHaveBeenCalledOnce();
    expect(preview).not.toHaveBeenCalled();

    scheduledCallbacks[0]?.();
    expect(preview).toHaveBeenCalledOnce();
    expect(preview.mock.calls[0]?.[0][0]).toBeCloseTo(0.65, 10);

    drag.move(0.2);
    drag.finish();

    expect(cancelFrame).toHaveBeenCalledWith(1);
    expect(commit).toHaveBeenCalledOnce();
    expect(commit.mock.calls[0]?.[0][0]).toBeCloseTo(0.7, 10);
  });
});
