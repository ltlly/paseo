use alacritty_terminal::{
    event::{Event, EventListener},
    grid::{Dimensions, Grid, Scroll},
    index::{Column, Line, Point, Side},
    selection::{Selection, SelectionType},
    term::{
        Config, RenderableContent, Term, TermMode,
        cell::{Cell as GridCell, Flags, Hyperlink, LineLength},
    },
    vte::ansi::{Color, CursorShape, CursorStyle, NamedColor, Processor, Rgb},
};
use anyhow::{Result, ensure};
use std::sync::{
    Arc,
    atomic::{AtomicU64, Ordering},
};

use crate::{
    appearance::TerminalPalette,
    settings::ThemePreference,
    wire::{ReplayMetadata, TerminalFrame, TerminalOpcode},
};

/// The generated dark terminal palette, used until the shell supplies the
/// resolved appearance. Keeps buffer-only tests and early construction stable.
fn default_terminal_palette() -> TerminalPalette {
    crate::appearance::resolve(ThemePreference::Dark, false).terminal
}

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

/// SGR 4 sub-styles. xterm keeps five underline decorations and renders each
/// with its own stroke pattern; the native view paints the same patterns from
/// `terminal_view::decorations` instead of relying on font underline metrics.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Underline {
    #[default]
    None,
    Single,
    Double,
    Curly,
    Dotted,
    Dashed,
}

impl Underline {
    pub fn is_some(self) -> bool {
        self != Underline::None
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
    pub underline: Underline,
    /// SGR 58 colour, resolved through the same palette and bold promotion as the
    /// foreground. `None` means "the cell foreground", which is what xterm uses.
    pub underline_color: Option<u32>,
    pub selected: bool,
    pub opacity: u8,
    pub hidden: bool,
    pub strikethrough: bool,
    /// OSC 8 hyperlink. This is the grid's shared cell value: cloning it bumps one
    /// reference count and never copies the URI.
    pub hyperlink: Option<Hyperlink>,
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
    palette: TerminalPalette,
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
            let bold = cell.flags.contains(Flags::BOLD);
            let mut foreground =
                resolve_color(foreground, self.content.colors, bold, &self.palette);
            let mut background =
                resolve_color(background, self.content.colors, false, &self.palette);
            if selected {
                let selection = blend_rgb(
                    self.palette.background,
                    self.palette.selection_background >> 8,
                    self.palette.selection_background as u8,
                );
                background = if cell.flags.contains(Flags::INVERSE)
                    || cell.bg != Color::Named(NamedColor::Background)
                {
                    blend_rgb(background, selection, 128)
                } else {
                    selection
                };
                foreground = self.palette.selection_foreground;
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
                            resolve_color(color, self.content.colors, false, &self.palette),
                            selection,
                            128,
                        )
                    };
                }
            }
            let underline_color = cell
                .underline_color()
                .map(|color| resolve_color(color, self.content.colors, bold, &self.palette));
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
                bold,
                italic: cell.flags.contains(Flags::ITALIC),
                underline: underline(cell.flags),
                underline_color,
                selected,
                opacity: if cell.flags.contains(Flags::DIM) && !selected {
                    128
                } else {
                    255
                },
                hidden: cell.flags.contains(Flags::HIDDEN),
                strikethrough: cell.flags.contains(Flags::STRIKEOUT),
                hyperlink: cell.hyperlink(),
            });
        }
        None
    }
}

/// One visible row's text cells, with their columns.
pub struct RowChars<'a> {
    grid: &'a Grid<GridCell>,
    line: Line,
    column: usize,
    columns: usize,
}

impl Iterator for RowChars<'_> {
    type Item = (usize, char);

    fn next(&mut self) -> Option<Self::Item> {
        while self.column < self.columns {
            let column = self.column;
            self.column += 1;
            let cell = &self.grid[self.line][Column(column)];
            if cell
                .flags
                .intersects(Flags::WIDE_CHAR_SPACER | Flags::LEADING_WIDE_CHAR_SPACER)
            {
                continue;
            }
            return Some((column, cell.c));
        }
        None
    }
}

/// The underline flags are exclusive in the parser, so the first match is the
/// cell's decoration.
fn underline(flags: Flags) -> Underline {
    if flags.contains(Flags::UNDERCURL) {
        Underline::Curly
    } else if flags.contains(Flags::DOUBLE_UNDERLINE) {
        Underline::Double
    } else if flags.contains(Flags::DOTTED_UNDERLINE) {
        Underline::Dotted
    } else if flags.contains(Flags::DASHED_UNDERLINE) {
        Underline::Dashed
    } else if flags.contains(Flags::UNDERLINE) {
        Underline::Single
    } else {
        Underline::None
    }
}

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

#[derive(Clone, Copy)]
enum SelectionGesture {
    Word,
    Line,
}

pub struct TerminalBuffer {
    term: Term<CursorEvents>,
    cursor_events: Arc<AtomicU64>,
    parser: Processor,
    written: crate::terminal_handler::WrittenExtents,
    history: usize,
    palette: TerminalPalette,
    pub size: GridSize,
    pub replaying: bool,
    mouse: crate::mouse::ModeTracker,
    gesture: Option<(SelectionGesture, Point, Point)>,
}

impl TerminalBuffer {
    fn config(history: usize) -> Config {
        Config {
            scrolling_history: history,
            kitty_keyboard: true,
            default_cursor_style: CursorStyle {
                shape: CursorShape::Beam,
                blinking: true,
            },
            ..Default::default()
        }
    }

    pub fn new(size: GridSize, history: usize) -> Self {
        let config = Self::config(history);
        let cursor_events = Arc::new(AtomicU64::new(0));
        Self {
            term: Term::new(config, &size, CursorEvents(cursor_events.clone())),
            cursor_events,
            parser: Processor::new(),
            written: crate::terminal_handler::WrittenExtents::new(size.cols, i32::from(size.rows)),
            history,
            palette: default_terminal_palette(),
            size,
            replaying: false,
            mouse: crate::mouse::ModeTracker::default(),
            gesture: None,
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
        self.gesture = None;
        if size == self.size {
            // `Term::resize` returns early too; written cells survive.
            return;
        }
        let alternate_active = self.term.mode().contains(TermMode::ALT_SCREEN);
        self.written.set_history_limit(self.history);
        let cursor_line = self.term.grid().cursor.point.line.0;
        let capture = self.written.capture(&self.term.grid(), alternate_active);
        let sizes = crate::terminal_handler::GridDimensions {
            lines: usize::from(size.rows),
            columns: usize::from(size.cols),
        };
        self.written.resize_inactive_primary(sizes);
        self.term.resize(size);
        self.size = size;
        self.written.migrate(
            capture,
            &self.term.grid(),
            alternate_active,
            cursor_line,
            size.cols,
            i32::from(size.rows),
        );
    }

    pub fn feed(&mut self, bytes: &[u8]) {
        let had_gesture = self.gesture.is_some();
        let selection_before = had_gesture.then(|| {
            self.term
                .selection
                .as_ref()
                .and_then(|selection| selection.to_range(&self.term))
        });
        self.mouse.feed(bytes);
        self.written.set_history_limit(self.history);
        let mut handler =
            crate::terminal_handler::TerminalHandler::new(&mut self.term, &mut self.written);
        self.parser.advance(&mut handler, bytes);
        let alternate_active = self.term.mode().contains(TermMode::ALT_SCREEN);
        self.written
            .prune(alternate_active, self.term.history_size());
        if had_gesture {
            let selection_after = self
                .term
                .selection
                .as_ref()
                .and_then(|selection| selection.to_range(&self.term));
            if selection_before != Some(selection_after) {
                self.gesture = None;
            }
        }
    }

    pub fn cursor_state(&self) -> (Point, bool, CursorStyle, u64) {
        (
            self.term.grid().cursor.point,
            self.term.mode().contains(TermMode::SHOW_CURSOR),
            self.term.cursor_style(),
            self.cursor_events.load(Ordering::Relaxed),
        )
    }

    pub fn kitty_input(&self) -> bool {
        self.mouse.kitty_input()
    }
    pub fn win32_input(&self) -> bool {
        self.mouse.win32_input()
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
        let palette = self.palette;
        *self = Self::new(size, self.history);
        self.palette = palette;
    }

    /// Resolve cell colors for the active appearance. Callers set this before
    /// projecting a frame; the grid itself owns no theme state.
    pub fn set_palette(&mut self, palette: TerminalPalette) {
        self.palette = palette;
    }

    /// Apply the configured scrollback bound in place. Alacritty trims the grid
    /// on resize, so the live terminal reflects the setting without replaying.
    pub fn set_history(&mut self, history: usize) {
        if self.history == history {
            return;
        }
        self.history = history;
        self.written.set_history_limit(history);
        self.term.set_options(Self::config(history));
        let alternate_active = self.term.mode().contains(TermMode::ALT_SCREEN);
        self.written
            .prune(alternate_active, self.term.history_size());
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
        self.gesture = None;
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

    pub fn selection_is_block(&self) -> bool {
        self.term
            .selection
            .as_ref()
            .is_some_and(|selection| selection.ty == SelectionType::Block)
    }
    pub fn selection_is_gesture(&self) -> bool {
        self.gesture.is_some()
    }

    pub fn begin_word_selection(&mut self, row: usize, column: usize) {
        self.begin_gesture(row, column, SelectionGesture::Word);
    }

    pub fn begin_line_selection(&mut self, row: usize, column: usize) {
        self.begin_gesture(row, column, SelectionGesture::Line);
    }

    fn begin_gesture(&mut self, row: usize, column: usize, kind: SelectionGesture) {
        if matches!(kind, SelectionGesture::Word) && column >= usize::from(self.size.cols) {
            return;
        }
        let point = self.selection_point(row, column).0;
        let (start, end) = self.gesture_range(point, kind);
        self.gesture = Some((kind, start, end));
        self.select_range(start, end);
    }

    fn select_range(&mut self, start: Point, end: Point) {
        let mut selection = Selection::new(SelectionType::Simple, start, Side::Left);
        selection.update(end, Side::Right);
        self.term.selection = Some(selection);
    }

    fn gesture_range(&self, mut point: Point, kind: SelectionGesture) -> (Point, Point) {
        if matches!(kind, SelectionGesture::Line) {
            return (
                self.term.line_search_left(point),
                self.term.line_search_right(point),
            );
        }
        if self.term.grid()[point]
            .flags
            .contains(Flags::WIDE_CHAR_SPACER)
        {
            point.column -= 1;
        }
        let character = self.term.grid()[point].c;
        if character == ' ' {
            let mut start = point;
            let mut end = point;
            while start.column.0 > 0
                && self.term.grid()[Point::new(start.line, start.column - 1)].c == ' '
            {
                start.column -= 1;
            }
            while end.column.0 + 1 < usize::from(self.size.cols)
                && self.term.grid()[Point::new(end.line, end.column + 1)].c == ' '
            {
                end.column += 1;
            }
            return (start, end);
        }
        let (mut start, mut end) = self.word_line_range(point);
        while start.column.0 == 0
            && start.line > self.term.topmost_line()
            && self.term.grid()[start].c != ' '
            && self.term.grid()[Point::new(start.line - 1, Column(usize::from(self.size.cols) - 1))]
                .flags
                .contains(Flags::WRAPLINE)
            && self.term.grid()[Point::new(start.line - 1, Column(usize::from(self.size.cols) - 1))]
                .c
                != ' '
        {
            start = self
                .word_line_range(Point::new(
                    start.line - 1,
                    Column(usize::from(self.size.cols) - 1),
                ))
                .0;
        }
        while end.column.0 + 1 == usize::from(self.size.cols)
            && end.line.0 + 1 < i32::from(self.size.rows)
            && self.term.grid()[end].flags.contains(Flags::WRAPLINE)
            && self.term.grid()[end].c != ' '
            && self.term.grid()[Point::new(end.line + 1, Column(0))].c != ' '
        {
            end = self.word_line_range(Point::new(end.line + 1, Column(0))).1;
        }
        (start, end)
    }

    fn word_line_range(&self, mut point: Point) -> (Point, Point) {
        if self.term.grid()[point]
            .flags
            .contains(Flags::WIDE_CHAR_SPACER)
        {
            point.column -= 1;
        }
        let mut start = point;
        let mut end = point;
        if self.term.grid()[point].flags.contains(Flags::WIDE_CHAR) {
            end.column += 1;
        }
        let whitespace = self.term.grid()[point].c == ' ';
        let separates = |point: Point| {
            let cell = &self.term.grid()[point];
            if cell.flags.contains(Flags::WIDE_CHAR_SPACER) {
                return false;
            }
            if whitespace {
                cell.c != ' '
            } else {
                cell.zerowidth().is_none_or(|marks| marks.is_empty())
                    && " ()[]{}',\"`".contains(cell.c)
            }
        };
        while start.column.0 > 0 && !separates(Point::new(start.line, start.column - 1)) {
            start.column -= 1;
        }
        while end.column.0 + 1 < usize::from(self.size.cols)
            && !separates(Point::new(end.line, end.column + 1))
        {
            end.column += 1;
        }
        (start, end)
    }

    pub fn extend_selection(&mut self, row: usize, column: usize) {
        let (point, side) = self.selection_point(row, column);
        if let Some((kind, anchor_start, anchor_end)) = self.gesture {
            let (start, end) = self.gesture_range(point, kind);
            self.select_range(start.min(anchor_start), end.max(anchor_end));
            return;
        }
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
        self.copy_selection()
    }

    /// Trimmed copy length of `line` from the occupancy table: the last cell
    /// the program wrote content to, plus its grid width. `None` for rows that
    /// predate the table (a restored snapshot).
    pub fn line_written_trimmed(&self, line: i32) -> Option<usize> {
        let alternate = self.term.mode().contains(TermMode::ALT_SCREEN);
        let mask = self.written.row(line, alternate)?;
        let columns = usize::from(self.size.cols);
        for column in (0..columns).rev() {
            if !mask.written(column) {
                continue;
            }
            let flags = self.term.grid()[Line(line)][Column(column)].flags;
            if flags.intersects(Flags::WIDE_CHAR_SPACER | Flags::LEADING_WIDE_CHAR_SPACER) {
                continue;
            }
            return Some(
                column
                    + if flags.contains(Flags::WIDE_CHAR) {
                        2
                    } else {
                        1
                    },
            );
        }
        Some(0)
    }

    /// Stable scroll identity for cross-view consumers: `line +
    /// scroll_revision(alternate)` keeps naming the same row while content
    /// only scrolls. Rows live in `-(history)..screen_lines`; the scrollback
    /// cap evicts revisions below the history and full-row clears retire
    /// them, so an anchor is only valid while its row still exists.
    ///
    /// Re-anchor whenever `layout_generation()` changes: it is a process-wide
    /// monotonic epoch bumped on every rebuild or reset of row identity —
    /// resize and reflow, whole-row clears (ED, DECALN, DECCOLM, full-region
    /// scroll resets), alternate-screen swaps and RIS. It never rewinds, so a
    /// `(scroll_revision, layout_generation)` pair observed before a replay
    /// or reset can never recur on new rows.
    pub fn scroll_revision(&self) -> i64 {
        let alternate = self.term.mode().contains(TermMode::ALT_SCREEN);
        self.written.scroll_revision(alternate)
    }

    pub fn layout_generation(&self) -> u64 {
        self.written.layout_generation()
    }

    /// Number of rows tracked by the occupancy table; stays bounded by history
    /// plus screen rows even at the scrollback cap.
    pub fn written_rows(&self) -> usize {
        self.written.row_count()
    }

    /// Whether `line` continues onto the next row, which is what the reference
    /// calls `isWrapped` on the following line.
    pub fn line_continues(&self, line: i32) -> bool {
        if line < -(self.history_size() as i32) || line >= i32::from(self.size.rows) {
            return false;
        }
        self.term.grid()[Line(line)][Column(usize::from(self.size.cols) - 1)]
            .flags
            .contains(Flags::WRAPLINE)
    }

    /// Append the cells of `[from, to)` on `line` the way the reference
    /// translates a row: a wide head copies its glyph and skips its spacer,
    /// a spacer the traversal enters copies as one space, and combining marks
    /// stay attached.
    pub fn push_row_text(&self, line: i32, from: usize, to: usize, out: &mut String) {
        if line < -(self.history_size() as i32) || line >= i32::from(self.size.rows) {
            return;
        }
        let row = &self.term.grid()[Line(line)];
        let mut column = from;
        let end = to.min(usize::from(self.size.cols));
        while column < end {
            let cell = &row[Column(column)];
            if cell.flags.contains(Flags::WIDE_CHAR) {
                out.push(cell.c);
                if let Some(marks) = cell.zerowidth() {
                    out.extend(marks);
                }
                column += 2;
            } else if cell
                .flags
                .intersects(Flags::WIDE_CHAR_SPACER | Flags::LEADING_WIDE_CHAR_SPACER)
            {
                out.push(' ');
                column += 1;
            } else {
                out.push(if cell.c == '\t' { ' ' } else { cell.c });
                if let Some(marks) = cell.zerowidth() {
                    out.extend(marks);
                }
                column += 1;
            }
        }
    }

    fn copy_selection(&self) -> Option<String> {
        let range = self.term.selection.as_ref()?.to_range(&self.term)?;
        // `SelectionRange` excludes the endpoint at the left of the next row.
        // Probe the authority's directed anchors through public update/equality:
        // no parallel anchor state can drift when Selection::rotate clips sides.
        let selection = self.term.selection.as_ref()?;
        let newline_terminated = if selection.ty == SelectionType::Simple
            && range.end.column.0 + 1 == usize::from(self.size.cols)
        {
            let endpoint = Point::new(range.end.line + 1, Column(0));
            let mut forward = selection.clone();
            forward.update(endpoint, Side::Left);
            let mut reverse = selection.clone();
            reverse.update(range.start, Side::Left);
            let mut expected_reverse = Selection::new(SelectionType::Simple, endpoint, Side::Left);
            expected_reverse.update(range.start, Side::Left);
            forward == *selection || reverse == expected_reverse
        } else {
            false
        };
        Some(crate::terminal_copy::text(
            self,
            range.start.line.0,
            range.start.column.0,
            range.end.line.0,
            range.end.column.0,
            range.is_block,
            newline_terminated,
        ))
    }

    pub fn clear_selection(&mut self) {
        self.gesture = None;
        self.term.selection = None;
    }

    /// Select the whole buffer, like xterm's Select All.
    pub fn select_all(&mut self) {
        self.gesture = None;
        let top = self.term.topmost_line();
        let bottom = Line(i32::from(self.size.rows) - 1);
        let mut selection = Selection::new(
            SelectionType::Simple,
            Point::new(top, Column(0)),
            Side::Left,
        );
        selection.update(
            Point::new(bottom, Column(usize::from(self.size.cols) - 1)),
            Side::Right,
        );
        self.term.selection = Some(selection);
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

    pub fn history_size(&self) -> usize {
        self.term.grid().history_size()
    }

    pub fn display_offset(&self) -> usize {
        self.term.grid().display_offset()
    }

    /// Scroll to an absolute display offset. Positive offsets show older rows.
    pub fn scroll_to(&mut self, offset: usize) -> bool {
        let current = self.term.grid().display_offset();
        if current == offset {
            return false;
        }
        self.term
            .scroll_display(Scroll::Delta(offset as i32 - current as i32));
        true
    }

    fn visible_line(&self, row: usize) -> Option<Line> {
        (row < usize::from(self.size.rows))
            .then(|| Line(row as i32 - self.term.grid().display_offset() as i32))
    }

    /// The cells of one visible row that carry text, with their columns. Wide
    /// spacers are skipped so the sequence matches the row's text.
    pub fn row_chars(&self, row: usize) -> Option<RowChars<'_>> {
        Some(RowChars {
            grid: self.term.grid(),
            line: self.visible_line(row)?,
            column: 0,
            columns: usize::from(self.size.cols),
        })
    }

    pub fn row_hyperlink(&self, row: usize, column: usize) -> Option<Hyperlink> {
        if column >= usize::from(self.size.cols) {
            return None;
        }
        let line = self.visible_line(row)?;
        self.term.grid()[line][Column(column)].hyperlink()
    }

    /// Whether the row continues the previous row (the previous row's last cell
    /// carries the wrap flag).
    pub fn row_continues(&self, row: usize) -> bool {
        if row == 0 || row >= usize::from(self.size.rows) {
            return false;
        }
        let Some(line) = self.visible_line(row - 1) else {
            return false;
        };
        self.term.grid()[line][Column(usize::from(self.size.cols) - 1)]
            .flags
            .contains(Flags::WRAPLINE)
    }

    /// Occupied cells of a row: alacritty trims untouched trailing blanks.
    pub fn row_trimmed_length(&self, row: usize) -> usize {
        let Some(line) = self.visible_line(row) else {
            return 0;
        };
        self.term.grid()[line].line_length().0
    }

    /// Text cells of any grid line, with their columns and cell widths.
    pub fn line_chars(&self, line: i32) -> Option<Vec<(usize, char, usize)>> {
        if line < -(self.history_size() as i32) || line >= i32::from(self.size.rows) {
            return None;
        }
        let row = &self.term.grid()[Line(line)];
        let mut chars = Vec::with_capacity(usize::from(self.size.cols));
        for column in 0..usize::from(self.size.cols) {
            let cell = &row[Column(column)];
            if cell
                .flags
                .intersects(Flags::WIDE_CHAR_SPACER | Flags::LEADING_WIDE_CHAR_SPACER)
            {
                continue;
            }
            let columns = if cell.flags.contains(Flags::WIDE_CHAR) {
                2
            } else {
                1
            };
            chars.push((column, cell.c, columns));
        }
        Some(chars)
    }

    pub fn line_trimmed_length(&self, line: i32) -> usize {
        if line < -(self.history_size() as i32) || line >= i32::from(self.size.rows) {
            return 0;
        }
        self.term.grid()[Line(line)].line_length().0
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
        let cursor_color = resolve_color(
            Color::Named(NamedColor::Cursor),
            content.colors,
            false,
            &self.palette,
        );
        Screen {
            cells: ScreenCells {
                content,
                rows: self.size.rows,
                palette: self.palette,
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
    palette: &TerminalPalette,
) -> u32 {
    let color = match color {
        Color::Indexed(index) if bold && index < 8 => Color::Indexed(index + 8),
        Color::Named(named) if bold && (named as usize) < 8 => Color::Indexed(named as u8 + 8),
        value => value,
    };
    match color {
        Color::Spec(Rgb { r, g, b }) => (u32::from(r) << 16) | (u32::from(g) << 8) | u32::from(b),
        Color::Named(named) => colors[named as usize]
            .map(|rgb| resolve_color(Color::Spec(rgb), colors, false, palette))
            .unwrap_or_else(|| match named {
                NamedColor::Foreground
                | NamedColor::BrightForeground
                | NamedColor::DimForeground => palette.foreground,
                NamedColor::Background => palette.background,
                NamedColor::Cursor => palette.cursor,
                _ => indexed_color(named as usize, palette),
            }),
        Color::Indexed(index) => colors[index as usize]
            .map(|rgb| resolve_color(Color::Spec(rgb), colors, false, palette))
            .unwrap_or_else(|| indexed_color(index as usize, palette)),
    }
}

fn indexed_color(index: usize, palette: &TerminalPalette) -> u32 {
    if index < 16 {
        return palette.ansi[index];
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
    palette.foreground
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn configured_palette_resolves_indexed_named_and_selection_colors() {
        let palette = crate::appearance::resolve(ThemePreference::Light, false).terminal;
        let mut buffer = TerminalBuffer::new(GridSize { rows: 2, cols: 8 }, 0);
        buffer.set_palette(palette);
        buffer.feed(b"\x1b[31mR\x1b[39mN\x1b[44mB");
        let cells = buffer.screen().cells.take(3).collect::<Vec<_>>();
        assert_eq!(cells[0].foreground, palette.ansi[1]);
        assert_eq!(cells[1].foreground, palette.foreground);
        assert_eq!(cells[1].background, palette.background);
        assert_eq!(cells[2].background, palette.ansi[4]);
        assert_eq!(buffer.screen().cursor_color, palette.cursor);

        buffer.begin_selection(0, 0, false);
        buffer.extend_selection(0, 2);
        let selected = buffer.screen().cells.next().unwrap();
        assert!(selected.selected);
        assert_eq!(selected.foreground, palette.selection_foreground);
    }

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
                ('X', 128, true, true, Underline::Single, 0x28323c),
                ('Y', 255, false, false, Underline::Single, 0x28323c)
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
    fn underline_styles_and_colors_follow_sgr_subparams_and_palette_promotion() {
        let mut buffer = TerminalBuffer::new(GridSize { rows: 2, cols: 8 }, 0);
        for (sgr, style) in [
            ("4:0", Underline::None),
            ("4:1", Underline::Single),
            ("4:2", Underline::Double),
            ("4:3", Underline::Curly),
            ("4:4", Underline::Dotted),
            ("4:5", Underline::Dashed),
            ("24", Underline::None),
            ("4:9", Underline::Single),
        ] {
            buffer.feed(format!("\x1b[H\x1b[0;{sgr}mX").as_bytes());
            assert_eq!(
                buffer.screen().cells.next().unwrap().underline,
                style,
                "{sgr}"
            );
        }
        // SGR 21 is ECMA-48 double underline, but the vendored parser maps it to
        // "cancel bold" and cannot express it; 4:2 is the portable spelling.
        buffer.feed(b"\x1b[H\x1b[0;21mX");
        assert_eq!(
            buffer.screen().cells.next().unwrap().underline,
            Underline::None
        );
        buffer.feed(b"\x1b[H\x1b[0;58;2;10;20;30;4mX");
        let cell = buffer.screen().cells.next().unwrap();
        assert_eq!(cell.underline_color, Some(0x0a141e));
        assert_eq!(cell.underline, Underline::Single);
        buffer.feed(b"\x1b[H\x1b[0;58;5;4;4mX");
        assert_eq!(
            buffer.screen().cells.next().unwrap().underline_color,
            Some(crate::theme::terminal::ANSI[4])
        );
        buffer.feed(b"\x1b[H\x1b[0;1;58;5;1;4mX");
        assert_eq!(
            buffer.screen().cells.next().unwrap().underline_color,
            Some(crate::theme::terminal::ANSI[9])
        );
        buffer.feed(b"\x1b[H\x1b[0;58;2;10;20;30m\x1b[59mX");
        assert_eq!(buffer.screen().cells.next().unwrap().underline_color, None);
    }

    #[test]
    fn osc8_hyperlinks_are_exposed_with_each_cell() {
        let mut buffer = TerminalBuffer::new(GridSize { rows: 2, cols: 12 }, 0);
        buffer.feed(b"\x1b]8;;https://paseo.sh\x07link\x1b]8;;\x07plain");
        let cells: Vec<_> = buffer
            .screen()
            .cells
            .take(9)
            .map(|cell| (cell.col, cell.hyperlink.map(|link| link.uri().to_owned())))
            .collect();
        assert_eq!(cells[0].1.as_deref(), Some("https://paseo.sh"));
        assert_eq!(cells[3].1.as_deref(), Some("https://paseo.sh"));
        assert_eq!(cells[4].1, None);
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
    fn word_gestures_include_initial_separators_and_follow_wrapped_edge_words() {
        for (text, column, expected) in [
            ("abc(def", 3, "abc(def"),
            ("abc()def", 3, "abc("),
            ("abc()def", 4, ")def"),
            ("中(文", 2, "中(文"),
            ("a(\u{301}b", 0, "a(\u{301}b"),
        ] {
            let mut buffer = TerminalBuffer::new(GridSize { rows: 3, cols: 20 }, 10);
            buffer.feed(text.as_bytes());
            buffer.begin_word_selection(0, column);
            assert_eq!(buffer.selected_text().as_deref(), Some(expected));
        }
        let mut wrapped = TerminalBuffer::new(GridSize { rows: 3, cols: 5 }, 10);
        wrapped.feed(b"abcd(ef ");
        wrapped.begin_word_selection(1, 0);
        assert_eq!(wrapped.selected_text().as_deref(), Some("abcd(ef"));
        let mut live = TerminalBuffer::new(GridSize { rows: 3, cols: 8 }, 10);
        live.feed(b"one\r\ntwo\r\nthree");
        live.begin_word_selection(1, 1);
        live.feed(b"\r\nfour");
        assert!(!live.selection_is_gesture());
        assert_eq!(live.selected_text().as_deref(), Some("two"));
    }

    /// The reference copy keeps the columns a program wrote: explicit trailing
    /// spaces survive, unwritten cells do not. The expected strings are the ones
    /// `terminal-copy.json` recorded from `@xterm/headless`.
    #[test]
    fn selection_copy_keeps_every_printed_space() {
        let mut buffer = TerminalBuffer::new(GridSize { rows: 4, cols: 6 }, 10);
        buffer.feed(b"abc   def\r\n");
        buffer.begin_selection(0, 0, false);
        buffer.extend_selection(0, 4);
        assert_eq!(buffer.selected_text().as_deref(), Some("abc "));
        buffer.extend_selection(1, 3);
        assert_eq!(buffer.selected_text().as_deref(), Some("abc   def"));
        buffer.select_all();
        assert_eq!(buffer.selected_text().as_deref(), Some("abc   def\n\n"));
    }

    #[test]
    fn word_and_logical_line_gestures_keep_xterm_separators_and_exact_copy_newlines() {
        let mut buffer = TerminalBuffer::new(GridSize { rows: 4, cols: 24 }, 20);
        buffer.feed("foo:bar|baz (word)   中e\u{301}\r\nsecond".as_bytes());
        buffer.begin_word_selection(0, 5);
        assert_eq!(buffer.selected_text().as_deref(), Some("foo:bar|baz"));
        buffer.begin_word_selection(0, 11);
        assert_eq!(buffer.selected_text().as_deref(), Some(" "));
        buffer.begin_word_selection(0, 12);
        assert_eq!(buffer.selected_text().as_deref(), Some("(word"));
        buffer.begin_word_selection(0, 14);
        assert_eq!(buffer.selected_text().as_deref(), Some("word"));
        buffer.begin_word_selection(0, 18);
        assert_eq!(buffer.selected_text().as_deref(), Some("   "));
        buffer.begin_line_selection(1, 2);
        assert_eq!(buffer.selected_text().as_deref(), Some("second"));
        buffer.extend_selection(1, 2);
        assert_eq!(buffer.selected_text().as_deref(), Some("second"));
        let mut wrapped = TerminalBuffer::new(GridSize { rows: 3, cols: 5 }, 10);
        wrapped.feed(b"abcdefghi\r\nnext");
        wrapped.begin_line_selection(0, 2);
        assert_eq!(wrapped.selected_text().as_deref(), Some("abcdefghi"));
        wrapped.begin_word_selection(1, 1);
        assert_eq!(wrapped.selected_text().as_deref(), Some("abcdefghi"));
        wrapped.extend_selection(2, 1);
        assert_eq!(wrapped.selected_text().as_deref(), Some("abcdefghi\nnext"));
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
                (
                    0,
                    0,
                    2,
                    "中".into(),
                    0x0a141e,
                    0x28323c,
                    true,
                    true,
                    Underline::Single
                ),
                (
                    0,
                    2,
                    1,
                    "e\u{301}".into(),
                    0x0a141e,
                    0x28323c,
                    true,
                    true,
                    Underline::Single
                ),
                (
                    0,
                    3,
                    1,
                    " ".into(),
                    0x0a141e,
                    0x28323c,
                    true,
                    true,
                    Underline::Single
                ),
                (
                    0,
                    4,
                    1,
                    "X".into(),
                    0xfafafa,
                    0x181b1a,
                    false,
                    false,
                    Underline::None
                ),
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

    #[test]
    fn a_history_change_bounds_output_that_arrives_before_the_next_render() {
        let mut buffer = TerminalBuffer::new(GridSize { rows: 2, cols: 12 }, 0);
        for row in 0..4 {
            buffer.feed(format!("old-{row}\r\n").as_bytes());
        }
        assert_eq!(buffer.term.grid().history_size(), 0);

        // The shell applies a new cap to a hidden terminal before frames land;
        // the next output must be bounded by the new value, not the old one.
        buffer.set_history(4);
        for row in 0..4 {
            buffer.feed(format!("new-{row}\r\n").as_bytes());
        }
        assert!(buffer.term.grid().history_size() > 0);
        assert!(buffer.term.grid().history_size() <= 4);

        buffer.set_history(0);
        assert_eq!(buffer.term.grid().history_size(), 0);
    }
}
