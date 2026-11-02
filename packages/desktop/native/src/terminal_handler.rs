//! Parser handler that records which grid cells a program actually wrote.
//!
//! The grid stores a written space and a never-written cell the same way, so
//! `LineLength` trims both. The reference copy path trims only cells without
//! content (`BufferLine.getTrimmedLength` tests `Content.HAS_CONTENT_MASK`), so
//! a copy keeps explicitly printed trailing spaces and drops unwritten cells.
//! That difference is only observable while the parser runs, so every
//! `vte::ansi::Handler` call below is forwarded to the `Term` unchanged and the
//! writes are recorded on the side.
//!
//! Occupancy is tracked per cell, one bit per column, because a single "last
//! written column" cannot tell untouched cells from erased ones: `ECH`, `EL`
//! and `DCH` must be able to remove written cells that sit before the tail.
//!
//! Rows are keyed by a stable line revision, `revision = line + scrolled`,
//! where `scrolled` counts the lines that have left the top of the primary
//! screen. Every scroll is mirrored from the terminal's own rules instead of
//! inferred from history growth, so partial scroll regions, region resets and
//! the scrollback cap all move exactly the rows the grid moves.
//!
//! The primary and alternate screens keep separate tables. Entering the
//! alternate screen clears the alternate table (the grid resets it too) and
//! never touches the primary one, so operations on either screen cannot
//! corrupt the other's copy.
//!
//! A resize rebuilds the table by streaming the occupancy of each logical line
//! (rows joined by wrap flags) into the rows of the resized grid, which
//! mirrors how the grid reflows cells. A same-size resize is a no-op, matching
//! `Term::resize`.

use alacritty_terminal::{
    event::EventListener,
    grid::{Dimensions, Grid, GridCell},
    index::{Column, Line},
    term::{Term, TermMode, cell::Cell, cell::Flags},
    vte::ansi::{
        Attr, CharsetIndex, ClearMode, Color, CursorShape, CursorStyle, Handler as _, Hyperlink,
        KeyboardModes, KeyboardModesApplyBehavior, LineClearMode, Mode, ModifyOtherKeys,
        NamedPrivateMode, PrivateMode, Rgb, ScpCharPath, ScpUpdateMode, StandardCharset,
        TabulationClearMode, cursor_icon::CursorIcon,
    },
};
use std::collections::BTreeMap;
use std::sync::atomic::{AtomicU64, Ordering};
use unicode_width::UnicodeWidthChar;

/// Process-wide layout epoch source. A fresh table must never reuse an epoch
/// a previous table already handed out, so `TerminalBuffer` resets (replay,
/// restore) cannot rewind `layout_generation` back to an older value.
static LAYOUT_EPOCH: AtomicU64 = AtomicU64::new(0);

fn next_layout_epoch() -> u64 {
    LAYOUT_EPOCH.fetch_add(1, Ordering::Relaxed) + 1
}

/// One cell of a row carries written content.
///
/// Only content heads carry a bit: the trailing spacer of a wide character
/// never does, and the trimmed length is derived from the head's grid width.
#[derive(Clone, Default)]
pub struct RowMask {
    cells: Vec<bool>,
}

impl RowMask {
    fn new(columns: usize) -> Self {
        Self {
            cells: vec![false; columns],
        }
    }

    /// Whether the cell at `column` was written with content.
    pub fn written(&self, column: usize) -> bool {
        self.cells.get(column).copied().unwrap_or(false)
    }

    fn set(&mut self, column: usize) {
        if let Some(cell) = self.cells.get_mut(column) {
            *cell = true;
        }
    }

    fn any_written(&self) -> bool {
        self.cells.iter().any(|written| *written)
    }

    fn erase(&mut self, from: usize, to: usize) {
        let from = from.min(self.cells.len());
        let to = to.min(self.cells.len());
        for cell in &mut self.cells[from..to] {
            *cell = false;
        }
    }

    /// Content at `at..` moves right by `by`; the vacated cells are unwritten.
    fn shift_right(&mut self, at: usize, by: usize) {
        let len = self.cells.len();
        if by == 0 || at >= len {
            return;
        }
        for index in (at..len.saturating_sub(by)).rev() {
            self.cells[index + by] = self.cells[index];
        }
        self.erase(at, at + by);
    }

    /// `Term::input`'s `IRM` shift: `swap(column + by, column)` from the last
    /// shifted column down, which moves the tail's cells into the positions
    /// the write is about to overwrite.
    fn rotate(&mut self, at: usize, by: usize) {
        let len = self.cells.len();
        if by == 0 || at + by >= len {
            return;
        }
        for column in (at..len - by).rev() {
            self.cells.swap(column + by, column);
        }
    }

    /// Content at `at + by..` moves left by `by`; the tail is unwritten.
    fn shift_left(&mut self, at: usize, by: usize) {
        let len = self.cells.len();
        if by == 0 || at >= len {
            return;
        }
        for index in at..len.saturating_sub(by) {
            self.cells[index] = self.cells[index + by];
        }
        self.erase(len.saturating_sub(by), len);
    }
}

/// Occupancy of one screen (primary or alternate).
struct ScreenTable {
    /// Lines that have left the top of the screen.
    scrolled: i64,
    /// Written cells per stable line revision.
    rows: BTreeMap<i64, RowMask>,
    columns: usize,
}

impl ScreenTable {
    fn new(columns: usize) -> Self {
        Self {
            scrolled: 0,
            rows: BTreeMap::new(),
            columns,
        }
    }

    fn revision(&self, line: i32) -> i64 {
        i64::from(line) + self.scrolled
    }

    fn row(&self, line: i32) -> Option<&RowMask> {
        self.rows.get(&self.revision(line))
    }

    fn row_entry(&mut self, line: i32) -> &mut RowMask {
        let revision = self.revision(line);
        self.rows
            .entry(revision)
            .or_insert_with(|| RowMask::new(self.columns))
    }

    fn mark(&mut self, line: i32, column: usize) {
        self.row_entry(line).set(column);
    }

    fn fill(&mut self, line: i32) {
        let columns = self.columns;
        let row = self.row_entry(line);
        for column in 0..columns {
            row.set(column);
        }
    }

    fn erase(&mut self, line: i32, from: usize, to: usize) {
        if let Some(row) = self.rows.get_mut(&self.revision(line)) {
            row.erase(from, to);
        }
    }

    /// Reserve a row the program wrote to but left without content, so a copy
    /// trims it against the table instead of the grid.
    fn observe(&mut self, line: i32) {
        self.row_entry(line);
    }

    fn shift_right(&mut self, line: i32, at: usize, by: usize) {
        if let Some(row) = self.rows.get_mut(&self.revision(line)) {
            row.shift_right(at, by);
        }
    }

    fn rotate(&mut self, line: i32, at: usize, by: usize) {
        if let Some(row) = self.rows.get_mut(&self.revision(line)) {
            row.rotate(at, by);
        }
    }

    fn shift_left(&mut self, line: i32, at: usize, by: usize) {
        if let Some(row) = self.rows.get_mut(&self.revision(line)) {
            row.shift_left(at, by);
        }
    }

    fn clear_row(&mut self, line: i32) {
        self.rows.remove(&self.revision(line));
    }

    fn clear_screen_rows(&mut self, from: i32, to: i32) {
        for line in from..to {
            self.clear_row(line);
        }
    }

    /// Mirror of `Term::scroll_up_relative`: content above `origin` is fixed,
    /// rows in `origin..region_end` move up, and a scroll from the top of the
    /// screen feeds history (rebased even when the scrollback is already full).
    /// Returns whether the grid reset the whole region instead of moving rows.
    fn scroll_up_relative(
        &mut self,
        origin: i32,
        lines: usize,
        region_start: i32,
        region_end: i32,
        screen_lines: i32,
    ) -> bool {
        let count = lines.min((region_end - region_start).max(0) as usize) as i32;
        if origin != 0 && count >= region_end - origin {
            // The grid resets the whole region instead of moving anything.
            self.clear_screen_rows(origin, region_end);
            return true;
        }
        if origin == 0 {
            // Every row moves up, so the rebase keeps the region rows in place.
            self.scrolled += i64::from(count);
            // Rows below the region stay put: undo their share of the rebase.
            // The grid swaps these fixed lines starting from the bottom so a
            // row is never swapped with a row already moved into its place
            // (`Grid::scroll_up`), so the rebases must run in the same order.
            let count = i64::from(count);
            let low = i64::from(region_end) + self.scrolled - count;
            let high = i64::from(screen_lines) + self.scrolled - count;
            for key in (low..high).rev() {
                if let Some(row) = self.rows.remove(&key) {
                    self.rows.insert(key + count, row);
                }
            }
        } else {
            for line in origin..(region_end - count) {
                let lower = self.revision(line);
                let upper = self.revision(line + count);
                let upper_row = self.rows.remove(&upper);
                let lower_row = self.rows.remove(&lower);
                if let Some(row) = upper_row {
                    self.rows.insert(lower, row);
                }
                if let Some(row) = lower_row {
                    self.rows.insert(upper, row);
                }
            }
        }
        for line in (region_end - count)..region_end {
            self.clear_row(line);
        }
        false
    }

    /// Mirror of `Term::scroll_down_relative`: rows in `origin..region_end`
    /// move down and the vacated rows at `origin` are cleared. Returns whether
    /// the grid reset the whole region instead of moving rows.
    fn scroll_down_relative(
        &mut self,
        origin: i32,
        lines: usize,
        region_start: i32,
        region_end: i32,
    ) -> bool {
        let count = lines
            .min((region_end - region_start).max(0) as usize)
            .min((region_end - origin).max(0) as usize) as i32;
        if count >= region_end - origin {
            // The grid resets the whole region instead of moving anything.
            self.clear_screen_rows(origin, region_end);
            return true;
        }
        for line in (origin + count..region_end).rev() {
            let lower = self.revision(line - count);
            let upper = self.revision(line);
            let upper_row = self.rows.remove(&upper);
            let lower_row = self.rows.remove(&lower);
            if let Some(row) = upper_row {
                self.rows.insert(lower, row);
            }
            if let Some(row) = lower_row {
                self.rows.insert(upper, row);
            }
        }
        self.clear_screen_rows(origin, origin + count);
        false
    }

    /// Drop rows that have left the scrollback, so a long session keeps at
    /// most one entry per history or screen row.
    fn prune(&mut self, history: usize) {
        let watermark = self.scrolled - history as i64;
        while let Some(oldest) = self.rows.keys().next().copied() {
            if oldest < watermark {
                self.rows.pop_first();
            } else {
                break;
            }
        }
    }

    /// Rebuild rows without reflow: the alternate screen keeps every row's
    /// content, so rows only shift for the lines the resize added or scrolled
    /// away.
    fn migrate_without_reflow(
        &mut self,
        capture: WrittenCapture,
        _cursor_line: i32,
        columns: usize,
        screen_lines: i32,
    ) {
        self.migrate_actual(capture, false, columns, screen_lines);
    }

    fn migrate_with_reflow(
        &mut self,
        capture: WrittenCapture,
        _grid: &Grid<Cell>,
        columns: usize,
        screen_lines: i32,
    ) {
        self.migrate_actual(capture, true, columns, screen_lines);
    }

    fn migrate_actual(
        &mut self,
        mut capture: WrittenCapture,
        reflow: bool,
        columns: usize,
        screen_lines: i32,
    ) {
        // Metadata is ignored by GridCell emptiness/equality. Alacritty's own
        // resize therefore makes exactly the same moves and discards as Term.
        capture
            .grid
            .resize::<Color>(reflow, screen_lines as usize, columns);
        let mut table = Self::new(columns);
        table.scrolled = capture.grid.history_size() as i64;
        for line in -(capture.grid.history_size() as i32)..screen_lines {
            let mut mask = RowMask::new(columns);
            for column in 0..columns {
                if capture.grid[Line(line)][Column(column)].written {
                    mask.set(column);
                }
            }
            if mask.any_written() {
                table.rows.insert(table.revision(line), mask);
            }
        }
        *self = table;
    }
}

#[derive(Clone, Default)]
struct CapturedCell {
    cell: Cell,
    written: bool,
}

impl PartialEq for CapturedCell {
    fn eq(&self, other: &Self) -> bool {
        self.cell == other.cell
    }
}

impl GridCell for CapturedCell {
    fn is_empty(&self) -> bool {
        self.cell.is_empty()
    }
    fn flags(&self) -> &Flags {
        &self.cell.flags
    }
    fn flags_mut(&mut self) -> &mut Flags {
        &mut self.cell.flags
    }
    fn reset(&mut self, template: &Self) {
        self.cell.reset(&template.cell);
        self.written = false;
    }
}

impl alacritty_terminal::term::cell::ResetDiscriminant<Color> for CapturedCell {
    fn discriminant(&self) -> Color {
        self.cell.bg
    }
}

pub struct GridDimensions {
    pub lines: usize,
    pub columns: usize,
}

pub struct WrittenCapture {
    grid: Grid<CapturedCell>,
}

/// Primary occupancy captured when the alternate screen was entered, kept so
/// a resize while the alternate is active can reflow it at swap-back: the
/// primary grid is not observable while inactive, but the swap makes it
/// current again, still carrying the reflowed content.
struct PendingPrimary {
    capture: WrittenCapture,
    resized: bool,
    columns: usize,
    screen_lines: i32,
    /// The primary grid's charset slots, which the swap hands back untouched.
    charsets: [StandardCharset; 4],
}

/// Written-cell occupancy for both screens.
pub struct WrittenExtents {
    primary: ScreenTable,
    alternate: ScreenTable,
    columns: usize,
    screen_lines: i32,
    /// The terminal's scrolling region, `[start, end)`.
    region: (i32, i32),
    /// Process-wide monotonic layout epoch: bumped whenever row identity is
    /// rebuilt or a whole screen's rows are reset (resize, reflow, clears,
    /// screen swaps, reset). Never rewinds, so an old
    /// `(scroll_revision, generation)` anchor can never recur on new rows.
    generation: u64,
    history_limit: usize,
    pending_primary: Option<PendingPrimary>,
    /// The terminal's active charset and its four slots, mirrored from `Term`
    /// (`Term::write_at_cursor` maps through `grid.cursor.charsets`).
    active_charset: CharsetIndex,
    charsets: [StandardCharset; 4],
}

impl WrittenExtents {
    pub fn set_history_limit(&mut self, history: usize) {
        self.history_limit = history;
        self.primary.prune(history);
        if let Some(pending) = &mut self.pending_primary {
            pending.capture.grid.update_history(history);
        }
    }

    pub fn resize_inactive_primary(&mut self, sizes: GridDimensions) {
        if let Some(pending) = &mut self.pending_primary {
            pending.resized = true;
            pending
                .capture
                .grid
                .resize::<Color>(true, sizes.lines, sizes.columns);
        }
    }

    pub fn new(columns: u16, screen_lines: i32) -> Self {
        Self {
            primary: ScreenTable::new(usize::from(columns)),
            alternate: ScreenTable::new(usize::from(columns)),
            columns: usize::from(columns),
            screen_lines,
            region: (0, screen_lines),
            generation: next_layout_epoch(),
            history_limit: 0,
            pending_primary: None,
            active_charset: CharsetIndex::G0,
            charsets: [StandardCharset::Ascii; 4],
        }
    }

    pub fn row_count(&self) -> usize {
        self.primary.rows.len() + self.alternate.rows.len()
    }

    pub fn scroll_revision(&self, alternate: bool) -> i64 {
        if alternate {
            self.alternate.scrolled
        } else {
            self.primary.scrolled
        }
    }

    pub fn layout_generation(&self) -> u64 {
        self.generation
    }

    pub fn region(&self) -> (i32, i32) {
        self.region
    }

    fn table_mut(&mut self, alternate: bool) -> &mut ScreenTable {
        if alternate {
            &mut self.alternate
        } else {
            &mut self.primary
        }
    }

    fn table(&self, alternate: bool) -> &ScreenTable {
        if alternate {
            &self.alternate
        } else {
            &self.primary
        }
    }

    /// Written cells of `line` on the given screen.
    pub fn row(&self, line: i32, alternate: bool) -> Option<&RowMask> {
        self.table(alternate).row(line)
    }

    /// The character `write_at_cursor` stores for `c`: the terminal's active
    /// charset mapped glyph. The handler is rebuilt for every feed, so the
    /// mirror lives here, where it survives a feed boundary.
    pub fn map_char(&self, c: char) -> char {
        self.charsets[self.active_charset as usize].map(c)
    }

    pub fn set_active_charset(&mut self, index: CharsetIndex) {
        self.active_charset = index;
    }

    pub fn configure_charset(&mut self, index: CharsetIndex, charset: StandardCharset) {
        self.charsets[index as usize] = charset;
    }

    /// Mirror of `Term::set_scrolling_region`; an invalid region keeps the old
    /// one, like the terminal.
    pub fn set_scrolling_region(&mut self, top: usize, bottom: Option<usize>, screen_lines: i32) {
        let bottom = bottom.unwrap_or(screen_lines as usize);
        if top >= bottom {
            return;
        }
        self.region = (
            i32::try_from(top).map_or(self.region.0, |top| (top - 1).min(screen_lines)),
            usize::try_from(bottom)
                .map_or(self.region.1, |bottom| (bottom as i32).min(screen_lines)),
        );
    }

    fn reset_region(&mut self) {
        self.region = (0, self.screen_lines);
    }

    fn invalidate(&mut self) {
        self.generation = next_layout_epoch();
    }

    /// `DECALN` and `DECCOLM` rewrite or clear the whole active screen.
    pub fn clear_active_screen_rows(&mut self, alternate: bool) {
        let screen_lines = self.screen_lines;
        self.table_mut(alternate).clear_screen_rows(0, screen_lines);
        self.invalidate();
    }

    /// Entering the alternate screen resets the alternate grid. `capture` is
    /// the primary occupancy taken before the screens swapped; it is kept for
    /// the swap-back, which must reflow it when the inactive grid resized.
    pub fn enter_alternate(&mut self, capture: WrittenCapture, columns: usize, screen_lines: i32) {
        self.alternate = ScreenTable::new(self.columns);
        self.pending_primary = Some(PendingPrimary {
            capture,
            resized: false,
            columns,
            screen_lines,
            charsets: self.charsets,
        });
        // The visible grid changes, so the rows their revisions named are gone.
        self.invalidate();
    }

    /// Leaving the alternate screen restores the primary grid. When it was
    /// resized while inactive, the stashed occupancy is reflowed against the
    /// now-observable primary grid; otherwise the old table is still valid.
    pub fn leave_alternate(&mut self, grid: &Grid<Cell>, columns: usize, screen_lines: i32) {
        if let Some(pending) = self.pending_primary.take() {
            if pending.resized || pending.columns != columns || pending.screen_lines != screen_lines
            {
                self.primary
                    .migrate_with_reflow(pending.capture, grid, columns, screen_lines);
            }
            // Charset writes went to the alternate grid's cursor; the primary's
            // slots come back with the swap.
            self.charsets = pending.charsets;
        }
        self.invalidate();
    }

    /// `RIS` resets both screens, the scroll region and the charsets.
    pub fn reset_all(&mut self) {
        let columns = self.columns;
        self.primary = ScreenTable::new(columns);
        self.alternate = ScreenTable::new(columns);
        self.pending_primary = None;
        self.active_charset = CharsetIndex::G0;
        self.charsets = [StandardCharset::Ascii; 4];
        self.reset_region();
        self.invalidate();
    }

    pub fn prune(&mut self, alternate: bool, history: usize) {
        self.table_mut(alternate).prune(history);
    }

    /// Capture the occupancy of the active screen, grouped into logical lines
    /// in reading order, before the grid is resized.
    ///
    /// The alternate screen keeps its rows apart: it does not reflow, so its
    /// rows are captured one group each.
    pub fn capture(&self, grid: &Grid<Cell>, alternate_active: bool) -> WrittenCapture {
        let history = grid.history_size();
        let lines = grid.screen_lines();
        let columns = grid.columns();
        let capacity = if alternate_active {
            0
        } else {
            self.history_limit.max(history)
        };
        let mut captured = Grid::new(lines, columns, capacity);
        if history != 0 {
            captured.scroll_up(&(Line(0)..Line(lines as i32)), history);
        }
        let table = self.table(alternate_active);
        for line in -(history as i32)..lines as i32 {
            for column in 0..columns {
                captured[Line(line)][Column(column)] = CapturedCell {
                    cell: grid[Line(line)][Column(column)].clone(),
                    written: table.row(line).is_some_and(|row| row.written(column)),
                };
            }
        }
        captured.cursor.point = grid.cursor.point;
        captured.cursor.input_needs_wrap = grid.cursor.input_needs_wrap;
        captured.cursor.template.cell = grid.cursor.template.clone();
        captured.saved_cursor.point = grid.saved_cursor.point;
        captured.saved_cursor.input_needs_wrap = grid.saved_cursor.input_needs_wrap;
        WrittenCapture { grid: captured }
    }

    /// Rebuild the active table for the resized grid. The inactive table is
    /// kept: `Term::resize` reflows the inactive grid too, and its occupancy
    /// is migrated against it when the screens swap back (`leave_alternate`).
    pub fn migrate(
        &mut self,
        capture: WrittenCapture,
        grid: &Grid<Cell>,
        alternate_active: bool,
        cursor_line: i32,
        columns: u16,
        screen_lines: i32,
    ) {
        let columns = usize::from(columns);
        self.columns = columns;
        self.screen_lines = screen_lines;
        self.reset_region();
        self.invalidate();
        if alternate_active {
            self.alternate
                .migrate_without_reflow(capture, cursor_line, columns, screen_lines);
        } else {
            self.alternate = ScreenTable::new(columns);
            self.pending_primary = None;
            self.primary
                .migrate_with_reflow(capture, grid, columns, screen_lines);
        }
    }
}

/// Forwards the parsed actions to the `Term` and records what was written.
pub struct TerminalHandler<'a, T: EventListener> {
    term: &'a mut Term<T>,
    written: &'a mut WrittenExtents,
}

impl<'a, T: EventListener> TerminalHandler<'a, T> {
    pub fn new(term: &'a mut Term<T>, written: &'a mut WrittenExtents) -> Self {
        Self { term, written }
    }

    /// Whether `1049` really switches screens: a repeated `1049h` on the
    /// alternate screen resets nothing.
    fn set_alternate(mode: PrivateMode, set: bool, was_alt: bool) -> bool {
        set && !was_alt
            && matches!(
                mode,
                PrivateMode::Named(NamedPrivateMode::SwapScreenAndSetRestoreCursor)
            )
    }

    fn columns(&self) -> usize {
        self.term.grid().columns()
    }

    fn screen_lines(&self) -> i32 {
        self.term.screen_lines() as i32
    }

    fn alternate(&self) -> bool {
        self.term.mode().contains(TermMode::ALT_SCREEN)
    }

    fn table(&mut self) -> &mut ScreenTable {
        let alternate = self.alternate();
        self.written.table_mut(alternate)
    }

    /// Mirror of `Term::scroll_up_relative` on the active screen.
    fn scroll_up_relative(&mut self, origin: i32, lines: usize) {
        let (region_start, region_end) = self.written.region();
        let screen_lines = self.screen_lines();
        if self
            .table()
            .scroll_up_relative(origin, lines, region_start, region_end, screen_lines)
        {
            // The grid reset the whole region instead of moving anything, so
            // the cleared revisions are free for reuse by new rows.
            self.written.invalidate();
        }
    }

    /// Mirror of `Term::scroll_down_relative` on the active screen.
    fn scroll_down_relative(&mut self, origin: i32, lines: usize) {
        let (region_start, region_end) = self.written.region();
        if self
            .table()
            .scroll_down_relative(origin, lines, region_start, region_end)
        {
            self.written.invalidate();
        }
    }

    /// Mirror of `Term::input`, in the reference's own order: the zero-width
    /// attach, `wrapline`, the `IRM` shift, then the leading spacer of a wide
    /// glyph that does not fit and the head write itself.
    fn observe_input(&mut self, c: char) {
        // `Term::input` drops characters that have no Unicode width.
        let Some(width) = c.width() else {
            return;
        };
        let columns = self.columns();
        let point = self.term.grid().cursor.point;
        if width == 0 {
            // A mark attaches to the cell before the cursor — or to the parked
            // cell — and never wraps, shifts or scrolls.
            let mut column = point.column.0;
            if !self.term.grid().cursor.input_needs_wrap {
                column = column.saturating_sub(1);
            }
            if self.term.grid()[point.line][Column(column)]
                .flags
                .contains(Flags::WIDE_CHAR_SPACER)
            {
                column = column.saturating_sub(1);
            }
            self.term.input(c);
            self.table().mark(point.line.0, column);
            return;
        }
        let region = self.written.region();
        let wrap_mode = self.term.mode().contains(TermMode::LINE_WRAP);
        let insert = self.term.mode().contains(TermMode::INSERT);
        // `wrapline` moves to the next row before anything is written. It
        // scrolls only when the next row is the region end, and below the
        // region it moves nowhere (`Term::linefeed`).
        let mut line = point.line.0;
        let mut column = point.column.0;
        let mut fresh_row = false;
        if self.term.grid().cursor.input_needs_wrap && wrap_mode {
            column = 0;
            if line + 1 == region.1 {
                self.scroll_up_relative(region.0, 1);
                line = region.1 - 1;
                fresh_row = true;
            } else if line + 1 < self.screen_lines() {
                line += 1;
            }
        }
        // `IRM` rotates the row before the write lands, and it decides on the
        // column the write starts at: a wide glyph with no room left shifts
        // nothing, it takes its leading spacer and wraps.
        let shift = insert && column + width < columns;
        if shift {
            self.table().rotate(line, column, width);
        }
        // A wide glyph with no room left in the row leaves its leading spacer
        // on the last column and wraps — or writes nothing with `DECAWM` off.
        if width == 2 && column + 1 >= columns {
            if !wrap_mode {
                self.term.input(c);
                return;
            }
            if !fresh_row {
                let spacer = self.term.grid()[Line(line)][Column(columns - 1)].flags;
                // `write_at_cursor` for the placeholder clears the head of a
                // wide glyph it overwrites.
                if spacer.intersects(Flags::WIDE_CHAR | Flags::WIDE_CHAR_SPACER) {
                    self.table().erase(line, columns - 2, columns - 1);
                }
            }
            // The placeholder is not content: the row keeps no cell for it,
            // but it was observed, so a copy trims it as empty.
            self.table().observe(line);
            self.table().erase(line, columns - 1, columns);
            if line + 1 == region.1 {
                self.scroll_up_relative(region.0, 1);
                line = region.1 - 1;
                fresh_row = true;
            } else if line + 1 < self.screen_lines() {
                line += 1;
            }
            column = 0;
        }
        // `write_at_cursor` looks at the cell `IRM` moved under the cursor, not
        // at the one that was there before the shift, and a row `wrapline` just
        // scrolled in carries none of the old row's flags.
        let source = |target: usize| {
            if shift {
                Self::shifted_source(column, width, columns, target)
            } else {
                target
            }
        };
        let read = |target: usize| {
            if fresh_row {
                Flags::empty()
            } else {
                self.term.grid()[Line(line)][Column(source(target))].flags
            }
        };
        let head_flags = read(column);
        let mut next_flags = if width == 2 && column + 1 < columns {
            read(column + 1)
        } else {
            Flags::empty()
        };
        // The head write, then its spacer: each clears the wide glyph it
        // overwrites, and the head write can already strip the spacer flag
        // from the cell the spacer write is about to take.
        if head_flags.intersects(Flags::WIDE_CHAR | Flags::WIDE_CHAR_SPACER) {
            if head_flags.contains(Flags::WIDE_CHAR) && column + 1 < columns {
                next_flags.remove(Flags::WIDE_CHAR_SPACER);
            } else if column > 0 {
                self.table().erase(line, column - 1, column);
            }
        }
        self.table().mark(line, column);
        if width == 2 {
            // The spacer stores no content. Retire any occupancy of the cell
            // it overwrites before a later head overwrite strips its flag.
            self.table().erase(line, column + 1, column + 2);
        }
        if next_flags.intersects(Flags::WIDE_CHAR | Flags::WIDE_CHAR_SPACER)
            && !(next_flags.contains(Flags::WIDE_CHAR) && column + 2 < columns)
        {
            // `clear_wide` on the cell the head write just took.
            self.table().erase(line, column, column + 1);
        }
        self.term.input(c);
    }

    /// Source column of the cell `Term::input`'s `IRM` loop moves into
    /// `target`: the same swaps, applied in reverse.
    fn shifted_source(at: usize, by: usize, columns: usize, target: usize) -> usize {
        let mut source = target;
        for column in at..columns.saturating_sub(by) {
            if source == column {
                source = column + by;
            } else if source == column + by {
                source = column;
            }
        }
        source
    }

    fn observe_linefeed(&mut self, newline: bool) {
        let region = self.written.region();
        let next = self.term.grid().cursor.point.line.0 + 1;
        if newline {
            self.term.newline();
        } else {
            self.term.linefeed();
        }
        if next == region.1 {
            self.scroll_up_relative(region.0, 1);
        }
    }

    fn observe_scroll_up(&mut self, lines: usize) {
        let region_start = self.written.region().0;
        self.term.scroll_up(lines);
        self.scroll_up_relative(region_start, lines);
    }

    fn observe_scroll_down(&mut self, lines: usize) {
        let region_start = self.written.region().0;
        self.term.scroll_down(lines);
        self.scroll_down_relative(region_start, lines);
    }

    fn observe_reverse_index(&mut self) {
        let origin = self.term.grid().cursor.point.line.0;
        let region = self.written.region();
        self.term.reverse_index();
        // Only a reverse index at the region top moves content.
        if origin == region.0 {
            self.scroll_down_relative(region.0, 1);
        }
    }

    fn observe_insert_blank_lines(&mut self, lines: usize) {
        let origin = self.term.grid().cursor.point.line.0;
        let region = self.written.region();
        self.term.insert_blank_lines(lines);
        if region.0 <= origin && origin < region.1 {
            self.scroll_down_relative(origin, lines);
        }
    }

    fn observe_delete_lines(&mut self, lines: usize) {
        let origin = self.term.grid().cursor.point.line.0;
        let region = self.written.region();
        let screen_lines = self.screen_lines();
        self.term.delete_lines(lines);
        let count = lines.min((screen_lines - origin).max(0) as usize);
        if count > 0 && region.0 <= origin && origin < region.1 {
            self.scroll_up_relative(origin, count);
        }
    }

    fn observe_insert_blank(&mut self, count: usize) {
        let point = self.term.grid().cursor.point;
        let columns = self.columns();
        self.term.insert_blank(count);
        let count = count.min(columns.saturating_sub(point.column.0));
        self.table()
            .shift_right(point.line.0, point.column.0, count);
    }

    fn observe_delete_chars(&mut self, count: usize) {
        let point = self.term.grid().cursor.point;
        let columns = self.columns();
        self.term.delete_chars(count);
        let count = count.min(columns);
        let end = (point.column.0 + count).min(columns - 1);
        self.table()
            .shift_left(point.line.0, point.column.0, end - point.column.0);
        self.table().erase(point.line.0, columns - count, columns);
    }

    fn observe_erase_chars(&mut self, count: usize) {
        let point = self.term.grid().cursor.point;
        let columns = self.columns();
        self.term.erase_chars(count);
        let end = (point.column.0 + count).min(columns);
        self.table().erase(point.line.0, point.column.0, end);
    }

    fn observe_clear_line(&mut self, mode: LineClearMode) {
        let point = self.term.grid().cursor.point;
        let parked = self.term.grid().cursor.input_needs_wrap;
        let columns = self.columns();
        let all = matches!(mode, LineClearMode::All);
        let left = matches!(mode, LineClearMode::Left);
        self.term.clear_line(mode);
        // `EL` to the right is a no-op while the cursor is parked.
        if all {
            self.table().erase(point.line.0, 0, columns);
        } else if left {
            self.table().erase(point.line.0, 0, point.column.0 + 1);
        } else if !parked {
            self.table().erase(point.line.0, point.column.0, columns);
        }
    }

    fn observe_clear_screen(&mut self, mode: ClearMode) {
        let point = self.term.grid().cursor.point;
        let screen_lines = self.screen_lines();
        let alternate = self.alternate();
        let columns = self.columns();
        // ED2 on the primary screen scrolls the viewport into history; count
        // the lines it will move before the grid is cleared.
        let scrolled_lines = if matches!(mode, ClearMode::All) && !alternate {
            let grid = self.term.grid();
            (0..screen_lines)
                .rev()
                .find(|line| {
                    (0..columns).any(|column| !grid[Line(*line)][Column(column)].is_empty())
                })
                .map_or(0, |line| line + 1)
        } else {
            0
        };
        let all = matches!(mode, ClearMode::All);
        let above = matches!(mode, ClearMode::Above);
        let below = matches!(mode, ClearMode::Below);
        self.term.clear_screen(mode);
        // Whatever the mode, whole rows are reset and their revisions become
        // reusable by new rows.
        self.written.invalidate();
        if all && alternate {
            self.table().clear_screen_rows(0, screen_lines);
        } else if all {
            let table = self.table();
            table.scrolled += i64::from(scrolled_lines);
            table.clear_screen_rows(0, screen_lines);
        } else if above {
            if point.line.0 > 1 {
                self.table().clear_screen_rows(0, point.line.0);
            }
            let end = (point.column.0 + 1).min(columns);
            self.table().erase(point.line.0, 0, end);
        } else if below {
            self.table().erase(point.line.0, point.column.0, columns);
            if point.line.0 as usize + 1 < screen_lines as usize {
                self.table()
                    .clear_screen_rows(point.line.0 + 1, screen_lines);
            }
        }
    }

    fn observe_decaln(&mut self) {
        self.term.decaln();
        let screen_lines = self.screen_lines();
        let table = self.table();
        for line in 0..screen_lines {
            table.fill(line);
        }
        self.written.invalidate();
    }

    fn observe_private_mode(&mut self, mode: PrivateMode) {
        let was_alt = self.alternate();
        // 1049 captures the primary screen, so it must run before the swap.
        let entering = Self::set_alternate(mode, true, was_alt).then(|| {
            let columns = self.columns();
            let screen_lines = self.screen_lines();
            let capture = self.written.capture(self.term.grid(), false);
            (capture, columns, screen_lines)
        });
        self.term.set_private_mode(mode);
        self.after_private_mode(mode, true, was_alt, entering);
    }

    fn observe_unset_private_mode(&mut self, mode: PrivateMode) {
        let was_alt = self.alternate();
        self.term.unset_private_mode(mode);
        self.after_private_mode(mode, false, was_alt, None);
    }

    fn after_private_mode(
        &mut self,
        mode: PrivateMode,
        set: bool,
        was_alt: bool,
        entering: Option<(WrittenCapture, usize, i32)>,
    ) {
        if matches!(mode, PrivateMode::Named(NamedPrivateMode::ColumnMode)) {
            // DECCOLM resets the margins and clears the screen.
            self.written.reset_region();
            let alternate = self.alternate();
            self.written.clear_active_screen_rows(alternate);
        }
        if matches!(
            mode,
            PrivateMode::Named(NamedPrivateMode::SwapScreenAndSetRestoreCursor)
        ) {
            // The swap only happens on a real transition: re-sending 1049h
            // while already on the alternate screen resets nothing.
            if let Some((capture, columns, screen_lines)) = entering {
                self.written.enter_alternate(capture, columns, screen_lines);
            } else if !set && was_alt {
                let columns = self.columns();
                let screen_lines = self.screen_lines();
                let grid = self.term.grid();
                self.written.leave_alternate(grid, columns, screen_lines);
            }
        }
    }

    fn observe_reset_state(&mut self) {
        self.term.reset_state();
        self.written.reset_all();
    }
}

impl<T: EventListener> alacritty_terminal::vte::ansi::Handler for TerminalHandler<'_, T> {
    fn input(&mut self, c: char) {
        self.observe_input(c);
    }

    fn linefeed(&mut self) {
        self.observe_linefeed(false);
    }

    fn scroll_up(&mut self, lines: usize) {
        self.observe_scroll_up(lines);
    }

    fn scroll_down(&mut self, lines: usize) {
        self.observe_scroll_down(lines);
    }

    fn reverse_index(&mut self) {
        self.observe_reverse_index();
    }

    fn insert_blank_lines(&mut self, lines: usize) {
        self.observe_insert_blank_lines(lines);
    }

    fn delete_lines(&mut self, lines: usize) {
        self.observe_delete_lines(lines);
    }

    fn insert_blank(&mut self, count: usize) {
        self.observe_insert_blank(count);
    }

    fn delete_chars(&mut self, count: usize) {
        self.observe_delete_chars(count);
    }

    fn erase_chars(&mut self, count: usize) {
        self.observe_erase_chars(count);
    }

    fn clear_line(&mut self, mode: LineClearMode) {
        self.observe_clear_line(mode);
    }

    fn clear_screen(&mut self, mode: ClearMode) {
        self.observe_clear_screen(mode);
    }

    fn reset_state(&mut self) {
        self.observe_reset_state();
    }

    fn set_title(&mut self, a0: Option<String>) {
        self.term.set_title(a0);
    }

    fn set_cursor_style(&mut self, a0: Option<CursorStyle>) {
        self.term.set_cursor_style(a0);
    }

    fn set_cursor_shape(&mut self, shape: CursorShape) {
        self.term.set_cursor_shape(shape);
    }

    fn goto(&mut self, line: i32, col: usize) {
        self.term.goto(line, col);
    }

    fn goto_line(&mut self, line: i32) {
        self.term.goto_line(line);
    }

    fn goto_col(&mut self, col: usize) {
        self.term.goto_col(col);
    }

    fn move_up(&mut self, a0: usize) {
        self.term.move_up(a0);
    }

    fn move_down(&mut self, a0: usize) {
        self.term.move_down(a0);
    }

    fn identify_terminal(&mut self, intermediate: Option<char>) {
        self.term.identify_terminal(intermediate);
    }

    fn device_status(&mut self, a0: usize) {
        self.term.device_status(a0);
    }

    fn move_forward(&mut self, col: usize) {
        self.term.move_forward(col);
    }

    fn move_backward(&mut self, col: usize) {
        self.term.move_backward(col);
    }

    fn move_down_and_cr(&mut self, row: usize) {
        self.term.move_down_and_cr(row);
    }

    fn move_up_and_cr(&mut self, row: usize) {
        self.term.move_up_and_cr(row);
    }

    fn put_tab(&mut self, count: u16) {
        let before = self.term.grid().cursor.clone();
        let region = self.written.region();
        let wrap_mode = self.term.mode().contains(TermMode::LINE_WRAP);
        self.term.put_tab(count);
        // A tab while the cursor is parked wraps first, which can scroll at
        // the region end; with `DECAWM` off `wrapline` returns and the cursor
        // stays put.
        if before.input_needs_wrap
            && wrap_mode
            && before.point.line.0 + 1 == region.1
            && self.term.grid().cursor.point.column != before.point.column
        {
            self.scroll_up_relative(region.0, 1);
        }
    }

    fn backspace(&mut self) {
        self.term.backspace();
    }

    fn carriage_return(&mut self) {
        self.term.carriage_return();
    }

    fn bell(&mut self) {
        self.term.bell();
    }

    fn substitute(&mut self) {
        self.term.substitute();
    }

    fn newline(&mut self) {
        self.observe_linefeed(true);
    }

    fn set_horizontal_tabstop(&mut self) {
        self.term.set_horizontal_tabstop();
    }

    fn move_backward_tabs(&mut self, count: u16) {
        self.term.move_backward_tabs(count);
    }

    fn move_forward_tabs(&mut self, count: u16) {
        self.term.move_forward_tabs(count);
    }

    fn save_cursor_position(&mut self) {
        self.term.save_cursor_position();
    }

    fn restore_cursor_position(&mut self) {
        self.term.restore_cursor_position();
    }

    fn clear_tabs(&mut self, mode: TabulationClearMode) {
        self.term.clear_tabs(mode);
    }

    fn set_tabs(&mut self, interval: u16) {
        self.term.set_tabs(interval);
    }

    fn terminal_attribute(&mut self, attr: Attr) {
        self.term.terminal_attribute(attr);
    }

    fn set_mode(&mut self, mode: Mode) {
        self.term.set_mode(mode);
    }

    fn unset_mode(&mut self, mode: Mode) {
        self.term.unset_mode(mode);
    }

    fn report_mode(&mut self, mode: Mode) {
        self.term.report_mode(mode);
    }

    fn set_private_mode(&mut self, mode: PrivateMode) {
        self.observe_private_mode(mode);
    }

    fn unset_private_mode(&mut self, mode: PrivateMode) {
        self.observe_unset_private_mode(mode);
    }

    fn report_private_mode(&mut self, mode: PrivateMode) {
        self.term.report_private_mode(mode);
    }

    fn set_scrolling_region(&mut self, top: usize, bottom: Option<usize>) {
        self.term.set_scrolling_region(top, bottom);
        self.written
            .set_scrolling_region(top, bottom, self.screen_lines());
    }

    fn set_keypad_application_mode(&mut self) {
        self.term.set_keypad_application_mode();
    }

    fn unset_keypad_application_mode(&mut self) {
        self.term.unset_keypad_application_mode();
    }

    fn set_active_charset(&mut self, a0: CharsetIndex) {
        self.term.set_active_charset(a0);
        self.written.set_active_charset(a0);
    }

    fn configure_charset(&mut self, a0: CharsetIndex, a1: StandardCharset) {
        self.term.configure_charset(a0, a1);
        self.written.configure_charset(a0, a1);
    }

    fn set_color(&mut self, a0: usize, a1: Rgb) {
        self.term.set_color(a0, a1);
    }

    fn dynamic_color_sequence(&mut self, a0: String, a1: usize, a2: &str) {
        self.term.dynamic_color_sequence(a0, a1, a2);
    }

    fn reset_color(&mut self, a0: usize) {
        self.term.reset_color(a0);
    }

    fn clipboard_store(&mut self, a0: u8, a1: &[u8]) {
        self.term.clipboard_store(a0, a1);
    }

    fn clipboard_load(&mut self, a0: u8, a1: &str) {
        self.term.clipboard_load(a0, a1);
    }

    fn decaln(&mut self) {
        self.observe_decaln();
    }

    fn push_title(&mut self) {
        self.term.push_title();
    }

    fn pop_title(&mut self) {
        self.term.pop_title();
    }

    fn text_area_size_pixels(&mut self) {
        self.term.text_area_size_pixels();
    }

    fn text_area_size_chars(&mut self) {
        self.term.text_area_size_chars();
    }

    fn set_hyperlink(&mut self, a0: Option<Hyperlink>) {
        self.term.set_hyperlink(a0);
    }

    fn set_mouse_cursor_icon(&mut self, a0: CursorIcon) {
        self.term.set_mouse_cursor_icon(a0);
    }

    fn report_keyboard_mode(&mut self) {
        self.term.report_keyboard_mode();
    }

    fn push_keyboard_mode(&mut self, mode: KeyboardModes) {
        self.term.push_keyboard_mode(mode);
    }

    fn pop_keyboard_modes(&mut self, to_pop: u16) {
        self.term.pop_keyboard_modes(to_pop);
    }

    fn set_keyboard_mode(&mut self, mode: KeyboardModes, behavior: KeyboardModesApplyBehavior) {
        self.term.set_keyboard_mode(mode, behavior);
    }

    fn set_modify_other_keys(&mut self, mode: ModifyOtherKeys) {
        self.term.set_modify_other_keys(mode);
    }

    fn report_modify_other_keys(&mut self) {
        self.term.report_modify_other_keys();
    }

    fn set_scp(&mut self, char_path: ScpCharPath, update_mode: ScpUpdateMode) {
        self.term.set_scp(char_path, update_mode);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn new_table() -> ScreenTable {
        ScreenTable::new(6)
    }

    fn marked(mut table: ScreenTable, line: i32, columns: std::ops::Range<usize>) -> ScreenTable {
        for column in columns {
            table.mark(line, column);
        }
        table
    }

    fn written(table: &ScreenTable, line: i32) -> Vec<usize> {
        let Some(row) = table.row(line) else {
            return Vec::new();
        };
        (0..table.columns)
            .filter(|column| row.written(*column))
            .collect()
    }

    #[test]
    fn writes_are_keyed_by_a_stable_revision() {
        let mut table = new_table();
        table = marked(table, 0, 0..3);
        table = marked(table, 2, 0..5);
        table.scroll_up_relative(0, 2, 0, 4, 4);
        assert_eq!(
            written(&table, 0),
            (0..5).collect::<Vec<_>>(),
            "row 2 moved up"
        );
        assert!(table.row(2).is_none(), "row 0 left the screen");
        assert_eq!(
            written(&table, -2),
            vec![0, 1, 2],
            "row 0 is now in history"
        );
    }

    #[test]
    fn erasing_removes_exactly_the_covered_cells() {
        let mut table = marked(new_table(), 0, 0..6);
        table.erase(0, 0, 1);
        assert_eq!(
            written(&table, 0),
            vec![1, 2, 3, 4, 5],
            "erase before the tail"
        );
        table.erase(0, 3, 6);
        assert_eq!(written(&table, 0), vec![1, 2], "erase covered the tail");
        table.erase(0, 0, 6);
        assert!(written(&table, 0).is_empty(), "whole row erased");
    }

    #[test]
    fn deleting_and_inserting_characters_move_written_cells() {
        let mut table = marked(new_table(), 0, 0..6);
        table.shift_left(0, 1, 2);
        assert_eq!(written(&table, 0), vec![0, 1, 2, 3]);
        table.shift_right(0, 1, 2);
        assert_eq!(written(&table, 0), vec![0, 3, 4, 5]);
        table.shift_right(0, 5, 2);
        assert_eq!(
            written(&table, 0),
            vec![0, 3, 4],
            "content pushed out is lost"
        );
    }

    #[test]
    fn partial_region_scrolls_move_only_region_rows() {
        let mut table = new_table();
        table = marked(table, 0, 0..4);
        table = marked(table, 1, 0..1);
        table = marked(table, 2, 0..2);
        table.scroll_down_relative(1, 1, 0, 4);
        assert_eq!(
            written(&table, 0),
            vec![0, 1, 2, 3],
            "rows above the origin stay"
        );
        assert!(table.row(1).is_none(), "the vacated row is cleared");
        assert_eq!(written(&table, 2), vec![0], "content moved down");
        assert_eq!(written(&table, 3), vec![0, 1], "content moved down");
    }

    #[test]
    fn scrolling_a_whole_region_from_inside_resets_it() {
        let mut table = new_table();
        table = marked(table, 1, 0..1);
        table = marked(table, 2, 0..2);
        // Region [1, 3): a scroll of the full region size resets every row.
        table.scroll_up_relative(1, 2, 1, 3, 3);
        assert!(table.row(1).is_none());
        assert!(table.row(2).is_none());
        table = marked(table, 1, 0..1);
        table.scroll_down_relative(1, 2, 1, 3);
        assert!(table.row(1).is_none());
    }

    #[test]
    fn a_full_screen_scroll_rebases_history_at_the_cap() {
        let mut table = new_table();
        table = marked(table, 0, 0..1);
        table = marked(table, 1, 0..2);
        table.scroll_up_relative(0, 1, 0, 2, 2);
        assert_eq!(written(&table, 0), vec![0, 1], "row 1 moved up");
        assert_eq!(written(&table, -1), vec![0], "row 0 moved into history");
        assert!(table.row(1).is_none(), "the bottom row was cleared");
        for _ in 0..50 {
            table.mark(1, 5);
            table.scroll_up_relative(0, 1, 0, 2, 2);
        }
        table.prune(1);
        assert!(table.rows.len() <= 3, "dead rows are evicted at the cap");
    }

    #[test]
    fn pruning_keeps_the_rows_inside_the_scrollback() {
        let mut table = new_table();
        for line in 0..100 {
            table.mark(line, 0);
        }
        table.prune(10 << 20);
        assert_eq!(table.rows.len(), 100, "small history keeps everything");
        let mut table = new_table();
        table.scrolled = 50;
        for line in -12..4 {
            table.mark(line, 0);
        }
        table.prune(10);
        assert_eq!(
            table.rows.len(),
            14,
            "only the rows beyond the cap are dropped"
        );
        assert!(table.row(-10).is_some(), "rows inside the scrollback stay");
        assert!(
            table.row(-12).is_none(),
            "rows outside the window are dropped"
        );
    }
}
