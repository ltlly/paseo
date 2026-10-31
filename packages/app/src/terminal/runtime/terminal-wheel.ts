import type { Terminal } from "@xterm/xterm";

function applicationOwnsWheel(terminal: Terminal): boolean {
  const mouseMode = terminal.modes.mouseTrackingMode;
  return (
    (mouseMode !== "none" && mouseMode !== "x10") ||
    terminal.buffer.active.type === "alternate" ||
    terminal.options.scrollback === 0
  );
}

function wheelRowUnit(event: WheelEvent, cellHeight: number, rows: number): number {
  if (event.deltaMode === WheelEvent.DOM_DELTA_PIXEL) return 1 / cellHeight;
  if (event.deltaMode === WheelEvent.DOM_DELTA_PAGE) return rows;
  return 1;
}

/**
 * COMPAT(xterm-wheel-distance): added in v0.14.0-ltlly.2, remove after 2027-04-01
 * once xterm preserves wheel line counts and pixel distances in application mode.
 * xterm 6.1 beta.292 consumes N lines but emits just one report, and scales small
 * pixel deltas by 0.3. Encode one row through its public wheel handler, then
 * repeat the encoded input in one batch. Coordinates, modifiers, mouse encoding
 * and alternate-screen logic remain owned by xterm without a DOM event per row.
 */
export function attachTerminalWheelHandler(
  terminal: Terminal,
  onInput: (data: string) => void,
): (data: string) => void {
  let replaying = false;
  let wheelInput: string[] | null = null;
  let remainder = 0;
  let lastDirection = 0;
  let lastTime = 0;
  let lastRoute = "";

  terminal.attachCustomWheelEventHandler((event) => {
    if (replaying) return true;
    const mouseMode = terminal.modes.mouseTrackingMode;
    if (!applicationOwnsWheel(terminal) || event.shiftKey || event.deltaY === 0) {
      remainder = 0;
      return true;
    }
    const target = event.target;
    const screen = terminal.element?.querySelector<HTMLElement>(".xterm-screen");
    const cellHeight = (screen?.getBoundingClientRect().height ?? 0) / terminal.rows;
    if (!target || cellHeight <= 0 || !Number.isFinite(event.deltaY)) return false;

    const direction = Math.sign(event.deltaY);
    const route = `${mouseMode}:${terminal.buffer.active.type}`;
    if (direction !== lastDirection || route !== lastRoute || event.timeStamp - lastTime > 500) {
      remainder = 0;
    }
    lastDirection = direction;
    lastRoute = route;
    lastTime = event.timeStamp;

    const sensitivity =
      (terminal.options.scrollSensitivity ?? 1) *
      (event.altKey || event.ctrlKey ? (terminal.options.fastScrollSensitivity ?? 5) : 1);
    const unit = wheelRowUnit(event, cellHeight, terminal.rows);
    remainder += event.deltaY * unit * sensitivity;
    const wholeRows = Math.trunc(remainder);
    remainder -= wholeRows;
    const rowCount = Math.min(Math.abs(wholeRows), 1_000);
    if (rowCount === 0) {
      event.preventDefault();
      return false;
    }

    replaying = true;
    wheelInput = [];
    let encodedInput: string;
    try {
      target.dispatchEvent(
        new WheelEvent("wheel", {
          bubbles: true,
          cancelable: true,
          clientX: event.clientX,
          clientY: event.clientY,
          screenX: event.screenX,
          screenY: event.screenY,
          ctrlKey: event.ctrlKey,
          altKey: event.altKey,
          shiftKey: event.shiftKey,
          metaKey: event.metaKey,
          deltaMode: WheelEvent.DOM_DELTA_LINE,
          deltaY: direction / sensitivity,
        }),
      );
      encodedInput = wheelInput.join("");
    } finally {
      replaying = false;
      wheelInput = null;
    }
    if (encodedInput.length > 0) onInput(encodedInput.repeat(rowCount));
    event.preventDefault();
    return false;
  });

  return (data) => {
    if (wheelInput !== null) wheelInput.push(data);
    else onInput(data);
  };
}
