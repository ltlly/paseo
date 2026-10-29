interface NativeTerminalMouseInput {
  protocol: number;
  encoding: number;
  point: { x: number; y: number };
  cellWidth: number;
  cellHeight: number;
  cols: number;
  screenRows: number;
}

function encodeMouseReport(
  input: NativeTerminalMouseInput,
  button: number,
  release = false,
): string {
  const col = Math.max(1, Math.min(input.cols, Math.floor(input.point.x / input.cellWidth) + 1));
  const row = Math.max(
    1,
    Math.min(input.screenRows, Math.floor(input.point.y / input.cellHeight) + 1),
  );
  let sequence: string;
  if (input.encoding === 1016) {
    const x = Math.max(
      1,
      Math.min(Math.floor(input.cols * input.cellWidth), Math.floor(input.point.x) + 1),
    );
    const y = Math.max(
      1,
      Math.min(Math.floor(input.screenRows * input.cellHeight), Math.floor(input.point.y) + 1),
    );
    sequence = `\x1b[<${button};${x};${y}${release ? "m" : "M"}`;
  } else if (input.encoding === 1006) {
    sequence = `\x1b[<${button};${col};${row}${release ? "m" : "M"}`;
  } else {
    // The original mouse encoding cannot address cells beyond 223.
    if (col > 223 || row > 223) return "";
    sequence = `\x1b[M${String.fromCharCode((release ? 3 : button) + 32, col + 32, row + 32)}`;
  }
  return sequence;
}

/** A null result leaves the gesture with the local scrollback viewport. */
export function encodeNativeTerminalWheel(
  input: NativeTerminalMouseInput & {
    direction: "up" | "down";
    rows: number;
  },
): string | null {
  if (![1000, 1002, 1003].includes(input.protocol)) return null;
  return encodeMouseReport(input, input.direction === "up" ? 64 : 65).repeat(
    Math.max(0, Math.floor(input.rows)),
  );
}

/** Emit a complete click only after a short tap, so long-press selection stays local. */
export function encodeNativeTerminalTap(input: NativeTerminalMouseInput): string | null {
  if (![9, 1000, 1002, 1003].includes(input.protocol)) return null;
  if (
    input.point.x < 0 ||
    input.point.y < 0 ||
    input.point.x >= input.cols * input.cellWidth ||
    input.point.y >= input.screenRows * input.cellHeight
  )
    return "";
  const press = encodeMouseReport(input, 0);
  return input.protocol === 9 ? press : press + encodeMouseReport(input, 0, true);
}
