/// Xterm keyboard encoding. Printable input is committed by the native IME handler.
#[derive(Clone, Copy, Default)]
pub struct Modifiers {
    pub shift: bool,
    pub alt: bool,
    pub control: bool,
    pub platform: bool,
}

impl Modifiers {
    fn parameter(self) -> u8 {
        1 + u8::from(self.shift) + 2 * u8::from(self.alt) + 4 * u8::from(self.control)
    }
}

pub fn encode(key: &str, modifiers: Modifiers, application_cursor: bool) -> Option<String> {
    if modifiers.platform {
        return None;
    }
    let parameter = modifiers.parameter();
    let final_byte = match key {
        "up" => Some('A'),
        "down" => Some('B'),
        "right" => Some('C'),
        "left" => Some('D'),
        "home" => Some('H'),
        "end" => Some('F'),
        _ => None,
    };
    if let Some(final_byte) = final_byte {
        return Some(if parameter != 1 {
            format!("\x1b[1;{parameter}{final_byte}")
        } else if application_cursor {
            format!("\x1bO{final_byte}")
        } else {
            format!("\x1b[{final_byte}")
        });
    }
    if let Some(final_byte) = match key {
        "f1" => Some('P'),
        "f2" => Some('Q'),
        "f3" => Some('R'),
        "f4" => Some('S'),
        _ => None,
    } {
        return Some(if parameter == 1 {
            format!("\x1bO{final_byte}")
        } else {
            format!("\x1b[1;{parameter}{final_byte}")
        });
    }
    let tilde = match key {
        "insert" => Some(2),
        "delete" => Some(3),
        "pageup" => Some(5),
        "pagedown" => Some(6),
        "f5" => Some(15),
        "f6" => Some(17),
        "f7" => Some(18),
        "f8" => Some(19),
        "f9" => Some(20),
        "f10" => Some(21),
        "f11" => Some(23),
        "f12" => Some(24),
        _ => None,
    };
    if let Some(number) = tilde {
        return Some(if parameter == 1 {
            format!("\x1b[{number}~")
        } else {
            format!("\x1b[{number};{parameter}~")
        });
    }
    let text = match key {
        "enter" => Some("\r".into()),
        "backspace" => Some(if modifiers.control { "\x08" } else { "\x7f" }.into()),
        "escape" => Some("\x1b".into()),
        "tab" if modifiers.shift => Some("\x1b[Z".into()),
        "tab" => Some("\t".into()),
        _ if modifiers.control => control_character(key).map(|value| char::from(value).to_string()),
        _ if modifiers.alt && key.chars().count() == 1 && key.is_ascii() => Some(key.into()),
        _ => None,
    }?;
    Some(if modifiers.alt {
        format!("\x1b{text}")
    } else {
        text
    })
}

fn control_character(key: &str) -> Option<u8> {
    match key {
        "space" | " " | "@" | "2" => Some(0),
        "[" | "3" => Some(27),
        "\\" | "4" => Some(28),
        "]" | "5" => Some(29),
        "^" | "6" => Some(30),
        "_" | "-" | "7" => Some(31),
        "?" | "8" => Some(127),
        _ if key.len() == 1 && key.as_bytes()[0].is_ascii_alphabetic() => {
            Some(key.as_bytes()[0].to_ascii_uppercase() - b'A' + 1)
        }
        _ => None,
    }
}

pub fn paste(text: &str, bracketed: bool) -> String {
    let text = text.replace("\r\n", "\r").replace('\n', "\r");
    if bracketed {
        format!("\x1b[200~{}\x1b[201~", text.replace('\x1b', "␛"))
    } else {
        text
    }
}

/// Format canonical terminal selection text for the OS clipboard, like xterm.
pub fn clipboard_text(mut text: String, windows: bool) -> String {
    if text.contains('\u{a0}') {
        text = text.replace('\u{a0}', " ");
    }
    if windows && text.contains('\n') {
        text = text.replace('\n', "\r\n");
    }
    text
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clipboard_paste_matches_xterm_line_endings_and_visible_escape_sanitization() {
        let oracle: serde_json::Value =
            serde_json::from_str(include_str!("testdata/terminal-clipboard.json")).unwrap();
        for case in oracle["pastes"].as_array().unwrap() {
            assert_eq!(
                paste(
                    case["text"].as_str().unwrap(),
                    case["bracketed"].as_bool().unwrap()
                ),
                case["value"].as_str().unwrap()
            );
        }
        assert_eq!(paste("a\r\nb\nc\rd", false), "a\rb\rc\rd");
        assert_eq!(
            paste("a\r\nb\n\x1b[201~中", true),
            "\x1b[200~a\rb\r␛[201~中\x1b[201~"
        );
        assert_eq!(paste("\x1b[31m", false), "\x1b[31m");
    }

    #[test]
    fn copied_terminal_text_normalizes_nonbreaking_spaces_and_windows_line_endings() {
        assert_eq!(clipboard_text("中\u{a0}a\nb".into(), false), "中 a\nb");
        assert_eq!(clipboard_text("中\u{a0}a\nb".into(), true), "中 a\r\nb");
        assert_eq!(clipboard_text("  a  ".into(), true), "  a  ");
    }

    #[test]
    fn application_cursor_and_modifier_sequences_remain_distinct() {
        assert_eq!(
            encode("up", Modifiers::default(), false).as_deref(),
            Some("\x1b[A")
        );
        assert_eq!(
            encode("up", Modifiers::default(), true).as_deref(),
            Some("\x1bOA")
        );
        assert_eq!(
            encode(
                "right",
                Modifiers {
                    control: true,
                    shift: true,
                    ..Default::default()
                },
                true
            )
            .as_deref(),
            Some("\x1b[1;6C")
        );
        assert_eq!(
            encode("home", Modifiers::default(), true).as_deref(),
            Some("\x1bOH")
        );
    }

    #[test]
    fn function_keys_and_tilde_keys_encode_modifiers() {
        assert_eq!(
            encode("f1", Modifiers::default(), false).as_deref(),
            Some("\x1bOP")
        );
        assert_eq!(
            encode(
                "f12",
                Modifiers {
                    alt: true,
                    ..Default::default()
                },
                false
            )
            .as_deref(),
            Some("\x1b[24;3~")
        );
        assert_eq!(
            encode(
                "delete",
                Modifiers {
                    control: true,
                    ..Default::default()
                },
                false
            )
            .as_deref(),
            Some("\x1b[3;5~")
        );
    }

    #[test]
    fn control_punctuation_is_explicit_and_never_corrupts_unicode() {
        let control = Modifiers {
            control: true,
            ..Default::default()
        };
        assert_eq!(encode("space", control, false).as_deref(), Some("\0"));
        assert_eq!(encode("?", control, false).as_deref(), Some("\x7f"));
        assert_eq!(encode("c", control, false).as_deref(), Some("\x03"));
        assert_eq!(encode("1", control, false), None);
        assert_eq!(encode("中", control, false), None);
    }

    #[test]
    fn ime_and_platform_shortcuts_are_not_duplicated_as_terminal_text() {
        for key in ["a", "中", "😀"] {
            assert_eq!(encode(key, Modifiers::default(), false), None);
        }
        assert_eq!(
            encode(
                "c",
                Modifiers {
                    platform: true,
                    control: true,
                    ..Default::default()
                },
                false
            ),
            None
        );
        assert_eq!(
            encode(
                "x",
                Modifiers {
                    alt: true,
                    ..Default::default()
                },
                false
            )
            .as_deref(),
            Some("\x1bx")
        );
    }
}
