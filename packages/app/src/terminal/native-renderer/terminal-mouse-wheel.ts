/** A null result leaves the gesture with the local scrollback viewport. */
export function encodeNativeTerminalWheel(input: {
  protocol: number;
  encoding: number;
  direction: "up" | "down";
  rows: number;
  point: { x: number; y: number };
  cellWidth: number;
  cellHeight: number;
  cols: number;
  screenRows: number;
}): string | null {
  if (![1000, 1002, 1003].includes(input.protocol)) return null;
  const button = input.direction === "up" ? 64 : 65;
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
    sequence = `\x1b[<${button};${x};${y}M`;
  } else if (input.encoding === 1006) {
    sequence = `\x1b[<${button};${col};${row}M`;
  } else {
    // The original mouse encoding cannot address cells beyond 223.
    if (col > 223 || row > 223) return "";
    sequence = `\x1b[M${String.fromCharCode(button + 32, col + 32, row + 32)}`;
  }
  return sequence.repeat(Math.max(0, Math.floor(input.rows)));
}
