import { expect, test } from "vitest";
import { createTerminalActions } from "./index.js";

test("an older host rejects terminal operations before sending requests", async () => {
  const unexpectedRequest = () => {
    throw new Error("Unexpected daemon request");
  };
  const terminals = createTerminalActions(
    {
      ensureConnected: () => {},
      getLastServerInfoMessage: () => ({
        status: "server_info",
        serverId: "older-host",
        hostname: null,
        version: null,
        features: {},
      }),
      createTerminal: unexpectedRequest,
      listTerminals: unexpectedRequest,
      sendTerminalInput: unexpectedRequest,
      writeTerminalRawInput: unexpectedRequest,
      captureTerminal: unexpectedRequest,
      killTerminal: unexpectedRequest,
    },
    unexpectedRequest,
  );

  await expect(terminals.create({ workspaceId: "wks_existing" })).rejects.toThrow(
    "Update the host",
  );
  await expect(terminals.list({ workspaceId: "wks_existing" })).rejects.toThrow("Update the host");
  const terminal = terminals.ref("terminal-existing");
  expect(terminal.current()).toBeNull();
  expect(() => terminal.write("text")).toThrow("Update the host");
  expect(() => terminal.sendKeys(["Enter"])).toThrow("Update the host");
  expect(() => terminal.capture()).toThrow("Update the host");
  await expect(terminal.kill()).rejects.toThrow("Update the host");
});

test("raw input public terminal handle exposes byte writes without converting UTF8 text", async () => {
  const writes: Array<{ id: string; bytes: Uint8Array; requestId?: string }> = [];
  const unavailable = () => {
    throw new Error("Unexpected request");
  };
  const terminals = createTerminalActions(
    {
      ensureConnected() {},
      getLastServerInfoMessage: () => ({
        status: "server_info",
        serverId: "raw-host",
        hostname: null,
        version: null,
        features: { workspaceTerminals: true },
      }),
      createTerminal: unavailable,
      listTerminals: unavailable,
      sendTerminalInput: unavailable,
      writeTerminalRawInput: async (id, bytes, requestId) => {
        writes.push({ id, bytes, requestId });
      },
      captureTerminal: unavailable,
      killTerminal: unavailable,
    },
    unavailable,
  );
  await terminals.ref("terminal-raw").writeRaw(new Uint8Array([128, 255]), "raw-handle");
  expect(writes).toEqual([
    { id: "terminal-raw", bytes: new Uint8Array([128, 255]), requestId: "raw-handle" },
  ]);
});
