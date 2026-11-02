//! Terminal links.
//!
//! Two providers, in the reference client's priority order:
//!
//! 1. OSC 8 hyperlinks carried by the grid. Only `http`/`https` links are
//!    activatable, because the application installs a link handler without
//!    `allowNonHttpProtocols`.
//! 2. The strict URL scan from `@xterm/addon-web-links` 0.13.0-beta.213:
//!    `(https?|HTTPS?)` followed by `//`, a run of characters that excludes
//!    quotes and the rfc 1738 unsafe set, and a final character that is not
//!    sentence punctuation or a bracket. The candidate then has to parse as an
//!    absolute URL with a host.
//!
//! The cases in `url_scan_uses_the_addon_character_classes_and_final_character_rule`
//! were checked against the installed addon's own `strictUrlRegex` and `isUrl`
//! (`WebLinksAddon.ts:21` and `WebLinkProvider.ts:52`, run under node 26).
//!
//! A candidate line is the hovered row joined with its wrapped neighbours, which
//! is how the addon finds URLs that wrap. Text and the character-to-column map
//! are built once per hovered row, never per cell or per frame.

use std::ops::Range;

use crate::terminal::TerminalBuffer;

/// A link covering a range of visible cells. `end` is inclusive.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Link {
    /// What activation opens. For a URL scan this is the matched text; for an
    /// OSC 8 hyperlink it is the link target, which is what xterm hands its
    /// `linkHandler` as `ILink.text` and is not the visible cells.
    pub text: String,
    pub start: (usize, usize),
    pub end: (usize, usize),
}

impl Link {
    pub fn contains(&self, row: usize, column: usize) -> bool {
        self.start <= (row, column) && (row, column) <= self.end
    }
}

/// Longest joined candidate line the addon will build.
const MAX_LINE: usize = 2048;

/// Characters that end a URL candidate: whitespace, quotes and the rfc
/// 3986/1738 unsafe set.
const STOP: &str = "\"'!*(){}|\\^<>`";
/// The final character additionally excludes sentence punctuation and brackets.
const FINAL_STOP: &str = "\"':,.!?{}|\\^~[]`()<>";

/// An absolute http(s) URL with an authority and a host, which is what
/// `new URL()` plus the addon's prefix check accepts.
pub fn is_url(text: &str) -> bool {
    let lower = text.to_ascii_lowercase();
    let Some(authority) = lower
        .strip_prefix("http://")
        .or_else(|| lower.strip_prefix("https://"))
    else {
        return false;
    };
    let authority = authority.split(['/', '?', '#']).next().unwrap_or_default();
    let host_port = match authority.rfind('@') {
        Some(at) => &authority[at + 1..],
        None => authority,
    };
    if host_port.is_empty() {
        return false;
    }
    if host_port.starts_with('[') {
        let Some(close) = host_port.find(']') else {
            return false;
        };
        if close == 1 {
            return false;
        }
        let rest = &host_port[close + 1..];
        return rest.is_empty() || rest.strip_prefix(':').is_some_and(valid_port);
    }
    let (host, port) = match host_port.rfind(':') {
        Some(colon) => (&host_port[..colon], Some(&host_port[colon + 1..])),
        None => (host_port, None),
    };
    !host.is_empty() && !host.contains(['[', ']']) && port.is_none_or(valid_port)
}

fn valid_port(port: &str) -> bool {
    port.is_empty() || port.chars().all(|c| c.is_ascii_digit())
}

/// Match positions of the reference URL scan. Offsets are character indices.
pub fn urls(text: &str) -> Vec<Range<usize>> {
    let chars: Vec<char> = text.chars().collect();
    let mut ranges = Vec::new();
    let mut index = 0;
    while index < chars.len() {
        let Some(after_scheme) = scheme_end(&chars, index) else {
            index += 1;
            continue;
        };
        let mut end = after_scheme;
        while end < chars.len() && !chars[end].is_whitespace() && !STOP.contains(chars[end]) {
            end += 1;
        }
        while end > after_scheme
            && (chars[end - 1].is_whitespace() || FINAL_STOP.contains(chars[end - 1]))
        {
            end -= 1;
        }
        if end == after_scheme {
            index += 1;
            continue;
        }
        let candidate: String = chars[index..end].iter().collect();
        if is_url(&candidate) {
            ranges.push(index..end);
            index = end;
        } else {
            index += 1;
        }
    }
    ranges
}

/// Length of `(https?|HTTPS?)//` at `start`, in characters.
fn scheme_end(chars: &[char], start: usize) -> Option<usize> {
    const SCHEMES: [&str; 4] = ["http", "https", "HTTP", "HTTPS"];
    for scheme in SCHEMES {
        let length = scheme.chars().count();
        if start + length + 3 > chars.len() {
            continue;
        }
        let matches = scheme
            .chars()
            .zip(&chars[start..start + length])
            .all(|(expected, actual)| expected == *actual);
        let slashes = &chars[start + length..start + length + 3];
        if matches && slashes == [':', '/', '/'] {
            return Some(start + length + 3);
        }
    }
    None
}

struct WindowRow {
    row: usize,
    text: String,
    columns: Vec<usize>,
}

fn row_text(buffer: &TerminalBuffer, row: usize) -> Option<WindowRow> {
    let mut text = String::new();
    let mut columns = Vec::new();
    for (column, character) in buffer.row_chars(row)? {
        text.push(character);
        columns.push(column);
    }
    Some(WindowRow { row, text, columns })
}

fn window_length(rows: &[WindowRow]) -> usize {
    rows.iter().map(|row| row.text.chars().count()).sum()
}

/// The hovered row joined with its wrapped neighbours.
fn window(buffer: &TerminalBuffer, row: usize) -> Vec<WindowRow> {
    let Some(current) = row_text(buffer, row) else {
        return Vec::new();
    };
    let expand_up = buffer.row_continues(row) && !current.text.starts_with(' ');
    let mut rows = vec![current];

    if expand_up {
        while window_length(&rows) < MAX_LINE {
            let top = rows[0].row;
            if !buffer.row_continues(top) || top == 0 {
                break;
            }
            let Some(previous) = row_text(buffer, top - 1) else {
                break;
            };
            let stop = !buffer.row_continues(top - 1) || previous.text.contains(' ');
            rows.insert(0, previous);
            if stop {
                break;
            }
        }
    }

    while window_length(&rows) < MAX_LINE {
        let bottom = rows.last().expect("window has a row").row;
        if !buffer.row_continues(bottom + 1) {
            break;
        }
        let Some(next) = row_text(buffer, bottom + 1) else {
            break;
        };
        let stop = next.text.contains(' ');
        rows.push(next);
        if stop {
            break;
        }
    }
    rows
}

/// Every URL on the row, including ones that wrap into neighbouring rows.
pub fn urls_at(buffer: &TerminalBuffer, row: usize) -> Vec<Link> {
    let rows = window(buffer, row);
    if rows.is_empty() {
        return Vec::new();
    }
    let text: String = rows.iter().map(|row| row.text.as_str()).collect();
    urls(&text)
        .into_iter()
        .filter_map(|range| map_range(&rows, &text, range))
        .collect()
}

fn map_range(rows: &[WindowRow], text: &str, range: Range<usize>) -> Option<Link> {
    let start = position(rows, range.start)?;
    let end = position(rows, range.end.checked_sub(1)?)?;
    Some(Link {
        text: text.chars().skip(range.start).take(range.len()).collect(),
        start,
        end,
    })
}

fn position(rows: &[WindowRow], index: usize) -> Option<(usize, usize)> {
    let mut remaining = index;
    for row in rows {
        let length = row.text.chars().count();
        if remaining < length {
            return Some((row.row, *row.columns.get(remaining)?));
        }
        remaining -= length;
    }
    None
}

fn http_uri(link: &alacritty_terminal::term::cell::Hyperlink) -> bool {
    let uri = link.uri();
    uri.get(..8)
        .is_some_and(|s| s.eq_ignore_ascii_case("https://"))
        || uri
            .get(..7)
            .is_some_and(|s| s.eq_ignore_ascii_case("http://"))
}

/// The OSC 8 link covering a cell, with its wrapped range.
pub fn hyperlink_at(buffer: &TerminalBuffer, row: usize, column: usize) -> Option<Link> {
    let link = buffer.row_hyperlink(row, column)?;
    if !http_uri(&link) {
        return None;
    }
    let same =
        |row: usize, column: usize| buffer.row_hyperlink(row, column).as_ref() == Some(&link);

    let (mut start_row, mut start_col) = (row, column);
    while start_col > 0 && same(start_row, start_col - 1) {
        start_col -= 1;
    }
    while start_col == 0 && start_row > 0 && buffer.row_continues(start_row) {
        let previous = start_row - 1;
        let length = buffer.row_trimmed_length(previous);
        if length == 0 || !same(previous, length - 1) {
            break;
        }
        start_row = previous;
        start_col = length - 1;
        while start_col > 0 && same(start_row, start_col - 1) {
            start_col -= 1;
        }
    }

    let (mut end_row, mut end_col) = (row, column);
    while same(end_row, end_col + 1) {
        end_col += 1;
    }
    while end_col + 1 >= buffer.row_trimmed_length(end_row) && buffer.row_continues(end_row + 1) {
        let next = end_row + 1;
        if !same(next, 0) {
            break;
        }
        end_row = next;
        end_col = 0;
        while same(end_row, end_col + 1) {
            end_col += 1;
        }
    }

    Some(Link {
        text: link.uri().to_owned(),
        start: (start_row, start_col),
        end: (end_row, end_col),
    })
}

/// The link under a cell, OSC 8 first like the reference provider order.
pub fn link_at(buffer: &TerminalBuffer, row: usize, column: usize) -> Option<Link> {
    if let Some(link) = hyperlink_at(buffer, row, column) {
        return Some(link);
    }
    urls_at(buffer, row)
        .into_iter()
        .find(|link| link.contains(row, column))
}

/// Whether releasing a left press over a link opens it.
///
/// The reference linkifier opens whenever the press and the release are inside
/// the same link, with no check on click count or movement. That consumes the
/// word and line selection gestures on any cell a URL covers, so this app
/// activates only on a plain single click that neither dragged nor turned into
/// a word/line selection. Drags and repeated clicks keep selecting.
pub fn activates(click_count: usize, dragged: bool, gesture: bool, pressed_on_link: bool) -> bool {
    click_count == 1 && !dragged && !gesture && pressed_on_link
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
    fn url_scan_uses_the_addon_character_classes_and_final_character_rule() {
        let cases = [
            ("https://paseo.sh", vec!["https://paseo.sh"]),
            (
                "see http://example.com/a?b=c#d ok",
                vec!["http://example.com/a?b=c#d"],
            ),
            ("(https://paseo.sh)", vec!["https://paseo.sh"]),
            ("https://paseo.sh.", vec!["https://paseo.sh"]),
            ("https://paseo.sh/a,b", vec!["https://paseo.sh/a,b"]),
            ("<https://paseo.sh>", vec!["https://paseo.sh"]),
            ("HTTPS://PASEO.SH/x", vec!["HTTPS://PASEO.SH/x"]),
            ("Https://paseo.sh", vec![]),
            ("https:/paseo.sh", vec![]),
            ("https://", vec![]),
            ("https:// paseo.sh", vec![]),
            (
                "http://user:pw@host:8080/path",
                vec!["http://user:pw@host:8080/path"],
            ),
            ("http://host:notaport/x", vec![]),
            ("http://[::1]:80/x", vec!["http://[::1]:80/x"]),
            ("xhttps://paseo.sh", vec!["https://paseo.sh"]),
            (
                "https://a.sh https://b.sh",
                vec!["https://a.sh", "https://b.sh"],
            ),
            ("https://paseo.sh/~user/", vec!["https://paseo.sh/~user/"]),
            ("https://paseo.sh/[x]", vec!["https://paseo.sh/[x"]),
        ];
        for (text, expected) in cases {
            let found: Vec<String> = urls(text)
                .into_iter()
                .map(|range| text.chars().skip(range.start).take(range.len()).collect())
                .collect();
            assert_eq!(found, expected, "{text:?}");
        }
    }

    #[test]
    fn urls_map_to_cells_and_follow_wrapped_rows() {
        let buffer = terminal(1, 30, "open https://paseo.sh/docs now");
        let links = urls_at(&buffer, 0);
        assert_eq!(links.len(), 1);
        assert_eq!(links[0].text, "https://paseo.sh/docs");
        assert_eq!(links[0].start, (0, 5));
        assert_eq!(links[0].end, (0, 25));

        let wrapped = terminal(3, 10, "https://paseo.sh/docs");
        assert_eq!(urls_at(&wrapped, 0)[0].end, (2, 0));
        let link = link_at(&wrapped, 1, 4).expect("link covers the wrapped row");
        assert_eq!(link.text, "https://paseo.sh/docs");
        assert_eq!(link.start, (0, 0));
        assert_eq!(link.end, (2, 0));
        assert!(link_at(&wrapped, 2, 1).is_none());
    }

    #[test]
    fn osc8_links_are_http_only_and_clamped_to_their_cells() {
        let buffer = terminal(
            2,
            16,
            "\x1b]8;;https://paseo.sh\x07link\x1b]8;;\x07 \x1b]8;;file:///etc/passwd\x07file\x1b]8;;\x07",
        );
        let link = link_at(&buffer, 0, 1).expect("http link");
        assert_eq!(
            link.text, "https://paseo.sh",
            "activation opens the OSC 8 target, not the visible cells"
        );
        assert_eq!(link.start, (0, 0));
        assert_eq!(link.end, (0, 3));
        assert!(link_at(&buffer, 0, 4).is_none());
        assert!(
            link_at(&buffer, 0, 6).is_none(),
            "file: links are not activatable"
        );
    }

    #[test]
    fn osc8_links_span_wrapped_rows() {
        let buffer = terminal(3, 6, "\x1b]8;;https://paseo.sh/x\x07abcdefghij\x1b]8;;\x07");
        let link = link_at(&buffer, 1, 1).expect("wrapped hyperlink");
        assert_eq!(link.start, (0, 0));
        assert_eq!(link.end, (1, 3));
        assert_eq!(link.text, "https://paseo.sh/x");
    }

    #[test]
    fn only_a_plain_single_click_activates_a_link() {
        assert!(activates(1, false, false, true));
        assert!(!activates(2, false, false, true), "word selection");
        assert!(!activates(3, false, false, true), "line selection");
        assert!(!activates(1, true, false, true), "drag selection");
        assert!(!activates(1, false, true, true), "gesture selection");
        assert!(!activates(1, false, false, false), "press missed the link");
    }
}
