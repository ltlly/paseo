//! Selection copy that mirrors `SelectionService.selectionText`.
//!
//! The reference builds the text row by row: the first row starts at the
//! selection's start column, middle rows start at column 0, and the last row
//! ends at the selection's end column. A row that continues the previous one
//! (`BufferLine.isWrapped`) is appended to it instead of starting a new line.
//! Column mode keeps every row inside `[start, end)` and never joins rows.
//!
//! Every row end is clamped by the columns the program wrote
//! (`BufferLine.getTrimmedLength`), which is what keeps explicitly printed
//! trailing spaces and drops never-written cells.

use crate::terminal::TerminalBuffer;

/// Copy `[start, end]` (inclusive grid cells) the way the reference does.
/// `newline_terminated` mirrors the reference's excluded endpoint row: a
/// selection whose anchors end left of column 0 on a later row copies a
/// trailing newline, unless that row is wrapped, in which case its empty
/// fragment joins the previous line instead (`SelectionService.selectionText`
/// checks `isWrapped` on the final row).
pub fn text(
    buffer: &TerminalBuffer,
    start_line: i32,
    start_column: usize,
    end_line: i32,
    end_column: usize,
    block: bool,
    newline_terminated: bool,
) -> String {
    let mut lines: Vec<String> = Vec::new();
    if block && start_column > end_column {
        return String::new();
    }
    for line in start_line..=end_line {
        let from = if block || line == start_line {
            start_column
        } else {
            0
        };
        let requested = if block || line == end_line {
            end_column.saturating_add(1)
        } else {
            usize::MAX
        };
        let written = buffer
            .line_written_trimmed(line)
            .unwrap_or_else(|| buffer.line_trimmed_length(line));
        let to = requested.min(written);

        let mut row = String::new();
        if to > from {
            buffer.push_row_text(line, from, to, &mut row);
        }

        if !block && line > start_line && buffer.line_continues(line - 1) {
            if let Some(previous) = lines.last_mut() {
                previous.push_str(&row);
                continue;
            }
        }
        lines.push(row);
    }
    if newline_terminated && !block && !buffer.line_continues(end_line) {
        lines.push(String::new());
    }
    lines.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::terminal::GridSize;

    fn terminal(rows: u16, cols: u16, text: &str) -> TerminalBuffer {
        let mut buffer = TerminalBuffer::new(GridSize { rows, cols }, 100);
        buffer.feed(text.as_bytes());
        buffer
    }

    #[test]
    fn printed_spaces_before_a_wrap_survive_the_copy() {
        let buffer = terminal(4, 6, "abc   def\r\n");
        // Rows 0..1, first row unbounded, second row ends at its last cell.
        assert_eq!(text(&buffer, 0, 0, 0, 5, false, false), "abc   ");
        assert_eq!(text(&buffer, 0, 0, 1, 3, false, false), "abc   def");
    }

    #[test]
    fn unwritten_cells_are_dropped_but_printed_ones_are_not() {
        let buffer = terminal(4, 6, "abc  \r\ndef\r\n");
        assert_eq!(text(&buffer, 0, 0, 0, 5, false, false), "abc  ");
        assert_eq!(text(&buffer, 0, 0, 1, 2, false, false), "abc  \ndef");
        // An erased tail is gone again.
        let buffer = terminal(4, 6, "abc  \r\x1b[1A\x1b[K");
        assert_eq!(
            text(&buffer, 0, 0, 0, 5, false, false),
            "",
            "row fully erased"
        );
    }

    #[test]
    fn middle_rows_are_trimmed_and_column_mode_never_joins() {
        let buffer = terminal(4, 8, "abcdefghijklmnop");
        // Four rows of eight columns: rows 1 and 2 are middle rows.
        assert_eq!(
            text(&buffer, 0, 0, 2, 3, false, false),
            "abcdefghijklmnop\n"
        );
        assert_eq!(text(&buffer, 0, 1, 2, 2, true, false), "bc\njk\n");
    }

    /// Reviewer sequences, with expected strings produced by the installed
    /// Electron `@xterm/xterm` 6.1.0-beta.292 source (see the `source` field in
    /// `testdata/terminal-copy.json`). Each case first failed against the
    /// max-column implementation.
    #[test]
    fn wrap_at_the_scrollback_bottom_keeps_row_identity() {
        let mut buffer = TerminalBuffer::new(GridSize { rows: 2, cols: 6 }, 1);
        buffer.feed(b"a\r\nbcdefgh");
        assert_eq!(text(&buffer, 0, 0, 0, 5, false, false), "bcdefg");
        assert_eq!(text(&buffer, 1, 0, 1, 5, false, false), "h");
    }

    #[test]
    fn partial_scroll_region_moves_only_the_region() {
        let mut buffer = TerminalBuffer::new(GridSize { rows: 4, cols: 6 }, 10);
        buffer.feed(b"a\x1b[2;1Hbb\x1b[3;1Hccc\x1b[4;1Hdddd\x1b[2;3r\x1b[3;1H\n");
        assert_eq!(text(&buffer, 1, 0, 1, 5, false, false), "ccc");
        assert_eq!(text(&buffer, 2, 0, 2, 5, false, false), "");
    }

    #[test]
    fn full_history_with_a_partial_scroll_region_still_tracks_scrolls() {
        let mut buffer = TerminalBuffer::new(GridSize { rows: 3, cols: 6 }, 1);
        buffer.feed(b"a \r\nbb  \r\nc\r\ndddd\x1b[1;2r\x1b[2;1H\n");
        assert_eq!(text(&buffer, -1, 0, -1, 5, false, false), "bb  ");
        assert_eq!(text(&buffer, 0, 0, 0, 5, false, false), "c");
        assert_eq!(text(&buffer, 1, 0, 1, 5, false, false), "");
    }

    #[test]
    fn reverse_index_keeps_history_identity() {
        let mut buffer = TerminalBuffer::new(GridSize { rows: 2, cols: 6 }, 1);
        buffer.feed(b"a \r\nbb  \r\nc\x1b[1;1H\x1bM");
        assert_eq!(text(&buffer, -1, 0, -1, 5, false, false), "a ");
        assert_eq!(text(&buffer, 0, 0, 0, 5, false, false), "");
        assert_eq!(text(&buffer, 1, 0, 1, 5, false, false), "bb  ");
    }

    #[test]
    fn alternate_screen_writes_are_recorded_separately() {
        let mut buffer = TerminalBuffer::new(GridSize { rows: 2, cols: 8 }, 10);
        buffer.feed(b"x\x1b[?1049h\x1b[Habcdef");
        assert_eq!(text(&buffer, 0, 0, 0, 7, false, false), "abcdef");
    }

    #[test]
    fn alternate_screen_erases_do_not_touch_the_primary_screen() {
        let mut buffer = TerminalBuffer::new(GridSize { rows: 2, cols: 8 }, 10);
        buffer.feed(b"x  \x1b[?1049h\x1b[H\x1b[2K\x1b[?1049l");
        assert_eq!(text(&buffer, 0, 0, 0, 7, false, false), "x  ");
    }

    #[test]
    fn erasing_the_last_written_cell_drops_unwritten_neighbors() {
        let mut buffer = TerminalBuffer::new(GridSize { rows: 2, cols: 6 }, 10);
        buffer.feed(b"a\x1b[1;6Hb\x1b[1;6H\x1b[K");
        assert_eq!(text(&buffer, 0, 0, 0, 5, false, false), "a");
    }

    #[test]
    fn insert_blank_pushing_content_out_empties_the_row() {
        let mut buffer = TerminalBuffer::new(GridSize { rows: 2, cols: 6 }, 10);
        buffer.feed(b"abc\r\x1b[6@");
        assert_eq!(text(&buffer, 0, 0, 0, 5, false, false), "");
    }

    #[test]
    fn resize_migrates_written_cells_and_later_writes_extend_them() {
        let mut buffer = TerminalBuffer::new(GridSize { rows: 2, cols: 8 }, 5);
        buffer.feed(b"abcdef  ");
        buffer.resize(GridSize { rows: 2, cols: 9 });
        assert_eq!(text(&buffer, 0, 0, 0, 8, false, false), "abcdef  ");
        buffer.feed(b"\rX");
        assert_eq!(text(&buffer, 0, 0, 0, 8, false, false), "Xbcdef  ");
    }

    #[test]
    fn same_size_resize_keeps_written_cells() {
        let mut buffer = TerminalBuffer::new(GridSize { rows: 2, cols: 8 }, 5);
        buffer.feed(b"abcdef  ");
        buffer.resize(GridSize { rows: 2, cols: 8 });
        assert_eq!(text(&buffer, 0, 0, 0, 7, false, false), "abcdef  ");
    }

    #[test]
    fn insert_mode_shifts_written_cells_right() {
        let mut buffer = TerminalBuffer::new(GridSize { rows: 2, cols: 8 }, 10);
        buffer.feed(b"abcdef\r\x1b[4hX");
        assert_eq!(text(&buffer, 0, 0, 0, 7, false, false), "Xabcdef");
    }

    #[test]
    fn decaln_marks_every_screen_cell_written() {
        let mut buffer = TerminalBuffer::new(GridSize { rows: 2, cols: 6 }, 10);
        buffer.feed(b"a\x1b#8");
        assert_eq!(text(&buffer, 0, 0, 0, 5, false, false), "EEEEEE");
        assert_eq!(text(&buffer, 1, 0, 1, 5, false, false), "EEEEEE");
    }

    #[test]
    fn block_selection_starting_at_a_wide_spacer_copies_a_space() {
        let mut buffer = TerminalBuffer::new(GridSize { rows: 2, cols: 8 }, 10);
        buffer.feed("abcde\r\na中de".as_bytes());
        buffer.begin_selection(0, 2, true);
        buffer.extend_selection(1, 5);
        assert_eq!(buffer.selected_text().as_deref(), Some("cde\n de"));
        buffer.begin_selection(0, 1, true);
        buffer.extend_selection(1, 4);
        assert_eq!(
            buffer.selected_text().as_deref(),
            Some("bcd\n中d"),
            "from the wide head the spacer is skipped"
        );
    }

    #[test]
    fn selection_ending_left_of_the_next_row_copies_a_trailing_newline() {
        let mut buffer = TerminalBuffer::new(GridSize { rows: 2, cols: 6 }, 10);
        buffer.feed(b"abc\r\ndef");
        buffer.begin_selection(0, 0, false);
        buffer.extend_selection(1, 0);
        assert_eq!(buffer.selected_text().as_deref(), Some("abc\n"));
    }

    /// Reviewer round two, with expected strings derived from the actual
    /// `alacritty_terminal 0.26.0` grid algorithms and the installed Electron
    /// `@xterm/xterm` 6.1.0-beta.292 sources (hash-gated in
    /// `sync-native-ui.mjs`). Each case first failed against the previous
    /// implementation.
    #[test]
    fn top_region_scroll_rebases_the_fixed_rows_from_the_bottom() {
        let mut buffer = TerminalBuffer::new(GridSize { rows: 4, cols: 6 }, 10);
        buffer.feed(b"a\x1b[2;1Hbb\x1b[3;1Hccc\x1b[4;1Hdddd\x1b[1;2r\x1b[2;1H\n");
        assert_eq!(text(&buffer, 0, 0, 0, 5, false, false), "bb");
        // Rows below the region are fixed; ascending rebase overwrote them.
        assert_eq!(text(&buffer, 2, 0, 2, 5, false, false), "ccc");
        assert_eq!(text(&buffer, 3, 0, 3, 5, false, false), "dddd");
    }

    #[test]
    fn resize_streams_logical_lines_in_content_order() {
        // Shrinking columns: the wrapped head row keeps the occupancy of the
        // first cells of the logical line, the tail row the trailing ones.
        let mut buffer = TerminalBuffer::new(GridSize { rows: 2, cols: 8 }, 5);
        buffer.feed(b"abcdef");
        buffer.resize(GridSize { rows: 2, cols: 4 });
        assert_eq!(text(&buffer, -1, 0, -1, 3, false, false), "abcd");
        assert_eq!(text(&buffer, 0, 0, 0, 3, false, false), "ef");
        // Growing rows without scrollback adds the new rows at the bottom;
        // the existing row keeps its occupancy, the blank rows get none.
        let mut buffer = TerminalBuffer::new(GridSize { rows: 2, cols: 8 }, 5);
        buffer.feed(b"a  ");
        buffer.resize(GridSize { rows: 3, cols: 8 });
        assert_eq!(text(&buffer, 0, 0, 0, 7, false, false), "a  ");
        assert_eq!(text(&buffer, 1, 0, 2, 7, false, false), "\n");
    }

    #[test]
    fn writes_record_wide_heads_at_the_last_column_and_mapped_charset_cells() {
        // A wide head at the second-to-last column parks the cursor on its
        // spacer, so the head sits directly below the landed cursor.
        let mut buffer = TerminalBuffer::new(GridSize { rows: 2, cols: 6 }, 10);
        buffer.feed("ab  \u{4e2d}".as_bytes());
        assert_eq!(text(&buffer, 0, 0, 0, 5, false, false), "ab  \u{4e2d}");
        // DEC special graphics: the reference stores the mapped glyph.
        let mut buffer = TerminalBuffer::new(GridSize { rows: 2, cols: 6 }, 10);
        buffer.feed(b"x\x1b(0q\x1b(B");
        assert_eq!(text(&buffer, 0, 0, 0, 5, false, false), "x\u{2500}");
    }

    #[test]
    fn zero_width_and_disabled_wrap_never_scroll_and_bottom_wide_spill_does() {
        // A combining mark attaches to the parked 'g' and never wraps.
        let mut buffer = TerminalBuffer::new(GridSize { rows: 2, cols: 6 }, 10);
        buffer.feed("a \r\nbcdefg\u{301}".as_bytes());
        assert_eq!(text(&buffer, 0, 0, 0, 5, false, false), "a ");
        assert_eq!(text(&buffer, 1, 0, 1, 5, false, false), "bcdefg\u{301}");
        // With DECAWM off the parked write overwrites the last cell in place.
        let mut buffer = TerminalBuffer::new(GridSize { rows: 2, cols: 6 }, 10);
        buffer.feed(b"a \r\nbcdefg\x1b[?7lX");
        assert_eq!(text(&buffer, 0, 0, 0, 5, false, false), "a ");
        // A wide glyph over the last column of the bottom row wraps and
        // scrolls, leaving its head on the fresh bottom row.
        let mut buffer = TerminalBuffer::new(GridSize { rows: 2, cols: 6 }, 10);
        buffer.feed("a\r\nabcde\u{4e2d}".as_bytes());
        assert_eq!(text(&buffer, 0, 0, 0, 5, false, false), "abcde");
        assert_eq!(text(&buffer, 1, 0, 1, 5, false, false), "\u{4e2d}");
    }

    #[test]
    fn alt_resizes_keep_the_primary_table_and_repeated_1049h_keeps_the_alt_table() {
        let mut buffer = TerminalBuffer::new(GridSize { rows: 2, cols: 8 }, 5);
        buffer.feed(b"abcdef  \r\x1b[?1049h");
        buffer.resize(GridSize { rows: 2, cols: 9 });
        buffer.feed(b"\x1b[?1049lX");
        assert_eq!(text(&buffer, 0, 0, 0, 8, false, false), "Xbcdef  ");
        let mut buffer = TerminalBuffer::new(GridSize { rows: 2, cols: 8 }, 5);
        buffer.feed(b"\x1b[?1049h\x1b[Habcdef  \r\x1b[?1049hX");
        assert_eq!(text(&buffer, 0, 0, 0, 7, false, false), "Xbcdef  ");
    }

    #[test]
    fn excluded_endpoint_row_joins_soft_wrapped_lines_without_an_extra_newline() {
        let mut buffer = TerminalBuffer::new(GridSize { rows: 2, cols: 6 }, 10);
        buffer.feed(b"abcdefg");
        buffer.begin_selection(0, 0, false);
        buffer.extend_selection(1, 0);
        assert_eq!(buffer.selected_text().as_deref(), Some("abcdef"));
        // The same soft boundary reached by a reverse drag.
        buffer.clear_selection();
        buffer.begin_selection(1, 0, false);
        buffer.extend_selection(0, 0);
        assert_eq!(buffer.selected_text().as_deref(), Some("abcdef"));
        // A hard boundary keeps the trailing newline.
        let mut buffer = TerminalBuffer::new(GridSize { rows: 2, cols: 6 }, 10);
        buffer.feed(b"abc\r\ndef");
        buffer.begin_selection(0, 0, false);
        buffer.extend_selection(1, 0);
        assert_eq!(buffer.selected_text().as_deref(), Some("abc\n"));
    }

    #[test]
    fn history_cap_keeps_copies_and_the_table_bounded() {
        let mut buffer = TerminalBuffer::new(GridSize { rows: 2, cols: 6 }, 1);
        for row in 0..40 {
            buffer.feed(format!("row{row:02}\r\n").as_bytes());
        }
        assert_eq!(text(&buffer, -1, 0, -1, 5, false, false), "row38");
        assert_eq!(text(&buffer, 0, 0, 0, 5, false, false), "row39");
        assert!(
            buffer.written_rows() <= 3,
            "the occupancy table must stay bounded by history plus screen"
        );
    }

    /// Reviewer round three: capture and migration must describe the same
    /// reflow, and the charset mirror must outlive a feed. Each case failed
    /// against the previous implementation.
    #[test]
    fn reflow_drops_the_wrap_spacer_and_keeps_a_merged_group() {
        // The wide glyph wrapped, leaving a spacer on the old last column. The
        // reflow drops that spacer, so the glyph and `Z` share row 0.
        let mut buffer = TerminalBuffer::new(GridSize { rows: 2, cols: 6 }, 10);
        buffer.feed("abcde\u{4e2d}Z".as_bytes());
        buffer.resize(GridSize { rows: 2, cols: 8 });
        assert_eq!(text(&buffer, 0, 0, 0, 7, false, false), "abcde\u{4e2d}Z");

        // Merging the wrapped group above moves `ZZZZ` up one row, so its
        // occupancy has to follow the content instead of the old row key.
        let mut buffer = TerminalBuffer::new(GridSize { rows: 3, cols: 6 }, 10);
        buffer.feed(b"abcdefg\r\nZZZZ");
        buffer.resize(GridSize { rows: 3, cols: 8 });
        buffer.feed(b"\x1b[2;1HX");
        assert_eq!(text(&buffer, 1, 0, 1, 7, false, false), "XZZZ");
    }

    #[test]
    fn resize_capture_uses_configured_history_capacity_and_alt_roundtrips_do_not_restore_stale_masks()
     {
        let mut buffer = TerminalBuffer::new(GridSize { rows: 3, cols: 6 }, 100);
        buffer.feed(b"a  \r\nBBBBBB\r\nC\x1b[H");
        buffer.resize(GridSize { rows: 3, cols: 4 });
        assert_eq!(text(&buffer, -1, 0, -1, 3, false, false), "a  ");
        let mut buffer = terminal(3, 6, "A\r\nB\r\nC\x1b[H\x1b[?1049h");
        buffer.resize(GridSize { rows: 2, cols: 6 });
        buffer.resize(GridSize { rows: 3, cols: 6 });
        buffer.feed(b"\x1b[?1049l");
        assert_eq!(text(&buffer, 2, 0, 2, 5, false, false), "");
    }

    #[test]
    fn actual_resize_keeps_trimmed_overflow_and_cursor_dependent_height_cut() {
        let mut buffer = terminal(3, 6, "abc   \r\nBBBB\r\nC\x1b[H");
        buffer.resize(GridSize { rows: 3, cols: 4 });
        assert_eq!(text(&buffer, 0, 0, 0, 3, false, false), "abc ");
        let mut buffer = terminal(3, 6, "a\r\nBBBB\r\nC\x1b[H");
        buffer.resize(GridSize { rows: 2, cols: 6 });
        assert_eq!(text(&buffer, 1, 0, 1, 5, false, false), "BBBB");
    }

    #[test]
    fn restored_cursor_charset_slots_drive_copy_after_a_feed_boundary() {
        let mut buffer = terminal(2, 6, "x\x1b7\x1b(0");
        buffer.feed(b"\x1b8q");
        assert_eq!(text(&buffer, 0, 0, 0, 5, false, false), "xq");
    }

    #[test]
    fn charset_state_survives_a_feed_boundary() {
        let mut buffer = TerminalBuffer::new(GridSize { rows: 2, cols: 6 }, 10);
        buffer.feed(b"x\x1b(0");
        buffer.feed(b"q\x1b(B");
        assert_eq!(text(&buffer, 0, 0, 0, 5, false, false), "x\u{2500}");
    }

    #[test]
    fn the_primary_screen_is_captured_before_the_swap() {
        let mut buffer = TerminalBuffer::new(GridSize { rows: 2, cols: 6 }, 10);
        buffer.feed(b"abcdefgh\r\x1b[?1049h");
        buffer.resize(GridSize { rows: 2, cols: 8 });
        buffer.feed(b"\x1b[?1049l");
        assert_eq!(text(&buffer, 0, 0, 0, 7, false, false), "abcdefgh");
    }

    #[test]
    fn every_row_identity_change_takes_a_new_layout_epoch() {
        let mut buffer = TerminalBuffer::new(GridSize { rows: 2, cols: 6 }, 10);
        let created = buffer.layout_generation();
        buffer.resize(GridSize { rows: 2, cols: 8 });
        let resized = buffer.layout_generation();
        assert_ne!(created, resized);
        // A table built after an invalidation must not reuse its epoch.
        let other = TerminalBuffer::new(GridSize { rows: 2, cols: 6 }, 10);
        assert_ne!(resized, other.layout_generation());
        // A real screen swap rebuilds the visible rows too.
        let before_swap = buffer.layout_generation();
        buffer.feed(b"\x1b[?1049h");
        assert_ne!(before_swap, buffer.layout_generation());
        let on_alternate = buffer.layout_generation();
        buffer.feed(b"\x1b[?1049l");
        assert_ne!(on_alternate, buffer.layout_generation());
    }

    /// Reviewer round four: `Term::input` order. Each case failed against the
    /// previous implementation, which located the write from where the cursor
    /// landed instead of running the reference's own steps.
    #[test]
    fn an_empty_selection_keeps_its_anchor_across_control_output() {
        let mut buffer = terminal(3, 6, "abc\r\ndef");
        buffer.begin_selection(0, 0, false);
        buffer.feed(b"\x1b[31m");
        buffer.extend_selection(1, 0);
        assert_eq!(buffer.selected_text().as_deref(), Some("abc\n"));
    }

    #[test]
    fn cropped_authority_endpoints_do_not_invent_a_newline() {
        let mut buffer = terminal(4, 6, "a\r\nbb\r\nccc\r\ndddd");
        buffer.begin_selection(0, 0, false);
        buffer.extend_selection(2, 0);
        buffer.feed(b"\x1b[2;3r\x1b[T");
        assert_eq!(buffer.selected_text().as_deref(), Some("a\n\nbb"));
    }

    #[test]
    fn scrolled_gesture_anchors_preserve_excluded_newline_endpoint() {
        let mut buffer = terminal(3, 6, "a\r\nbb\r\nc");
        buffer.begin_word_selection(1, 0);
        buffer.feed(b"\r\n");
        buffer.extend_selection(1, 0);
        assert_eq!(buffer.selected_text().as_deref(), Some("bb\n"));
    }

    #[test]
    fn height_resize_moves_copy_anchors_with_the_authoritative_selection() {
        let mut buffer = terminal(3, 6, "a\r\nbb\r\nc");
        buffer.begin_word_selection(1, 0);
        buffer.resize(GridSize { rows: 2, cols: 6 });
        buffer.extend_selection(1, 0);
        assert_eq!(buffer.selected_text().as_deref(), Some("bb\n"));
    }

    #[test]
    fn inactive_primary_history_limit_prunes_the_occupancy_immediately() {
        let mut buffer = terminal(3, 6, "");
        for _ in 0..20 {
            buffer.feed(b"a  \r\n");
        }
        buffer.feed(b"\x1b[?1049h");
        buffer.set_history(1);
        assert!(buffer.written_rows() <= 4);
    }

    #[test]
    fn spacer_overwrites_retire_content_before_its_flag_is_removed() {
        let buffer = terminal(2, 6, "ab\r中\rX");
        assert_eq!(text(&buffer, 0, 0, 0, 5, false, false), "X");
    }

    #[test]
    fn spill_placeholder_overwrites_a_last_column_head_and_clears_the_previous_cell() {
        let buffer = terminal(2, 6, "\x1b[1;5H中\x1b[1;5H\x1b[@Z中");
        assert_eq!(text(&buffer, 0, 0, 0, 5, false, false), "");
    }

    #[test]
    fn a_mark_attaches_to_the_blank_cell_before_the_cursor() {
        // `Term::input` pushes the mark onto cell 4 and makes it content even
        // though the program never wrote that cell.
        let mut buffer = TerminalBuffer::new(GridSize { rows: 2, cols: 6 }, 10);
        buffer.feed("a\x1b[1;6H\u{301}".as_bytes());
        assert_eq!(text(&buffer, 0, 0, 0, 5, false, false), "a    \u{301}");
    }

    #[test]
    fn insert_mode_decides_the_wide_spill_before_it_shifts() {
        // `\u{4e2d}` does not fit next to column 5, so only its leading spacer
        // lands on row 0 and the head wraps to row 1. The insert shift applies
        // to the row that does not take the write, so `Z` stays put.
        let mut buffer = TerminalBuffer::new(GridSize { rows: 2, cols: 6 }, 10);
        buffer.feed("\x1b[2;5HZ\x1b[1;6H\x1b[4h\u{4e2d}".as_bytes());
        assert_eq!(text(&buffer, 0, 0, 0, 5, false, false), "");
        assert_eq!(text(&buffer, 1, 0, 1, 5, false, false), "\u{4e2d}  Z");
    }

    #[test]
    fn insert_mode_shifts_the_spacer_off_the_cursor_cell() {
        // The shift moves the spacer away from column 1 before `X` is written,
        // so the head at column 0 survives and `EL` leaves only it.
        let mut buffer = TerminalBuffer::new(GridSize { rows: 2, cols: 6 }, 10);
        buffer.feed("\u{4e2d}\x1b[1;2H\x1b[4hX\x1b[1;2H\x1b[K".as_bytes());
        assert_eq!(text(&buffer, 0, 0, 0, 5, false, false), "\u{4e2d}");
    }

    #[test]
    fn insert_mode_rotates_the_row_tail_into_the_written_cells() {
        // `IRM` swaps the tail into the cells the write overwrites, so the
        // parked `a` leaves the row instead of staying on the last column.
        let mut buffer = TerminalBuffer::new(GridSize { rows: 2, cols: 8 }, 10);
        buffer.feed(b"\x1b[1;8Ha\x1b[1;1H\x1b[4hX");
        assert_eq!(text(&buffer, 0, 0, 0, 7, false, false), "X");
    }

    #[test]
    fn a_parked_write_below_the_scroll_region_wraps_in_place() {
        // The region ends above the cursor, so `wrapline` and `linefeed` move
        // nowhere and the glyph overwrites the parked row's first cell.
        let mut buffer = TerminalBuffer::new(GridSize { rows: 4, cols: 6 }, 10);
        buffer.feed(b"\x1b[1;2r\x1b[4;2HbcdefX");
        assert_eq!(text(&buffer, 3, 0, 3, 5, false, false), "Xbcdef");
        assert_eq!(text(&buffer, 1, 0, 2, 5, false, false), "\n");
    }

    #[test]
    fn a_wrapped_wide_glyph_lands_on_the_fresh_row_the_scroll_created() {
        // The wrap scrolls, so the head takes the new blank row: the old row's
        // cells cannot clear it, and the moved occupancy follows the scroll.
        let mut buffer = TerminalBuffer::new(GridSize { rows: 3, cols: 6 }, 10);
        buffer.feed("\x1b[3;1H\u{4e2d}abcd\u{4e2d}".as_bytes());
        assert_eq!(text(&buffer, 0, 0, 0, 5, false, false), "");
        assert_eq!(text(&buffer, 1, 0, 1, 5, false, false), "\u{4e2d}abcd");
        assert_eq!(text(&buffer, 2, 0, 2, 5, false, false), "\u{4e2d}");
    }

    /// Expected strings produced by running `SelectionService.selectionText`
    /// from the installed Electron `@xterm/xterm` 6.1.0-beta.292 source; see
    /// the `source` field in the fixture.
    #[test]
    fn copy_matches_the_reference_selection_text() {
        let oracle: serde_json::Value =
            serde_json::from_str(include_str!("testdata/terminal-copy.json")).unwrap();
        for case in oracle["cases"].as_array().unwrap() {
            for chunk_size in [1, 7, usize::MAX] {
                let cols = case["cols"].as_u64().unwrap() as u16;
                let rows = case["rows"].as_u64().unwrap() as u16;
                let scrollback = case["scrollback"].as_u64().unwrap_or(10) as usize;
                let check = |buffer: &TerminalBuffer, selection: &serde_json::Value| {
                    let line = |key: &str| selection[key].as_i64().unwrap() as i32;
                    let column = |key: &str| selection[key].as_u64().unwrap() as usize;
                    assert_eq!(
                        text(
                            buffer,
                            line("startLine"),
                            column("startColumn"),
                            line("endLine"),
                            column("endColumn"),
                            selection["block"].as_bool().unwrap(),
                            false,
                        ),
                        selection["text"].as_str().unwrap(),
                        "{} / {}",
                        case["name"].as_str().unwrap(),
                        selection["name"].as_str().unwrap(),
                    );
                };
                if let Some(steps) = case["steps"].as_array() {
                    let mut buffer = TerminalBuffer::new(GridSize { rows, cols }, scrollback);
                    for step in steps {
                        if let Some(write) = step["write"].as_str() {
                            for bytes in write.as_bytes().chunks(chunk_size) {
                                buffer.feed(bytes);
                            }
                        } else if let Some(resize) = step["resize"].as_object() {
                            buffer.resize(GridSize {
                                rows: resize["rows"].as_u64().unwrap() as u16,
                                cols: resize["cols"].as_u64().unwrap() as u16,
                            });
                        } else if step.get("select").is_some() {
                            check(&buffer, &step["select"]);
                        }
                    }
                } else {
                    let mut buffer = TerminalBuffer::new(GridSize { rows, cols }, scrollback);
                    for write in case["writes"].as_array().unwrap() {
                        for bytes in write.as_str().unwrap().as_bytes().chunks(chunk_size) {
                            buffer.feed(bytes);
                        }
                    }
                    for selection in case["selections"].as_array().unwrap() {
                        check(&buffer, selection);
                    }
                }
            }
        }
    }
}
