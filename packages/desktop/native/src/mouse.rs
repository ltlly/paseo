use serde::{Deserialize, Serialize};
use std::time::{Duration, Instant};

use crate::terminal::GridSize;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Deserialize, Serialize)]
pub struct Modes {
    pub protocol: u16,
    pub encoding: u16,
}

impl Modes {
    pub fn active(self) -> bool {
        self.protocol != 0
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Button {
    Left,
    Middle,
    Right,
    None,
    WheelUp,
    WheelDown,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Action {
    Press,
    Release,
    Move,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize, Serialize)]
pub struct Event {
    pub col: u32,
    pub row: u32,
    pub x: u32,
    pub y: u32,
    pub button: Button,
    pub action: Action,
    pub shift: bool,
    pub alt: bool,
    pub control: bool,
}

#[derive(Default)]
pub struct Reporter {
    last: Option<(Modes, Event)>,
}

impl Reporter {
    pub fn reset(&mut self) {
        self.last = None;
    }
    pub fn report(&mut self, modes: Modes, mut event: Event, size: GridSize) -> Option<Vec<u8>> {
        if event.col >= u32::from(size.cols) || event.row >= u32::from(size.rows) {
            return None;
        }
        let wheel = matches!(event.button, Button::WheelUp | Button::WheelDown);
        if wheel && event.action != Action::Press
            || event.button == Button::None && event.action != Action::Move
        {
            return None;
        }
        if event.action == Action::Move
            && self.last.is_some_and(|(_, last)| {
                last.button == event.button
                    && last.action == event.action
                    && last.shift == event.shift
                    && last.alt == event.alt
                    && last.control == event.control
                    && if modes.encoding == 1016 {
                        last.x == event.x && last.y == event.y
                    } else {
                        last.col == event.col && last.row == event.row
                    }
            })
        {
            return None;
        }
        match modes.protocol {
            9 => {
                if wheel || event.action != Action::Press {
                    return None;
                }
                event.shift = false;
                event.alt = false;
                event.control = false;
            }
            1000 if event.action == Action::Move => return None,
            1002 if event.action == Action::Move && event.button == Button::None => return None,
            1000 | 1002 | 1003 => {}
            _ => return None,
        }
        let modifiers =
            u8::from(event.shift) * 4 | u8::from(event.alt) * 8 | u8::from(event.control) * 16;
        let mut code = modifiers
            | match event.button {
                Button::Left => 0,
                Button::Middle => 1,
                Button::Right => 2,
                Button::None => 3,
                Button::WheelUp => 64,
                Button::WheelDown => 65,
            };
        if event.action == Action::Move {
            code |= 32;
        }
        let result = match modes.encoding {
            1006 | 1016 => {
                let (x, y) = if modes.encoding == 1016 {
                    (event.x, event.y)
                } else {
                    (event.col + 1, event.row + 1)
                };
                let end = if event.action == Action::Release {
                    'm'
                } else {
                    'M'
                };
                format!("\x1b[<{code};{x};{y}{end}").into_bytes()
            }
            0 => {
                if event.action == Action::Release {
                    code |= 3;
                }
                if event.col > 222 || event.row > 222 {
                    Vec::new()
                } else {
                    vec![
                        27,
                        b'[',
                        b'M',
                        code + 32,
                        (event.col + 33) as u8,
                        (event.row + 33) as u8,
                    ]
                }
            }
            _ => return None,
        };
        self.last = Some((modes, event));
        Some(result)
    }
}

#[derive(Default)]
pub struct Wheel {
    remainder: f32,
    last: Option<(Instant, (u16, bool), f32)>,
}

impl Wheel {
    pub fn rows(&mut self, delta: f32, sensitivity: f32, now: Instant, route: (u16, bool)) -> i32 {
        if delta == 0. {
            return 0;
        }
        if self.last.is_none_or(|(time, previous, direction)| {
            previous != route
                || direction != delta.signum()
                || now.duration_since(time) > Duration::from_millis(500)
        }) {
            self.remainder = 0.;
        }
        self.last = Some((now, route, delta.signum()));
        self.remainder += delta * sensitivity;
        let whole = self.remainder.trunc();
        self.remainder -= whole;
        whole.clamp(-1000., 1000.) as i32
    }
}

#[derive(Default, Clone, Copy)]
enum State {
    #[default]
    Ground,
    Escape,
    EscapeIntermediate,
    Csi,
    Osc,
    String,
}

/// Alacritty omits X10/pixel and Win32 input state and implements deprecated UTF-8
/// mouse differently from xterm. Observe those modes at this client boundary.
/// Discard string payloads instead of duplicating the VT parser's OSC buffers.
#[derive(Default)]
pub struct ModeTracker {
    state: State,
    modes: Modes,
    win32: bool,
    kitty: u16,
    kitty_stack: Vec<u16>,
    prefix: u8,
    param_present: [bool; 2],
    utf8: [u8; 3],
    utf8_count: usize,
    protocol_revision: u64,
    reset_revision: u64,
    params: [u16; 32],
    count: usize,
    private: bool,
    started: bool,
    invalid: bool,
    subparam: bool,
    excess_params: bool,
}

impl ModeTracker {
    pub fn modes(&self) -> Modes {
        self.modes
    }
    pub fn kitty_input(&self) -> bool {
        self.kitty != 0
    }
    pub fn win32_input(&self) -> bool {
        self.win32
    }
    pub fn protocol_revision(&self) -> u64 {
        self.protocol_revision
    }
    pub fn reset_revision(&self) -> u64 {
        self.reset_revision
    }

    pub fn feed(&mut self, data: &[u8]) {
        let mut cursor = 0;
        while cursor < data.len() {
            let tail = &data[cursor..];
            if matches!(self.state, State::Ground) && self.utf8_count == 0 {
                let Some(skip) = memchr::memchr3(0x1b, 0xc2, 0xef, tail) else {
                    break;
                };
                cursor += skip;
            } else if matches!(self.state, State::Osc | State::String) && self.utf8_count == 0 {
                let control = memchr::memchr3(0x1b, 0x18, 0x1a, tail);
                let unicode = memchr::memchr2(0xc2, 0xef, tail);
                let control = match (control, unicode) {
                    (Some(a), Some(b)) => Some(a.min(b)),
                    (a, b) => a.or(b),
                };
                let stop = if matches!(self.state, State::Osc) {
                    match (control, memchr::memchr(7, tail)) {
                        (Some(a), Some(b)) => Some(a.min(b)),
                        (a, b) => a.or(b),
                    }
                } else {
                    control
                };
                let Some(skip) = stop else {
                    break;
                };
                cursor += skip;
            }
            let byte = data[cursor];
            cursor += 1;
            if self.utf8_count > 0 {
                if byte & 0xc0 == 0x80 {
                    self.utf8[self.utf8_count] = byte;
                    self.utf8_count += 1;
                    let needed = if self.utf8[0] == 0xc2 { 2 } else { 3 };
                    if self.utf8_count < needed {
                        continue;
                    }
                    self.utf8_count = 0;
                    if self.utf8 == [0xef, 0xbb, 0xbf] {
                        continue;
                    }
                    if self.utf8[0] == 0xc2 && (0x80..=0x9f).contains(&byte) {
                        match byte {
                            0x9b => {
                                self.start_csi();
                                continue;
                            }
                            0x9d => {
                                self.state = State::Osc;
                                continue;
                            }
                            0x90 | 0x98 | 0x9e | 0x9f => {
                                self.state = State::String;
                                continue;
                            }
                            _ => {
                                self.state = State::Ground;
                                continue;
                            }
                        }
                    }
                    if !matches!(self.state, State::Osc | State::String) {
                        self.state = State::Ground;
                    }
                    continue;
                }
                self.utf8_count = 0;
                if !matches!(self.state, State::Osc | State::String) {
                    self.state = State::Ground;
                }
            }
            if byte == 0xc2 || byte == 0xef {
                self.utf8 = [byte, 0, 0];
                self.utf8_count = 1;
                continue;
            }
            if byte == 0x1b {
                self.state = State::Escape;
                continue;
            }
            if byte == 0x18 || byte == 0x1a {
                self.state = State::Ground;
                continue;
            }
            match self.state {
                State::Ground => {}
                State::Osc => {
                    if byte == 7 {
                        self.state = State::Ground;
                    }
                }
                State::String => {}
                State::Escape => match byte {
                    b'[' => self.start_csi(),
                    b']' => self.state = State::Osc,
                    b'P' | b'X' | b'^' | b'_' => self.state = State::String,
                    b'c' => {
                        self.modes = Modes::default();
                        self.win32 = false;
                        self.kitty = 0;
                        self.kitty_stack.clear();
                        self.protocol_revision = self.protocol_revision.wrapping_add(1);
                        self.reset_revision = self.reset_revision.wrapping_add(1);
                        self.state = State::Ground;
                    }
                    0x20..=0x2f => self.state = State::EscapeIntermediate,
                    0x30..=0x7e | 0x80..=0xff => self.state = State::Ground,
                    _ => {}
                },
                State::EscapeIntermediate => {
                    if (0x30..=0x7e).contains(&byte) {
                        self.state = State::Ground;
                    }
                }
                State::Csi => match byte {
                    b'?' | b'>' | b'=' | b'<' if !self.started => {
                        self.prefix = byte;
                        self.private = byte == b'?';
                        self.started = true;
                    }
                    b'0'..=b'9' if !self.subparam && !self.excess_params => {
                        self.started = true;
                        if self.count < 2 {
                            self.param_present[self.count] = true;
                        }
                        self.params[self.count] = self.params[self.count]
                            .saturating_mul(10)
                            .saturating_add(u16::from(byte - b'0'));
                    }
                    b';' => {
                        self.started = true;
                        if self.count + 1 == self.params.len() {
                            self.excess_params = true;
                        } else {
                            self.count += 1;
                        }
                        self.subparam = false;
                    }
                    b':' => {
                        self.subparam = true;
                        self.started = true;
                    }
                    b'0'..=b'9' => {}
                    0x40..=0x7e => {
                        if self.private && !self.invalid && matches!(byte, b'h' | b'l') {
                            for index in 0..=self.count {
                                self.set_mode(self.params[index], byte == b'h');
                            }
                        } else if !self.invalid && byte == b'u' {
                            self.set_kitty();
                        }
                        self.state = State::Ground;
                    }
                    0x80..=0xff => {
                        self.state = State::Ground;
                    }
                    0x20..=0x3f => self.invalid = true,
                    _ => {}
                },
            }
        }
    }

    fn start_csi(&mut self) {
        self.state = State::Csi;
        self.params.fill(0);
        self.count = 0;
        self.prefix = 0;
        self.param_present = [false; 2];
        self.private = false;
        self.started = false;
        self.invalid = false;
        self.subparam = false;
        self.excess_params = false;
    }

    fn set_kitty(&mut self) {
        // Match Paseo's shared input-mode tracker, not Alacritty's independent
        // screen-local keyboard stack. Only modified Enter consumes these flags.
        match self.prefix {
            b'>' => {
                self.kitty_stack.push(self.kitty);
                self.kitty = if self.param_present[0] {
                    self.params[0]
                } else {
                    1
                };
            }
            b'=' => {
                self.kitty = if self.param_present[1] && self.params[1] == 0 {
                    0
                } else {
                    self.params[0]
                }
            }
            b'<' => {
                let count = usize::from(self.params[0].max(1)).min(self.kitty_stack.len() + 1);
                for _ in 0..count {
                    self.kitty = self.kitty_stack.pop().unwrap_or(0);
                }
            }
            _ => {}
        }
    }

    fn set_mode(&mut self, mode: u16, enabled: bool) {
        match mode {
            9 | 1000 | 1002 | 1003 => {
                self.modes.protocol = if enabled { mode } else { 0 };
                self.protocol_revision = self.protocol_revision.wrapping_add(1);
            }
            9001 => self.win32 = enabled,
            1006 | 1016 => self.modes.encoding = if enabled { mode } else { 0 },
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn enhanced_input_modes_follow_shared_stack_and_utf8_control_boundaries() {
        for data in [
            "\x1b[=1u\x1b[>7u\x1b[<u",
            "\x1b[>7u\x1b[=7;3u",
            "\x1b[>7u\x1b[?1049h",
            "\x1b[>u",
            "\u{009b}>7u",
        ] {
            for split in 0..=data.len() {
                let mut tracker = ModeTracker::default();
                tracker.feed(&data.as_bytes()[..split]);
                tracker.feed(&data.as_bytes()[split..]);
                assert!(tracker.kitty_input(), "{data:?} split{split}");
            }
        }
        for data in [
            "\u{009b}?9001h",
            "\x1b[?90\u{feff}01h",
            "\u{009d}ignored [?9001l\u{009c}\u{009b}?9001h",
        ] {
            for split in 0..=data.len() {
                let mut tracker = ModeTracker::default();
                tracker.feed(&data.as_bytes()[..split]);
                tracker.feed(&data.as_bytes()[split..]);
                assert!(tracker.win32_input(), "{data:?} split{split}");
            }
        }
        for data in [
            b"\x1b[?9001\xc2h".as_slice(),
            b"\x1b[>1\xefu".as_slice(),
            b"\x1b[?9001\xef\xbbh".as_slice(),
            b"\x1b[?9001\xd0\x9bh".as_slice(),
            b"\x1b[>1\xe2\x82\x9bu".as_slice(),
        ] {
            for split in 0..=data.len() {
                let mut tracker = ModeTracker::default();
                tracker.feed(&data[..split]);
                tracker.feed(&data[split..]);
                assert!(!tracker.win32_input());
                assert!(!tracker.kitty_input());
            }
        }
        let mut tracker = ModeTracker::default();
        tracker.feed(b"\x1b[>7u\x1b[=7;0u");
        assert!(!tracker.kitty_input());
        tracker.feed(b"\x1b[>7u\x1bc");
        assert!(!tracker.kitty_input());
    }

    #[test]
    fn win32_input_mode_survives_chunk_boundaries_and_resets_without_parsing_string_payloads() {
        for split in 0..=9 {
            let data = b"\x1b[?9001h";
            let split = split.min(data.len());
            let mut tracker = ModeTracker::default();
            tracker.feed(&data[..split]);
            tracker.feed(&data[split..]);
            assert!(tracker.win32_input());
            tracker.feed(b"\x1b]title ?9001l\x07\x1bPpayload ?9001l\x1b\\");
            assert!(tracker.win32_input());
            tracker.feed(b"\x1b[?9001l");
            assert!(!tracker.win32_input());
            tracker.feed(b"\x1b[?9001h\x1bc");
            assert!(!tracker.win32_input());
        }
    }

    #[test]
    fn wheel_preserves_fractional_distance_and_resets_on_direction_route_and_idle() {
        let mut wheel = Wheel::default();
        let now = Instant::now();
        let normal = (0, false);
        assert_eq!(wheel.rows(0.4, 1., now, normal), 0);
        assert_eq!(wheel.rows(0.7, 1., now, normal), 1);
        assert_eq!(wheel.rows(-0.2, 1., now, normal), 0);
        assert_eq!(wheel.rows(-0.9, 1., now, normal), -1);
        assert_eq!(wheel.rows(0.9, 1., now, normal), 0);
        assert_eq!(wheel.rows(0.2, 1., now, (1000, false)), 0);
        assert_eq!(
            wheel.rows(0.9, 1., now + Duration::from_millis(501), (1000, false)),
            0
        );
        assert_eq!(
            wheel.rows(10_000., 1., now + Duration::from_millis(501), (1000, false)),
            1000
        );
        assert_eq!(
            wheel.rows(0.05, 1., now + Duration::from_millis(501), (1000, false)),
            0
        );
        assert_eq!(
            wheel.rows(-0.21, 5., now + Duration::from_millis(501), (1000, false)),
            -1
        );
    }
    #[test]
    fn reports_match_real_xterm_protocol_filtering_encodings_and_motion_deduplication() {
        let oracle: serde_json::Value =
            serde_json::from_str(include_str!("testdata/terminal-mouse.json")).unwrap();
        for trace in oracle["traces"].as_array().unwrap() {
            let modes: Modes = serde_json::from_value(trace["modes"].clone()).unwrap();
            let size = GridSize {
                cols: trace["size"]["cols"].as_u64().unwrap() as u16,
                rows: trace["size"]["rows"].as_u64().unwrap() as u16,
            };
            let mut reporter = Reporter::default();
            for item in trace["events"].as_array().unwrap() {
                let event: Event = serde_json::from_value(item["event"].clone()).unwrap();
                let bytes: Vec<u8> = serde_json::from_value(item["bytes"].clone()).unwrap();
                let expected = item["handled"].as_bool().unwrap().then_some(bytes);
                assert_eq!(
                    reporter.report(modes, event, size),
                    expected,
                    "{modes:?} {event:?}"
                );
            }
        }
    }

    #[test]
    fn private_mode_observation_matches_xterm_even_with_fragmented_and_extreme_params() {
        let oracle: serde_json::Value =
            serde_json::from_str(include_str!("testdata/terminal-mouse.json")).unwrap();
        for trace in oracle["modes"].as_array().unwrap() {
            let expected: Modes = serde_json::from_value(trace["expected"].clone()).unwrap();
            let mut tracker = ModeTracker::default();
            for chunk in trace["chunks"].as_array().unwrap() {
                for byte in chunk.as_str().unwrap().as_bytes() {
                    tracker.feed(std::slice::from_ref(byte));
                }
            }
            assert_eq!(tracker.modes(), expected, "{}", trace["chunks"]);
        }
    }
    #[test]
    fn mode_sequences_survive_every_chunk_split_and_do_not_collect_string_payloads() {
        let bytes = b"\x1b[?9;1016h";
        for split in 0..=bytes.len() {
            let mut tracker = ModeTracker::default();
            tracker.feed(&bytes[..split]);
            tracker.feed(&bytes[split..]);
            assert_eq!(
                tracker.modes(),
                Modes {
                    protocol: 9,
                    encoding: 1016
                }
            );
            tracker.feed(b"\x1b]title ?1003h\x07\x1bPpayload ?1000h\x1b\\");
            assert_eq!(
                tracker.modes(),
                Modes {
                    protocol: 9,
                    encoding: 1016
                }
            );
            tracker.feed(b"\x1b[?1003l\x1b[?1006l");
            assert_eq!(tracker.modes(), Modes::default());
            tracker.feed(b"\x1b[?1002;1006h\x1b[?1005h");
            assert_eq!(
                tracker.modes(),
                Modes {
                    protocol: 1002,
                    encoding: 1006
                }
            );
            tracker.feed(b"\x1bc");
            assert_eq!(tracker.modes(), Modes::default());
        }
    }
    #[test]
    fn sgr_release_retains_the_button_and_press_modifiers() {
        let mut reporter = Reporter::default();
        let modes = Modes {
            protocol: 1000,
            encoding: 1006,
        };
        let mut event = Event {
            col: 4,
            row: 2,
            x: 36,
            y: 45,
            button: Button::Right,
            action: Action::Press,
            shift: false,
            alt: true,
            control: true,
        };
        assert_eq!(
            reporter.report(modes, event, GridSize { rows: 24, cols: 80 }),
            Some(b"\x1b[<26;5;3M".to_vec())
        );
        event.action = Action::Release;
        assert_eq!(
            reporter.report(modes, event, GridSize { rows: 24, cols: 80 }),
            Some(b"\x1b[<26;5;3m".to_vec())
        );
    }
}
