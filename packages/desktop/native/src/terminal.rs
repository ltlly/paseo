use alacritty_terminal::{
    event::VoidListener,
    grid::{Dimensions, Scroll},
    term::{Config, RenderableContent, Term, TermMode, cell::Flags},
    vte::ansi::{Color, NamedColor, Processor, Rgb},
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
}

pub struct Screen<'a> {
    pub cells: ScreenCells<'a>,
    pub cursor: Option<(usize, usize)>,
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
            let mut foreground = resolve_color(cell.fg, self.content.colors);
            let mut background = resolve_color(cell.bg, self.content.colors);
            if cell.flags.contains(Flags::INVERSE) {
                std::mem::swap(&mut foreground, &mut background);
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
            });
        }
        None
    }
}

pub struct TerminalBuffer {
    term: Term<VoidListener>,
    parser: Processor,
    history: usize,
    pub size: GridSize,
    pub replaying: bool,
}

impl TerminalBuffer {
    pub fn new(size: GridSize, history: usize) -> Self {
        let config = Config {
            scrolling_history: history,
            kitty_keyboard: true,
            ..Default::default()
        };
        Self {
            term: Term::new(config, &size, VoidListener),
            parser: Processor::new(),
            history,
            size,
            replaying: false,
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
        self.parser.advance(&mut self.term, bytes);
    }

    fn reset(&mut self, size: GridSize) {
        *self = Self::new(size, self.history);
    }

    pub fn scroll(&mut self, lines: i32) {
        self.term.scroll_display(Scroll::Delta(lines));
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
        let cursor = (self.term.mode().contains(TermMode::SHOW_CURSOR)
            && cursor_row >= 0
            && cursor_row < i32::from(self.size.rows))
        .then_some((cursor_row as usize, content.cursor.point.column.0));
        Screen {
            cells: ScreenCells {
                content,
                rows: self.size.rows,
            },
            cursor,
        }
    }
}

fn resolve_color(color: Color, colors: &alacritty_terminal::term::color::Colors) -> u32 {
    match color {
        Color::Spec(Rgb { r, g, b }) => (u32::from(r) << 16) | (u32::from(g) << 8) | u32::from(b),
        Color::Named(named) => colors[named as usize]
            .map(|rgb| resolve_color(Color::Spec(rgb), colors))
            .unwrap_or_else(|| match named {
                NamedColor::Foreground
                | NamedColor::BrightForeground
                | NamedColor::DimForeground => 0xd8dee9,
                NamedColor::Background => 0x171919,
                NamedColor::Cursor => 0x82aaff,
                _ => indexed_color(named as usize),
            }),
        Color::Indexed(index) => colors[index as usize]
            .map(|rgb| resolve_color(Color::Spec(rgb), colors))
            .unwrap_or_else(|| indexed_color(index as usize)),
    }
}

fn indexed_color(index: usize) -> u32 {
    const BASE: [u32; 16] = [
        0x171919, 0xf07178, 0xc3e88d, 0xffcb6b, 0x82aaff, 0xc792ea, 0x89ddff, 0xd8dee9, 0x676e95,
        0xff5370, 0xd5ffa8, 0xffe082, 0xadd7ff, 0xe1acff, 0xb2ffff, 0xffffff,
    ];
    if index < 16 {
        return BASE[index];
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
    0xd8dee9
}

#[cfg(test)]
mod tests {
    use super::*;

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
        assert_eq!(first.foreground, 0xf07178);
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
                (0, 4, 1, "X".into(), 0xd8dee9, 0x171919, false, false, false),
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
