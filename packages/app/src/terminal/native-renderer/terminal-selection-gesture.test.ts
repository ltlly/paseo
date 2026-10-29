import { describe, expect, it } from "vitest";
import { TerminalInputModeTracker } from "@getpaseo/protocol/terminal-input-mode";
import { encodeNativeTerminalTap, encodeNativeTerminalWheel } from "./terminal-mouse-wheel";

import {
  TERMINAL_GESTURE_LONG_PRESS_MS,
  TERMINAL_GESTURE_TAP_TOLERANCE_PX,
  TERMINAL_GESTURE_VERTICAL_SCROLL_THRESHOLD_PX,
  advanceTerminalScrollGesture,
  classifyTerminalGestureIntent,
  resolveTerminalGestureReleaseAction,
} from "./terminal-selection-gesture";

describe("native terminal gesture policy", () => {
  describe("advanceTerminalScrollGesture", () => {
    it("keeps consuming a slow drag after scroll intent arms below one row", () => {
      let state = { lastDy: 0, rowRemainder: 0, didScroll: false };
      const rowDeltas: number[] = [];

      for (const currentDy of [13, 15, 18, 22, 28]) {
        const update = advanceTerminalScrollGesture({
          state,
          currentDy,
          cellHeight: 14,
        });
        state = update.state;
        rowDeltas.push(update.rowDelta);
      }

      expect({ rowDeltas, state }).toEqual({
        rowDeltas: [0, 1, 0, 0, 1],
        state: { lastDy: 28, rowRemainder: 0, didScroll: true },
      });
    });
  });

  describe("classifyTerminalGestureIntent", () => {
    it("treats a motionless press as a tap", () => {
      expect(
        classifyTerminalGestureIntent({
          status: "pressing",
          dx: 0,
          dy: 0,
          vx: 0,
          vy: 0,
        }),
      ).toEqual("tap");
    });

    it("treats a tiny jitter within tap tolerance as a tap", () => {
      expect(
        classifyTerminalGestureIntent({
          status: "pressing",
          dx: TERMINAL_GESTURE_TAP_TOLERANCE_PX - 4,
          dy: TERMINAL_GESTURE_TAP_TOLERANCE_PX - 4,
          vx: 0,
          vy: 0,
        }),
      ).toEqual("tap");
    });

    it("leaves horizontal movement available to terminal selection", () => {
      expect(
        classifyTerminalGestureIntent({
          status: "pressing",
          dx: TERMINAL_GESTURE_TAP_TOLERANCE_PX + 20,
          dy: 4,
          vx: 0,
          vy: 0,
        }),
      ).toEqual("pending");
    });

    it("does not infer selection from slow horizontal drag dwell", () => {
      expect(
        classifyTerminalGestureIntent({
          status: "pressing",
          dx: TERMINAL_GESTURE_TAP_TOLERANCE_PX + 40,
          dy: 3,
          vx: 0,
          vy: 0,
          elapsedMs: TERMINAL_GESTURE_LONG_PRESS_MS + 50,
        }),
      ).toEqual("pending");
    });

    it("keeps tiny horizontal jitter inside tap tolerance as a tap", () => {
      expect(
        classifyTerminalGestureIntent({
          status: "pressing",
          dx: TERMINAL_GESTURE_TAP_TOLERANCE_PX - 1,
          dy: 1,
          vx: 2,
          vy: 0,
          elapsedMs: 20,
        }),
      ).toEqual("tap");
    });

    it("keeps horizontal movement below navigation distance pending", () => {
      expect(
        classifyTerminalGestureIntent({
          status: "pressing",
          dx: TERMINAL_GESTURE_TAP_TOLERANCE_PX + 10,
          dy: 2,
          vx: 0,
          vy: 0,
        }),
      ).toEqual("pending");
    });

    it("treats a clearly vertical drag as scroll", () => {
      expect(
        classifyTerminalGestureIntent({
          status: "pressing",
          dx: 4,
          dy: TERMINAL_GESTURE_VERTICAL_SCROLL_THRESHOLD_PX + 5,
          vx: 0,
          vy: 0.2,
        }),
      ).toEqual("scroll");
    });

    it("does not navigate on a fast rightward swipe", () => {
      expect(
        classifyTerminalGestureIntent({
          status: "pressing",
          dx: TERMINAL_GESTURE_TAP_TOLERANCE_PX + 40,
          dy: 4,
          vx: 2,
          vy: 0,
        }),
      ).toEqual("pending");
    });

    it("does not navigate on a slow rightward swipe", () => {
      expect(
        classifyTerminalGestureIntent({
          status: "pressing",
          dx: TERMINAL_GESTURE_TAP_TOLERANCE_PX + 40,
          dy: 4,
          vx: 0,
          vy: 0,
        }),
      ).toEqual("pending");
    });

    it("does not navigate on a fast leftward swipe", () => {
      expect(
        classifyTerminalGestureIntent({
          status: "pressing",
          dx: -(TERMINAL_GESTURE_TAP_TOLERANCE_PX + 40),
          dy: -4,
          vx: -2,
          vy: 0,
        }),
      ).toEqual("pending");
    });

    it("does not navigate while movement stays inside tap tolerance", () => {
      expect(
        classifyTerminalGestureIntent({
          status: "pressing",
          dx: TERMINAL_GESTURE_TAP_TOLERANCE_PX - 1,
          dy: 2,
          vx: 2,
          vy: 0,
        }),
      ).toEqual("tap");
    });

    it("keeps selection intent once selection is active", () => {
      expect(
        classifyTerminalGestureIntent({
          status: "selecting",
          dx: 100,
          dy: 50,
          vx: 2,
          vy: 1,
        }),
      ).toEqual("select");
    });
  });

  describe("resolveTerminalGestureReleaseAction", () => {
    it("focuses the terminal after a normal tap", () => {
      expect(
        resolveTerminalGestureReleaseAction({
          status: "pressing",
          didScroll: false,
          didNavigate: false,
          movedBeyondTapTolerance: false,
          pressDurationMs: 80,
          longPressMs: TERMINAL_GESTURE_LONG_PRESS_MS,
          scrollMode: "following",
        }),
      ).toEqual("focus");
    });

    it("focuses the terminal after a normal tap while scrolled", () => {
      expect(
        resolveTerminalGestureReleaseAction({
          status: "pressing",
          didScroll: false,
          didNavigate: false,
          movedBeyondTapTolerance: false,
          pressDurationMs: 80,
          longPressMs: TERMINAL_GESTURE_LONG_PRESS_MS,
          scrollMode: "scrolled",
        }),
      ).toEqual("focus");
    });

    it("starts selection after a long press", () => {
      expect(
        resolveTerminalGestureReleaseAction({
          status: "pressing",
          didScroll: false,
          didNavigate: false,
          movedBeyondTapTolerance: false,
          pressDurationMs: TERMINAL_GESTURE_LONG_PRESS_MS + 50,
          longPressMs: TERMINAL_GESTURE_LONG_PRESS_MS,
          scrollMode: "following",
        }),
      ).toEqual("select");
    });

    it("starts selection after a 500ms hold", () => {
      expect(
        resolveTerminalGestureReleaseAction({
          status: "pressing",
          didScroll: false,
          didNavigate: false,
          movedBeyondTapTolerance: false,
          pressDurationMs: 500,
          longPressMs: TERMINAL_GESTURE_LONG_PRESS_MS,
          scrollMode: "following",
        }),
      ).toEqual("select");
    });

    it("keeps a 400ms press as a tap", () => {
      expect(
        resolveTerminalGestureReleaseAction({
          status: "pressing",
          didScroll: false,
          didNavigate: false,
          movedBeyondTapTolerance: false,
          pressDurationMs: 400,
          longPressMs: TERMINAL_GESTURE_LONG_PRESS_MS,
          scrollMode: "following",
        }),
      ).toEqual("focus");
    });

    it("does nothing after a scroll gesture", () => {
      expect(
        resolveTerminalGestureReleaseAction({
          status: "pressing",
          didScroll: true,
          didNavigate: false,
          movedBeyondTapTolerance: true,
          pressDurationMs: 80,
          longPressMs: TERMINAL_GESTURE_LONG_PRESS_MS,
          scrollMode: "scrolled",
        }),
      ).toEqual("none");
    });

    it("does nothing after a navigation swipe", () => {
      expect(
        resolveTerminalGestureReleaseAction({
          status: "pressing",
          didScroll: false,
          didNavigate: true,
          movedBeyondTapTolerance: true,
          pressDurationMs: 80,
          longPressMs: TERMINAL_GESTURE_LONG_PRESS_MS,
          scrollMode: "following",
        }),
      ).toEqual("none");
    });

    it("does nothing while actively selecting", () => {
      expect(
        resolveTerminalGestureReleaseAction({
          status: "selecting",
          didScroll: false,
          didNavigate: false,
          movedBeyondTapTolerance: true,
          pressDurationMs: 80,
          longPressMs: TERMINAL_GESTURE_LONG_PRESS_MS,
          scrollMode: "following",
        }),
      ).toEqual("none");
    });

    it("clears an existing selection after a short tap", () => {
      expect(
        resolveTerminalGestureReleaseAction({
          status: "selecting",
          startedWithSelection: true,
          didScroll: false,
          didNavigate: false,
          movedBeyondTapTolerance: false,
          pressDurationMs: 80,
          longPressMs: TERMINAL_GESTURE_LONG_PRESS_MS,
          scrollMode: "following",
        }),
      ).toEqual("clear");
    });

    it("preserves a word selection created by the current long press", () => {
      expect(
        resolveTerminalGestureReleaseAction({
          status: "selecting",
          startedWithSelection: false,
          didScroll: false,
          didNavigate: false,
          movedBeyondTapTolerance: false,
          pressDurationMs: TERMINAL_GESTURE_LONG_PRESS_MS + 50,
          longPressMs: TERMINAL_GESTURE_LONG_PRESS_MS,
          scrollMode: "following",
        }),
      ).toEqual("none");
    });

    it("does not focus after an undecided fast drag", () => {
      expect(
        resolveTerminalGestureReleaseAction({
          status: "pressing",
          didScroll: false,
          didNavigate: false,
          movedBeyondTapTolerance: true,
          pressDurationMs: 80,
          longPressMs: TERMINAL_GESTURE_LONG_PRESS_MS,
          scrollMode: "following",
        }),
      ).toEqual("none");
    });

    it("does nothing from idle", () => {
      expect(
        resolveTerminalGestureReleaseAction({
          status: "idle",
          didScroll: false,
          didNavigate: false,
          movedBeyondTapTolerance: false,
          pressDurationMs: 80,
          longPressMs: TERMINAL_GESTURE_LONG_PRESS_MS,
          scrollMode: "following",
        }),
      ).toEqual("none");
    });
  });
});

describe("native terminal wheel routing", () => {
  const geometry = {
    point: { x: 25, y: 45 },
    cellWidth: 10,
    cellHeight: 20,
    cols: 80,
    screenRows: 24,
  };
  it("sends each accumulated row once, in both directions, and returns to local history after mouse mode exits", () => {
    const tracker = new TerminalInputModeTracker();
    const wheel = (direction: "up" | "down", rows: number) =>
      encodeNativeTerminalWheel({ ...geometry, ...tracker.getMouseState(), direction, rows });
    expect(wheel("up", 1)).toBeNull();
    tracker.feed("\x1b[?100");
    tracker.feed("0;1006h");
    let state = { lastDy: 0, rowRemainder: 0, didScroll: false };
    let output = "";
    for (const currentDy of [13, 25, 39, 45, 20]) {
      const update = advanceTerminalScrollGesture({ state, currentDy, cellHeight: 20 });
      state = update.state;
      output += wheel(update.rowDelta > 0 ? "up" : "down", Math.abs(update.rowDelta));
    }
    expect(output).toBe("\x1b[<64;3;3M\x1b[<64;3;3M\x1b[<65;3;3M");
    const restored = new TerminalInputModeTracker();
    restored.feed(tracker.getPreamble());
    expect(restored.getMouseState()).toEqual(tracker.getMouseState());
    tracker.feed("\x1b[?1000l");
    expect(wheel("down", 1)).toBeNull();
  });
  it.each([1000, 1002, 1003])(
    "supports legacy wheel reports for mode %s and clamps the pointer to the screen",
    (protocol) => {
      expect(
        encodeNativeTerminalWheel({
          ...geometry,
          point: { x: -10, y: 9999 },
          protocol,
          encoding: 0,
          direction: "down",
          rows: 1,
        }),
      ).toBe("\x1b[Ma!8");
    },
  );
  it("uses pixel coordinates for SGR pixel mode and ignores X10's press-only mode", () => {
    expect(
      encodeNativeTerminalWheel({
        ...geometry,
        protocol: 1003,
        encoding: 1016,
        direction: "up",
        rows: 1,
      }),
    ).toBe("\x1b[<64;26;46M");
    expect(
      encodeNativeTerminalWheel({
        ...geometry,
        protocol: 9,
        encoding: 1006,
        direction: "up",
        rows: 1,
      }),
    ).toBeNull();
  });
});

it("turns a short tap into a complete left click at its grid cell, while long presses stay local", () => {
  const input = {
    protocol: 1000,
    encoding: 1006,
    point: { x: 25, y: 45 },
    cellWidth: 10,
    cellHeight: 20,
    cols: 80,
    screenRows: 24,
  };
  expect(encodeNativeTerminalTap(input)).toBe("\x1b[<0;3;3M\x1b[<0;3;3m");
  expect(encodeNativeTerminalTap({ ...input, encoding: 0 })).toBe("\x1b[M ##\x1b[M###");
  expect(encodeNativeTerminalTap({ ...input, protocol: 9 })).toBe("\x1b[<0;3;3M");
  expect(encodeNativeTerminalTap({ ...input, encoding: 1016 })).toBe(
    "\x1b[<0;26;46M\x1b[<0;26;46m",
  );
  expect(encodeNativeTerminalTap({ ...input, protocol: 0 })).toBeNull();
  expect(encodeNativeTerminalTap({ ...input, point: { x: 10, y: 9999 } })).toBe("");
  expect(
    resolveTerminalGestureReleaseAction({
      status: "selecting",
      startedWithSelection: true,
      didScroll: false,
      didNavigate: false,
      movedBeyondTapTolerance: false,
      pressDurationMs: 20,
      longPressMs: 450,
      scrollMode: "following",
    }),
  ).toBe("clear");
  expect(
    resolveTerminalGestureReleaseAction({
      status: "pressing",
      didScroll: false,
      didNavigate: false,
      movedBeyondTapTolerance: false,
      pressDurationMs: 500,
      longPressMs: 450,
      scrollMode: "following",
    }),
  ).toBe("select");
});
