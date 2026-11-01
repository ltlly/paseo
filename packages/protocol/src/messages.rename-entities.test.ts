import { z } from "zod";
import { describe, expect, test } from "vitest";
import {
  SubscribeTerminalResponseSchema,
  SessionInboundMessageSchema,
  SessionOutboundMessageSchema,
} from "./messages.js";

type SessionMessageOption = z.ZodDiscriminatedUnionOption<"type">;

function schemaWithoutMessageTypes(
  schema: { options: SessionMessageOption[] },
  excludedTypes: string[],
) {
  const excluded = new Set(excludedTypes);
  const options = schema.options.filter((option) => !excluded.has(option.shape.type.value));

  return z.discriminatedUnion("type", options as [SessionMessageOption, ...SessionMessageOption[]]);
}

describe("rename entity message schemas", () => {
  test("new client schema still parses old daemon checkout and terminal responses", () => {
    const checkoutResponse = SessionOutboundMessageSchema.parse({
      type: "checkout_switch_branch_response",
      payload: {
        cwd: "/tmp/repo",
        success: true,
        branch: "main",
        source: "local",
        error: null,
        requestId: "request-switch",
      },
    });
    const terminalResponse = SessionOutboundMessageSchema.parse({
      type: "kill_terminal_response",
      payload: {
        terminalId: "terminal-1",
        success: true,
        requestId: "request-kill",
      },
    });

    expect(checkoutResponse).toEqual({
      type: "checkout_switch_branch_response",
      payload: {
        cwd: "/tmp/repo",
        success: true,
        branch: "main",
        source: "local",
        error: null,
        requestId: "request-switch",
      },
    });
    expect(terminalResponse).toEqual({
      type: "kill_terminal_response",
      payload: {
        terminalId: "terminal-1",
        success: true,
        requestId: "request-kill",
      },
    });
  });

  test("old unions without rename variants reject rename messages and still parse existing messages", () => {
    const legacyInboundSchema = schemaWithoutMessageTypes(SessionInboundMessageSchema, [
      "terminal.rename.request",
      "checkout.rename_branch.request",
    ]);
    const legacyOutboundSchema = schemaWithoutMessageTypes(SessionOutboundMessageSchema, [
      "terminal.rename.response",
      "checkout.rename_branch.response",
    ]);

    expect(
      legacyInboundSchema.safeParse({
        type: "terminal.rename.request",
        terminalId: "terminal-1",
        title: "Server logs",
        requestId: "request-terminal-rename",
      }).success,
    ).toBe(false);
    expect(
      legacyInboundSchema.safeParse({
        type: "checkout.rename_branch.request",
        cwd: "/tmp/repo",
        branch: "feature/new-name",
        requestId: "request-branch-rename",
      }).success,
    ).toBe(false);
    expect(
      legacyOutboundSchema.safeParse({
        type: "terminal.rename.response",
        payload: {
          requestId: "request-terminal-rename",
          success: true,
          error: null,
        },
      }).success,
    ).toBe(false);
    expect(
      legacyOutboundSchema.safeParse({
        type: "checkout.rename_branch.response",
        payload: {
          requestId: "request-branch-rename",
          success: true,
          cwd: "/tmp/repo",
          currentBranch: "feature/new-name",
          error: null,
        },
      }).success,
    ).toBe(false);

    expect(
      legacyInboundSchema.parse({
        type: "checkout_switch_branch_request",
        cwd: "/tmp/repo",
        branch: "main",
        requestId: "request-switch",
      }),
    ).toEqual({
      type: "checkout_switch_branch_request",
      cwd: "/tmp/repo",
      branch: "main",
      requestId: "request-switch",
    });
    expect(
      legacyOutboundSchema.parse({
        type: "kill_terminal_response",
        payload: {
          terminalId: "terminal-1",
          success: true,
          requestId: "request-kill",
        },
      }),
    ).toEqual({
      type: "kill_terminal_response",
      payload: {
        terminalId: "terminal-1",
        success: true,
        requestId: "request-kill",
      },
    });
  });
});

describe("raw input message compatibility", () => {
  test("adds a bounded ID-addressed raw request without changing UTF8 input", () => {
    const request = {
      type: "terminal.input.write_raw.request",
      terminalId: "terminal-1",
      dataBase64: "gP8=",
      requestId: "raw-1",
    };
    expect(SessionInboundMessageSchema.parse(request)).toEqual(request);
    expect(
      SessionInboundMessageSchema.parse({
        type: "terminal_input",
        terminalId: "terminal-1",
        message: { type: "input", data: "中文🙂" },
      }),
    ).toEqual({
      type: "terminal_input",
      terminalId: "terminal-1",
      message: { type: "input", data: "中文🙂" },
    });
    const response = {
      type: "terminal.input.write_raw.response",
      payload: {
        terminalId: "terminal-1",
        requestId: "raw-1",
        success: false,
        error: "Terminal does not support raw input",
      },
    };
    expect(SessionOutboundMessageSchema.parse(response)).toEqual(response);
    const success = SubscribeTerminalResponseSchema.shape.payload.options[0];
    const legacySuccess = success.omit({ rawByteInput: true });
    const subscribed = {
      type: "subscribe_terminal_response",
      payload: {
        terminalId: "terminal-1",
        slot: 1,
        error: null,
        requestId: "sub-1",
        rawByteInput: false,
      },
    };
    expect(legacySuccess.parse(subscribed.payload)).toEqual({
      terminalId: "terminal-1",
      slot: 1,
      error: null,
      requestId: "sub-1",
    });
    const old = {
      ...subscribed,
      payload: { terminalId: "terminal-1", slot: 1, error: null, requestId: "sub-1" },
    };
    expect(SessionOutboundMessageSchema.parse(old)).toEqual(old);
    expect(
      SessionInboundMessageSchema.safeParse({ ...request, dataBase64: "A".repeat(87385) }).success,
    ).toBe(false);
  });
});
