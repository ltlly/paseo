import { describe, expect, it } from "vitest";
import {
  classifyTerminalWorkerProtocol,
  supportsTerminalRawInput,
  adaptTerminalWorkerRequest,
  TERMINAL_WORKER_PROTOCOL_VERSION,
} from "./terminal-worker-protocol.js";

describe("classifyTerminalWorkerProtocol", () => {
  it("rejects the unreleased cell protocol without a draining adapter", () => {
    expect(classifyTerminalWorkerProtocol(1, 2)).toBe("obsolete");
    expect(classifyTerminalWorkerProtocol(2, 3)).toBe("obsolete");
    expect(classifyTerminalWorkerProtocol(3, 4)).toBe("obsolete");
    expect(classifyTerminalWorkerProtocol(4, 5)).toBe("obsolete");
  });
  it.each([
    [6, "current"],
    [5, "previous"],
    [4, "obsolete"],
    [-1, "obsolete"],
    [7, "newer"],
    // A version this daemon cannot read comes from a Paseo it does not know.
    ["6", "newer"],
    [4.5, "newer"],
    [Number.NaN, "newer"],
    [undefined, "newer"],
  ])("a worker reporting %s to a daemon at 6 is %s", (reported, fit) => {
    expect(classifyTerminalWorkerProtocol(reported, 6)).toBe(fit);
  });
});

describe("raw input worker compatibility", () => {
  it("advertises raw bytes only for POSIX owners of the new protocol", () => {
    expect(TERMINAL_WORKER_PROTOCOL_VERSION).toBe(6);
    expect(supportsTerminalRawInput(6, "linux")).toBe(true);
    expect(supportsTerminalRawInput(6, "darwin")).toBe(true);
    expect(supportsTerminalRawInput(6, "win32")).toBe(false);
    expect(supportsTerminalRawInput(5, "linux")).toBe(false);
    expect(supportsTerminalRawInput(7, "linux")).toBe(false);
  });
  it("keeps v5 string send and frozen envelopes intact, refusing new raw requests", () => {
    const send = {
      type: "send" as const,
      requestId: "input",
      terminalId: "term",
      message: { type: "input" as const, data: "中文🙂" },
    };
    expect(adaptTerminalWorkerRequest(send, 5)).toBe(send);
    const list = { type: "listTerminals" as const, requestId: "list" };
    expect(adaptTerminalWorkerRequest(list, 5)).toBe(list);
    const shutdown = { type: "shutdown" as const, requestId: "stop" };
    expect(adaptTerminalWorkerRequest(shutdown, 5)).toBe(shutdown);
    const raw = {
      type: "writeRawInput" as const,
      requestId: "raw",
      terminalId: "term",
      data: Buffer.from([128, 255]),
    };
    expect(() => adaptTerminalWorkerRequest(raw, 5)).toThrow("does not support raw-byte input");
    expect(adaptTerminalWorkerRequest(raw, 6)).toBe(raw);
  });
});
