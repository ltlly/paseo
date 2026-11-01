export interface TerminalInputModeFeedResult {
  changed: boolean;
  responses: string[];
}

export interface TerminalInputModeState {
  kittyKeyboardFlags: number;
  win32InputMode: boolean;
  applicationCursorKeys?: boolean;
  bracketedPaste?: boolean;
}

export const DEFAULT_TERMINAL_INPUT_MODE_STATE: TerminalInputModeState = {
  kittyKeyboardFlags: 0,
  win32InputMode: false,
  applicationCursorKeys: false,
  bracketedPaste: false,
};

// xterm bounds its CSI main parameters; never retain output or string payloads.
const MAX_CSI_PARAMS = 32;
const MAX_CSI_VALUE = 0x7fffffff;
const CONTROL_INTRODUCER = new RegExp(`[${String.fromCharCode(0x1b)}\\u0080-\\u009f]`, "g");
const STRING_CONTROL = new RegExp(`[${String.fromCharCode(0x18, 0x1a, 0x1b)}\\u0080-\\u009f]`, "g");
const OSC_CONTROL = new RegExp(`[${String.fromCharCode(7, 0x18, 0x1a, 0x1b)}\\u0080-\\u009f]`, "g");
type ParserState =
  | "ground"
  | "escape"
  | "escape-intermediate"
  | "csi-entry"
  | "csi-param"
  | "csi-ignore"
  | "osc"
  | "string";

export function terminalInputModeSupportsModifiedEnter(state: TerminalInputModeState): boolean {
  return state.kittyKeyboardFlags > 0 || state.win32InputMode;
}

export function terminalInputModeStatesEqual(
  left: TerminalInputModeState,
  right: TerminalInputModeState,
): boolean {
  return (
    left.kittyKeyboardFlags === right.kittyKeyboardFlags &&
    left.win32InputMode === right.win32InputMode &&
    Boolean(left.applicationCursorKeys) === Boolean(right.applicationCursorKeys) &&
    Boolean(left.bracketedPaste) === Boolean(right.bracketedPaste)
  );
}

export class TerminalInputModeTracker {
  private kittyKeyboardFlags = 0;
  private win32InputMode = false;
  private applicationCursorKeys = false;
  private bracketedPaste = false;
  private mouseProtocol = 0;
  private mouseEncoding = 0;
  private focusReporting = false;
  private readonly kittyKeyboardStack: number[] = [];
  private parserState: ParserState = "ground";
  private readonly params = new Int32Array(MAX_CSI_PARAMS);
  private readonly paramPresent = new Uint8Array(MAX_CSI_PARAMS);
  private paramIndex = 0;
  private prefix = "";
  private subparam = false;

  feed(data: string): TerminalInputModeFeedResult {
    const result: TerminalInputModeFeedResult = { changed: false, responses: [] };
    for (let cursor = 0; cursor < data.length; cursor += 1) {
      let delimiter: RegExp | null = null;
      if (this.parserState === "ground") delimiter = CONTROL_INTRODUCER;
      else if (this.parserState === "osc") delimiter = OSC_CONTROL;
      else if (this.parserState === "string") delimiter = STRING_CONTROL;
      if (delimiter) {
        delimiter.lastIndex = cursor;
        const introducer = delimiter.exec(data);
        if (!introducer) break;
        cursor = introducer.index;
      }
      this.observe(data.charCodeAt(cursor), result);
    }
    return result;
  }

  private observe(code: number, result: TerminalInputModeFeedResult): void {
    // The authoritative xterm decoder discards BOMs, including inside controls.
    if (code === 0xfeff) return;
    if (this.control(code)) return;
    if (this.parserState === "osc") {
      if (code === 7) this.parserState = "ground";
      return;
    }
    if (this.parserState === "string" || code < 0x20 || code === 0x7f) return;
    switch (this.parserState) {
      case "escape":
        this.escape(code, result);
        return;
      case "escape-intermediate":
        if (code >= 0x30) this.parserState = "ground";
        return;
      case "csi-entry":
      case "csi-param":
      case "csi-ignore":
        this.csi(code, result);
        return;
      default:
        return;
    }
  }

  private control(code: number): boolean {
    switch (code) {
      case 0x1b:
        this.parserState = "escape";
        return true;
      case 0x9b:
        this.startCsi();
        return true;
      case 0x9d:
        this.parserState = "osc";
        return true;
      case 0x90:
      case 0x98:
      case 0x9e:
      case 0x9f:
        this.parserState = "string";
        return true;
      default:
        if (code === 0x18 || code === 0x1a || (code >= 0x80 && code <= 0x9f)) {
          this.parserState = "ground";
          return true;
        }
        return false;
    }
  }

  private escape(code: number, result: TerminalInputModeFeedResult): void {
    switch (code) {
      case 0x5b:
        this.startCsi();
        return;
      case 0x5d:
        this.parserState = "osc";
        return;
      case 0x50:
      case 0x58:
      case 0x5e:
      case 0x5f:
        this.parserState = "string";
        return;
      case 0x63: {
        const hasModes =
          this.kittyKeyboardFlags !== 0 ||
          this.win32InputMode ||
          this.applicationCursorKeys ||
          this.bracketedPaste ||
          this.mouseProtocol !== 0 ||
          this.mouseEncoding !== 0 ||
          this.focusReporting;
        this.reset();
        result.changed = hasModes || result.changed;
        return;
      }
      default:
        this.parserState = code >= 0x20 && code <= 0x2f ? "escape-intermediate" : "ground";
    }
  }

  private csi(code: number, result: TerminalInputModeFeedResult): void {
    if (code >= 0x40 && code <= 0x7e) {
      if (this.parserState !== "csi-ignore") this.dispatchCsi(code, result);
      this.parserState = "ground";
      return;
    }
    if (this.parserState === "csi-ignore") return;
    if (this.parserState === "csi-entry" && code >= 0x3c && code <= 0x3f) {
      this.prefix = String.fromCharCode(code);
    } else if (code >= 0x30 && code <= 0x39) {
      if (!this.subparam && this.paramIndex < MAX_CSI_PARAMS) {
        this.params[this.paramIndex] = Math.min(
          MAX_CSI_VALUE,
          this.params[this.paramIndex] * 10 + code - 0x30,
        );
        this.paramPresent[this.paramIndex] = 1;
      }
    } else if (code === 0x3b) {
      this.paramIndex = Math.min(this.paramIndex + 1, MAX_CSI_PARAMS);
      this.subparam = false;
    } else if (code === 0x3a) {
      this.subparam = true;
    } else {
      this.parserState = code >= 0x20 && code <= 0x3f ? "csi-ignore" : "ground";
      return;
    }
    this.parserState = "csi-param";
  }

  private dispatchCsi(code: number, result: TerminalInputModeFeedResult): void {
    if (this.prefix === "?" && (code === 0x68 || code === 0x6c)) {
      for (let index = 0; index <= Math.min(this.paramIndex, MAX_CSI_PARAMS - 1); index += 1) {
        result.changed = this.applyPrivateMode(this.params[index], code === 0x68) || result.changed;
      }
    } else if (code === 0x75 && this.prefix !== "") {
      const kitty = this.applyKittyKeyboardSequence();
      result.changed = kitty.changed || result.changed;
      result.responses.push(...kitty.responses);
    }
  }

  private startCsi(): void {
    this.parserState = "csi-entry";
    this.params.fill(0);
    this.paramPresent.fill(0);
    this.paramIndex = 0;
    this.prefix = "";
    this.subparam = false;
  }

  reset(): void {
    this.kittyKeyboardFlags = 0;
    this.win32InputMode = false;
    this.applicationCursorKeys = false;
    this.bracketedPaste = false;
    this.mouseProtocol = 0;
    this.mouseEncoding = 0;
    this.focusReporting = false;
    this.kittyKeyboardStack.length = 0;
    this.parserState = "ground";
  }

  getState(): TerminalInputModeState {
    return {
      kittyKeyboardFlags: this.kittyKeyboardFlags,
      win32InputMode: this.win32InputMode,
      applicationCursorKeys: this.applicationCursorKeys,
      bracketedPaste: this.bracketedPaste,
    };
  }

  getKittyKeyboardFlags(): number {
    return this.kittyKeyboardFlags;
  }

  getMouseState(): { protocol: number; encoding: number } {
    return { protocol: this.mouseProtocol, encoding: this.mouseEncoding };
  }

  supportsModifiedEnter(): boolean {
    return terminalInputModeSupportsModifiedEnter(this.getState());
  }

  getPreamble(): string {
    const parts: string[] = [];
    if (this.kittyKeyboardFlags > 0) {
      parts.push(`\x1b[=${this.kittyKeyboardFlags};1u`);
    }
    if (this.win32InputMode) {
      parts.push("\x1b[?9001h");
    }
    if (this.applicationCursorKeys) {
      parts.push("\x1b[?1h");
    }
    if (this.bracketedPaste) {
      parts.push("\x1b[?2004h");
    }
    if (this.mouseProtocol) parts.push(`\x1b[?${this.mouseProtocol}h`);
    if (this.mouseEncoding) parts.push(`\x1b[?${this.mouseEncoding}h`);
    if (this.focusReporting) parts.push("\x1b[?1004h");
    return parts.join("");
  }

  private applyKittyKeyboardSequence(): { changed: boolean; responses: string[] } {
    const previousFlags = this.kittyKeyboardFlags;
    const first = this.paramPresent[0] ? this.params[0] : null;
    const second = this.paramPresent[1] ? this.params[1] : null;

    switch (this.prefix) {
      case ">": {
        this.kittyKeyboardStack.push(this.kittyKeyboardFlags);
        this.kittyKeyboardFlags = first ?? 1;
        break;
      }
      case "=": {
        const mode = second ?? 1;
        this.kittyKeyboardFlags = mode === 0 ? 0 : (first ?? 0);
        break;
      }
      case "<": {
        const count = Math.min(Math.max(1, first ?? 1), this.kittyKeyboardStack.length + 1);
        for (let index = 0; index < count; index += 1) {
          this.kittyKeyboardFlags = this.kittyKeyboardStack.pop() ?? 0;
        }
        break;
      }
      case "?":
        return {
          changed: false,
          responses: [`\x1b[?${this.kittyKeyboardFlags}u`],
        };
      default:
        return { changed: false, responses: [] };
    }

    return {
      changed: this.kittyKeyboardFlags !== previousFlags,
      responses: [],
    };
  }

  private applyPrivateMode(mode: number, enabled: boolean): boolean {
    switch (mode) {
      case 9001: {
        const changed = this.win32InputMode !== enabled;
        this.win32InputMode = enabled;
        return changed;
      }
      case 1: {
        const changed = this.applicationCursorKeys !== enabled;
        this.applicationCursorKeys = enabled;
        return changed;
      }
      case 2004: {
        const changed = this.bracketedPaste !== enabled;
        this.bracketedPaste = enabled;
        return changed;
      }
      case 9:
      case 1000:
      case 1002:
      case 1003: {
        const next = enabled ? mode : 0;
        const changed = this.mouseProtocol !== next;
        this.mouseProtocol = next;
        return changed;
      }
      case 1006:
      case 1016: {
        const next = enabled ? mode : 0;
        const changed = this.mouseEncoding !== next;
        this.mouseEncoding = next;
        return changed;
      }
      case 1004: {
        const changed = this.focusReporting !== enabled;
        this.focusReporting = enabled;
        return changed;
      }
      default:
        return false;
    }
  }
}
