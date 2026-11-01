use alacritty_terminal::{
    event::{Event, EventListener},
    grid::{Dimensions, Scroll},
    index::{Column, Line, Point, Side},
    selection::{Selection, SelectionType},
    term::{Config, RenderableContent, Term, TermMode, cell::Flags},
    vte::ansi::{Color, CursorShape, CursorStyle, NamedColor, Processor, Rgb},
};
use anyhow::{Result, ensure};

use crate::wire::{ReplayMetadata, TerminalFrame, TerminalOpcode};

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct GridSize {
    pub rows: u16,
    pub cols: u16,
}

impl Dimensions for GridSize {
    fn total_lines(&self) -> usize {
        self.rows.into()
    }
    fn screen_lines(&self) -> usize {
        self.rows.into()
    }
    fn columns(&self) -> usize {
        self.cols.into()
    }
}

pub struct Cell<'a> {
    pub row: usize,
    pub col: usize,
    pub columns: usize,
    pub character: char,
    pub zerowidth: &'a [char],
    pub foreground: u32,
    pub background: u32,
    pub bold: bool,
    pub italic: bool,
    pub underline: bool,
    pub selected: bool,
    pub opacity: u8,
    pub hidden: bool,
    pub strikethrough: bool,
}

pub struct Screen<'a> {
    pub cells: ScreenCells<'a>,
    pub cursor: Option<(usize, usize)>,
    pub cursor_style: CursorStyle,
    pub cursor_columns: usize,
    pub cursor_color: u32,
}

impl Cell<'_> {
    pub fn chars(&self) -> impl Iterator<Item = char> + '_ {
        std::iter::once(self.character).chain(self.zerowidth.iter().copied())
    }
}

pub struct ScreenCells<'a> {
    content: RenderableContent<'a>,
    rows: u16,
}

impl<'a> Iterator for ScreenCells<'a> {
    type Item = Cell<'a>;

    fn next(&mut self) -> Option<Self::Item> {
        for indexed in self.content.display_iter.by_ref() {
            let row = indexed.point.line.0 + self.content.display_offset as i32;
            if row < 0
                || row >= i32::from(self.rows)
                || indexed.cell.flags.contains(Flags::WIDE_CHAR_SPACER)
            {
                continue;
            }
            let cell = indexed.cell;
            let (foreground, background) = if cell.flags.contains(Flags::INVERSE) {
                (cell.bg, cell.fg)
            } else {
                (cell.fg, cell.bg)
            };
            let selected = self.content.selection.is_some_and(|selection| {
                selection.contains(indexed.point)
                    || (cell.flags.contains(Flags::WIDE_CHAR)
                        && selection
                            .contains(Point::new(indexed.point.line, indexed.point.column + 1)))
            });
            let mut foreground = resolve_color(
                foreground,
                self.content.colors,
                cell.flags.contains(Flags::BOLD),
            );
            let mut background = resolve_color(background, self.content.colors, false);
            if selected {
                let selection = blend_rgb(
                    crate::theme::terminal::BACKGROUND,
                    crate::theme::terminal_selection::BACKGROUND >> 8,
                    crate::theme::terminal_selection::BACKGROUND as u8,
                );
                background = if cell.flags.contains(Flags::INVERSE)
                    || cell.bg != Color::Named(NamedColor::Background)
                {
                    blend_rgb(background, selection, 128)
                } else {
                    selection
                };
                foreground = crate::theme::terminal_selection::FOREGROUND;
                if matches!(cell.c as u32, 0x2500..=0x259f | 0xe0a4..=0xe0d6) {
                    foreground = if cell.flags.contains(Flags::INVERSE)
                        && cell.bg == Color::Named(NamedColor::Background)
                    {
                        selection
                    } else {
                        let color = if cell.flags.contains(Flags::INVERSE) {
                            cell.bg
                        } else {
                            cell.fg
                        };
                        blend_rgb(
                            resolve_color(color, self.content.colors, false),
                            selection,
                            128,
                        )
                    };
                }
            }
            return Some(Cell {
                row: row as usize,
                col: indexed.point.column.0,
                columns: if cell.flags.contains(Flags::WIDE_CHAR) {
                    2
                } else {
                    1
                },
                character: cell.c,
                zerowidth: cell.zerowidth().unwrap_or_default(),
                foreground,
                background,
                bold: cell.flags.contains(Flags::BOLD),
                italic: cell.flags.contains(Flags::ITALIC),
                underline: cell.flags.intersects(Flags::ALL_UNDERLINES),
                selected,
                opacity: if cell.flags.contains(Flags::DIM) && !selected {
                    128
                } else {
                    255
                },
                hidden: cell.flags.contains(Flags::HIDDEN),
                strikethrough: cell.flags.contains(Flags::STRIKEOUT),
            });
        }
        None
    }
}

use std::sync::{
    Arc,
    atomic::{AtomicU64, Ordering},
};

#[derive(Clone)]
struct CursorEvents(Arc<AtomicU64>);

impl EventListener for CursorEvents {
    fn send_event(&self, event: Event) {
        // Only observe display lifecycle; query replies remain daemon-owned.
        if matches!(event, Event::CursorBlinkingChange) {
            self.0.fetch_add(1, Ordering::Relaxed);
        }
    }
}

pub struct TerminalBuffer {
    term: Term<CursorEvents>,
    cursor_events: Arc<AtomicU64>,
    parser: Processor,
    history: usize,
    pub size: GridSize,
    pub replaying: bool,
    mouse: crate::mouse::ModeTracker,
}

impl TerminalBuffer {
    pub fn new(size: GridSize, history: usize) -> Self {
        let config = Config {
            scrolling_history: history,
            kitty_keyboard: true,
            default_cursor_style: CursorStyle {
                shape: CursorShape::Beam,
                blinking: true,
            },
            ..Default::default()
        };
        let cursor_events = Arc::new(AtomicU64::new(0));
        Self {
            term: Term::new(config, &size, CursorEvents(cursor_events.clone())),
            cursor_events,
            parser: Processor::new(),
            history,
            size,
            replaying: false,
            mouse: crate::mouse::ModeTracker::default(),
        }
    }

    pub fn apply(&mut self, frame: TerminalFrame) -> Result<()> {
        match frame.opcode {
            TerminalOpcode::ReplayBegin => {
                ensure!(!self.replaying, "terminal replay already active");
                let metadata = ReplayMetadata::decode(&frame.data)?;
                self.reset(GridSize {
                    rows: metadata.rows,
                    cols: metadata.cols,
                });
                self.replaying = true;
            }
            TerminalOpcode::ReplayData => {
                ensure!(self.replaying, "terminal replay data without begin");
                self.feed(&frame.data);
            }
            TerminalOpcode::ReplayEnd => {
                ensure!(self.replaying, "terminal replay end without begin");
                self.replaying = false;
            }
            TerminalOpcode::Restore => {
                // COMPAT(terminalSingleRestore): added after v0.14.0, remove after 2027-04-30.
                self.reset(self.size);
                self.feed(&frame.data);
            }
            TerminalOpcode::Output => self.feed(&frame.data),
            _ => {}
        }
        Ok(())
    }

    pub fn resize(&mut self, size: GridSize) {
        self.term.resize(size);
        self.size = size;
    }

    pub fn feed(&mut self, bytes: &[u8]) {
        self.mouse.feed(bytes);
        self.parser.advance(&mut self.term, bytes);
    }

    pub fn cursor_state(&self) -> (Point, bool, CursorStyle, u64) {
        (
            self.term.grid().cursor.point,
            self.term.mode().contains(TermMode::SHOW_CURSOR),
            self.term.cursor_style(),
            self.cursor_events.load(Ordering::Relaxed),
        )
    }

    pub fn mouse_modes(&self) -> crate::mouse::Modes {
        self.mouse.modes()
    }
    pub fn mouse_protocol_revision(&self) -> u64 {
        self.mouse.protocol_revision()
    }
    pub fn mouse_reset_revision(&self) -> u64 {
        self.mouse.reset_revision()
    }

    fn reset(&mut self, size: GridSize) {
        *self = Self::new(size, self.history);
    }

    pub fn scroll(&mut self, lines: i32) {
        self.term.scroll_display(Scroll::Delta(lines));
    }

    fn selection_point(&self, row: usize, column: usize) -> (Point, Side) {
        let cols = usize::from(self.size.cols);
        let column = column.min(cols);
        (
            Point::new(
                Line(
                    row.min(usize::from(self.size.rows) - 1) as i32
                        - self.term.grid().display_offset() as i32,
                ),
                Column(column.min(cols - 1)),
            ),
            if column == cols {
                Side::Right
            } else {
                Side::Left
            },
        )
    }

    pub fn begin_selection(&mut self, row: usize, column: usize, block: bool) {
        let (point, side) = self.selection_point(row, column);
        self.term.selection = Some(Selection::new(
            if block {
                SelectionType::Block
            } else {
                SelectionType::Simple
            },
            point,
            side,
        ));
    }

    pub fn extend_selection(&mut self, row: usize, column: usize) {
        let (point, side) = self.selection_point(row, column);
        if let Some(selection) = &mut self.term.selection {
            selection.update(point, side);
        }
    }

    pub fn has_selection(&self) -> bool {
        self.term
            .selection
            .as_ref()
            .is_some_and(|selection| !selection.is_empty())
    }

    pub fn selected_text(&self) -> Option<String> {
        self.term.selection_to_string()
    }

    pub fn clear_selection(&mut self) {
        self.term.selection = None;
    }

    pub fn prepare_input(&mut self) -> bool {
        let redraw = self.has_selection() || self.term.grid().display_offset() != 0;
        self.clear_selection();
        self.term.scroll_display(Scroll::Bottom);
        redraw
    }

    pub fn modes(&self) -> TermMode {
        *self.term.mode()
    }

    // Borrow the authoritative grid for the paint pass. No second grid or per-cell
    // strings outlive rendering; combining marks remain borrowed without truncation.
    pub fn screen(&self) -> Screen<'_> {
        let content = self.term.renderable_content();
        let offset = content.display_offset as i32;
        let cursor_row = content.cursor.point.line.0 + offset;
        let cursor_col = content
            .cursor
            .point
            .column
            .0
            .min(usize::from(self.size.cols) - 1);
        let cursor = (content.cursor.shape != CursorShape::Hidden
            && cursor_row >= 0
            && cursor_row < i32::from(self.size.rows))
        .then_some((cursor_row as usize, cursor_col));
        let cursor_columns = if self.term.grid()[content.cursor.point]
            .flags
            .contains(Flags::WIDE_CHAR)
        {
            2
        } else {
            1
        };
        let cursor_style = self.term.cursor_style();
        let cursor_color = resolve_color(Color::Named(NamedColor::Cursor), content.colors, false);
        Screen {
            cells: ScreenCells {
                content,
                rows: self.size.rows,
            },
            cursor,
            cursor_style,
            cursor_columns,
            cursor_color,
        }
    }
}

fn blend_rgb(background: u32, foreground: u32, alpha: u8) -> u32 {
    let mut result = 0;
    for shift in [16, 8, 0] {
        let background = ((background >> shift) & 255) as i32;
        let foreground = ((foreground >> shift) & 255) as i32;
        // Match JS Math.round for descending channels as well as ascending ones.
        let channel =
            background + ((foreground - background) * i32::from(alpha) + 127).div_euclid(255);
        result |= (channel as u32) << shift;
    }
    result
}

fn resolve_color(
    color: Color,
    colors: &alacritty_terminal::term::color::Colors,
    bold: bool,
) -> u32 {
    let color = match color {
        Color::Indexed(index) if bold && index < 8 => Color::Indexed(index + 8),
        Color::Named(named) if bold && (named as usize) < 8 => Color::Indexed(named as u8 + 8),
        value => value,
    };
    match color {
        Color::Spec(Rgb { r, g, b }) => (u32::from(r) << 16) | (u32::from(g) << 8) | u32::from(b),
        Color::Named(named) => colors[named as usize]
            .map(|rgb| resolve_color(Color::Spec(rgb), colors, false))
            .unwrap_or_else(|| match named {
                NamedColor::Foreground
                | NamedColor::BrightForeground
                | NamedColor::DimForeground => crate::theme::terminal::FOREGROUND,
                NamedColor::Background => crate::theme::terminal::BACKGROUND,
                NamedColor::Cursor => crate::theme::terminal::CURSOR,
                _ => indexed_color(named as usize),
            }),
        Color::Indexed(index) => colors[index as usize]
            .map(|rgb| resolve_color(Color::Spec(rgb), colors, false))
            .unwrap_or_else(|| indexed_color(index as usize)),
    }
}

fn indexed_color(index: usize) -> u32 {
    if index < 16 {
        return crate::theme::terminal::ANSI[index];
    }
    if index < 232 {
        const LEVELS: [u32; 6] = [0, 95, 135, 175, 215, 255];
        let n = index - 16;
        return (LEVELS[n / 36] << 16) | (LEVELS[n / 6 % 6] << 8) | LEVELS[n % 6];
    }
    if index < 256 {
        let gray = 8 + (index as u32 - 232) * 10;
        return gray * 0x010101;
    }
    crate::theme::terminal::FOREGROUND
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dim_hidden_and_strike_preserve_the_grid_and_selection_disables_dim() {
        let mut buffer = TerminalBuffer::new(GridSize { rows: 2, cols: 8 }, 0);
        buffer.feed(b"\x1b[2;8;9;4;48;2;40;50;60mX\x1b[22;28;29mY");
        let screen = buffer.screen();
        let cells: Vec<_> = screen
            .cells
            .take(2)
            .map(|cell| {
                (
                    cell.character,
                    cell.opacity,
                    cell.hidden,
                    cell.strikethrough,
                    cell.underline,
                    cell.background,
                )
            })
            .collect();
        assert_eq!(
            cells,
            [
                ('X', 128, true, true, true, 0x28323c),
                ('Y', 255, false, false, true, 0x28323c)
            ]
        );
        buffer.begin_selection(0, 0, false);
        buffer.extend_selection(0, 1);
        let cell = buffer.screen().cells.next().unwrap();
        assert_eq!(cell.opacity, 255);
        assert!(cell.hidden);
        assert_eq!(buffer.selected_text().as_deref(), Some("X"));
    }

    #[test]
    fn selection_colors_preserve_explicit_backgrounds_inverse_and_decorative_glyphs() {
        for (data, background, foreground) in [
            ("X", 0x464948, 0xfafafa),
            ("\x1b]11;#28323c\x07X", 0x464948, 0xfafafa),
            ("\x1b[48;2;40;50;60mX", 0x373e42, 0xfafafa),
            ("\x1b[7mX", 0xa0a1a1, 0xfafafa),
            ("\x1b[1;31;7mX", 0x935c5c, 0xfafafa),
            ("█", 0x464948, 0xa0a1a1),
            ("\x1b[7m█", 0xa0a1a1, 0x464948),
        ] {
            let mut buffer = TerminalBuffer::new(GridSize { rows: 2, cols: 8 }, 0);
            buffer.feed(data.as_bytes());
            buffer.begin_selection(0, 0, false);
            buffer.extend_selection(0, 1);
            let cell = buffer.screen().cells.next().unwrap();
            assert!(cell.selected);
            assert_eq!(cell.background, background, "{data:?} background");
            assert_eq!(cell.foreground, foreground, "{data:?} foreground");
        }
    }

    #[test]
    fn palette_matches_actual_electron_terminal_pixels_including_osc_and_inverse_bold() {
        let oracle: serde_json::Value =
            serde_json::from_str(include_str!("testdata/terminal-palette.json")).unwrap();
        for case in oracle["cases"].as_array().unwrap() {
            let mut buffer = TerminalBuffer::new(GridSize { rows: 2, cols: 8 }, 0);
            buffer.feed(case["data"].as_str().unwrap().as_bytes());
            if case["selected"].as_bool() == Some(true) {
                buffer.begin_selection(0, 0, false);
                buffer.extend_selection(0, 2);
            }
            let cell = buffer.screen().cells.next().unwrap();
            if case["rasterOnly"].as_bool() == Some(true) {
                assert_eq!(cell.opacity, case["opacity"].as_u64().unwrap() as u8);
            } else if let Some(foreground) = case["foreground"].as_u64() {
                let projected = if case["hidden"].as_bool() == Some(true) {
                    cell.background
                } else {
                    blend_rgb(cell.background, cell.foreground, cell.opacity)
                };
                assert_eq!(projected, foreground as u32, "{} foreground", case["name"]);
            }
            assert_eq!(
                cell.background,
                case["background"].as_u64().unwrap() as u32,
                "{} background",
                case["name"]
            );
        }
    }

    #[test]
    fn cursor_lifecycle_retains_raw_wide_cell_movement_and_same_point_style_resets() {
        let mut buffer = TerminalBuffer::new(GridSize { rows: 3, cols: 8 }, 10);
        buffer.feed("中\x1b[H".as_bytes());
        let before = buffer.cursor_state();
        let projected = buffer.screen().cursor;
        buffer.feed(b"\x1b[1;2H");
        assert_ne!(buffer.cursor_state(), before);
        assert_eq!(buffer.screen().cursor, projected);
        let before = buffer.cursor_state();
        buffer.feed(b"\x1b[2 q\x1b[5 q");
        assert_eq!(buffer.cursor_state().0, before.0);
        assert_ne!(buffer.cursor_state().3, before.3);
    }

    #[test]
    fn cursor_uses_app_defaults_and_program_styles_without_losing_wide_cell_geometry() {
        let mut buffer = TerminalBuffer::new(GridSize { rows: 3, cols: 8 }, 10);
        assert_eq!(buffer.screen().cursor_style.shape, CursorShape::Beam);
        assert!(buffer.screen().cursor_style.blinking);
        for (code, shape, blinking) in [
            (1, CursorShape::Block, true),
            (2, CursorShape::Block, false),
            (3, CursorShape::Underline, true),
            (4, CursorShape::Underline, false),
            (5, CursorShape::Beam, true),
            (6, CursorShape::Beam, false),
            (0, CursorShape::Beam, true),
        ] {
            buffer.feed(format!("\x1b[{code} q").as_bytes());
            assert_eq!(
                buffer.screen().cursor_style,
                CursorStyle { shape, blinking }
            );
        }
        buffer.feed("中\x1b[H".as_bytes());
        assert_eq!(buffer.screen().cursor, Some((0, 0)));
        assert_eq!(buffer.screen().cursor_columns, 2);
        buffer.feed(b"\x1b[1;2H");
        assert_eq!(buffer.screen().cursor, Some((0, 0)));
        assert_eq!(buffer.screen().cursor_columns, 2);
        buffer.feed(b"\x1b[1;8HX");
        assert_eq!(buffer.screen().cursor, Some((0, 7)));
        assert_eq!(buffer.screen().cursor_columns, 1);
        buffer.feed(b"\x1b[?25l");
        assert_eq!(buffer.screen().cursor, None);
        buffer.feed(b"\x1b[?25h\x1b]12;#123456\x07");
        assert_eq!(buffer.screen().cursor_color, 0x123456);
        buffer.feed(b"\x1b]112\x07\x1bc");
        assert_eq!(buffer.screen().cursor_color, crate::theme::terminal::CURSOR);
        assert_eq!(buffer.screen().cursor_style.shape, CursorShape::Beam);
        assert!(buffer.screen().cursor_style.blinking);
    }

    #[test]
    fn default_theme_and_bold_ansi_match_the_authored_desktop_terminal() {
        let mut buffer = TerminalBuffer::new(GridSize { rows: 2, cols: 20 }, 0);
        buffer.feed(b"\x1b[31mR\x1b[1mB\x1b[0mX");
        let colors: Vec<_> = buffer
            .screen()
            .cells
            .take(3)
            .map(|cell| (cell.foreground, cell.background))
            .collect();
        assert_eq!(
            colors,
            [
                (0xe07070, 0x181b1a),
                (0xe89090, 0x181b1a),
                (0xfafafa, 0x181b1a)
            ]
        );
        assert_eq!(buffer.screen().cursor_color, 0xfafafa);
        buffer.feed(b"\x1b]12;#123456\x07");
        assert_eq!(buffer.screen().cursor_color, 0x123456);
    }

    fn frame(opcode: TerminalOpcode, data: &[u8]) -> TerminalFrame {
        TerminalFrame {
            opcode,
            slot: 1,
            data: data.into(),
        }
    }

    #[test]
    fn replay_keeps_utf8_and_escape_parser_state_across_chunks() {
        let mut buffer = TerminalBuffer::new(GridSize { rows: 24, cols: 80 }, 100);
        buffer.feed(b"old content");
        buffer
            .apply(frame(
                TerminalOpcode::ReplayBegin,
                br#"{"rows":12,"cols":40,"retainedHistoryLines":0}"#,
            ))
            .unwrap();
        buffer
            .apply(frame(TerminalOpcode::ReplayData, b"\x1b["))
            .unwrap();
        buffer
            .apply(frame(TerminalOpcode::ReplayData, b"31m\xe4"))
            .unwrap();
        buffer
            .apply(frame(TerminalOpcode::ReplayData, b"\xb8\xad"))
            .unwrap();
        buffer.apply(frame(TerminalOpcode::ReplayEnd, b"")).unwrap();
        let first = buffer.screen().cells.next().unwrap();
        assert_eq!(first.chars().collect::<String>(), "中");
        assert_eq!(first.foreground, 0xe07070);
        assert_eq!(buffer.size, GridSize { rows: 12, cols: 40 });
        assert!(!buffer.replaying);
    }

    #[test]
    fn query_replies_are_not_sent_by_the_display_parser() {
        let mut buffer = TerminalBuffer::new(GridSize { rows: 24, cols: 80 }, 100);
        buffer.feed(b"ok\x1b[6n\x1b[c\x1b[?1000h\x1b[?1006h");
        assert!(buffer.modes().contains(TermMode::MOUSE_REPORT_CLICK));
        assert!(buffer.modes().contains(TermMode::SGR_MOUSE));
        assert_eq!(
            buffer
                .screen()
                .cells
                .next()
                .unwrap()
                .chars()
                .collect::<String>(),
            "o"
        );
    }

    #[test]
    fn selection_copy_tracks_wrapping_reverse_drag_scrollback_and_input_clear() {
        let mut buffer = TerminalBuffer::new(GridSize { rows: 3, cols: 8 }, 20);
        buffer.feed("hello中e\u{301}123\r\nsecond\r\nthird\r\nfourth".as_bytes());
        buffer.scroll(20);
        buffer.begin_selection(0, 0, false);
        buffer.extend_selection(1, 3);
        assert_eq!(
            buffer.selected_text().as_deref(),
            Some("hello中e\u{301}123")
        );
        buffer.begin_selection(1, 3, false);
        buffer.extend_selection(0, 0);
        assert_eq!(
            buffer.selected_text().as_deref(),
            Some("hello中e\u{301}123")
        );
        assert!(buffer.screen().cells.any(|cell| cell.selected));
        buffer.prepare_input();
        assert!(!buffer.has_selection());
        assert_eq!(buffer.selected_text(), None);
        assert!(!buffer.screen().cells.any(|cell| cell.selected));
        assert_eq!(buffer.term.grid().display_offset(), 0);
    }

    #[test]
    fn block_selection_preserves_columns_and_does_not_copy_wide_spacers() {
        let mut buffer = TerminalBuffer::new(GridSize { rows: 3, cols: 10 }, 20);
        buffer.feed("ab中cd\r\nef中gh".as_bytes());
        buffer.begin_selection(0, 2, true);
        buffer.extend_selection(1, 5);
        assert_eq!(buffer.selected_text().as_deref(), Some("中c\n中g"));
        buffer.begin_selection(0, 0, false);
        assert!(!buffer.has_selection());
    }

    #[test]
    fn visible_cells_keep_wide_columns_colored_spaces_and_unbounded_combining_text() {
        let mut buffer = TerminalBuffer::new(GridSize { rows: 3, cols: 8 }, 20);
        let combining = format!("a{}", "\u{301}".repeat(32));
        buffer.feed(
            format!("\x1b[1;3;4;38;2;10;20;30;48;2;40;50;60m中e\u{301} \x1b[0mX\r\n{combining}")
                .as_bytes(),
        );
        let screen = buffer.screen();
        let cells = screen
            .cells
            .into_iter()
            .map(|cell| {
                (
                    cell.row,
                    cell.col,
                    cell.columns,
                    cell.chars().collect::<String>(),
                    cell.foreground,
                    cell.background,
                    cell.bold,
                    cell.italic,
                    cell.underline,
                )
            })
            .collect::<Vec<_>>();
        assert_eq!(
            &cells[..4],
            &[
                (0, 0, 2, "中".into(), 0x0a141e, 0x28323c, true, true, true),
                (
                    0,
                    2,
                    1,
                    "e\u{301}".into(),
                    0x0a141e,
                    0x28323c,
                    true,
                    true,
                    true
                ),
                (0, 3, 1, " ".into(), 0x0a141e, 0x28323c, true, true, true),
                (0, 4, 1, "X".into(), 0xfafafa, 0x181b1a, false, false, false),
            ]
        );
        assert_eq!(
            cells
                .iter()
                .find(|cell| cell.0 == 1 && cell.1 == 0)
                .unwrap()
                .3,
            combining
        );
        assert_eq!(cells.len(), 23);
        assert_eq!(screen.cursor, Some((1, 1)));
    }

    #[test]
    fn scrolling_and_resize_preserve_bounded_history() {
        let mut buffer = TerminalBuffer::new(GridSize { rows: 3, cols: 40 }, 20);
        for row in 0..50 {
            buffer.feed(format!("row-{row}\r\n").as_bytes());
        }
        buffer.scroll(10);
        let rows: String = buffer
            .screen()
            .cells
            .flat_map(|cell| cell.chars().collect::<Vec<_>>())
            .collect();
        assert!(rows.contains("row-38"));
        buffer.resize(GridSize { rows: 4, cols: 42 });
        assert_eq!(buffer.size.cols, 42);
        assert!(buffer.term.grid().history_size() <= 20);
    }
}
