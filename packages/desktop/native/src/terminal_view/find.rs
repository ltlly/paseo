//! Terminal search.
//!
//! The reference client searches with `@xterm/addon-search` 0.17.0-beta.213 as
//! `{ regex: false, caseSensitive: false }` (`terminal-emulator-runtime.ts`).
//! Search logical lines across soft wraps, keeping hard line breaks separate.
//! The addon uses UTF-16 string offsets; preserve its conversion to grid columns
//! even for combining marks, wide cells and supplementary-plane characters.
//! Decoration geometry and colours follow `DecorationManager.ts` (one-pixel
//! border, match vs active colour) with the ANSI yellow/blue the runtime passes.

use crate::terminal::TerminalBuffer;
use alacritty_terminal::term::cell::Flags;

/// Decoration limit. The addon defaults to 1000, but the app constructs it as
/// `new SearchAddon({ highlightLimit: FIND_HIGHLIGHT_LIMIT })` with 20_000
/// (`terminal-emulator-runtime.ts`), so the count and the `limited` flag have to
/// use the same number to match what the reference client reports.
pub const HIGHLIGHT_LIMIT: usize = 20_000;
/// Upper bound for one scan so a single-character query cannot pin unbounded
/// work. It matches the decoration limit, so a full result list is never
/// truncated below what the reference reports.
pub const MATCH_LIMIT: usize = HIGHLIGHT_LIMIT;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Match {
    /// Absolute grid line: `0` is the bottom of the history, positive lines are
    /// the visible screen.
    pub line: i32,
    pub column: usize,
    /// Columns covered by the match, wide characters counted as two.
    pub columns: usize,
}

impl Match {
    pub fn visible_row(&self, display_offset: usize) -> Option<usize> {
        let row = self.line + display_offset as i32;
        (row >= 0).then_some(row as usize)
    }

    /// Visible pieces of a wrapped decoration, clipped before painting.
    pub fn segments(
        self,
        cols: usize,
        rows: usize,
        display_offset: usize,
    ) -> impl Iterator<Item = Self> {
        let cols = cols as i64;
        let start = (i64::from(self.line) + display_offset as i64) * cols + self.column as i64;
        let end = start + self.columns as i64;
        let first = start.div_euclid(cols).clamp(0, rows as i64);
        let last = (end + cols - 1).div_euclid(cols).clamp(first, rows as i64);
        (first..last).map(move |row| {
            let from = start.max(row * cols);
            let to = end.min((row + 1) * cols);
            Self {
                line: row as i32,
                column: (from - row * cols) as usize,
                columns: (to - from) as usize,
            }
        })
    }
}

fn cell_units(buffer: &TerminalBuffer, line: i32, column: usize) -> usize {
    let Some(cell) = buffer.line_cell(line, column) else {
        return 1;
    };
    if cell.flags.contains(Flags::WIDE_CHAR_SPACER) {
        return 0;
    }
    cell.c.len_utf16()
        + cell
            .zerowidth()
            .unwrap_or_default()
            .iter()
            .map(|c| c.len_utf16())
            .sum::<usize>()
}

/// SearchEngine._stringLengthToBufferSize mutates its UTF-16 endpoint while
/// traversing cells. Rounding endpoints independently by glyph width differs
/// from the installed addon for endpoints inside an emoji or combining cell.
fn column_at(buffer: &TerminalBuffer, line: i32, offset: usize) -> usize {
    let mut offset = offset as i64;
    let mut column = 0;
    while (column as i64) < offset && column < usize::from(buffer.size.cols) {
        let units = cell_units(buffer, line, column);
        if units > 1 {
            offset -= units as i64 - 1;
        }
        if buffer
            .line_cell(line, column + 1)
            .is_some_and(|cell| cell.flags.contains(Flags::WIDE_CHAR_SPACER))
        {
            offset += 1;
        }
        column += 1;
    }
    offset.max(0) as usize
}

/// Every match of `query`, oldest line first.
pub fn find(buffer: &TerminalBuffer, query: &str, limit: usize) -> (Vec<Match>, bool) {
    if query.is_empty() {
        return (Vec::new(), false);
    }
    let query_units = query.encode_utf16().count();
    let folded_query = query.to_lowercase();
    let finder = memchr::memmem::Finder::new(folded_query.as_bytes());
    let mut found = Vec::new();
    let cols = usize::from(buffer.size.cols);
    let mut line = -(buffer.history_size() as i32);
    let mut text = String::new();
    let mut folded = String::new();
    let mut offsets = Vec::new();
    let mut cell_offsets = Vec::new();
    while line < i32::from(buffer.size.rows) {
        text.clear();
        offsets.clear();
        cell_offsets.clear();
        let first = line;
        let mut units = 0;
        let mut cell_units_before = 0;
        loop {
            offsets.push(units);
            cell_offsets.push(cell_units_before);
            cell_units_before += (0..cols)
                .map(|col| cell_units(buffer, line, col))
                .sum::<usize>();
            let wraps = buffer.line_continues(line) && line + 1 < i32::from(buffer.size.rows);
            let end = if wraps {
                cols
            } else {
                buffer
                    .line_written_trimmed(line)
                    .unwrap_or_else(|| buffer.line_trimmed_length(line))
            };
            // SearchLineCache omits the null filler before a wide glyph wraps.
            let end = if wraps
                && buffer
                    .line_cell(line, end.saturating_sub(1))
                    .is_some_and(|cell| cell.flags.contains(Flags::LEADING_WIDE_CHAR_SPACER))
            {
                end.saturating_sub(1)
            } else {
                end
            };
            let before = text.len();
            buffer.push_row_text(line, 0, end, &mut text);
            units += text[before..].encode_utf16().count();
            line += 1;
            if !wraps {
                break;
            }
        }
        folded.clear();
        if text.is_ascii() {
            folded.push_str(&text);
            folded.make_ascii_lowercase();
        } else {
            folded.push_str(&text.to_lowercase());
        }
        let mut start = 0;
        let mut start_units = 0;
        while let Some(relative) = finder.find(&folded.as_bytes()[start..]) {
            let index = start + relative;
            let index_units = start_units + folded[start..index].encode_utf16().count();
            let first_row = offsets
                .partition_point(|offset| *offset <= index_units)
                .saturating_sub(1);
            let end_units = index_units + query_units;
            let last_row = offsets
                .partition_point(|offset| *offset <= end_units)
                .saturating_sub(1);
            let column = column_at(
                buffer,
                first + first_row as i32,
                index_units - offsets[first_row],
            );
            let end_column = column_at(
                buffer,
                first + last_row as i32,
                end_units - offsets[last_row],
            );
            if found.len() >= limit.min(MATCH_LIMIT) {
                return (found, true);
            }
            found.push(Match {
                line: first + first_row as i32,
                column,
                columns: (end_column + cols * (last_row - first_row)).saturating_sub(column),
            });
            // The addon highlights overlapping hits, then starts at the next
            // row when col + the original UTF-16 query length reaches its edge.
            let (next_row, next_column) = if column + query_units >= cols {
                (first_row + 1, 0)
            } else {
                (first_row, column + 1)
            };
            if next_row >= offsets.len() {
                break;
            }
            let minimum: usize = cell_offsets[next_row]
                + (0..next_column)
                    .map(|col| cell_units(buffer, first + next_row as i32, col))
                    .sum::<usize>();
            if minimum <= index_units {
                break;
            }
            start = index;
            start_units = index_units;
            while start < folded.len() && start_units < minimum {
                let c = folded[start..]
                    .chars()
                    .next()
                    .expect("valid string boundary");
                start += c.len_utf8();
                start_units += c.len_utf16();
            }
        }
    }
    (found, false)
}

/// Index of the next match after `active`, wrapping. When nothing is active the
/// search starts at the top of the buffer.
/// The active match after a rescan triggered by new output.
///
/// A scan is only valid for the frame it ran on: buffer lines are absolute, so
/// new lines shift every stored line. Keeping the index keeps the highlighted
/// match on the same result as content scrolls past it, which is what a
/// marker-bound decoration does in the reference (`SearchAddon._updateMatches`
/// re-runs the search 200 ms after a write with `noScroll`).
///
/// Returns `None` when nothing matches any more.
pub fn reanchored(previous: Option<usize>, count: usize) -> Option<usize> {
    if count == 0 {
        return None;
    }
    previous.map(|index| index.min(count - 1))
}

pub fn step(matches: &[Match], active: Option<usize>, forward: bool) -> Option<usize> {
    if matches.is_empty() {
        return None;
    }
    let Some(active) = active.filter(|index| *index < matches.len()) else {
        return Some(if forward { 0 } else { matches.len() - 1 });
    };
    Some(if forward {
        (active + 1) % matches.len()
    } else {
        (active + matches.len() - 1) % matches.len()
    })
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
    fn installed_xterm_search_oracle() {
        let oracle: serde_json::Value =
            serde_json::from_str(include_str!("../testdata/terminal-find.json")).unwrap();
        for case in oracle["cases"].as_array().unwrap() {
            let buffer = terminal(
                case["rows"].as_u64().unwrap() as u16,
                case["cols"].as_u64().unwrap() as u16,
                case["input"].as_str().unwrap(),
            );
            let (matches, limited) = find(&buffer, case["query"].as_str().unwrap(), MATCH_LIMIT);
            let actual: Vec<_> = matches
                .iter()
                .map(|hit| [hit.line, hit.column as i32, hit.columns as i32])
                .collect();
            assert_eq!(
                serde_json::json!(actual),
                case["matches"],
                "{}",
                case["name"]
            );
            let expected: Vec<[i32; 3]> =
                serde_json::from_value(case["decorations"].clone()).unwrap();
            let expected: Vec<_> = expected
                .into_iter()
                .filter(|piece| piece[0] >= 0 && piece[0] < i32::from(buffer.size.rows))
                .collect();
            let actual: Vec<_> = matches
                .iter()
                .flat_map(|hit| {
                    hit.segments(
                        usize::from(buffer.size.cols),
                        usize::from(buffer.size.rows),
                        0,
                    )
                })
                .map(|piece| [piece.line, piece.column as i32, piece.columns as i32])
                .collect();
            assert_eq!(actual, expected, "{} decoration segments", case["name"]);
            assert!(!limited);
        }
    }

    #[test]
    fn search_is_case_insensitive_and_keeps_hard_breaks_separate() {
        let buffer = terminal(3, 20, "Paseo paseo\r\nPASEO\r\npaseo");
        let (found, limited) = find(&buffer, "paseo", MATCH_LIMIT);
        assert!(!limited);
        assert_eq!(
            found,
            [
                Match {
                    line: 0,
                    column: 0,
                    columns: 5
                },
                Match {
                    line: 0,
                    column: 6,
                    columns: 5
                },
                Match {
                    line: 1,
                    column: 0,
                    columns: 5
                },
                Match {
                    line: 2,
                    column: 0,
                    columns: 5
                },
            ]
        );
        assert!(find(&buffer, "Paseo\r\nPASEO", MATCH_LIMIT).0.is_empty());
        assert!(find(&buffer, "", MATCH_LIMIT).0.is_empty());
    }

    #[test]
    fn trailing_blanks_and_wide_spacers_keep_match_geometry() {
        let buffer = terminal(2, 12, "a中b  \r\nxx");
        let (found, _) = find(&buffer, "a中b", MATCH_LIMIT);
        assert_eq!(
            found,
            [Match {
                line: 0,
                column: 0,
                columns: 4
            }]
        );
        let (found, _) = find(&buffer, "b  ", MATCH_LIMIT);
        assert_eq!(
            found,
            [Match {
                line: 0,
                column: 3,
                columns: 3
            }],
            "explicitly printed spaces are retained by the addon"
        );
    }

    #[test]
    fn history_lines_are_searchable_and_map_to_visible_rows() {
        let mut buffer = terminal(2, 8, "first\r\nsecond\r\nthird\r\nfourth");
        assert_eq!(buffer.history_size(), 2);
        let (found, _) = find(&buffer, "first", MATCH_LIMIT);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].line, -2);
        assert_eq!(found[0].visible_row(0), None);
        assert_eq!(found[0].visible_row(2), Some(0));
        buffer.scroll_to(2);
        assert_eq!(found[0].visible_row(buffer.display_offset()), Some(0));
    }

    #[test]
    fn result_limits_and_navigation_wrap() {
        let buffer = terminal(1, 8, "aaaa");
        let (found, limited) = find(&buffer, "aa", 1);
        assert_eq!(found.len(), 1);
        assert!(limited);
        assert_eq!(step(&found, None, true), Some(0));
        assert_eq!(step(&found, Some(0), true), Some(0));
        let (found, _) = find(&buffer, "a", MATCH_LIMIT);
        assert_eq!(step(&found, Some(3), true), Some(0));
        assert_eq!(step(&found, Some(0), false), Some(3));
        assert_eq!(step(&[], Some(0), true), None);
    }

    #[test]
    fn rescanning_keeps_the_active_match() {
        assert_eq!(reanchored(Some(2), 5), Some(2));
        assert_eq!(reanchored(Some(9), 5), Some(4), "clamped to the last match");
        assert_eq!(reanchored(None, 5), None);
        assert_eq!(reanchored(Some(0), 0), None, "query stopped matching");
    }

    #[test]
    fn wrapped_decorations_clip_both_edges_of_the_viewport() {
        let hit = Match {
            line: -2,
            column: 3,
            columns: 20,
        };
        assert_eq!(
            hit.segments(5, 2, 0).collect::<Vec<_>>(),
            [
                Match {
                    line: 0,
                    column: 0,
                    columns: 5
                },
                Match {
                    line: 1,
                    column: 0,
                    columns: 5
                },
            ]
        );
        assert_eq!(
            hit.segments(5, 2, 2).collect::<Vec<_>>(),
            [
                Match {
                    line: 0,
                    column: 3,
                    columns: 2
                },
                Match {
                    line: 1,
                    column: 0,
                    columns: 5
                },
            ]
        );
        assert_eq!(
            Match {
                line: 2,
                column: 0,
                columns: 4
            }
            .segments(5, 2, 0)
            .count(),
            0
        );
    }
}
