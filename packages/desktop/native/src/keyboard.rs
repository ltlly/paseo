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
        1 + u8::from(self.shift)
            + 2 * u8::from(self.alt)
            + 4 * u8::from(self.control)
            + 8 * u8::from(self.platform)
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum Action {
    Send(String),
    CommitText,
    Page(i32),
    SelectAll,
    Ignore,
}

pub fn legacy_action(
    key: &str,
    modifiers: Modifiers,
    application_cursor: bool,
    mac: bool,
) -> Action {
    let Modifiers {
        shift,
        alt,
        control,
        platform,
    } = modifiers;
    if mac && !shift && !control {
        let editing = match (key, platform, alt) {
            ("left", true, false) => Some("\x01"),
            ("right", true, false) => Some("\x05"),
            ("backspace", true, false) => Some("\x15"),
            ("left", false, true) => Some("\x1bb"),
            ("right", false, true) => Some("\x1bf"),
            _ => None,
        };
        if let Some(data) = editing {
            return Action::Send(data.into());
        }
        if platform && !alt && key.eq_ignore_ascii_case("a") {
            return Action::SelectAll;
        }
    }
    if shift && matches!(key, "pageup" | "pagedown") {
        return Action::Page(if key == "pageup" { 1 } else { -1 });
    }
    let parameter = modifiers.parameter();
    let letter = match key {
        "up" if !platform => Some('A'),
        "down" if !platform => Some('B'),
        "right" if !platform => Some('C'),
        "left" if !platform => Some('D'),
        "home" => Some('H'),
        "end" => Some('F'),
        _ => None,
    };
    if let Some(letter) = letter {
        return Action::Send(if parameter != 1 {
            format!("\x1b[1;{parameter}{letter}")
        } else if application_cursor {
            format!("\x1bO{letter}")
        } else {
            format!("\x1b[{letter}")
        });
    }
    if let Some(letter) = match key {
        "f1" => Some('P'),
        "f2" => Some('Q'),
        "f3" => Some('R'),
        "f4" => Some('S'),
        _ => None,
    } {
        return Action::Send(if parameter == 1 {
            format!("\x1bO{letter}")
        } else {
            format!("\x1b[1;{parameter}{letter}")
        });
    }
    let tilde = match key {
        "delete" => Some(3),
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
        return Action::Send(if parameter == 1 {
            format!("\x1b[{number}~")
        } else {
            format!("\x1b[{number};{parameter}~")
        });
    }
    match key {
        "insert" => {
            return if shift || control {
                Action::Ignore
            } else {
                Action::Send("\x1b[2~".into())
            };
        }
        "pageup" | "pagedown" => {
            let number = if key == "pageup" { 5 } else { 6 };
            return Action::Send(if control {
                format!("\x1b[{number};{parameter}~")
            } else {
                format!("\x1b[{number}~")
            });
        }
        "tab" => return Action::Send(if shift { "\x1b[Z" } else { "\t" }.into()),
        "enter" => return Action::Send(if alt { "\x1b\r" } else { "\r" }.into()),
        "escape" => return Action::Send(if alt { "\x1b\x1b" } else { "\x1b" }.into()),
        "backspace" => {
            let byte = if control { '\x08' } else { '\x7f' };
            return Action::Send(if alt {
                format!("\x1b{byte}")
            } else {
                byte.to_string()
            });
        }
        _ => {}
    }
    if control && !shift && !alt && !platform {
        let byte = match key {
            "space" | " " => Some(0),
            "[" | "3" => Some(27),
            "\\" | "4" => Some(28),
            "]" | "5" => Some(29),
            "6" => Some(30),
            "7" | "/" => Some(31),
            "8" => Some(127),
            _ if key.len() == 1 && key.as_bytes()[0].is_ascii_alphabetic() => {
                Some(key.as_bytes()[0].to_ascii_uppercase() - b'A' + 1)
            }
            _ => None,
        };
        return byte
            .map(|byte| Action::Send(char::from(byte).to_string()))
            .unwrap_or(Action::Ignore);
    }
    if alt && !platform {
        if matches!(key, "space" | " ") {
            return Action::Send(if control { "\x1b\0" } else { "\x1b " }.into());
        }
        if key.len() == 1 && key.as_bytes()[0].is_ascii_alphabetic() {
            let value = if control {
                char::from(key.as_bytes()[0].to_ascii_uppercase() - b'A' + 1)
            } else if shift {
                key.as_bytes()[0].to_ascii_uppercase() as char
            } else {
                key.as_bytes()[0].to_ascii_lowercase() as char
            };
            return Action::Send(format!("\x1b{value}"));
        }
        if key.len() == 1
            && (key.as_bytes()[0].is_ascii_digit()
                || ";:=+,<-_.>/?`~[{\\|]}\"'!@#$%^&*()".contains(key))
        {
            return Action::Send(format!("\x1b{key}"));
        }
        return Action::Ignore;
    }
    if control && shift {
        let byte = match key {
            "-" | "_" => Some(31),
            "2" | "@" => Some(0),
            "6" | "^" => Some(30),
            _ => None,
        };
        return byte
            .map(|byte| Action::Send(char::from(byte).to_string()))
            .unwrap_or(Action::Ignore);
    }
    if control || platform {
        Action::Ignore
    } else {
        Action::CommitText
    }
}

pub fn enhanced_enter(modifiers: Modifiers, kitty: bool, win32: bool) -> Option<String> {
    let modified = modifiers.shift || modifiers.alt || modifiers.control || modifiers.platform;
    if !modified {
        return None;
    }
    if win32 && (modifiers.shift || modifiers.alt || modifiers.control) {
        let state = 16 * u8::from(modifiers.shift)
            + 8 * u8::from(modifiers.control)
            + 2 * u8::from(modifiers.alt);
        return Some(format!("\x1b[13;28;13;1;{state};1_"));
    }
    kitty.then(|| format!("\x1b[13;{}u", modifiers.parameter()))
}

pub fn encode(key: &str, modifiers: Modifiers, application_cursor: bool) -> Option<String> {
    match legacy_action(key, modifiers, application_cursor, false) {
        Action::Send(data) => Some(data),
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
    fn legacy_keys_match_the_installed_xterm_and_authored_mac_shortcuts() {
        let oracle: serde_json::Value =
            serde_json::from_str(include_str!("testdata/terminal-keyboard.json")).unwrap();
        for case in oracle["cases"].as_array().unwrap() {
            let mask = case[1].as_u64().unwrap();
            let action = legacy_action(
                case[0].as_str().unwrap(),
                Modifiers {
                    shift: mask & 1 != 0,
                    alt: mask & 2 != 0,
                    control: mask & 4 != 0,
                    platform: mask & 8 != 0,
                },
                case[2].as_bool().unwrap(),
                case[3].as_bool().unwrap(),
            );
            let (kind, data) = match action {
                Action::Send(data) => (0, Some(data)),
                Action::SelectAll => (1, None),
                Action::Page(1) => (2, None),
                Action::Page(-1) => (3, None),
                Action::CommitText
                    if mask & 14 == 0 && case[6].as_str().unwrap().chars().count() == 1 =>
                {
                    (0, Some(case[6].as_str().unwrap().to_owned()))
                }
                Action::CommitText | Action::Ignore => (0, None),
                Action::Page(_) => panic!("page direction must be +/-1"),
            };
            assert_eq!(
                (kind, serde_json::json!(data)),
                (case[4].as_i64().unwrap(), case[5].clone()),
                "{case}"
            );
        }
    }

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
    fn enhanced_enter_matches_shared_kitty_and_win32_encoding_without_claiming_all_key_events() {
        for (modifiers, kitty, win32, expected) in [
            (
                Modifiers {
                    shift: true,
                    ..Default::default()
                },
                true,
                false,
                "\x1b[13;2u",
            ),
            (
                Modifiers {
                    alt: true,
                    ..Default::default()
                },
                true,
                false,
                "\x1b[13;3u",
            ),
            (
                Modifiers {
                    control: true,
                    ..Default::default()
                },
                true,
                false,
                "\x1b[13;5u",
            ),
            (
                Modifiers {
                    platform: true,
                    ..Default::default()
                },
                true,
                false,
                "\x1b[13;9u",
            ),
            (
                Modifiers {
                    shift: true,
                    control: true,
                    ..Default::default()
                },
                true,
                false,
                "\x1b[13;6u",
            ),
            (
                Modifiers {
                    shift: true,
                    ..Default::default()
                },
                true,
                true,
                "\x1b[13;28;13;1;16;1_",
            ),
            (
                Modifiers {
                    alt: true,
                    control: true,
                    ..Default::default()
                },
                false,
                true,
                "\x1b[13;28;13;1;10;1_",
            ),
        ] {
            assert_eq!(
                enhanced_enter(modifiers, kitty, win32).as_deref(),
                Some(expected)
            );
        }
        assert_eq!(enhanced_enter(Modifiers::default(), true, true), None);
        assert_eq!(
            enhanced_enter(
                Modifiers {
                    shift: true,
                    ..Default::default()
                },
                false,
                false
            ),
            None
        );
        assert_eq!(
            enhanced_enter(
                Modifiers {
                    platform: true,
                    ..Default::default()
                },
                false,
                true
            ),
            None
        );
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
        assert_eq!(encode("8", control, false).as_deref(), Some("\x7f"));
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
