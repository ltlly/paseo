import { describe, expect, it } from "vitest";
import { TerminalInputModeTracker } from "./terminal-input-mode.js";

describe("TerminalInputModeTracker", () => {
  it("activates from a pushed Kitty keyboard mode and builds a replay preamble", () => {
    const tracker = new TerminalInputModeTracker();

    expect(tracker.feed("\x1b[>7u").changed).toBe(true);

    expect(tracker.supportsModifiedEnter()).toBe(true);
    expect(tracker.getKittyKeyboardFlags()).toBe(7);
    expect(tracker.getPreamble()).toBe("\x1b[=7;1u");
  });

  it("tracks split terminal output chunks", () => {
    const tracker = new TerminalInputModeTracker();

    tracker.feed("\x1b[>");
    const result = tracker.feed("1u");

    expect(result.changed).toBe(true);
    expect(tracker.getKittyKeyboardFlags()).toBe(1);
  });

  it("restores pushed Kitty keyboard modes when the foreground program pops them", () => {
    const tracker = new TerminalInputModeTracker();

    tracker.feed("\x1b[>1u");
    tracker.feed("\x1b[>7u");
    expect(tracker.getKittyKeyboardFlags()).toBe(7);

    expect(tracker.feed("\x1b[<u").changed).toBe(true);
    expect(tracker.getKittyKeyboardFlags()).toBe(1);

    expect(tracker.feed("\x1b[<u").changed).toBe(true);
    expect(tracker.supportsModifiedEnter()).toBe(false);
  });

  it("answers Kitty keyboard mode queries with the current flags", () => {
    const tracker = new TerminalInputModeTracker();
    tracker.feed("\x1b[=3;1u");

    expect(tracker.feed("\x1b[?u").responses).toEqual(["\x1b[?3u"]);
  });

  it("tracks ConPTY Win32 input mode and replays it after snapshots", () => {
    const tracker = new TerminalInputModeTracker();

    expect(tracker.feed("\x1b[?9001h").changed).toBe(true);

    expect(tracker.getState()).toEqual({
      kittyKeyboardFlags: 0,
      win32InputMode: true,
      applicationCursorKeys: false,
      bracketedPaste: false,
    });
    expect(tracker.supportsModifiedEnter()).toBe(true);
    expect(tracker.getPreamble()).toBe("\x1b[?9001h");

    expect(tracker.feed("\x1b[?9001l").changed).toBe(true);
    expect(tracker.supportsModifiedEnter()).toBe(false);
  });

  it("keeps Kitty and Win32 input modes independent", () => {
    const tracker = new TerminalInputModeTracker();

    tracker.feed("\x1b[>7u\x1b[?9001h");

    expect(tracker.getState()).toEqual({
      kittyKeyboardFlags: 7,
      win32InputMode: true,
      applicationCursorKeys: false,
      bracketedPaste: false,
    });
    expect(tracker.getPreamble()).toBe("\x1b[=7;1u\x1b[?9001h");
  });

  it("tracks application cursor keys mode independently", () => {
    const tracker = new TerminalInputModeTracker();

    expect(tracker.feed("\x1b[?1h").changed).toBe(true);
    expect(tracker.getState()).toEqual({
      kittyKeyboardFlags: 0,
      win32InputMode: false,
      applicationCursorKeys: true,
      bracketedPaste: false,
    });
    expect(tracker.getPreamble()).toBe("\x1b[?1h");

    expect(tracker.feed("\x1b[?1l").changed).toBe(true);
    expect(tracker.getState()).toEqual({
      kittyKeyboardFlags: 0,
      win32InputMode: false,
      applicationCursorKeys: false,
      bracketedPaste: false,
    });
  });

  it("tracks bracketed paste mode independently", () => {
    const tracker = new TerminalInputModeTracker();

    expect(tracker.feed("\x1b[?2004h").changed).toBe(true);
    expect(tracker.getState()).toEqual({
      kittyKeyboardFlags: 0,
      win32InputMode: false,
      applicationCursorKeys: false,
      bracketedPaste: true,
    });
    expect(tracker.getPreamble()).toBe("\x1b[?2004h");

    expect(tracker.feed("\x1b[?2004l").changed).toBe(true);
    expect(tracker.getState()).toEqual({
      kittyKeyboardFlags: 0,
      win32InputMode: false,
      applicationCursorKeys: false,
      bracketedPaste: false,
    });
    expect(tracker.getPreamble()).toBe("");
  });

  it("ignores encoded key input sequences", () => {
    const tracker = new TerminalInputModeTracker();

    tracker.feed("\x1b[13;2u");

    expect(tracker.supportsModifiedEnter()).toBe(false);
  });
});
it("replays mouse tracking, SGR encoding and focus reporting after a restore", () => {
  const tracker = new TerminalInputModeTracker();
  tracker.feed("\x1b[?1003;1006;1004h");
  expect(tracker.getPreamble()).toBe("\x1b[?1003h\x1b[?1006h\x1b[?1004h");
  tracker.feed("\x1b[?1000l");
  expect(tracker.getMouseState()).toEqual({ protocol: 0, encoding: 1006 });
  expect(tracker.getPreamble()).toBe("\x1b[?1006h\x1b[?1004h");
  tracker.feed("\x1b[?1003;1006;1004l");
  expect(tracker.getPreamble()).toBe("");
  tracker.feed("\x1b[?9;1016;1004h");
  tracker.reset();
  expect(tracker.getPreamble()).toBe("");
});

describe("incremental input mode observation", () => {
  it("preserves every chunk boundary, including ESC before CSI and RIS", () => {
    const data = "\x1b[>7u\x1b[?9001;1;2004;1003;1016;1004h\x1b[?u\x1bc\x1b[?u\x1b[?9;1006h";
    const expectedState = {
      kittyKeyboardFlags: 0,
      win32InputMode: false,
      applicationCursorKeys: false,
      bracketedPaste: false,
    };
    for (let split = 0; split <= data.length; split += 1) {
      const tracker = new TerminalInputModeTracker();
      const responses = [...tracker.feed(data.slice(0, split)).responses];
      expect(tracker.feed("")).toEqual({ changed: false, responses: [] });
      responses.push(...tracker.feed(data.slice(split)).responses);
      expect(responses).toEqual(["\x1b[?7u", "\x1b[?0u"]);
      expect(tracker.getState()).toEqual(expectedState);
      expect(tracker.getMouseState()).toEqual({ protocol: 9, encoding: 1006 });
      expect(tracker.getPreamble()).toBe("\x1b[?9h\x1b[?1006h");
    }
    const tracker = new TerminalInputModeTracker();
    const responses = [...data].flatMap((character) => tracker.feed(character).responses);
    expect(responses).toEqual(["\x1b[?7u", "\x1b[?0u"]);
    expect(tracker.getState()).toEqual(expectedState);
    expect(tracker.getMouseState()).toEqual({ protocol: 9, encoding: 1006 });
  });

  it("resets mouse categories even when the disabled mode is not the active mode", () => {
    for (const active of [9, 1000, 1002, 1003]) {
      for (const disabled of [9, 1000, 1002, 1003]) {
        const tracker = new TerminalInputModeTracker();
        tracker.feed(`\x1b[?${active};1016;1004h`);
        expect(tracker.feed(`\x1b[?${disabled}l`).changed).toBe(true);
        expect(tracker.getMouseState()).toEqual({ protocol: 0, encoding: 1016 });
        expect(tracker.getPreamble()).toBe("\x1b[?1016h\x1b[?1004h");
      }
    }
    for (const active of [1006, 1016]) {
      for (const disabled of [1006, 1016]) {
        const tracker = new TerminalInputModeTracker();
        tracker.feed(`\x1b[?1003;${active}h`);
        tracker.feed(`\x1b[?${disabled}l`);
        expect(tracker.getMouseState()).toEqual({ protocol: 1003, encoding: 0 });
      }
    }
  });

  it("applies repeated parameters in order with xterm parameter limits", () => {
    const tracker = new TerminalInputModeTracker();
    tracker.feed("\x1b[?1003;1000;1003;1006;1016;1006h");
    expect(tracker.getMouseState()).toEqual({ protocol: 1003, encoding: 1006 });
    tracker.feed("\x1b[?1002:5;1016h");
    expect(tracker.getMouseState()).toEqual({ protocol: 1002, encoding: 1016 });
    tracker.feed(`\x1b[?${"9".repeat(10000)};1003h`);
    expect(tracker.getMouseState()).toEqual({ protocol: 1003, encoding: 1016 });
    tracker.feed(`\x1b[?${Array(40).fill(1000).join(";")};1003h`);
    expect(tracker.getMouseState()).toEqual({ protocol: 1000, encoding: 1016 });
  });

  it("ignores string payloads but lets real ESC sequences abort them like xterm", () => {
    const tracker = new TerminalInputModeTracker();
    for (const introducer of ["]", "P", "X", "^", "_"]) {
      const payload = `\x1b${introducer}title [?1003h [?u c${"x".repeat(10000)}\x1b\\`;
      for (const chunk of [payload.slice(0, 1), payload.slice(1, -1), payload.slice(-1)]) {
        expect(tracker.feed(chunk)).toEqual({ changed: false, responses: [] });
      }
      expect(tracker.getPreamble()).toBe("");
    }
    tracker.feed("\x1bPqpayload\x07[?1000h");
    expect(tracker.getPreamble()).toBe("");
    tracker.feed("\x1b[?1003h");
    expect(tracker.getMouseState()).toEqual({ protocol: 1003, encoding: 0 });
    tracker.feed("\x1b]title\x1b[?1016h");
    expect(tracker.getMouseState()).toEqual({ protocol: 1003, encoding: 1016 });
    tracker.feed("\x9dtitle [?9h\x9c\x9b?1002h");
    expect(tracker.getMouseState()).toEqual({ protocol: 1002, encoding: 1016 });
  });

  it("recovers from cancellation and invalid CSI without confusing cursor controls with modes", () => {
    const tracker = new TerminalInputModeTracker();
    tracker.feed("\x1b[?1003\x18h\x1b[?1003\x1ah\x1b[0?9h\x1b[?1003 h");
    expect(tracker.getPreamble()).toBe("");
    tracker.feed("\x1b\x1b[?10\x0003\x7fh");
    expect(tracker.getMouseState()).toEqual({ protocol: 1003, encoding: 0 });
    tracker.feed("\x1b#c\x1b7\x1b8\x1b[s\x1b[u\x1b[?1003s\x1b[?1003r\x1b[!p");
    expect(tracker.getPreamble()).toBe("\x1b[?1003h");
  });

  it("RIS clears saved Kitty flags and replay modes without reviving them on pop", () => {
    const tracker = new TerminalInputModeTracker();
    tracker.feed("\x1b[>1u\x1b[>7u\x1b[?9001;1;2004;1003;1016;1004h");
    const saved = tracker.getPreamble();
    tracker.feed("\x1b");
    expect(tracker.feed("c")).toEqual({ changed: true, responses: [] });
    expect(tracker.getPreamble()).toBe("");
    expect(tracker.feed("\x1b[<u\x1b[?u")).toEqual({ changed: false, responses: ["\x1b[?0u"] });
    const restored = new TerminalInputModeTracker();
    restored.feed(saved);
    expect(restored.getState()).toEqual({
      kittyKeyboardFlags: 7,
      win32InputMode: true,
      applicationCursorKeys: true,
      bracketedPaste: true,
    });
    expect(restored.getMouseState()).toEqual({ protocol: 1003, encoding: 1016 });
    expect(restored.getPreamble()).toBe(saved);
    restored.feed("\x1bc");
    expect(restored.getState()).toEqual(tracker.getState());
    expect(restored.getMouseState()).toEqual(tracker.getMouseState());
  });
});
